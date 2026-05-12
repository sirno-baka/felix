use crate::drivers::pic::PICS;
use crate::multitasking::task::TASK_MANAGER;
use crate::print;
use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::slice;
use core::str;

pub const SYSCALL_INT: u8 = 0x80;

#[naked]
pub extern "C" fn syscall() {
    unsafe {
        asm!(
        "push edx",   // arg3
        "push ecx",   // arg2
        "push ebx",   // arg1
        "push eax",   // syscall number
        "call syscall_handler",
        "add esp, 16",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn syscall_handler(
    syscall: u32,
    arg1: u32,   // ebx
    arg2: u32,   // ecx
    arg3: u32,   // edx
) -> u32 {
    let ret = match syscall {
        // 1 — exit
        crate::syscalls::SYS_EXIT => unsafe {
            TASK_MANAGER.remove_current_task();
            0
        }

        // 4 — write(fd, buf, len)  ← вот и stdout
        crate::syscalls::SYS_WRITE => unsafe {
            let fd = arg1;
            let buf = arg2;
            let len = arg3;

            if fd == 1 || fd == 2 {  // stdout / stderr
                let slice = unsafe { slice::from_raw_parts(buf as *const u8, len as usize) };
                if let Ok(s) = str::from_utf8(slice) {
                    print::PRINTER.prints(s);
                }
            }
            len   // возвращаем, сколько байт записали
        }

        // 200 — malloc(size, align)
        crate::syscalls::SYS_MALLOC => {
            let layout = Layout::from_size_align(arg1 as usize, arg2 as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.alloc(layout) as u32 }
        }

        // 201 — free(ptr, size, align)
        crate::syscalls::SYS_FREE => {
            let layout = Layout::from_size_align(arg2 as usize, arg3 as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.dealloc(arg1 as *mut u8, layout); }
            0
        }

        _ => 0,
    };

    PICS.end_interrupt(SYSCALL_INT);
    ret
}