//! Central ownership tracking for physical resources, DMA memory and kernel MMIO VA.
//!
//! This module deliberately sits on top of the existing PageManager instead of
//! replacing it. Normal VM/user allocations may keep using paging.rs; hardware
//! drivers must use this layer for DMA and device mappings so every range has an
//! owner and overlap checks happen in one place.

use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering, compiler_fence, fence};

use interrupt_sync::{SpinMutex, without_interrupts};

use crate::memory::paging::{
    KERNEL_MMIO_BASE, KERNEL_MMIO_END, PAGE_SIZE, PAGING, PTEFlags, PhysAddr, VirtAddr,
    detected_ram_bytes, phys_to_virt,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Free,
    Reserved,
    Kernel,
    Mmio,
    Framebuffer,
    Dma,
    Acpi,
    Firmware,
}

impl ResourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Free => "free",
            ResourceKind::Reserved => "reserved",
            ResourceKind::Kernel => "kernel",
            ResourceKind::Mmio => "mmio",
            ResourceKind::Framebuffer => "framebuffer",
            ResourceKind::Dma => "dma",
            ResourceKind::Acpi => "acpi",
            ResourceKind::Firmware => "firmware",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysRange {
    pub start: u64,
    pub size: u64,
    pub kind: ResourceKind,
    pub owner: &'static str,
}

impl PhysRange {
    pub fn end(self) -> Result<u64, ResourceError> {
        self.start
            .checked_add(self.size)
            .ok_or(ResourceError::Overflow)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceError {
    ZeroSize,
    InvalidAlignment,
    Overflow,
    AddressTooHigh,
    OutOfMemory,
    NotFound,
    MappingFailed(&'static str),
    Overlap {
        requested: PhysRange,
        existing: PhysRange,
    },
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ResourceError::ZeroSize => write!(f, "zero-sized resource"),
            ResourceError::InvalidAlignment => write!(f, "alignment is not a power of two"),
            ResourceError::Overflow => write!(f, "resource range overflow"),
            ResourceError::AddressTooHigh => {
                write!(f, "physical address is not addressable on i386")
            }
            ResourceError::OutOfMemory => write!(f, "no suitable physical/virtual range"),
            ResourceError::NotFound => write!(f, "resource mapping not found"),
            ResourceError::MappingFailed(e) => write!(f, "page mapping failed: {}", e),
            ResourceError::Overlap {
                requested,
                existing,
            } => write!(
                f,
                "overlap: {} [{:#x}..{:#x}) conflicts with {} [{:#x}..{:#x})",
                requested.owner,
                requested.start,
                requested.start.saturating_add(requested.size),
                existing.owner,
                existing.start,
                existing.start.saturating_add(existing.size),
            ),
        }
    }
}

pub struct ResourceManager {
    ranges: Vec<PhysRange>,
}

impl ResourceManager {
    pub const fn new() -> Self {
        Self { ranges: Vec::new() }
    }

    pub fn ranges(&self) -> &[PhysRange] {
        &self.ranges
    }

    pub fn is_free(&self, start: u64, size: u64) -> bool {
        let Some(end) = start.checked_add(size) else {
            return false;
        };
        if size == 0 {
            return false;
        }
        !self.ranges.iter().any(|r| {
            let r_end = r.start.saturating_add(r.size);
            start < r_end && r.start < end
        })
    }

    pub fn reserve_range(
        &mut self,
        start: u64,
        size: u64,
        kind: ResourceKind,
        owner: &'static str,
    ) -> Result<(), ResourceError> {
        if size == 0 {
            return Err(ResourceError::ZeroSize);
        }
        let end = start.checked_add(size).ok_or(ResourceError::Overflow)?;
        let requested = PhysRange {
            start,
            size,
            kind,
            owner,
        };

        for &existing in &self.ranges {
            let existing_end = existing.end()?;
            if start < existing_end && existing.start < end {
                // Exact idempotent reservation is useful when a probe path runs
                // twice but still refers to the same firmware-owned BAR.
                if existing.start == start
                    && existing.size == size
                    && existing.kind == kind
                    && existing.owner == owner
                {
                    return Ok(());
                }
                return Err(ResourceError::Overlap {
                    requested,
                    existing,
                });
            }
        }

        self.ranges.push(requested);
        self.ranges.sort_unstable_by_key(|r| r.start);
        Ok(())
    }

