use alloc::vec::Vec;
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
use crate::net::{SockAddrIn, SocketState, AF_INET, SOCKET_TABLE, SOCK_DGRAM, SOCK_STREAM};

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
        // НЕ делать sti перед iretd:
        // user eflags (0x202) уже с IF=1, iretd сам включит прерывания.
        // sti здесь давал окно для IRQ0, который затирал EAX.
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

    // exit must switch to another task — never return to the dead one
    if syscall_num == crate::syscalls::SYS_EXIT {
        return sys_exit(current_slot, esp);
    }

    let ret = match syscall_num {
        // === File descriptors ===
        crate::syscalls::SYS_OPEN  => sys_open(current_slot, state.ebx as *const u8, state.ecx as usize),
        crate::syscalls::SYS_READ  => sys_read(current_slot, state.ebx as usize, state.ecx as *mut u8, state.edx as usize),
        crate::syscalls::SYS_WRITE => sys_write(current_slot, state.ebx as usize, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_CLOSE => sys_close(current_slot, state.ebx as usize),

        // === Filesystem / process ===
        crate::syscalls::SYS_MKDIR  => sys_mkdir(state.ebx as *const u8),
        crate::syscalls::SYS_RMDIR  => sys_rmdir(state.ebx as *const u8),
        crate::syscalls::SYS_UNLINK => sys_unlink(state.ebx as *const u8),
        crate::syscalls::SYS_EXECVE => sys_execve(current_slot, state.ebx as *const u8, state.ecx as usize),
        crate::syscalls::SYS_WAIT   => sys_wait(current_slot, state.ebx as i32),
        crate::syscalls::SYS_LS => sys_ls(state.ebx as *const u8, state.ecx as *mut u8, state.edx as usize),
        // === Memory ===
        crate::syscalls::SYS_MALLOC => {
            let size = state.ebx as usize;
            let align = state.ecx as usize;

            let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;
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
                            let need_map = task.page_refcounts.inc(addr);
                            if need_map {
                                task.page_dir.alloc_and_map_user_page(addr);
                            }
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

                    // Аллоцируем новый блок
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
                            let need_map = task.page_refcounts.inc(addr);
                            if need_map {
                                task.page_dir.alloc_and_map_user_page(addr);
                            }
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

                    // Освобождаем старый блок
                    if old_ptr != 0 && old_size > 0 {
                        let page_size = crate::memory::paging::PAGE_SIZE as u32;
                        let old_start_page = old_ptr & !(page_size - 1);
                        let old_end = old_ptr + old_size as u32;
                        let old_end_page = (old_end + page_size - 1) & !(page_size - 1);

                        let mut addr = old_start_page;
                        while addr < old_end_page {
                            let should_unmap = task.page_refcounts.dec(addr);
                            if should_unmap {
                                task.page_dir.unmap(addr);
                            }
                            addr += page_size;
                        }
                    }

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
                    let page_size = crate::memory::paging::PAGE_SIZE as u32;
                    let start_page = ptr & !(page_size - 1);
                    let end = ptr + size;
                    let end_page = (end + page_size - 1) & !(page_size - 1);

                    if let Some(ref mut task) = TASK_MANAGER.tasks[current_slot] {
                        let mut addr = start_page;
                        while addr < end_page {
                            let should_unmap = task.page_refcounts.dec(addr);
                            if should_unmap {
                                task.page_dir.unmap(addr);
                            }
                            addr += page_size;
                        }
                    }
                }
                0
            }
        }
        // === Sockets ===
        crate::syscalls::SYS_SOCKET   => sys_socket(current_slot, state.ebx as u16, state.ecx as u16, state.edx as u8),
        crate::syscalls::SYS_BIND     => sys_bind(current_slot, state.ebx as usize, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_LISTEN   => sys_listen(current_slot, state.ebx as usize, state.ecx as usize),
        crate::syscalls::SYS_ACCEPT4  => sys_accept4(current_slot, state.ebx as usize, state.ecx as *mut u8, state.edx as *mut u32, state.esi as u32),
        crate::syscalls::SYS_CONNECT  => sys_connect(current_slot, state.ebx as usize, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_SENDTO   => sys_sendto(current_slot, state.ebx as usize, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_RECVFROM => sys_recvfrom(current_slot, state.ebx as usize, state.ecx as *mut u8, state.edx as usize),
        crate::syscalls::SYS_SHUTDOWN => sys_shutdown(current_slot, state.ebx as usize, state.ecx as u32),


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
                    let desc = FileDescriptor::new_file(inode, FileMode::ReadWrite);
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
            match current.fd_table.get_mut(fd) {
                Some(FileDescriptor::File { inode, offset, mode }) => {
                    if *mode == FileMode::WriteOnly { return 0; }

                    let mut temp = alloc::vec![0u8; count];
                    let bytes = VFS.get().read_at(*inode, *offset, &mut temp);

                    if bytes > 0 {
                        core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes);
                        *offset += bytes as u64;
                    }
                    return bytes;
                }
                Some(FileDescriptor::Socket { socket_id }) => {
                    // позже: сюда придёт socket read/write
                    // пока можно возвращать 0 или ошибку
                    return 0;
                }
                None => 0,
            };
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
                println!("{:02x?}", &buf);
            }
        }
        return count;
    }

    // Для обычных файлов
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(desc) = current.fd_table.get_mut(fd) {
                match desc {
                    FileDescriptor::File { inode, offset, mode } => {
                        if *mode == FileMode::ReadOnly { return 0; }

                        let written = VFS.get().write_at(*inode, *offset, buf);
                        *offset += written as u64;
                        return written;
                    }
                    FileDescriptor::Socket { socket_id } => {
                        return 0
                    }
                }
            }
        }
    }
    0
}

