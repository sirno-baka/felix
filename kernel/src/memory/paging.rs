use core::arch::asm;
use core::ptr::write_bytes;
use core::fmt;
use interrupt_sync::SpinMutex;
use crate::println;
use crate::sync::mutex::Mutex;

const PAGE_SHIFT: usize = 12;
pub const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const ENTRIES: usize = 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(pub u32);

impl VirtAddr {
    pub const fn null() -> Self {
        VirtAddr(0)
    }
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }
    pub const fn page_align(&self) -> Self {
        VirtAddr(self.0 & !(PAGE_SIZE as u32 - 1))
    }
}

impl fmt::Display for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

impl From<u32> for VirtAddr {
    fn from(v: u32) -> Self {
        VirtAddr(v)
    }
}

impl From<usize> for VirtAddr {
    fn from(v: usize) -> Self {
        VirtAddr(v as u32)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(pub u32);

impl PhysAddr {
    pub const fn null() -> Self {
        PhysAddr(0)
    }
    pub const fn is_null(&self) -> bool {
        self.0 == 0
    }
    pub const fn page_align(&self) -> Self {
        PhysAddr(self.0 & !(PAGE_SIZE as u32 - 1))
    }
    pub const fn page_offset(&self) -> u32 {
        self.0 & (PAGE_SIZE as u32 - 1)
    }
    pub const fn page_number(&self) -> u32 {
        self.0 >> PAGE_SHIFT
    }
}

impl fmt::Display for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

impl From<u32> for PhysAddr {
    fn from(v: u32) -> Self {
        PhysAddr(v)
    }
}

impl From<usize> for PhysAddr {
    fn from(v: usize) -> Self {
        PhysAddr(v as u32)
    }
}

// Индекс для рекурсивного отображения (последняя запись каталога)
const RECURSIVE_INDEX: usize = 1023;
// Базовый виртуальный адрес для доступа к таблицам страниц
const VIRT_PT_BASE: u32 = 0xFFC00000;
// Виртуальный адрес самого каталога страниц (через рекурсию)
const VIRT_PD_BASE: u32 = 0xFFFFF000;

pub const KERNEL_OFFSET: u32 = 0xC000_0000;
pub const KERNEL_PHYS:   u32 = 0x0100_0000;
pub const KERNEL_VIRT:   u32 = KERNEL_PHYS + KERNEL_OFFSET; // 0xC100_0000

/// 4 MiB large pages covering identity + higher-half.
/// 32 × 4 MiB = 128 MiB — must match QEMU `-m 128M` (room for 64 MiB ramdisk).
pub const LARGE_PAGE_COUNT: u32 = 32;
/// First free physical frame. Below this: IVT/FB_INFO/TEMP_PD/kernel/boot stack/heap.
/// NEVER allocate page 0 — that destroys FB_INFO at 0x5000.
pub const FRAME_ALLOC_START: u32 = 0x0200_0000;

// замени существующий USER_HEAP_NEXT:
static mut USER_HEAP_NEXT: u32 = 0x1000_0000; // низкий адрес — теперь свободно

#[derive(Copy, Clone)]
pub struct PTEFlags(u32);

impl PTEFlags {
    pub const PRESENT: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const USER: u32 = 1 << 2;
    pub const ACCESSED: u32 = 1 << 5;
    pub const DIRTY: u32 = 1 << 6;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn present(mut self) -> Self {
        self.0 |= Self::PRESENT;
        self
    }

    pub fn writable(mut self) -> Self {
        self.0 |= Self::WRITABLE;
        self
    }

    pub fn user(mut self) -> Self {
        self.0 |= Self::USER;
        self
    }

    pub fn accessed(mut self) -> Self {
        self.0 |= Self::ACCESSED;
        self
    }

    pub fn dirty(mut self) -> Self {
        self.0 |= Self::DIRTY;
        self
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Copy, Clone)]
pub struct PDEFlags(u32);

impl PDEFlags {
    pub const PRESENT: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const USER: u32 = 1 << 2;
    pub const ACCESSED: u32 = 1 << 5;
    pub const DIR_PAGE_SIZE: u32 = 1 << 7;

    pub fn new() -> Self {
        Self(0)
    }

    pub fn present(mut self) -> Self {
        self.0 |= Self::PRESENT;
        self
    }

    pub fn writable(mut self) -> Self {
        self.0 |= Self::WRITABLE;
        self
    }

    pub fn user(mut self) -> Self {
        self.0 |= Self::USER;
        self
    }

    pub fn accessed(mut self) -> Self {
        self.0 |= Self::ACCESSED;
        self
    }

    pub fn large_page(mut self) -> Self {
        self.0 |= Self::DIR_PAGE_SIZE;
        self
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Copy, Clone)]
#[repr(C, align(4096))]
pub struct PageDirectory {
    pub entries: [u32; ENTRIES],
}

impl PageDirectory {
    pub const fn new() -> Self {
        PageDirectory {
            entries: [0; ENTRIES],
        }
    }

    /// Page table of this PDE via higher-half, independent of current CR3.
    #[inline]
    pub fn pt_from_pde(pde: u32) -> *mut [u32; ENTRIES] {
        phys_to_virt(pde & 0xFFFF_F000) as *mut [u32; ENTRIES]
    }

    /// Делегирует выделение в глобальный FrameAllocator (PAGING).
    /// Это главное исправление — теперь все таски получают уникальные физические фреймы.
    pub fn alloc_frame(&mut self) -> u32 {
        unsafe {
            PAGING.lock().alloc_frame()
        }
    }

    pub fn ensure_page_table(
        &mut self,
        pd_index: usize,
        alloc_frame: &mut dyn FnMut() -> u32,
    ) -> *mut [u32; ENTRIES] {
        let pde = self.entries[pd_index];
        if pde & PDEFlags::PRESENT == 0 || pde & PDEFlags::DIR_PAGE_SIZE != 0 {
            let pt_frame = alloc_frame();
            let pt_phys = pt_frame << 12;
            unsafe {
                write_bytes(phys_to_virt(pt_phys) as *mut u8, 0, PAGE_SIZE);
            }
            self.entries[pd_index] = pt_phys | PDEFlags::PRESENT | PDEFlags::WRITABLE;
            PageDirectory::flush_page((pd_index as u32) << 22);
        }
        Self::pt_from_pde(self.entries[pd_index])
    }

    pub fn map_existing(&mut self, virtual_page: u32, physical_page: u32, flags: PTEFlags) {
        let pd_index = (virtual_page >> 10) as usize;
        let pt_index = (virtual_page & 0x3FF) as usize;

        let pde = self.entries[pd_index];
        assert!(
            pde & PDEFlags::PRESENT != 0 && pde & PDEFlags::DIR_PAGE_SIZE == 0,
            "Page table not present"
        );

        let pt = Self::pt_from_pde(pde);
        unsafe {
            (*pt)[pt_index] = (physical_page << 12) | flags.bits();
        }
        PageDirectory::flush_page(virtual_page << 12);
    }

    pub(crate) fn get_page_table_ptr(pd_index: usize) -> *mut [u32; ENTRIES] {
        let vaddr = VIRT_PT_BASE + (pd_index as u32 * 4096);
        vaddr as *mut [u32; ENTRIES]
    }

    fn setup_recursive(&mut self, pd_phys: u32) {
        self.entries[RECURSIVE_INDEX] = pd_phys | PDEFlags::PRESENT | PDEFlags::WRITABLE;
    }

    pub fn map(&mut self, virtual_page: u32, physical_page: u32, flags: PTEFlags, alloc_frame: &mut dyn FnMut() -> u32) {
        let pd_index = (virtual_page >> 10) as usize;
        let pt_index = (virtual_page & 0x3FF) as usize;
        let virt_addr = virtual_page << 12;

        let pde = self.entries[pd_index];
        if pde & PDEFlags::PRESENT == 0 || pde & PDEFlags::DIR_PAGE_SIZE != 0 {
            let pt_frame = alloc_frame();
            let pt_phys = pt_frame << 12;
            unsafe {
                write_bytes(phys_to_virt(pt_phys) as *mut u8, 0, PAGE_SIZE);
            }
            self.entries[pd_index] = pt_phys
                | PDEFlags::PRESENT
                | PDEFlags::WRITABLE
                | PDEFlags::USER;
        }

        let pt = Self::pt_from_pde(self.entries[pd_index]);
        unsafe {
            (*pt)[pt_index] = (physical_page << 12) | flags.bits();
        }
        PageDirectory::flush_page(virt_addr);
    }

    pub fn map_large(&mut self, virtual_addr: u32, physical_addr: u32, flags: PDEFlags) {
        let pd_index = (virtual_addr >> 22) as usize;
        let pde_value = (physical_addr & 0xFFC00000)
            | flags.bits()
            | PDEFlags::DIR_PAGE_SIZE;
        self.entries[pd_index] = pde_value;
        PageDirectory::flush_page(virtual_addr & 0xFFC00000);
    }

    pub fn unmap(&mut self, virtual_addr: u32) {
        let pd_index = (virtual_addr >> 22) as usize;
        if pd_index >= ENTRIES {
            return;
        }
        let pd_index = (virtual_addr >> 22) as usize;
        let pt_index = ((virtual_addr >> 12) & 0x3FF) as usize;

        let entry = self.entries[pd_index];
        if entry & PDEFlags::PRESENT == 0 {
            return;
        }

        if entry & PDEFlags::DIR_PAGE_SIZE != 0 {
            self.entries[pd_index] = 0;
        } else {
            let pt = Self::pt_from_pde(entry);
            unsafe {
                (*pt)[pt_index] = 0;
            }
        }
        PageDirectory::flush_page(virtual_addr);
    }

    pub fn translate(&self, virtual_addr: u32) -> Option<u32> {
        let pd_index = (virtual_addr >> 22) as usize;
        let pt_index = ((virtual_addr >> 12) & 0x3FF) as usize;
        let offset = virtual_addr & 0xFFF;

        let pd_entry = self.entries[pd_index];
        if pd_entry & PDEFlags::PRESENT == 0 {
            return None;
        }

        if pd_entry & PDEFlags::DIR_PAGE_SIZE != 0 {
            let phys_base = pd_entry & 0xFFC0_0000;
            Some(phys_base | (virtual_addr & 0x003F_FFFF))
        } else {
            let pt = Self::pt_from_pde(pd_entry);
            let pt_entry = unsafe { (*pt)[pt_index] };
            if pt_entry & PTEFlags::PRESENT == 0 {
                return None;
            }
            Some((pt_entry & 0xFFFF_F000) | offset)
        }
    }

    pub fn switch(&self) {
        // После higher-half &self — это virtual address.
        // CR3 всегда должен получать physical.
        let virt = &self.entries as *const [u32; ENTRIES] as u32;
        let pd_phys = if virt >= KERNEL_OFFSET {
            virt - KERNEL_OFFSET
        } else {
            virt
        };
        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) pd_phys);
        }
    }

    pub fn dir_phys(&self) -> u32 {
        let virt = &self.entries as *const [u32; ENTRIES] as u32;
        if virt >= KERNEL_OFFSET {
            virt - KERNEL_OFFSET
        } else {
            virt
        }
    }
    pub fn flush_all() {
        unsafe {
            let cr3: u32;
            asm!("mov {}, cr3", out(reg) cr3);
            asm!("mov cr3, {}", in(reg) cr3);
        }
    }

    pub fn flush_page(addr: u32) {
        unsafe {
            asm!("invlpg [{0}]", in(reg) addr);
        }
    }

    pub fn alloc_and_map_user_page(&mut self, virt_addr: u32) {
        let vpage = virt_addr >> 12;
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        let pde = self.entries[pd_idx];
        let need_table = pde == 0
            || (pde & PDEFlags::PRESENT) == 0
            || (pde & PDEFlags::DIR_PAGE_SIZE) != 0; // ← важно: large page = пересоздать

        if need_table {
            let pt_frame = self.alloc_frame();
            let pt_phys = pt_frame << 12;
            self.entries[pd_idx] = pt_phys
                | PDEFlags::PRESENT
                | PDEFlags::WRITABLE
                | PDEFlags::USER;

            let pt_virt = phys_to_virt(pt_phys) as *mut u8;
            unsafe { core::ptr::write_bytes(pt_virt, 0, 4096); }
        } else {
            // PDE есть, но вдруг без USER — допиши
            self.entries[pd_idx] |= PDEFlags::USER;
        }

        let pde = self.entries[pd_idx];
        let pt_phys = pde & 0xFFFF_F000;
        let pt = phys_to_virt(pt_phys) as *mut [u32; 1024];

        let existing = unsafe { (*pt)[pt_idx] };
        if existing & PTEFlags::PRESENT != 0 {
            // уже есть — убедись что USER стоит
            unsafe { (*pt)[pt_idx] |= PTEFlags::USER; }
            return;
        }

        let frame = self.alloc_frame();
        unsafe {
            (*pt)[pt_idx] = (frame << 12)
                | PTEFlags::PRESENT
                | PTEFlags::WRITABLE
                | PTEFlags::USER
                | PTEFlags::DIRTY;
        }
    }
}

