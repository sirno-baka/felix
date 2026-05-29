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

        // Сохраняем все регистры
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

    let syscall_num = state.eax;

    // Фиксируем текущий таск ОДИН раз (чтобы таймер не успел переключить)
    let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;

    let ret = match syscall_num {
        // === Process control ===
        crate::syscalls::SYS_EXIT => {
            unsafe { TASK_MANAGER.remove_current_task(); }
            0
        }

        // === File descriptors ===
        crate::syscalls::SYS_OPEN  => sys_open(current_slot, state.ebx as *const u8, state.ecx as usize),
        crate::syscalls::SYS_READ  => sys_read(current_slot, state.ebx as usize, state.ecx as *mut u8, state.edx as usize),
        crate::syscalls::SYS_WRITE => sys_write(current_slot, state.ebx as usize, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_CLOSE => sys_close(current_slot, state.ebx as usize),

        // === Filesystem operations ===
        crate::syscalls::SYS_MKDIR  => sys_mkdir(state.ebx as *const u8),
        crate::syscalls::SYS_RMDIR  => sys_rmdir(state.ebx as *const u8),
        crate::syscalls::SYS_UNLINK => sys_unlink(state.ebx as *const u8),
        crate::syscalls::SYS_EXECVE => sys_execve(state.ebx as *const u8),
        // === Memory ===
        crate::syscalls::SYS_MALLOC => {
            let size = state.ebx as usize;
            let align = state.ecx as usize;

            let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;
            let ptr = if current_slot == 0 || current_slot >= 8 {
                0
            } else {
                unsafe {
                    let mut paging = PAGING.lock();
                    let task = &mut TASK_MANAGER.tasks[current_slot];
                    let mut start = task.heap_next;

                    let align = if align == 0 { 8 } else { align.next_power_of_two().max(8) };
                    if align > 0 {
                        let align_mask = (align - 1) as u32;
                        start = (start + align_mask) & !align_mask;
                    }

                    if size > 0 {
                        let page_size = crate::memory::paging::PAGE_SIZE as u32;
                        let start_page = start & !(page_size - 1);
                        let end = start + size as u32;
                        let end_page = (end + page_size - 1) & !(page_size - 1);

                        let mut addr = start_page;
                        while addr < end_page {
                            paging.alloc_and_map(addr);
                            addr += page_size;
                        }
                    }

                    let dump_size = size.min(64);
                    unsafe {
                        let data = core::slice::from_raw_parts(start as *const u8, dump_size);
                        println!("malloc: 0x{:x} size={} align={} dump: {:02x?}",
                                 start, size, align, data);
                    }

                    task.heap_next = start + size as u32;
                    start as usize
                }
            };

            // state.eax = ptr as u32;
            ptr
        }

        crate::syscalls::SYS_REALLOC => {
            let old_ptr = state.ebx as *mut u8;
            let old_size = state.ecx as usize;
            let new_size = state.edx as usize;

            let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;
            let new_ptr = if current_slot == 0 || current_slot >= 8 {
                0
            } else {
                unsafe {
                    let mut paging = PAGING.lock();
                    let task = &mut TASK_MANAGER.tasks[current_slot];

                    let mut new_start = task.heap_next;

                    // align
                    let align = 8u32;
                    let align_mask = (align - 1);
                    new_start = (new_start + align_mask) & !align_mask;

                    // map new region
                    if new_size > 0 {
                        let page_size = crate::memory::paging::PAGE_SIZE as u32;
                        let start_page = new_start & !(page_size - 1);
                        let end = new_start + new_size as u32;
                        let end_page = (end + page_size - 1) & !(page_size - 1);

                        let mut addr = start_page;
                        while addr < end_page {
                            paging.alloc_and_map(addr);
                            addr += page_size;
                        }
                    }

                    // === КРИТИЧНЫЙ ФИКС: переводим user-указатели в kernel-адреса ===
                    if old_size > 0 && !old_ptr.is_null() {
                        let copy_len = old_size.min(new_size);

                        let src_addr = old_ptr as u32;
                        let src_ptr = if src_addr >= 0x20000000 {
                            old_ptr as *const u8          // heap-область — как в malloc
                        } else {
                            (src_addr.wrapping_add(0x40000000)) as *const u8
                        };

                        let dst_addr = new_start;
                        let dst_ptr = if dst_addr >= 0x20000000 {
                            new_start as *mut u8
                        } else {
                            (dst_addr.wrapping_add(0x40000000)) as *mut u8
                        };

                        core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_len);
                    }

                    task.heap_next = new_start + new_size as u32;

                    // TODO: безопасный unmap (пока закомментируем, чтобы не ломать страницы)
                    /*
                    if old_size > 0 && !old_ptr.is_null() {
                        let old_pages = ((old_size as u32) + 4095) / 4096;
                        for i in 0..old_pages {
                            let virt = ((old_ptr as u32) & !0xFFF) + i * 4096;
                            let pd_idx = (virt >> 22) as usize;
                            if pd_idx < 1024 && (paging.dir.entries[pd_idx] & PDEFlags::PRESENT) != 0 {
                                paging.dir.unmap(virt);
                            }
                        }
                    }
                    */

                    new_start as usize
                }
            };

            println!("realloc: old=0x{:x} → new=0x{:x} (size={})", old_ptr as u32, new_ptr, new_size);
            new_ptr as usize
        }


        crate::syscalls::SYS_FREE => {
            let ptr = state.ebx as u32;
            let layout = Layout::from_size_align(state.ecx as usize, state.edx as usize)
                .unwrap_or(Layout::new::<u8>());
            let size = layout.size() as u32;

            if ptr == 0 || size == 0 {
                0
            } else {
                unsafe {
                    let mut paging = PAGING.lock();
                    let pages = (size + 4095) / 4096;
                    for i in 0..pages {
                        let virt_addr = ptr + i * 4096;
                        paging.dir.unmap(virt_addr);   // уже есть в PageDirectory
                    }
                    println!("free: 0x{:x} ({} bytes)", ptr, size);
                }
                0
            }
        }


        _ => 0,
    };
    
    println!("ret: 0x{:x}", ret);
    // КЛАДЁМ результат обратно в eax (чтобы пользовательская программа его получила)
    state.eax = ret as u32;

    // Для int 0x80 EOI на PIC НЕ нужен!
    // PICS.end_interrupt(SYSCALL_INT);  ← УДАЛИТЬ

    // Возвращаем ESP — стек НЕ ломается
    esp
}

