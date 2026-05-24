use crate::drivers::pic::PICS;
use crate::multitasking::task::{CPUState, TASK_MANAGER};
use crate::{print, println};
use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ffi::CStr;
use crate::filesystem::file::{FileDescriptor, FileMode};
use crate::filesystem::VFS;
use crate::memory::paging::{PDEFlags, PTEFlags, PAGING};

pub const SYSCALL_INT: u8 = 0x80;

#[naked]
pub extern "C" fn syscall() {
    unsafe {
        asm!(
        "cli",
        // Сохраняем все регистры (точно как в timer)
        "push ebp",
        "push edi",
        "push esi",
        "push edx",
        "push ecx",
        "push ebx",
        "push eax",
        // Передаём указатель на стек (CPUState)
        "push esp",
        "call syscall_handler",
        "add esp, 4",
        // Возвращаем новый esp (handler может вернуть тот же)
        "mov esp, eax",
        // Восстанавливаем регистры
        "pop eax",
        "pop ebx",
        "pop ecx",
        "pop edx",
        "pop esi",
        "pop edi",
        "pop ebp",
        "sti",
        "iretd",
        options(noreturn)
        );
    }
}

#[no_mangle]
pub extern "C" fn syscall_handler(esp: u32) -> u32 {
    let state = unsafe { &mut *(esp as *mut CPUState) };

    // Номер syscall лежит в eax (после push'ей)
    let syscall_num = state.eax;

    let ret = match syscall_num {
        // === Process control ===
        crate::syscalls::SYS_EXIT => {
            unsafe { TASK_MANAGER.remove_current_task(); }
            0
        }

        // === File descriptors ===
        crate::syscalls::SYS_OPEN  => sys_open(state.ebx as *const u8, state.ecx as usize),
        crate::syscalls::SYS_READ  => sys_read(state.ebx as usize, state.ecx as *mut u8, state.edx as usize),
        crate::syscalls::SYS_WRITE => sys_write(state.ebx as usize, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_CLOSE => sys_close(state.ebx as usize),

        // === Filesystem operations ===
        crate::syscalls::SYS_MKDIR  => sys_mkdir(state.ebx as *const u8),
        crate::syscalls::SYS_RMDIR  => sys_rmdir(state.ebx as *const u8),
        crate::syscalls::SYS_UNLINK => sys_unlink(state.ebx as *const u8),
        crate::syscalls::SYS_EXECVE => sys_execve(state.ebx as *const u8),

        // === Memory ===
        crate::syscalls::SYS_MALLOC => {
            let layout = Layout::from_size_align(state.ebx as usize, state.ecx as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.alloc(layout) as usize }
        }

        crate::syscalls::SYS_FREE => {
            let layout = Layout::from_size_align(state.ecx as usize, state.edx as usize)
                .unwrap_or(Layout::new::<u8>());
            unsafe { ALLOCATOR.dealloc(state.ebx as *mut u8, layout); }
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
        let mut written = 0;
        let buf = unsafe { core::slice::from_raw_parts(buf_ptr, count) };
        if fd == 0 || fd == 1 {
            for &byte in buf.iter().take(count) {
                print!("{}", byte as char);
                written += 1
            }
        } else {
            written = VFS.get().write_at(desc.inode, desc.offset, buf);
            desc.offset += written as u64;
        }

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

// ====================== EXECVE (ИСПРАВЛЕННЫЙ) ======================
pub fn sys_execve(path_ptr: *const u8) -> usize {
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

    const APP_TARGET: u32 = 0x40000000;
    const APP_SIZE: u32 = 4 * 1024 * 1024; // 4 MiB на задачу
    let target = APP_TARGET + (slot as u32 * APP_SIZE);
    let user_stack_top = target + APP_SIZE - 0x2000; // 8 KiB стек

    // ===== НОВЫЙ БЕЗОПАСНЫЙ МАППИНГ =====
    let pages = (APP_SIZE >> 12) as u32;        // ← оставляем только эту
    unsafe {
        for i in 0..pages {
            let virt_addr = target + (i << 12);
            PAGING.alloc_and_map(virt_addr);     // теперь работает
        }
    }


    println!("[execve] Mapping done for task {} at {:#x} ({} pages)", slot, target, pages);

    // 1. Kernel может читать/писать (должен работать)
    unsafe {
        let test_ptr = target as *mut u32;
        *test_ptr = 0xDEADBEEF;
        println!("[execve] Kernel test write/read: {:#x}", *test_ptr);
    }

    // 2. Проверка translate (должен вернуть Some(phys))
    if let Some(phys) = unsafe { PAGING.dir.translate(target) } {
        println!("[execve] translate OK: {:#x} → {:#x}", target, phys);
    } else {
        println!("[execve] !!! translate FAILED for {:#x}", target);
    }

    // 3. Самое важное — проверка PDE (USER бит)
    let pd_idx = (target >> 22) as usize;
    let pde_val = unsafe { PAGING.dir.entries[pd_idx] };
    let has_user = (pde_val & PDEFlags::USER) != 0;
    println!("[execve] PDE[{}] = {:#x} | USER bit = {}", pd_idx, pde_val, has_user);

    if !has_user {
        println!("[execve] CRITICAL: PDE missing USER bit → user mode will fault!");
    }

    // ----- Загрузка ELF -----
    match crate::elf::load_elf(&data, target, APP_SIZE) {
        Ok(entry_point) => {
            unsafe {
                TASK_MANAGER.add_task(entry_point, user_stack_top);
            }
            println!("[execve] Started: {} (entry {:#x})", path, entry_point);
            0
        }
        Err(e) => {
            println!("[execve] ELF load failed: {:?}", e);
            usize::MAX
        }
    }
}