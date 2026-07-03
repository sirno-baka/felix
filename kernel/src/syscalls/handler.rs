use crate::drivers::pic::PICS;
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::multitasking::task::{CPUState, Task, TASK_MANAGER};
use crate::{print, println};
use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ffi::CStr;
use crate::filesystem::file::{FileDescriptor, FileMode};
use crate::filesystem::VFS;
use crate::memory::paging::{PageDirectory, PDEFlags, PTEFlags, PAGING, PhysAddr, VirtAddr, copy_kernel_mappings, PAGE_SIZE};

pub const SYSCALL_INT: u8 = 0x80;

#[naked]
pub extern "C" fn syscall() {
    unsafe {
        asm!(
        "cli",

        "push ebp",
        "push edi",
        "push esi",
        "push edx",
        "push ecx",
        "push ebx",
        "push eax",

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
        crate::syscalls::SYS_LS => sys_ls(state.ebx as *const u8, state.ecx as *mut u8, state.edx as usize),
        // === Memory ===
        crate::syscalls::SYS_MALLOC => {
            let size = state.ebx as usize;
            let align = state.ecx as usize;

            let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;
            // unsafe {
            //     println!("[MALLOC] task={} size={} align={} heap_next_before=0x{:x}",
            //              current_slot, size, align,
            //              if let Some(t) = TASK_MANAGER.tasks.get(current_slot) {
            //                  t.as_ref().map_or(0, |tt| tt.heap_next)
            //              } else { 0 });
            // }
            if current_slot == 0 || current_slot >= 8 {
                0
            } else {
                unsafe {
                    let task = TASK_MANAGER.tasks[current_slot].as_mut().unwrap();
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
                            task.page_dir.alloc_and_map_user_page(addr);
                            addr += page_size;
                        }
                        core::ptr::write_bytes(start as *mut u8, 0, size);
                    }

                    task.heap_next = start + size as u32;
                    start as usize
                }
            }
        }

        crate::syscalls::SYS_REALLOC => {
            let old_ptr = state.ebx;
            let old_size = state.ecx as usize;
            let new_size = state.edx as usize;

            let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;
            if current_slot == 0 || current_slot >= 8 {
                0
            } else {
                unsafe {
                    let task = TASK_MANAGER.tasks[current_slot].as_mut().unwrap();

                    let mut new_start = task.heap_next;
                    let align_mask = 7u32;
                    new_start = (new_start + align_mask) & !align_mask;

                    if new_size > 0 {
                        let page_size = crate::memory::paging::PAGE_SIZE as u32;
                        let start_page = new_start & !(page_size - 1);
                        let end = new_start + new_size as u32;
                        let end_page = (end + page_size - 1) & !(page_size - 1);

                        let mut addr = start_page;
                        while addr < end_page {
                            task.page_dir.alloc_and_map_user_page(addr);
                            addr += page_size;
                        }

                        core::ptr::write_bytes(new_start as *mut u8, 0, new_size);

                        if old_ptr != 0 && old_size > 0 {
                            let copy_len = old_size.min(new_size);
                            core::ptr::copy_nonoverlapping(
                                old_ptr as *const u8,
                                new_start as *mut u8,
                                copy_len,
                            );
                        }
                    }

                    task.heap_next = new_start + new_size as u32;
                    new_start as usize
                }
            }
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
                    let pages = (size + 4095) / 4096;
                    for i in 0..pages {
                        let virt_addr = ptr + i * 4096;
                        if let Some(ref mut task) = TASK_MANAGER.tasks[current_slot] {
                            task.page_dir.unmap(virt_addr);
                        }
                    }
                    // println!("free: 0x{:x} ({} bytes) task={}", ptr, size, current_slot);
                }
                0
            }
        }


        _ => 0,
    };
    
    // println!("ret: 0x{:x}", ret);
    // КЛАДЁМ результат обратно в eax (чтобы пользовательская программа его получила)
    state.eax = ret as u32;

    // Для int 0x80 EOI на PIC НЕ нужен!
    // PICS.end_interrupt(SYSCALL_INT);  ← УДАЛИТЬ

    // Возвращаем ESP — стек НЕ ломается
    esp
}

// ====================== FILE DESCRIPTORS ======================

