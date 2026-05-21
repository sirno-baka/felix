use crate::drivers::pic::PICS;
use crate::multitasking::task::TASK_MANAGER;
use crate::{print, println};
use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ffi::CStr;
use crate::filesystem::file::{FileDescriptor, FileMode};
use crate::filesystem::VFS;

pub const SYSCALL_INT: u8 = 0x80;

#[naked]
pub extern "C" fn syscall() {
    unsafe {
        asm!(
        "push edx",
        "push ecx",
        "push ebx",
        "push eax",
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
    arg1: u32,
    arg2: u32,
    arg3: u32,
) -> u32 {
    let ret = match syscall {
        // === Process control ===
        crate::syscalls::SYS_EXIT => {
            unsafe { TASK_MANAGER.remove_current_task(); }
            0
        }

        // === File descriptors ===
        crate::syscalls::SYS_OPEN  => sys_open(arg1 as *const u8, arg2 as usize),
        crate::syscalls::SYS_READ  => sys_read(arg1 as usize, arg2 as *mut u8, arg3 as usize),
        crate::syscalls::SYS_WRITE => sys_write(arg1 as usize, arg2 as *const u8, arg3 as usize),
        crate::syscalls::SYS_CLOSE => sys_close(arg1 as usize),

        // === Filesystem operations (новые обработчики) ===
        crate::syscalls::SYS_MKDIR  => sys_mkdir(arg1 as *const u8),
        crate::syscalls::SYS_RMDIR  => sys_rmdir(arg1 as *const u8),
        crate::syscalls::SYS_UNLINK => sys_unlink(arg1 as *const u8),
        crate::syscalls::SYS_EXECVE => sys_execve(arg1 as *const u8),
        // === Memory ===
        crate::syscalls::SYS_MALLOC => {
            let layout = Layout::from_size_align(arg1 as usize, arg2 as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.alloc(layout) as usize }
        },

        crate::syscalls::SYS_FREE => {
            let layout = Layout::from_size_align(arg2 as usize, arg3 as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.dealloc(arg1 as *mut u8, layout); }
            0
        }

        _ => 0,
    };

    PICS.end_interrupt(SYSCALL_INT);
    ret as u32
}

// ====================== FILE DESCRIPTORS ======================

fn sys_open(path_ptr: *const u8, _flags: usize) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };

    if let Some(inode) = VFS.get().resolve_path(path) {
        let current = unsafe { &mut TASK_MANAGER.tasks[TASK_MANAGER.get_current_slot() as usize] };

        if let Some(fd) = current.fd_table.alloc_fd() {
            let desc = FileDescriptor::new(inode, FileMode::ReadWrite);
            current.fd_table.insert(fd, desc);
            return fd;
        }
    }
    usize::MAX
}

fn sys_read(fd: usize, buf_ptr: *mut u8, count: usize) -> usize {
    let current = unsafe { &mut TASK_MANAGER.tasks[TASK_MANAGER.get_current_slot() as usize] };

    if let Some(desc) = current.fd_table.get_mut(fd) {
        if desc.mode == FileMode::WriteOnly { return 0; }

        let mut temp = alloc::vec![0u8; count];
        let bytes = VFS.get().read_at(desc.inode, desc.offset, &mut temp);

        if bytes > 0 {
            unsafe { core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes); }
            desc.offset += bytes as u64;
        }
        bytes
    } else {
        0
    }
}

fn sys_write(fd: usize, buf_ptr: *const u8, count: usize) -> usize {
    let current = unsafe { &mut TASK_MANAGER.tasks[TASK_MANAGER.get_current_slot() as usize] };

    if let Some(desc) = current.fd_table.get_mut(fd) {
        if desc.mode == FileMode::ReadOnly { return 0; }

        let buf = unsafe { core::slice::from_raw_parts(buf_ptr, count) };
        let written = VFS.get().write_at(desc.inode, desc.offset, buf);
        desc.offset += written as u64;
        written
    } else {
        0
    }
}

fn sys_close(fd: usize) -> usize {
    let current = unsafe { &mut TASK_MANAGER.tasks[TASK_MANAGER.get_current_slot() as usize] };
    if current.fd_table.close(fd) { 0 } else { usize::MAX }
}

// ====================== FILESYSTEM OPERATIONS ======================

fn sys_mkdir(path_ptr: *const u8) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    let success = VFS.get().mkdir(path);
    if success { 0 } else { usize::MAX }
}

fn sys_rmdir(path_ptr: *const u8) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    let success = VFS.get().rmdir(path);
    if success { 0 } else { usize::MAX }
}

fn sys_unlink(path_ptr: *const u8) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    let success = VFS.get().remove_file(path);
    if success { 0 } else { usize::MAX }
}

use crate::memory::paging::{PAGING, TABLES};   // ← добавь этот импорт

pub fn sys_execve(path_ptr: *const u8) -> usize {
    // Безопасное чтение пути
    let path = unsafe {
        let mut len = 0;
        while len < 256 && *path_ptr.add(len) != 0 {
            len += 1;
        }
        match core::str::from_utf8(core::slice::from_raw_parts(path_ptr, len)) {
            Ok(s) => s,
            Err(_) => {
                println!("[execve] Invalid UTF-8 in path");
                return usize::MAX;
            }
        }
    };

    let data = match VFS.get().read_file(path) {
        Some(d) => d,
        None => {
            println!("[execve] File not found: {}", path);
            return usize::MAX;
        }
    };

    let slot = unsafe { TASK_MANAGER.get_free_slot() };
    if slot < 0 {
        println!("[execve] No free task slot!");
        return usize::MAX;
    }

    // === Фиксированная область приложений (как работало раньше) ===
    const APP_TARGET: u32 = 0x02000000;   // 32 MiB
    const APP_SIZE:   u32 = 0x00f00000;   // 2 MiB на приложение

    let target = APP_TARGET + (slot as u32 * APP_SIZE);

    // Маппим память под приложение через 8-ю таблицу страниц
    unsafe {
        TABLES[8].set(target);           // заполняем таблицу
        PAGING.set_table(8, &TABLES[8]); // подключаем в page directory
    }

    println!("[execve] Mapped {} -> {:#x} (size {})", path, target, data.len());

    // Копируем бинарник
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            target as *mut u8,
            data.len()
        );
    }

    // Запускаем задачу
    unsafe {
        TASK_MANAGER.add_task(target as u32);
    }

    println!("[execve] Started application: {} (entry: {:#x})", path, target);
    0
}