// ====================== FILE DESCRIPTORS ======================

fn sys_open(current_slot: usize, path_ptr: *const u8, _flags: usize) -> usize {
    let current = unsafe { &mut TASK_MANAGER.tasks[current_slot] };
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };

    if let Some(inode) = VFS.get().resolve_path(path) {
        if let Some(fd) = current.fd_table.alloc_fd() {
            let desc = FileDescriptor::new(inode, FileMode::ReadWrite);
            current.fd_table.insert(fd, desc);
            return fd;
        }
    }
    usize::MAX
}

fn sys_read(current_slot: usize, fd: usize, buf_ptr: *mut u8, count: usize) -> usize {
    let current = unsafe { &mut TASK_MANAGER.tasks[current_slot] };
    let real_buf_ptr = if (buf_ptr as u32) < 0x40000000 {
        (buf_ptr as u32 + 0x40000000) as *mut u8
    } else {
        buf_ptr
    };

    if let Some(desc) = current.fd_table.get_mut(fd) {
        if desc.mode == FileMode::WriteOnly { return 0; }

        let mut temp = alloc::vec![0u8; count];
        let bytes = VFS.get().read_at(desc.inode, desc.offset, &mut temp);

        if bytes > 0 {
            unsafe { core::ptr::copy_nonoverlapping(temp.as_ptr(), real_buf_ptr, bytes); }
            desc.offset += bytes as u64;
        }
        bytes
    } else {
        0
    }
}