pub struct PageManager {
    pub dir: PageDirectory,
    pub next_free_page: u32,
}

impl PageManager {
    pub const fn new() -> Self {
        PageManager {
            dir: PageDirectory::new(),
            next_free_page: 0,
        }
    }

    pub fn map_physical_range(
        &mut self,
        phys_start: u32,
        size: u32,
        virt_start: u32,
        flags: PTEFlags,
    ) -> Result<(), &'static str> {
        let pages = ((size as usize) + PAGE_SIZE - 1) / PAGE_SIZE;

        for i in 0..pages {
            let virt = virt_start + (i as u32) * PAGE_SIZE as u32;
            let phys = phys_start + (i as u32) * PAGE_SIZE as u32;

            let vpage = virt >> 12;
            let ppage = phys >> 12;

            let pd_idx = (vpage >> 10) as usize;
            let pt_idx = (vpage & 0x3FF) as usize;

            // Создаём Page Table, если нужно
            let need_table = {
                let pde = self.dir.entries[pd_idx];
                pde == 0 || (pde & PDEFlags::PRESENT) == 0
            };

            if need_table {
                let pt_frame = self.alloc_frame(); // возвращает номер фрейма
                let pt_phys = pt_frame << 12;

                let pde_flags = PDEFlags::new()
                    .present()
                    .writable()
                    .bits(); // kernel-only

                self.dir.entries[pd_idx] = pt_phys | pde_flags;
                PageDirectory::flush_page((pd_idx as u32) << 22);

                let pt_ptr = PageDirectory::pt_from_pde(self.dir.entries[pd_idx]);
                unsafe {
                    write_bytes(pt_ptr as *mut u8, 0, 4096);
                }
            }

            // Маппим страницу
            let pt_ptr = PageDirectory::pt_from_pde(self.dir.entries[pd_idx]);
            unsafe {
                (*pt_ptr)[pt_idx] = (ppage << 12) | flags.bits();
            }
            PageDirectory::flush_page(virt);
        }

