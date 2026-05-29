use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use crate::syscall::{write, SYS_MALLOC};

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
        let size = layout.size() as u32;
        let align = layout.align() as u32;

        core::arch::asm!(
        "int 0x80",
        in("eax") 201,                 // SYS_FREE = 201
        in("ebx") ptr as u32,
        in("ecx") size,
        in("edx") align,
        options(nostack, preserves_flags)
        );
    }
    // ←←← НОВОЕ
    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> *mut u8 {
        let align = layout.align();
        let ret: usize;
        asm!(
        "int 0x80",
        inlateout("eax") crate::syscall::SYS_REALLOC => ret,
        in("ebx") ptr as u32,
        in("ecx") layout.size(),
        in("edx") new_size,
        // in("esi") align,          // можно передать align
        options(nostack, preserves_flags)
        );
        ret as *mut u8
    }
}
#[global_allocator]
static ALLOCATOR: SyscallAllocator = SyscallAllocator;