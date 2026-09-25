use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ptr;
use dlmalloc::{Allocator as DlAllocator, Dlmalloc};
use std::sync::Mutex;

const SYS_MMAP: u32 = 90;
const SYS_MUNMAP: u32 = 91;
const PAGE_SIZE: usize = 4096;
const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const MAP_PRIVATE: u32 = 2;
const MAP_ANONYMOUS: u32 = 0x20;

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
        result == 0
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
        unsafe { heap.malloc(layout.size(), layout.align()) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Ok(mut heap) = self.0.lock() {
            unsafe { heap.free(ptr, layout.size(), layout.align()) };
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let Ok(mut heap) = self.0.lock() else {
            return ptr::null_mut();
        };
        unsafe { heap.calloc(layout.size(), layout.align()) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(mut heap) = self.0.lock() else {
            return ptr::null_mut();
        };
        unsafe { heap.realloc(ptr, layout.size(), layout.align(), new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: TwitchAllocator =
    TwitchAllocator(Mutex::new(Dlmalloc::new_with_allocator(PopugosPages)));