fn sys_close(current_slot: usize, fd: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(desc) = current.fd_table.get(fd) {
                if let FileDescriptor::Socket { socket_id } = *desc {
                    SOCKET_TABLE.lock().free(socket_id);
                }
            }
            if current.fd_table.close(fd) {
                return 0;
            }
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

// ====================== EXIT ======================
/// Mark current task as zombie and switch to another task.
/// Returns the new task's CPU-state pointer so the syscall iretd
/// resumes a different task (never the dead one).
fn sys_exit(current_slot: usize, esp: u32) -> u32 {
    unsafe {
        if current_slot != 0 {
            if let Some(ref mut t) = TASK_MANAGER.tasks[current_slot] {
                t.running = false;
                t.zombie = true;
                // exit code currently always 0; can take from ebx later
                t.exit_code = 0;
            }
        }
        // Force a context switch away from the zombie
        TASK_MANAGER.schedule(esp as *mut CPUState) as u32
    }
}

// ====================== WAIT ======================
/// Block until a child matching `pid` becomes a zombie, then reap it.
/// `pid == -1` waits for any child.
/// Returns the reaped child's slot (pid), or usize::MAX if no such child can ever appear.
fn sys_wait(current_slot: usize, pid: i32) -> usize {
    let parent = current_slot as i8;

    loop {
        let found = unsafe { TASK_MANAGER.find_zombie_child(parent, pid) };
        if let Some((child_slot, _exit_code)) = found {
            unsafe {
                TASK_MANAGER.reap(child_slot);
            }
            return child_slot;
        }

        // No zombie yet — sleep until the next interrupt (timer / keyboard),
        // same pattern as blocking stdin. Scheduler can run other tasks
        // while we are in hlt; when we are scheduled again we re-check.
        unsafe {
            asm!("sti");
            asm!("hlt");
            asm!("cli");
        }
    }
}

// ====================== EXECVE ======================
/// Spawn a new task from an ELF image in memory.
/// Returns the new task's slot (pid) on success, or usize::MAX on failure.
pub fn sys_execve(parent_slot: usize, buf_ptr: *const u8, count: usize) -> usize {
    let mut kernel_buf = alloc::vec![0u8; count];
    unsafe {
        core::ptr::copy_nonoverlapping(buf_ptr, kernel_buf.as_mut_ptr(), count);
    }
    let buf = &kernel_buf[..];
    if count == 0 {
        return usize::MAX;
    }
    let slot_i8 = unsafe { TASK_MANAGER.get_free_slot() };
    if slot_i8 < 0 {
        println!("[execve] No free task slot!");
        return usize::MAX;
    }
    let slot = slot_i8 as usize;

    // Стек пользователя — высоко в user space, растёт вниз
    // Не зависит от base ELF
    const USER_STACK_TOP: u32 = 0xBFFF_F000;
    const USER_STACK_PAGES: u32 = 32; // 128 KiB
    // Heap — отдельный регион
    let heap_start = 0x4000_0000 + (slot as u32 * 0x1000_0000);

    unsafe {
        asm!("cli");

        let mut task = Task::new_task();
        let kernel_pd_phys = crate::memory::paging::KERNEL_PD_PHYS;

        // Kernel mappings в PD задачи
        let pd_virt = &task.page_dir as *const PageDirectory as u32;
        let pd_phys = if pd_virt >= crate::memory::paging::KERNEL_OFFSET {
            pd_virt - crate::memory::paging::KERNEL_OFFSET
        } else {
            pd_virt
        };
        copy_kernel_mappings(&mut task.page_dir, pd_phys);

        // User stack
        for i in 0..USER_STACK_PAGES {
            let page = USER_STACK_TOP - (i + 1) * PAGE_SIZE as u32;
            task.page_dir.alloc_and_map_user_page(page);
        }
        // Немного heap
        for i in 0..8u32 {
            task.page_dir.alloc_and_map_user_page(heap_start + i * PAGE_SIZE as u32);
        }

        // Переключаемся на PD задачи и грузим ELF по его p_vaddr
        // task.page_dir.switch();

        let entry_point = match crate::elf::load_elf(buf, &mut task.page_dir) {
            Ok(e) => e,
            Err(e) => {
                println!("[execve] ELF load failed: {:?}", e);
                if kernel_pd_phys != 0 {
                    asm!("mov cr3, {}", in(reg) kernel_pd_phys);
                }
                asm!("sti");
                return usize::MAX;
            }
        };

        // // Назад в kernel PD
        // if kernel_pd_phys != 0 {
        //     asm!("mov cr3, {}", in(reg) kernel_pd_phys);
        // }

        // Кладём Task в массив и фиксируем указатели
        TASK_MANAGER.tasks[slot] = Some(task);
        TASK_MANAGER.task_count += 1;

        if let Some(ref mut t) = TASK_MANAGER.tasks[slot] {
            let kernel_stack_top = (t.stack.as_ptr() as usize
                + crate::multitasking::task::STACK_SIZE) as u32;
            t.kernel_stack = kernel_stack_top;

            let state_ptr = (kernel_stack_top as usize
                - crate::multitasking::task::HEADROOM
                - core::mem::size_of::<CPUState>())
                as *mut CPUState;
            t.cpu_state_ptr = state_ptr as u32;

            *state_ptr = CPUState {
                eax: 0, ebx: 0, ecx: 0, edx: 0,
                esi: 0, edi: 0, ebp: 0,
                eip:    entry_point,   // e_entry из ELF, без сдвига
                cs:     0x1B,
                eflags: 0x202,
                esp:    USER_STACK_TOP,
                ss:     0x23,
            };

            let pd_virt = &t.page_dir as *const _ as u32;
            let pd_phys = if pd_virt >= crate::memory::paging::KERNEL_OFFSET {
                pd_virt - crate::memory::paging::KERNEL_OFFSET
            } else {
                pd_virt
            };
            t.page_dir.entries[1023] = pd_phys
                | crate::memory::paging::PDEFlags::PRESENT
                | crate::memory::paging::PDEFlags::WRITABLE;

            t.running = true;
            t.heap_next = heap_start;
            t.parent = parent_slot as i8;
            t.zombie = false;
            t.exit_code = 0;

            println!("[execve] OK pid={} entry={:#x} stack={:#x} pd_phys={:#x} parent={}",
                     slot, entry_point, USER_STACK_TOP, pd_phys, parent_slot);
        }

        asm!("sti");
        slot // return pid to caller
    }
}

use crate::net::stack::{NET_STACK, poll_stack};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};
use smoltcp::socket::{tcp, udp};