fn sys_open(current_slot: usize, path_ptr: *const u8, _flags: usize) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };

    if let Some(inode) = VFS.get().resolve_path(path) {
        unsafe {
            if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
                if let Some(fd) = current.fd_table.alloc_fd() {
                    let desc = FileDescriptor::new(inode, FileMode::ReadWrite);
                    current.fd_table.insert(fd, desc);
                    return fd;
                }
            }
        }
    }
    usize::MAX
}

fn sys_read(current_slot: usize, fd: usize, buf_ptr: *mut u8, count: usize) -> usize {
    // fd 0 = stdin — читаем из буфера клавиатуры (блокирующий ввод)
    if fd == 0 {
        return sys_read_stdin(buf_ptr, count);
    }

    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(desc) = current.fd_table.get_mut(fd) {
                if desc.mode == FileMode::WriteOnly { return 0; }

                let mut temp = alloc::vec![0u8; count];
                let bytes = VFS.get().read_at(desc.inode, desc.offset, &mut temp);

                if bytes > 0 {
                    core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes);
                    desc.offset += bytes as u64;
                }
                return bytes;
            }
        }
    }
    0
}

/// Блокирующее чтение из stdin (буфер клавиатуры).
///
/// Сигналы `sti` чтобы прерывания клавиатуры (IRQ1) и таймера (IRQ0)
/// могли срабатывать. Когда буфер пуст, `hlt` усыпляет CPU до следующего
/// прерывания. KMutex буфера сам делает `cli`/`sti` при lock/unlock,
/// поэтому гонки между чтением и обработчиком клавиатуры нет.
fn sys_read_stdin(buf_ptr: *mut u8, count: usize) -> usize {
    let mut read = 0;

    // Включаем прерывания — обработчик клавиатуры сможет наполнять буфер
    unsafe { asm!("sti"); }

    while read < count {
        let byte = {
            // KMutex: lock → cli, drop → sti
            let mut guard = KEYBOARD_BUFFER.lock();
            match &mut *guard {
                Some(b) if !b.is_empty() => Some(b.pop()),
                _ => None,
            }
        };

        match byte {
            Some(b) => {
                unsafe { *buf_ptr.add(read) = b; }
                read += 1;
            }
            None => {
                // Буфер пуст — спим до следующего прерывания
                unsafe { asm!("hlt"); }
            }
        }
    }

    // Восстанавливаем состояние (syscall entry сделал cli)
    unsafe { asm!("cli"); }
    read
}

fn sys_write(current_slot: usize, fd: usize, buf_ptr: *const u8, count: usize) -> usize {
    if count == 0 {
        return 0;
    }

    let mut kernel_buf = alloc::vec![0u8; count];

    // Адреса пользовательских буферов всегда доступны через PD задачи
    // (таск PD активен во время syscall). Трансляция не нужна.
    unsafe {
        core::ptr::copy_nonoverlapping(buf_ptr, kernel_buf.as_mut_ptr(), count);
    }

    let buf = &kernel_buf[..];

    // println!("KERNEL WRITE: fd={} ptr={:p} ptr_usr={:p} len={} task={} {:02x?}", fd, buf_ptr, src_ptr, count, current_slot, &buf[0..buf.len().min(32)]);



    // ====================== STDOUT / STDERR ======================
    if fd == 0 || fd == 1 {
        match core::str::from_utf8(buf) {
            Ok(v) => print!("{}", v),
            Err(_) => {
                println!("[write] invalid utf8! addr={:p} len={} first 32 bytes: {:02x?}",
                         buf_ptr, count, &buf[0..buf.len().min(32)]);
            }
        }
        return count;
    }

    // Для обычных файлов
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(desc) = current.fd_table.get_mut(fd) {
                if desc.mode == FileMode::ReadOnly {
                    return 0;
                }
                let written = VFS.get().write_at(desc.inode, desc.offset, buf);
                desc.offset += written as u64;
                return written;
            }
        }
    }
    0
}

fn sys_close(current_slot: usize, fd: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if current.fd_table.close(fd) { return 0; }
        }
    }
    usize::MAX
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

