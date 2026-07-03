use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use crate::syscall::{SYS_MALLOC, SYS_FREE, SYS_REALLOC};

pub struct SyscallAllocator;

unsafe impl GlobalAlloc for SyscallAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size() as u32;
        let align = layout.align() as u32;

        let ret: usize;
        asm!(
        "int 0x80",
        inlateout("eax") SYS_MALLOC => ret,
        in("ebx") size,
        in("ecx") align,
        options(nostack, preserves_flags)
        );
        ret as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }
        let size = layout.size() as u32;
        let align = layout.align() as u32;

        asm!(
        "int 0x80",
        in("eax") SYS_FREE,
        in("ebx") ptr as u32,
        in("ecx") size,
        in("edx") align,
        options(nostack, preserves_flags)
        );
    }

    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> *mut u8 {
        if ptr.is_null() {
            return self.alloc(layout);
        }
        if new_size == 0 {
            self.dealloc(ptr, layout);
            return core::ptr::null_mut();
        }

        let old_size = layout.size();

        let ret: usize;
        asm!(
        "int 0x80",
        inlateout("eax") SYS_REALLOC => ret,
        in("ebx") ptr as u32,
        in("ecx") old_size as u32,
        in("edx") new_size,
        options(nostack, preserves_flags)
        );
        // Ядро копирует данные и освобождает старые страницы (с refcounting).
        ret as *mut u8
    }
}
#[global_allocator]
static ALLOCATOR: SyscallAllocator = SyscallAllocator;