fn sys_socket(current_slot: usize, domain: u16, ty: u16, protocol: u8) -> usize {
    let mut stack_guard = match NET_STACK.try_lock() {
        Some(g) => g,
        None => return usize::MAX,
    };
    let stack = match stack_guard.as_mut() {
        Some(s) => s,
        None => return usize::MAX,
    };

    let (socket_id, _handle) = match stack.create_socket(domain, ty, protocol) {
        Some(v) => v,
        None => return usize::MAX,
    };

    // один и тот же id в NET_STACK и SOCKET_TABLE
    {
        let mut table = SOCKET_TABLE.lock();
        if !table.insert_with_id(socket_id, domain, ty, protocol, current_slot) {
            stack.remove_handle(socket_id);
            return usize::MAX;
        }
    }

    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(fd) = current.fd_table.alloc_fd() {
                let desc = FileDescriptor::new_socket(socket_id);
                if current.fd_table.insert(fd, desc) {
                    return fd;
                }
            }
        }
    }

    // rollback
    {
        let mut table = SOCKET_TABLE.lock();
        table.free(socket_id);
    }
    stack.remove_handle(socket_id);
    usize::MAX
}

fn sys_bind(current_slot: usize, fd: usize, addr_ptr: *const u8, addrlen: usize) -> usize {
    println!("current_slot: {}, fd: {} addr_ptr: {:x} addrlen: {}", current_slot, fd, addr_ptr as usize, addrlen);
    if addrlen < core::mem::size_of::<SockAddrIn>() {
        return usize::MAX;
    }
    let addr = unsafe { *(addr_ptr as *const SockAddrIn) };

    let socket_id = unsafe {
        match TASK_MANAGER.tasks[current_slot]
            .as_ref()
            .and_then(|t| t.fd_table.get(fd))
        {
            Some(FileDescriptor::Socket { socket_id }) => *socket_id,
            _ => return usize::MAX,
        }
    };

    let mut stack_guard = NET_STACK.lock();
    let stack = match stack_guard.as_mut() {
        Some(s) => s,
        None => return usize::MAX,
    };

    let (handle, is_tcp) = match stack.get_handle(socket_id) {
        Some(h) => h,
        None => return usize::MAX,
    };

    let port = u16::from_be(addr.sin_port);

    // smoltcp: 0.0.0.0 / UNSPECIFIED → addr: None (слушать на всех адресах).
    // Если передать IpEndpoint{0.0.0.0, port}, стек биндится
    // только на нулевой адрес и отвечает ICMP port unreachable
    // на пакеты к 10.0.2.15 — как в твоём pcap.
    let listen = if addr.sin_addr.s_addr == 0 {
        IpListenEndpoint { addr: None, port }
    } else {
        IpListenEndpoint {
            addr: Some(IpAddress::Ipv4(Ipv4Address(addr.sin_addr.s_addr.to_be_bytes()))),
            port,
        }
    };

    let result = if is_tcp {
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        socket.listen(listen).map_err(|_| ()).map(|_| ())
    } else {
        let socket = stack.sockets.get_mut::<udp::Socket>(handle);
        match socket.bind(listen) {
            Ok(()) => {
                println!("udp bind ok port={}", port);
                Ok(())
            }
            Err(e) => {
                println!("udp bind err: {:?} port={}", e, port);
                Err(())
            }
        }
    };

    // обновляем наше состояние
    if result.is_ok() {
        let mut table = SOCKET_TABLE.lock();
        if let Some(sock) = table.get_mut(socket_id) {
            sock.local_addr = Some(addr);
            sock.state = SocketState::Bound;
        }
    }

    if result.is_ok() { 0 } else { usize::MAX }
}