/// Читает содержимое директории и записывает имена файлов
/// (разделённые '\n') в пользовательский буфер.
/// Возвращает количество записанных байт или 0 при ошибке.
fn sys_ls(path_ptr: *const u8, buf_ptr: *mut u8, buf_size: usize) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("/") };
    let path = if path.is_empty() { "/" } else { path };

    let entries = match VFS.get().list_directory_entries(path) {
        Some(e) => e,
        None => return 0,
    };

    // Сериализуем: "name\nname\n..." + помечаем директории символом '/'
    let mut pos = 0usize;
    for entry in &entries {
        let name = entry.name.as_bytes();
        let is_dir = entry.file_type == 2;
        let entry_len = name.len() + if is_dir { 1 } else { 0 } + 1; // +1 for '\n'

        if pos + entry_len > buf_size {
            break;
        }

        unsafe {
            core::ptr::copy_nonoverlapping(name.as_ptr(), buf_ptr.add(pos), name.len());
            pos += name.len();
            if is_dir {
                *buf_ptr.add(pos) = b'/';
                pos += 1;
            }
            *buf_ptr.add(pos) = b'\n';
            pos += 1;
        }
    }

    pos
}

// ====================== EXECVE ======================
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

    let slot_i8 = unsafe { TASK_MANAGER.get_free_slot() };
    if slot_i8 < 0 {
        println!("[execve] No free task slot!");
        return usize::MAX;
    }
    let slot = slot_i8 as usize;

    const APP_TARGET: u32 = 0x40000000;
    const APP_SIZE: u32 = 4 * 1024 * 1024;
    // Каждая задача имеет собственный page directory — все приложения
    // грузятся по одному виртуальному адресу (== линковому адресу ELF).
    let target = APP_TARGET;
    let user_stack_top = target + APP_SIZE - 0x8000;
    let heap_start = 0x80000000 + (slot as u32 * 0x10000000);

    unsafe {
        asm!("cli");

        let mut task = Task::new_task();

        let pd_phys = &task.page_dir as *const PageDirectory as u32;
        let kernel_pd_phys = crate::memory::paging::KERNEL_PD_PHYS; // сохраняем kernel PD

        copy_kernel_mappings(&mut task.page_dir, pd_phys);

        // println!("[execve] Mapping app at {:#x} (1 MB) for task {}", target, slot);

        let app_pages = (APP_SIZE >> 12) as u32;
        for i in 0..app_pages {
            let virt_addr = target + (i << 12);
            task.page_dir.alloc_and_map_user_page(virt_addr);
            if i % 64 == 0 || i == app_pages - 1 {
                // println!("[execve] Mapped {}/{} app pages", i + 1, app_pages);
            }
        }

        // println!("[execve] Mapping user stack + heap...");
        for i in 0..32u32 {
            let stack_page = user_stack_top - (i * PAGE_SIZE as u32);
            task.page_dir.alloc_and_map_user_page(stack_page);
        }
        for i in 0..8u32 {
            let heap_page = heap_start + (i * PAGE_SIZE as u32);
            task.page_dir.alloc_and_map_user_page(heap_page);
        }

        // === ВРЕМЕННО ПЕРЕКЛЮЧАЕМСЯ НА НОВУЮ ТАБЛИЦУ СТРАНИЦ ===
        // println!("[execve] Switching to task PD for ELF loading...");
        task.page_dir.switch();
        println!("data size: {}", data.len());
        match crate::elf::load_elf(&data, target, APP_SIZE) {
            Ok(entry_point) => {
                // Загрузка прошла успешно — возвращаемся обратно в kernel PD
                if kernel_pd_phys != 0 {
                    unsafe {
                        asm!("mov cr3, {}", in(reg) kernel_pd_phys);
                    }
                }

                task.init(entry_point, user_stack_top, heap_start);
                // println!("[execve DEBUG] slot={} heap_start=0x{:x} heap_next after init=0x{:x}",
                //          slot, heap_start, task.heap_next);
                TASK_MANAGER.tasks[slot] = Some(task);
                TASK_MANAGER.task_count += 1;

                // println!("[execve] SUCCESS: {} started! entry={:#x} task={} stack={:#x}",
                //          path, entry_point, slot, user_stack_top);
                asm!("sti");
                0
            }
            Err(e) => {
                // Если ошибка — тоже возвращаемся в kernel PD
                println!("[execve] ELF load failed: {:?}", e);
                if kernel_pd_phys != 0 {
                    unsafe {
                        asm!("mov cr3, {}", in(reg) kernel_pd_phys);
                    }
                }
                asm!("sti");

                usize::MAX
            }
        }
    }
}