        Ok(())
    }

    pub fn alloc_phys_frame(&mut self) -> u32 {
        let frame = self.next_free_page;
        self.next_free_page += 1;
        frame
    }

    pub fn free_phys_frame(&mut self, _frame: u32) {
    }

    pub(crate) fn alloc_frame(&mut self) -> u32 {
        // Guard: never hand out frames past the large-page mapped window.
        const MAX_PAGE: u32 = (128 * 1024 * 1024) >> 12;
        let frame = self.next_free_page;
        if frame >= MAX_PAGE {
            panic!("[pg] out of physical frames (next={})", frame);
        }
        self.next_free_page += 1;
        frame
    }

    /// Higher-half aware page-manager initialisation.
    ///
    /// Called from higher_half_entry() after the early dual-mapping PD
    /// has already been installed.  Builds the definitive kernel PD:
    ///
    ///   • identity 0–32 MiB          (VGA, devices, early code)
    ///   • higher-half 0xC0000000+    (kernel image, stacks, heap)
    ///   • recursive mapping at 0xFFC00000
    pub fn init(&mut self, kernel_end_virt: u32) {
        // Enable PSE (4 MiB pages)
        unsafe {
            let mut cr4: u32;
            core::arch::asm!("mov {}, cr4", out(reg) cr4);
            cr4 |= 1 << 4;
            core::arch::asm!("mov cr4, {}", in(reg) cr4);
        }

        // Identity + higher-half: first 128 MiB as 4 MiB large pages.
        for i in 0..LARGE_PAGE_COUNT {
            let phys = i * 0x400000u32;
            self.dir.map_large(
                phys,
                phys,
                PDEFlags::new().present().writable().accessed().large_page(),
            );
            self.dir.map_large(
                KERNEL_OFFSET + phys,
                phys,
                PDEFlags::new().present().writable().accessed().large_page(),
            );
        }

        // Physical address of this PageDirectory.
        // After the higher-half jump the Rust reference is a high virtual
        // address, so we convert it back to physical.
        let pd_virt = &self.dir as *const PageDirectory as u32;
        let pd_phys = if pd_virt >= KERNEL_OFFSET {
            pd_virt - KERNEL_OFFSET
        } else {
            pd_virt
        };

        self.dir.setup_recursive(pd_phys);
        self.dir.switch(); // CR3 = our new PD

        // Large-page window already covers low RAM. Do NOT 4K-map that range:
        // the old loop called alloc_frame() while next_free_page was still 0
        // and turned phys pages 0..N (including FB_INFO @ 0x5000) into page tables.
        let _ = kernel_end_virt;
        self.next_free_page = FRAME_ALLOC_START >> 12;
        println!(
            "[pg] {}MiB large pages, frames from phys {:#x} (page {})",
            LARGE_PAGE_COUNT * 4,
            FRAME_ALLOC_START,
            self.next_free_page
        );

        // Ensure PG is set (should already be from early bootstrap)
        self.dir.switch();
        unsafe {
            let mut cr0: u32;
            core::arch::asm!("mov {}, cr0", out(reg) cr0);
            cr0 |= 1 << 31;
            core::arch::asm!("mov cr0, {}", in(reg) cr0);
        }
    }


    pub fn alloc_user_memory(&mut self, size: u32) -> u32 {
        if size == 0 {
            return 0;
        }

        let start = unsafe { USER_HEAP_NEXT };

        let pages = (size + 4095) / 4096;
        for i in 0..pages {
            let virt_addr = start + (i as u32 * 4096);
            self.alloc_and_map(virt_addr);
        }

        unsafe {
            USER_HEAP_NEXT = start + (pages as u32 * 4096);
        }

        start
    }

    pub fn alloc_and_map(&mut self, virt: u32) -> u32 {
        let vpage = virt >> 12;
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        let need_table = {
            let pde = self.dir.entries[pd_idx];
            pde == 0 || (pde & PDEFlags::PRESENT) == 0
        };

        if need_table {
            let pt_phys = self.alloc_frame();

            let pde = &mut self.dir.entries[pd_idx];
            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();

            *pde = (pt_phys << 12) | pde_flags;

            PageDirectory::flush_page((pd_idx as u32) << 22);
            let pt_virt = VIRT_PT_BASE + (pd_idx as u32) * PAGE_SIZE as u32;
            PageDirectory::flush_page(pt_virt);

            let pt_ptr = PageDirectory::pt_from_pde(self.dir.entries[pd_idx]);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
        }

        let phys_frame = self.alloc_frame();

        let pt_ptr = PageDirectory::pt_from_pde(self.dir.entries[pd_idx]);
        unsafe {
            (*pt_ptr)[pt_idx] = (phys_frame << 12)
                | PTEFlags::PRESENT
                | PTEFlags::WRITABLE
                | PTEFlags::USER
                | PTEFlags::DIRTY;
        }

        PageDirectory::flush_page(virt);

        unsafe {
            core::ptr::write_bytes(virt as *mut u8, 0, PAGE_SIZE);
        }
        PageDirectory::flush_page(virt);

        phys_frame
    }

    pub fn dir_phys(&self) -> u32 {
        let virt = &self.dir.entries as *const [u32; ENTRIES] as u32;
        if virt >= KERNEL_OFFSET {
            virt - KERNEL_OFFSET
        } else {
            virt
        }
    }
}

