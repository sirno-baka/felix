use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ptr;
use dlmalloc::{Allocator as DlAllocator, Dlmalloc};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;


static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_PEAK: AtomicUsize = AtomicUsize::new(0);

fn add_with_peak(current: &AtomicUsize, peak: &AtomicUsize, amount: usize) {
    let now = current.fetch_add(amount, Ordering::Relaxed).saturating_add(amount);
    let mut seen = peak.load(Ordering::Relaxed);
    while now > seen {
        match peak.compare_exchange_weak(seen, now, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => seen = actual,
        }
    }
}

fn sub_saturating(current: &AtomicUsize, amount: usize) {
    let mut old = current.load(Ordering::Relaxed);
    loop {
        let new = old.saturating_sub(amount);
        match current.compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => old = actual,
        }
    }
}

#[derive(Clone, Copy)]
pub struct MemoryStats {
    pub mapped_bytes: usize,
    pub mapped_peak: usize,
    pub live_bytes: usize,
    pub live_peak: usize,
}

pub fn stats() -> MemoryStats {
    MemoryStats {
        mapped_bytes: MMAP_LIVE_BYTES.load(Ordering::Relaxed),
        mapped_peak: MMAP_PEAK_BYTES.load(Ordering::Relaxed),
        live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
        live_peak: LIVE_PEAK.load(Ordering::Relaxed),
    }
}

const SYS_MMAP: u32 = 90;
const SYS_MUNMAP: u32 = 91;
const PAGE_SIZE: usize = 4096;
const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const MAP_PRIVATE: u32 = 2;
const MAP_ANONYMOUS: u32 = 0x20;

static MMAP_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static MMAP_PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
static MMAP_ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
static MMAP_FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

fn note_mmap_alloc(size: usize) {
    MMAP_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
    let live = MMAP_LIVE_BYTES.fetch_add(size, Ordering::Relaxed).saturating_add(size);
    let mut peak = MMAP_PEAK_BYTES.load(Ordering::Relaxed);
    while live > peak {
        match MMAP_PEAK_BYTES.compare_exchange_weak(
            peak,
            live,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(now) => peak = now,
        }
    }
}

fn note_mmap_free(size: usize) {
    MMAP_FREE_CALLS.fetch_add(1, Ordering::Relaxed);
    let _ = MMAP_LIVE_BYTES.fetch_update(
        Ordering::Relaxed,
        Ordering::Relaxed,
        |live| Some(live.saturating_sub(size)),
    );
}

pub fn mapped_stats() -> (usize, usize, usize, usize) {
    (
        MMAP_LIVE_BYTES.load(Ordering::Relaxed),
        MMAP_PEAK_BYTES.load(Ordering::Relaxed),
        MMAP_ALLOC_CALLS.load(Ordering::Relaxed),
        MMAP_FREE_CALLS.load(Ordering::Relaxed),
    )
}

#[repr(C)]
struct MmapArgs {
    addr: u32,
    len: u32,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: u32,
}

struct PopugosPages;

unsafe impl DlAllocator for PopugosPages {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        let Some(size) = size.checked_add(PAGE_SIZE - 1).map(|n| n & !(PAGE_SIZE - 1)) else {
            return (ptr::null_mut(), 0, 0);
        };
        let Ok(len) = u32::try_from(size) else {
            return (ptr::null_mut(), 0, 0);
        };
        let args = MmapArgs {
            addr: 0,
            len,
            prot: PROT_READ | PROT_WRITE,
            flags: MAP_PRIVATE | MAP_ANONYMOUS,
            fd: -1,
            offset: 0,
        };
        let mut result = SYS_MMAP;
        unsafe {
            asm!(
                "int 0x80",
                inlateout("eax") result,
                in("ebx") ptr::from_ref(&args) as u32,
                options(nostack)
            );
        }
        // Linux-style syscall errors occupy -4095..=-1. Valid PopugOS user
        // mappings may have bit 31 set, so testing result as i32 is incorrect.
        if result >= (-4095i32) as u32 {
            (ptr::null_mut(), 0, 0)
        } else {
            note_mmap_alloc(size);
            (ptr::with_exposed_provenance_mut(result as usize), size, 0)
        }
    }

    fn remap(
        &self,
        _ptr: *mut u8,
        _old_size: usize,
        _new_size: usize,
        _can_move: bool,
    ) -> *mut u8 {
        ptr::null_mut()
    }

    fn free_part(&self, _ptr: *mut u8, _old_size: usize, _new_size: usize) -> bool {
        false
    }

    fn free(&self, ptr: *mut u8, size: usize) -> bool {
        let mut result = SYS_MUNMAP;
        unsafe {
            asm!(
                "int 0x80",
                inlateout("eax") result,
                in("ebx") ptr as u32,
                in("ecx") size as u32,
                options(nostack)
            );
        }
        if result == 0 {
            note_mmap_free(size);
            true
        } else {
            false
        }
    }

    fn can_release_part(&self, _flags: u32) -> bool {
        false
    }

    fn allocates_zeros(&self) -> bool {
        true
    }

    fn page_size(&self) -> usize {
        PAGE_SIZE
    }
}

struct TwitchAllocator(Mutex<Dlmalloc<PopugosPages>>);

unsafe impl GlobalAlloc for TwitchAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Ok(mut heap) = self.0.lock() else {
            return ptr::null_mut();
        };
        let ptr = unsafe { heap.malloc(layout.size(), layout.align()) };
        if !ptr.is_null() {
            add_with_peak(&LIVE_BYTES, &LIVE_PEAK, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Ok(mut heap) = self.0.lock() {
            unsafe { heap.free(ptr, layout.size(), layout.align()) };
            sub_saturating(&LIVE_BYTES, layout.size());
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let Ok(mut heap) = self.0.lock() else {
            return ptr::null_mut();
        };
        let ptr = unsafe { heap.calloc(layout.size(), layout.align()) };
        if !ptr.is_null() {
            add_with_peak(&LIVE_BYTES, &LIVE_PEAK, layout.size());
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(mut heap) = self.0.lock() else {
            return ptr::null_mut();
        };
        let new_ptr = unsafe { heap.realloc(ptr, layout.size(), layout.align(), new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                add_with_peak(&LIVE_BYTES, &LIVE_PEAK, new_size - layout.size());
            } else {
                sub_saturating(&LIVE_BYTES, layout.size() - new_size);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: TwitchAllocator =
    TwitchAllocator(Mutex::new(Dlmalloc::new_with_allocator(PopugosPages)));