fn sys_listen(current_slot: usize, fd: usize, backlog: usize) -> usize {
    // Для smoltcp TCP listen уже делается в bind (socket.listen()).
    // Здесь просто меняем состояние.
    let socket_id = unsafe {
        match TASK_MANAGER.tasks[current_slot]
            .as_ref()
            .and_then(|t| t.fd_table.get(fd))
        {
            Some(FileDescriptor::Socket { socket_id }) => *socket_id,
            _ => return usize::MAX,
        }
    };

    let mut table = SOCKET_TABLE.lock();
    if let Some(sock) = table.get_mut(socket_id) {
        if sock.state != SocketState::Bound {
            return usize::MAX;
        }
        sock.backlog = backlog.min(128);
        sock.state = SocketState::Listening;
        return 0;
    }
    usize::MAX
}

fn sys_accept4(
    current_slot: usize,
    fd: usize,
    _addr: *mut u8,
    _addrlen: *mut u32,
    _flags: u32,
) -> usize {
    // Пока заглушка — возвращаем ошибку
    // Позже: берём из accept_queue, создаём новый сокет + новый fd
    usize::MAX
}

fn sys_connect(current_slot: usize, fd: usize, addr_ptr: *const u8, addrlen: usize) -> usize {
    if addrlen < core::mem::size_of::<SockAddrIn>() {
        return usize::MAX;
    }
    let addr = unsafe { *(addr_ptr as *const SockAddrIn) };

    let socket_id = unsafe {
        match TASK_MANAGER.tasks[current_slot]
            .as_ref()
            .and_then(|t| t.fd_table.get(fd))
        {
            Some(FileDescriptor::Socket { socket_id }) => *socket_id,
            _ => return usize::MAX,
        }
    };

    let mut stack_guard = NET_STACK.lock();
    let stack = match stack_guard.as_mut() {
        Some(s) => s,
        None => return usize::MAX,
    };

    let (handle, is_tcp) = match stack.get_handle(socket_id) {
        Some(h) => h,
        None => return usize::MAX,
    };

    let endpoint = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Address(addr.sin_addr.s_addr.to_be_bytes())),
        port: u16::from_be(addr.sin_port),
    };

    let result = if is_tcp {
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        // local endpoint можно оставить unspecified
        socket.connect(stack.iface.context(), endpoint, 0).map_err(|_| ())
    } else {
        // UDP connect (опционально)
        let socket = stack.sockets.get_mut::<udp::Socket>(handle);
        socket.bind(IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0))
            .map_err(|_| ())
            .and_then(|_| {
                // smoltcp UDP не имеет connect, просто запоминаем peer
                Ok(())
            })
    };

    if result.is_ok() {
        let mut table = SOCKET_TABLE.lock();
        if let Some(sock) = table.get_mut(socket_id) {
            sock.peer_addr = Some(addr);
            sock.state = SocketState::Connected;
        }
        0
    } else {
        usize::MAX
    }
}
fn sys_sendto(current_slot: usize, fd: usize, buf: *const u8, len: usize) -> usize {
    let socket_id = unsafe {
        match TASK_MANAGER.tasks[current_slot]
            .as_ref()
            .and_then(|t| t.fd_table.get(fd))
        {
            Some(FileDescriptor::Socket { socket_id }) => *socket_id,
            _ => return 0,
        }
    };

    let mut stack_guard = match NET_STACK.try_lock() {
        Some(g) => g,
        None => return 0,
    };
    let stack = match stack_guard.as_mut() {
        Some(s) => s,
        None => return 0,
    };

    stack.poll(crate::time::jiffies() as i64);

    let (handle, is_tcp) = match stack.get_handle(socket_id) {
        Some(h) => h,
        None => return 0,
    };

    let data = unsafe { core::slice::from_raw_parts(buf, len) };

    if is_tcp {
        return stack.sockets.get_mut::<tcp::Socket>(handle)
            .send_slice(data).unwrap_or(0);
    }

    let peer = {
        let table = SOCKET_TABLE.lock();
        table.get(socket_id).and_then(|s| s.peer_addr)
    };

    let Some(peer) = peer else { return 0 };

    let endpoint = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Address(peer.sin_addr.s_addr.to_be_bytes())),
        port: u16::from_be(peer.sin_port),
    };

    let socket = stack.sockets.get_mut::<udp::Socket>(handle);
    match socket.send_slice(data, endpoint) {
        Ok(()) => {
            stack.poll(crate::time::jiffies() as i64);
            len
        }
        Err(_) => 0,
    }
}