pub fn setup_kernel_page_dir(pd: &mut PageManager, end_page: u32) {
    pd.dir = PageDirectory::new();
    let pd_phys = &pd.dir as *const PageDirectory as u32;
    pd.dir.setup_recursive(pd_phys);
    pd.dir.switch();

    for i in 0..8u32 {
        let base = i * 0x400000u32; // 4 MiB large pages, identity mapped
        pd.dir.map_large(
            base,
            base,
            PDEFlags::new().present().writable().accessed().user().large_page(),
        );
    }

    for vpage in (32 * 1024 * 1024 >> 12)..end_page {
        let pd_idx = (vpage >> 10) as usize;
        let pt_idx = (vpage & 0x3FF) as usize;

        let need_table = {
            let pde = pd.dir.entries[pd_idx];
            pde == 0 || (pde & PDEFlags::PRESENT) == 0
        };

        if need_table {
            let pt_phys = pd.alloc_frame();
            let pde = &mut pd.dir.entries[pd_idx];
            let pde_flags = PDEFlags::new()
                .present()
                .writable()
                .user()
                .bits();
            *pde = (pt_phys << 12) | pde_flags;
            PageDirectory::flush_page((pd_idx as u32) << 22);
            let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
            unsafe {
                write_bytes(pt_ptr as *mut u8, 0, 4096);
            }
        }

        let pt_ptr = PageDirectory::get_page_table_ptr(pd_idx);
        unsafe {
            (*pt_ptr)[pt_idx] = (vpage << 12) | PTEFlags::PRESENT | PTEFlags::WRITABLE | PTEFlags::USER | PTEFlags::ACCESSED;
        }
        PageDirectory::flush_page((vpage << 12) as u32);
    }

    pd.dir.switch();
}
pub static mut KERNEL_PD_PHYS: u32 = 0;
pub static mut KERNEL_END_PAGE: u32 = 0; // physical page number

