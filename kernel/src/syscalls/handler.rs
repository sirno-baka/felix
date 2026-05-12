use crate::drivers::pic::PICS;
use crate::multitasking::task::TASK_MANAGER;
use crate::syscalls::print;
use crate::memory::allocator::ALLOCATOR;   // ← добавь эту строку
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::slice;
use core::str;

pub const SYSCALL_INT: u8 = 0x80;

// Новый, удобный syscall handler
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
    syscall: u32,   // номер syscall в eax
    arg1: u32,      // ebx
    arg2: u32,      // ecx
    arg3: u32,      // edx
) -> u32 {
    let ret = match syscall {
        // 0 — print (как было)
        0 => unsafe {
            let slice = unsafe { slice::from_raw_parts(arg1 as *const u8, arg2 as usize) };
            if let Ok(s) = str::from_utf8(slice) {
                print::PRINTER.prints(s);
            }
            0
        }

        // 1 — удалить текущую задачу (как было)
        1 => unsafe {
            TASK_MANAGER.remove_current_task();
            0
        }

        // 2 — alloc (size, align)
        2 => {
            let layout = Layout::from_size_align(arg1 as usize, arg2 as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.alloc(layout) as u32 }
        }

        // 3 — dealloc (ptr, size, align)
        3 => {
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