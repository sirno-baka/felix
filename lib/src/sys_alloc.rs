use core::alloc::{GlobalAlloc, Layout};

pub struct SyscallAllocator;

unsafe impl GlobalAlloc for SyscallAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size() as u32;
        let align = layout.align() as u32;

        let ptr: u32;
        core::arch::asm!(
        "int 0x80",
        inlateout("eax") 200 => ptr,   // SYS_MALLOC = 200
        in("ebx") size,
        in("ecx") align,
        options(nostack, preserves_flags)
        );
        ptr as *mut u8
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
}
#[global_allocator]
static ALLOCATOR: SyscallAllocator = SyscallAllocator;