/// Share kernel address-space mappings into a newly created task page directory.
///
/// Kernel half must be SHARED (same page-table frames), not deep-copied.
/// Otherwise NIC/heap/MMIO mappings that live only in the kernel PD are
/// missing or stale when a user task's CR3 is active during a syscall.
///
/// Shared:
///   1. Identity large pages 0–32 MiB          (PDE 0..7)
///   2. Entire higher-half kernel (PDE 768..1022) including MMIO
/// Own:
///   3. Recursive mapping → this task's PD phys
pub fn copy_kernel_mappings(task_dir: &mut PageDirectory, task_pd_phys: u32) {
    unsafe {
        let kernel_pd_phys = KERNEL_PD_PHYS;
        if kernel_pd_phys == 0 {
            return;
        }

        let kernel_pd = phys_to_virt(kernel_pd_phys) as *const [u32; ENTRIES];

        // 1. Identity large pages (PDE 0..LARGE_PAGE_COUNT-1)
        //    NOTE: user ELF @ 0x400000 will overwrite PDE[1] later with 4K+USER.
        for i in 0..LARGE_PAGE_COUNT as usize {
            let pde = (*kernel_pd)[i];
            if (pde & PDEFlags::PRESENT) != 0 {
                task_dir.entries[i] = pde;
            }
        }

        // 2. Kernel higher-half + MMIO: SHARE the same page tables.
        //    Do NOT deep-copy — deep copy was the source of page faults
        //    (CR2 like 0xc520ae28) when poll/recv ran under a user CR3.
        for pd_idx in 768usize..1023 {
            let pde = (*kernel_pd)[pd_idx];
            if (pde & PDEFlags::PRESENT) != 0 {
                task_dir.entries[pd_idx] = pde;
            }
        }

        // 3. Recursive mapping → this task's own PD
        task_dir.entries[1023] = task_pd_phys
            | PDEFlags::PRESENT
            | PDEFlags::WRITABLE;
    }
}

