use crate::drivers::pic::PICS;
use crate::multitasking::task::TASK_MANAGER;
use crate::print;
use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::slice;
use core::str;

pub const SYSCALL_INT: u8 = 0x80;
use crate::filesystem::fat::FAT;

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

        // 4 — write(fd, buf, len) — теперь поддерживает и файлы!
        crate::syscalls::SYS_WRITE => unsafe {
            let fd = arg1 as i32;
            let buf = arg2;
            let len = arg3 as usize;

            if fd == 1 || fd == 2 {  // stdout / stderr
                let slice = slice::from_raw_parts(buf as *const u8, len);
                if let Ok(s) = str::from_utf8(slice) {
                    print::PRINTER.prints(s);
                }
                len as u32
            } else if fd >= 3 {
                // Запись в файл по fd
                let data = slice::from_raw_parts(buf as *const u8, len);
                FAT.lock(|fat| {
                    fat.load_header();
                    fat.load_entries();
                    fat.load_table();
                    fat.write_fd(fd, data) as u32
                })
            } else {
                0
            }
        }

        // 5 — open(filename) → fd
        crate::syscalls::SYS_OPEN => unsafe {
            let filename_ptr = arg1 as *const u8;
            let filename = unsafe {
                let mut len = 0;
                while *filename_ptr.add(len) != 0 && len < 255 {
                    len += 1;
                }
                core::str::from_utf8_unchecked(core::slice::from_raw_parts(filename_ptr, len))
            };

            FAT.lock(|fat| {
                fat.load_header();
                fat.load_entries();
                fat.open(filename) as u32
            })
        }

        // 6 — close(fd)
        crate::syscalls::SYS_CLOSE => {
            // Пока просто успех (в будущем можно освободить fd)
            0
        }

        // 10 — unlink / delete
        crate::syscalls::SYS_UNLINK => unsafe {
            let filename_ptr = arg1 as *const u8;
            let filename = unsafe {
                let mut len = 0;
                while *filename_ptr.add(len) != 0 && len < 255 {
                    len += 1;
                }
                core::str::from_utf8_unchecked(core::slice::from_raw_parts(filename_ptr, len))
            };

            let success = FAT.lock(|fat| {
                fat.load_header();
                fat.load_entries();
                fat.load_table();
                fat.delete_file(filename)
            });

            if success { 0 } else { u32::MAX }  // -1 в unsigned
        }

        // 302 — ls
        crate::syscalls::SYS_LS => unsafe {
            FAT.lock(|fat| {
                fat.load_header();
                fat.load_entries();
                fat.list_entries();
            });
            0
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