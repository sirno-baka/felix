use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use crate::syscall::SYS_MALLOC;

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

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // No-op: bump-аллокатор ядра не переиспользует память.
        // Размапливание страниц здесь ломает соседние выделения на той же странице.
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
        inlateout("eax") crate::syscall::SYS_REALLOC => ret,
        in("ebx") ptr as u32,
        in("ecx") old_size as u32,
        in("edx") new_size,
        options(nostack, preserves_flags)
        );
        // Ядро уже копирует старые данные в новый блок.
        // Не вызываем dealloc — старая память на той же странице
        // может содержать новое выделение.
        ret as *mut u8
    }
}
#[global_allocator]
static ALLOCATOR: SyscallAllocator = SyscallAllocator;