/// One 4K frame for a task PD. Not Box — debug rustc would put PageDirectory on the stack.
pub fn alloc_task_page_dir() -> (*mut PageDirectory, u32) {
    let frame = alloc_frame_irqsafe();
    let phys = frame << 12;
    let virt = phys_to_virt(phys) as *mut PageDirectory;
    unsafe {
        core::ptr::write_bytes(virt as *mut u8, 0, PAGE_SIZE);
    }
    (virt, phys)
}

/// `size` consecutive frames (bump allocator hands them out sequentially).
pub fn alloc_kernel_stack(size: usize) -> u32 {
    let pages = (size + PAGE_SIZE - 1) / PAGE_SIZE;
    let first = interrupt_sync::without_interrupts(|| unsafe {
        let mut pm = PAGING.lock();
        let f = pm.alloc_frame();
        for _ in 1..pages {
            let _ = pm.alloc_frame();
        }
        f
    });
    let base = phys_to_virt(first << 12);
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, pages * PAGE_SIZE);
    }
    base
}

/// Physical → kernel virtual (higher-half).
/// Requires `phys` to sit in the higher-half large-page window (now 0..128 MiB).
#[inline]
pub fn phys_to_virt(phys: u32) -> u32 {
    phys.wrapping_add(KERNEL_OFFSET)
}

/// Глобальный экземпляр менеджера страниц
pub static mut PAGING: SpinMutex<PageManager> = SpinMutex::new(PageManager::new());

/// Allocate a frame with interrupts disabled (PAGING is a plain SpinMutex).
pub fn alloc_frame_irqsafe() -> u32 {
    interrupt_sync::without_interrupts(|| unsafe { PAGING.lock().alloc_frame() })
}