fn sys_write(current_slot: usize, fd: usize, buf_ptr: *const u8, count: usize) -> usize {
    if count == 0 {
        return 0;
    }

    let mut kernel_buf = alloc::vec![0u8; count];
    let user_addr = buf_ptr as u32;
    let src_ptr = if user_addr >= 0x20000000 {
        buf_ptr as *const u8
    } else {
        (user_addr.wrapping_add(0x40000000)) as *const u8
    };
    println!("KERNEL WRITE: fd={} ptr={:p} ptr_usr={:p} len={} task={}", fd, buf_ptr, src_ptr, count, current_slot);
    unsafe {
        core::ptr::copy_nonoverlapping(src_ptr, kernel_buf.as_mut_ptr(), count);
    }

    let buf = &kernel_buf[..];

    // ====================== STDOUT / STDERR ======================
    if fd == 0 || fd == 1 {
        match core::str::from_utf8(buf) {
            Ok(v) => println!("{}", v),
            Err(_) => {
                println!("[write] invalid utf8! addr={:p} len={} first 32 bytes: {:02x?}",
                         buf_ptr, count, &buf[0..buf.len().min(32)]);
            }
        }
        return count;
    }

    // Для обычных файлов (пока как было)
    let current = unsafe { &mut TASK_MANAGER.tasks[current_slot] };
    if let Some(desc) = current.fd_table.get_mut(fd) {
        if desc.mode == FileMode::ReadOnly {
            return 0;
        }
        let written = VFS.get().write_at(desc.inode, desc.offset, buf);
        desc.offset += written as u64;
        written
    } else {
        0
    }
}

fn sys_close(current_slot: usize, fd: usize) -> usize {
    let current = unsafe { &mut TASK_MANAGER.tasks[current_slot] };
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

    // println!("[execve] Start exec: {:}", path);

    let data = match VFS.get().read_file(path) {
        Some(d) => d,
        None => {
            println!("[execve] File not found: {}", path);
            return usize::MAX;
        }
    };
    // println!("1");

    let slot = unsafe { TASK_MANAGER.get_free_slot() };
    if slot < 0 {
        println!("[execve] No free task slot!");
        return usize::MAX;
    }
    // println!("2");

    const APP_TARGET: u32 = 0x40000000;
    //0x40000000
    //  0x400000
    const APP_SIZE: u32 = 4 * 1024 * 1024; // 4 MiB на задачу
    let target = APP_TARGET + (slot as u32 * APP_SIZE);
    let user_stack_top = target + APP_SIZE - 0x10000;  // 64 KiB стек        println!("3");
    let heap_start = 0x20000000 + (slot as u32 * 0x10000000);
    // ===== НОВЫЙ БЕЗОПАСНЫЙ МАППИНГ =====
    let pages = (APP_SIZE >> 12) as u32;        // ← оставляем только эту
    unsafe {
        let mut paging = PAGING.lock();
        for i in 0..pages {
            let virt_addr = target + (i << 12);
            paging.alloc_and_map(virt_addr);

        }
    }
    // println!("4");
    //
    //
    // println!("[execve] Mapping done for task {} at {:#x} ({} pages)", slot, target, pages);
    //
    // // 1. Kernel может читать/писать (должен работать)
    // unsafe {
    //     let test_ptr = target as *mut u32;
    //     *test_ptr = 0xDEADBEEF;
    //     println!("[execve] Kernel test write/read: {:#x}", *test_ptr);
    // }
    //
    // // 2. Проверка translate (должен вернуть Some(phys))
    // if let Some(phys) = unsafe { PAGING.lock().dir.translate(target) } {
    //     println!("[execve] translate OK: {:#x} → {:#x}", target, phys);
    // } else {
    //     println!("[execve] !!! translate FAILED for {:#x}", target);
    // }
    //
    // // 3. Самое важное — проверка PDE (USER бит)
    // let pd_idx = (target >> 22) as usize;
    // let pde_val = unsafe { PAGING.lock().dir.entries[pd_idx] };
    // let has_user = (pde_val & PDEFlags::USER) != 0;
    // println!("[execve] PDE[{}] = {:#x} | USER bit = {}", pd_idx, pde_val, has_user);
    //
    // if !has_user {
    //     println!("[execve] CRITICAL: PDE missing USER bit → user mode will fault!");
    // }

    // ----- Загрузка ELF -----
    match crate::elf::load_elf(&data, target, APP_SIZE) {
        Ok(entry_point) => {
            unsafe {
                TASK_MANAGER.add_task(entry_point, user_stack_top, heap_start);
            }
            // println!("[execve] Started: {} (entry {:#x})", path, entry_point);
            0
        }
        Err(e) => {
            println!("[execve] ELF load failed: {:?}", e);
            usize::MAX
        }
    }
}