fn sys_recvfrom(current_slot: usize, fd: usize, buf: *mut u8, len: usize) -> usize {
    let socket_id = unsafe {
        match TASK_MANAGER.tasks[current_slot]
            .as_ref()
            .and_then(|t| t.fd_table.get(fd))
        {
            Some(FileDescriptor::Socket { socket_id }) => *socket_id,
            _ => return 0,
        }
    };

    let mut stack_guard = match NET_STACK.try_lock() {
        Some(g) => g,
        None => return 0,
    };
    let stack = match stack_guard.as_mut() {
        Some(s) => s,
        None => return 0,
    };

    let ts = crate::time::jiffies() as i64; // лучше реальное время
    stack.poll(ts);

    let (handle, is_tcp) = match stack.get_handle(socket_id) {
        Some(h) => h,
        None => return 0,
    };

    let mut temp = alloc::vec![0u8; len.min(1500)];

    let received = if is_tcp {
        stack.sockets.get_mut::<tcp::Socket>(handle)
            .recv_slice(&mut temp).unwrap_or(0)
    } else {
        let socket = stack.sockets.get_mut::<udp::Socket>(handle);
        match socket.recv_slice(&mut temp) {
            Ok((size, ep)) => {
                // peer для последующего sendto (ep: IpEndpoint)
                let mut table = SOCKET_TABLE.lock();
                if let Some(sock) = table.get_mut(socket_id) {
                    let ip = match ep.endpoint.addr {
                        IpAddress::Ipv4(a) => u32::from_be_bytes(a.0),
                        _ => 0,
                    };
                    sock.peer_addr = Some(SockAddrIn {
                        sin_family: AF_INET,
                        sin_port: ep.endpoint.port.to_be(),
                        sin_addr: crate::net::InAddr { s_addr: ip },
                        sin_zero: [0; 8],
                    });
                }
                size
            }
            Err(_) => 0,
        }
    };

    if received > 0 {
        unsafe { core::ptr::copy_nonoverlapping(temp.as_ptr(), buf, received); }
    }
    received
}

fn sys_shutdown(current_slot: usize, fd: usize, _how: u32) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(FileDescriptor::Socket { socket_id }) = current.fd_table.get(fd) {
                let mut table = SOCKET_TABLE.lock();
                if let Some(sock) = table.get_mut(*socket_id) {
                    sock.state = SocketState::Closed;
                    return 0;
                }
            }
        }
    }
    usize::MAX
}