    pub fn release_range(&mut self, start: u64, size: u64) -> Result<PhysRange, ResourceError> {
        if let Some(index) = self
            .ranges
            .iter()
            .position(|r| r.start == start && r.size == size)
        {
            Ok(self.ranges.remove(index))
        } else {
            Err(ResourceError::NotFound)
        }
    }

    pub fn alloc_phys(
        &mut self,
        size: usize,
        align: usize,
        kind: ResourceKind,
        owner: &'static str,
    ) -> Result<PhysAddr, ResourceError> {
        self.alloc_phys_below(u32::MAX as u64, size, align, kind, owner)
    }

    /// Allocate contiguous physical memory whose final byte is <= `max_addr`.
    pub fn alloc_phys_below(
        &mut self,
        max_addr: u64,
        size: usize,
        align: usize,
        kind: ResourceKind,
        owner: &'static str,
    ) -> Result<PhysAddr, ResourceError> {
        if size == 0 {
            return Err(ResourceError::ZeroSize);
        }
        if align == 0 || !align.is_power_of_two() {
            return Err(ResourceError::InvalidAlignment);
        }

        let page = PAGE_SIZE as u64;
        let alloc_size = align_up_u64(size as u64, page)?;
        let alloc_align = align.max(PAGE_SIZE);
        if !alloc_align.is_power_of_two() {
            return Err(ResourceError::InvalidAlignment);
        }

        let max_addr = max_addr.min(u32::MAX as u64);
        let limit_exclusive = max_addr.checked_add(1).ok_or(ResourceError::Overflow)?;
        let max_page_exclusive = (limit_exclusive / page) as u32;
        let pages = (alloc_size / page) as u32;
        let align_pages = (alloc_align / PAGE_SIZE) as u32;

        let first_page = without_interrupts(|| unsafe {
            let mut paging = PAGING.lock();
            paging
                .alloc_contiguous_frames_aligned_below(pages, align_pages, max_page_exclusive)
                .map_err(|_| ResourceError::OutOfMemory)
        })?;
        let phys = (first_page as u64) << 12;

        if let Err(e) = self.reserve_range(phys, alloc_size, kind, owner) {
            without_interrupts(|| unsafe {
                let mut paging = PAGING.lock();
                for frame in first_page..first_page + pages {
                    paging.free_phys_frame(frame);
                }
            });
            return Err(e);
        }

        Ok(PhysAddr(phys as u32))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VirtualRange {
    start: u32,
    size: u32,
    owner: &'static str,
}

pub struct VirtualRangeAllocator {
    base: u32,
    end: u32,
    ranges: Vec<VirtualRange>,
}

impl VirtualRangeAllocator {
    pub const fn new(base: u32, end: u32) -> Self {
        Self {
            base,
            end,
            ranges: Vec::new(),
        }
    }

    pub fn alloc_kernel_virtual(
        &mut self,
        size: usize,
        align: usize,
        owner: &'static str,
    ) -> Result<VirtAddr, ResourceError> {
        if size == 0 {
            return Err(ResourceError::ZeroSize);
        }
        if align == 0 || !align.is_power_of_two() {
            return Err(ResourceError::InvalidAlignment);
        }

        let size = align_up_u64(size as u64, PAGE_SIZE as u64)?;
        if size > u32::MAX as u64 {
            return Err(ResourceError::Overflow);
        }
        let size = size as u32;
        let align = align.max(PAGE_SIZE) as u32;
        let mut cursor = align_up_u32(self.base, align)?;

        self.ranges.sort_unstable_by_key(|r| r.start);
        for r in &self.ranges {
            let candidate_end = cursor.checked_add(size).ok_or(ResourceError::Overflow)?;
            if candidate_end <= r.start {
                self.ranges.push(VirtualRange {
                    start: cursor,
                    size,
                    owner,
                });
                self.ranges.sort_unstable_by_key(|x| x.start);
                return Ok(VirtAddr(cursor));
            }
            let r_end = r.start.checked_add(r.size).ok_or(ResourceError::Overflow)?;
            if cursor < r_end {
                cursor = align_up_u32(r_end, align)?;
            }
        }

        let candidate_end = cursor.checked_add(size).ok_or(ResourceError::Overflow)?;
        if candidate_end > self.end {
            return Err(ResourceError::OutOfMemory);
        }
        self.ranges.push(VirtualRange {
            start: cursor,
            size,
            owner,
        });
        self.ranges.sort_unstable_by_key(|r| r.start);
        Ok(VirtAddr(cursor))
    }

    pub fn free_kernel_virtual(
        &mut self,
        start: VirtAddr,
        size: usize,
    ) -> Result<(), ResourceError> {
        let size = align_up_u64(size as u64, PAGE_SIZE as u64)? as u32;
        if let Some(index) = self
            .ranges
            .iter()
            .position(|r| r.start == start.0 && r.size == size)
        {
            self.ranges.remove(index);
            Ok(())
        } else {
            Err(ResourceError::NotFound)
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct IoMapRecord {
    returned: VirtAddr,
    map_base: VirtAddr,
    map_size: u32,
    phys_base: u32,
    owner: &'static str,
}

#[derive(Debug)]
pub struct MmioMapping {
    pub virt: VirtAddr,
    phys: u64,
    size: usize,
    mapped: bool,
    reserved: bool,
}

impl MmioMapping {
    pub const fn as_usize(&self) -> usize {
        self.virt.0 as usize
    }

    fn release_owned(&mut self) -> Result<(), ResourceError> {
        if self.mapped {
            iounmap(self.virt)?;
            self.mapped = false;
        }
        if self.reserved {
            let _ = release_range(self.phys, self.size as u64)?;
            self.reserved = false;
        }
        Ok(())
    }
}

impl Drop for MmioMapping {
    fn drop(&mut self) {
        let _ = self.release_owned();
    }
}

#[derive(Debug)]
pub struct DmaBuffer {
    pub phys: PhysAddr,
    pub virt: VirtAddr,
    pub len: usize,
    alloc_len: usize,
    owner: &'static str,
    released: bool,
}

/// PCI DMA uses the cache-coherent write-back direct mapping on x86.
/// Order descriptor/payload accesses explicitly; WBINVD is neither necessary
/// nor an ownership protocol, and SSE2 fences are unavailable on older CPUs.
#[inline]
pub fn dma_wmb() { compiler_fence(Ordering::Release); }
#[inline]
pub fn dma_rmb() { compiler_fence(Ordering::Acquire); }
#[inline]
pub fn dma_mb() { fence(Ordering::SeqCst); }

impl DmaBuffer {
    /// Translate an address inside this owned DMA allocation to its bus-visible
    /// physical address. This intentionally replaces ad-hoc `virt-KERNEL_OFFSET`
    /// arithmetic in drivers and rejects pointers outside the allocation.
    pub fn phys_of(&self, ptr: *const u8, len: usize) -> Result<PhysAddr, ResourceError> {
        let base = self.virt.0 as usize;
        let addr = ptr as usize;
        let offset = addr.checked_sub(base).ok_or(ResourceError::NotFound)?;
        let end = offset.checked_add(len).ok_or(ResourceError::Overflow)?;
        if end > self.len {
            return Err(ResourceError::NotFound);
        }
        let phys = (self.phys.0 as u64)
            .checked_add(offset as u64)
            .ok_or(ResourceError::Overflow)?;
        if phys > u32::MAX as u64 {
            return Err(ResourceError::Overflow);
        }
        Ok(PhysAddr(phys as u32))
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.virt.0 as *mut u8
    }

    pub const fn allocated_len(&self) -> usize {
        self.alloc_len
    }

    fn release_owned(&mut self) -> Result<(), ResourceError> {
        if self.released {
            return Ok(());
        }

        let pages = self.alloc_len / PAGE_SIZE;
        let first = self.phys.0 >> 12;
        let released = release_range(self.phys.0 as u64, self.alloc_len as u64)?;
        self.released = true;

        without_interrupts(|| unsafe {
            let mut paging = PAGING.lock();
            for frame in first..first + pages as u32 {
                paging.free_phys_frame(frame);
            }
        });
        crate::println!(
            "[dma] free {:<16} phys={:08x} size={}",
            self.owner,
            self.phys.0,
            self.alloc_len
        );

        if released.kind != ResourceKind::Dma {
            Err(ResourceError::NotFound)
        } else {
            Ok(())
        }
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        let _ = self.release_owned();
    }
}

pub static RESOURCE_MANAGER: SpinMutex<ResourceManager> = SpinMutex::new(ResourceManager::new());
static KERNEL_VA: SpinMutex<VirtualRangeAllocator> = SpinMutex::new(VirtualRangeAllocator::new(
    KERNEL_MMIO_BASE,
    KERNEL_MMIO_END,
));
static IO_MAPPINGS: SpinMutex<Vec<IoMapRecord>> = SpinMutex::new(Vec::new());
static INITIALIZED: AtomicBool = AtomicBool::new(false);

fn align_up_u64(value: u64, align: u64) -> Result<u64, ResourceError> {
    if align == 0 || !align.is_power_of_two() {
        return Err(ResourceError::InvalidAlignment);
    }
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .ok_or(ResourceError::Overflow)
}

fn align_up_u32(value: u32, align: u32) -> Result<u32, ResourceError> {
    if align == 0 || !align.is_power_of_two() {
        return Err(ResourceError::InvalidAlignment);
    }
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .ok_or(ResourceError::Overflow)
}

fn log_overlap(error: ResourceError) {
    if let ResourceError::Overlap {
        requested,
        existing,
    } = error
    {
        crate::println!(
            "[mem] OVERLAP {} {:#010x}..{:#010x} with {} {:#010x}..{:#010x}",
            requested.owner,
            requested.start,
            requested.start.saturating_add(requested.size),
            existing.owner,
            existing.start,
            existing.start.saturating_add(existing.size),
        );
    }
}

pub fn reserve_range(
    start: u64,
    size: u64,
    kind: ResourceKind,
    owner: &'static str,
) -> Result<(), ResourceError> {
    let result = RESOURCE_MANAGER
        .lock()
        .reserve_range(start, size, kind, owner);
    match result {
        Ok(()) => {
            crate::println!(
                "[mem] reserve {:<11} phys={:08x}..{:08x} owner={}",
                kind.as_str(),
                start,
                start.saturating_add(size),
                owner
            );
            Ok(())
        }
        Err(e) => {
            log_overlap(e);
            Err(e)
        }
    }
}

pub fn release_range(start: u64, size: u64) -> Result<PhysRange, ResourceError> {
    RESOURCE_MANAGER.lock().release_range(start, size)
}

pub fn is_free(start: u64, size: u64) -> bool {
    RESOURCE_MANAGER.lock().is_free(start, size)
}

pub fn alloc_phys(
    size: usize,
    align: usize,
    kind: ResourceKind,
    owner: &'static str,
) -> Result<PhysAddr, ResourceError> {
    let result = RESOURCE_MANAGER.lock().alloc_phys(size, align, kind, owner);
    if let Err(e) = result {
        log_overlap(e);
    }
    result
}

pub fn alloc_phys_below(
    max_addr: u64,
    size: usize,
    align: usize,
    kind: ResourceKind,
    owner: &'static str,
) -> Result<PhysAddr, ResourceError> {
    let result = RESOURCE_MANAGER
        .lock()
        .alloc_phys_below(max_addr, size, align, kind, owner);
    if let Err(e) = result {
        log_overlap(e);
    }
    result
}

pub fn alloc_kernel_virtual(
    size: usize,
    align: usize,
    owner: &'static str,
) -> Result<VirtAddr, ResourceError> {
    KERNEL_VA.lock().alloc_kernel_virtual(size, align, owner)
}

pub fn free_kernel_virtual(virt: VirtAddr, size: usize) -> Result<(), ResourceError> {
    KERNEL_VA.lock().free_kernel_virtual(virt, size)
}

/// Map a physical device range uncached into the central kernel MMIO window.
/// This function only maps; callers reserve physical ownership separately.
pub fn ioremap(phys: u64, size: usize, owner: &'static str) -> Result<VirtAddr, ResourceError> {
    if size == 0 {
        return Err(ResourceError::ZeroSize);
    }
    if phys > u32::MAX as u64 {
        return Err(ResourceError::AddressTooHigh);
    }

    let page_mask = (PAGE_SIZE as u64) - 1;
    let phys_base = phys & !page_mask;
    let offset = phys - phys_base;
    let span = (size as u64)
        .checked_add(offset)
        .ok_or(ResourceError::Overflow)?;
    let map_size = align_up_u64(span, PAGE_SIZE as u64)?;
    if phys_base
        .checked_add(map_size)
        .ok_or(ResourceError::Overflow)?
        > (u32::MAX as u64) + 1
    {
        return Err(ResourceError::AddressTooHigh);
    }

    let map_base = alloc_kernel_virtual(map_size as usize, PAGE_SIZE, owner)?;
    let flags = PTEFlags::new()
        .present()
        .writable()
        .write_through()
        .cache_disable();

    let map_result = without_interrupts(|| unsafe {
        let mut paging = PAGING.lock();
        paging.map_physical_range(phys_base as u32, map_size as u32, map_base.0, flags)
    });
    if let Err(e) = map_result {
        let _ = free_kernel_virtual(map_base, map_size as usize);
        return Err(ResourceError::MappingFailed(e));
    }

    let returned = VirtAddr(
        map_base
            .0
            .checked_add(offset as u32)
            .ok_or(ResourceError::Overflow)?,
    );
    IO_MAPPINGS.lock().push(IoMapRecord {
        returned,
        map_base,
        map_size: map_size as u32,
        phys_base: phys_base as u32,
        owner,
    });

    crate::println!(
        "[mmio] {:<20} phys={:08x} virt={:08x} size={:#x}",
        owner,
        phys,
        returned.0,
        size
    );
    Ok(returned)
}

pub fn iounmap(virt: VirtAddr) -> Result<(), ResourceError> {
    let record = {
        let mut maps = IO_MAPPINGS.lock();
        let Some(index) = maps.iter().position(|m| m.returned == virt) else {
            return Err(ResourceError::NotFound);
        };
        maps.remove(index)
    };

    without_interrupts(|| unsafe {
        let mut paging = PAGING.lock();
        let mut off = 0u32;
        while off < record.map_size {
            paging.dir.unmap(record.map_base.0 + off);
            off += PAGE_SIZE as u32;
        }
    });
    free_kernel_virtual(record.map_base, record.map_size as usize)?;
    crate::println!(
        "[mmio] unmap {:<18} phys={:08x} virt={:08x} size={:#x}",
        record.owner,
        record.phys_base,
        record.returned.0,
        record.map_size
    );
    Ok(())
}

pub fn reserve_and_ioremap(
    phys: u64,
    size: usize,
    kind: ResourceKind,
    owner: &'static str,
) -> Result<VirtAddr, ResourceError> {
    reserve_range(phys, size as u64, kind, owner)?;
    match ioremap(phys, size, owner) {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = release_range(phys, size as u64);
            Err(e)
        }
    }
}

/// Owning variant of reserve_and_ioremap(). The mapping and physical resource
/// reservation are released automatically if probe/initialization unwinds.
pub fn map_resource(
    phys: u64,
    size: usize,
    kind: ResourceKind,
    owner: &'static str,
) -> Result<MmioMapping, ResourceError> {
    let virt = reserve_and_ioremap(phys, size, kind, owner)?;
    Ok(MmioMapping {
        virt,
        phys,
        size,
        mapped: true,
        reserved: true,
    })
}

pub fn dma_alloc_for(
    owner: &'static str,
    size: usize,
    align: usize,
    max_phys: u64,
) -> Result<DmaBuffer, ResourceError> {
    let alloc_len = align_up_u64(size as u64, PAGE_SIZE as u64)? as usize;
    if size == 0 { return Err(ResourceError::ZeroSize); }
    let max_phys = max_phys.min(detected_ram_bytes() as u64 - 1);
    let phys = alloc_phys_below(max_phys, size, align, ResourceKind::Dma, owner)?;
    let virt = VirtAddr(phys_to_virt(phys.0));
    unsafe {
        core::ptr::write_bytes(virt.0 as *mut u8, 0, alloc_len);
    }
    crate::println!(
        "[dma] {:<21} phys={:08x} virt={:08x} size={} align={} max={:#x}",
        owner,
        phys.0,
        virt.0,
        size,
        align,
        max_phys
    );
    Ok(DmaBuffer {
        phys,
        virt,
        len: size,
        alloc_len,
        owner,
        released: false,
    })
}

pub fn dma_alloc(size: usize, align: usize, max_phys: u64) -> Result<DmaBuffer, ResourceError> {
    dma_alloc_for("dma", size, align, max_phys)
}

pub fn dma_free(mut buffer: DmaBuffer) -> Result<(), ResourceError> {
    buffer.release_owned()
}

pub fn dump_ranges() {
    let ranges = RESOURCE_MANAGER.lock();
    crate::println!(
        "[mem] physical resource map ({} ranges):",
        ranges.ranges().len()
    );
    for r in ranges.ranges() {
        crate::println!(
            "[mem] {:<11} {:08x}..{:08x} {}",
            r.kind.as_str(),
            r.start,
            r.start.saturating_add(r.size),
            r.owner
        );
    }
}

/// Register fixed Felix-owned memory. Device BARs/framebuffers are added later
/// when their authoritative firmware/PCI addresses are known.
pub fn init() -> Result<(), ResourceError> {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    // Low-memory boot ABI. Keep the individual pages visible in diagnostics
    // instead of hiding all of low memory behind one coarse reservation.
    reserve_range(
        0x0000_0000,
        0x0000_5000,
        ResourceKind::Firmware,
        "bios-lowmem",
    )?;
    reserve_range(
        0x0000_5000,
        0x0000_1000,
        ResourceKind::Reserved,
        "boot-fb-info",
    )?;
    reserve_range(
        0x0000_6000,
        0x0000_1000,
        ResourceKind::Reserved,
        "vesa-scratch",
    )?;
    reserve_range(
        0x0000_7000,
        0x0000_1000,
        ResourceKind::Reserved,
        "boot-info",
    )?;
    reserve_range(
        0x0000_8000,
        0x000f_8000,
        ResourceKind::Firmware,
        "bios-lowmem",
    )?;

    // Felix's current linked/direct-mapped layout. These remain fixed until the
    // linker/heap layout itself is made dynamic; drivers must never allocate here.
    reserve_range(
        0x0010_0000,
        0x00f0_0000,
        ResourceKind::Reserved,
        "boot-early",
    )?;
    reserve_range(
        0x0100_0000,
        0x0080_0000,
        ResourceKind::Kernel,
        "kernel+stack",
    )?;
    reserve_range(
        0x0180_0000,
        0x0100_0000,
        ResourceKind::Kernel,
        "kernel-heap",
    )?;

    // If PXE supplied a RAM disk, claim it now. The frame allocator is already
    // bumped past this range in paging::init; this entry adds ownership/overlap
    // diagnostics for device allocations.
    unsafe {
        let bi = core::ptr::read_volatile(
            crate::memory::paging::BOOTINFO_PHYS as *const crate::memory::paging::BootInfo,
        );
        if bi.magic == crate::memory::paging::BOOTINFO_MAGIC
            && (bi.flags & 1) != 0
            && bi.disk_phys != 0
            && bi.disk_sectors != 0
        {
            let bytes = (bi.disk_sectors as u64)
                .checked_mul(512)
                .ok_or(ResourceError::Overflow)?;
            reserve_range(
                bi.disk_phys as u64,
                bytes,
                ResourceKind::Reserved,
                "boot-ramdisk",
            )?;
        }
    }

    crate::println!(
        "[mem] resource manager ready: RAM={} MiB, MMIO-VA={:#x}..{:#x}",
        detected_ram_bytes() / (1024 * 1024),
        KERNEL_MMIO_BASE,
        KERNEL_MMIO_END
    );
    Ok(())
}
