use alloc::vec::Vec;
use crate::drivers::pic::PICS;
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::multitasking::task::{CPUState, Task, TASK_MANAGER};
use crate::{print, println};
use crate::memory::allocator::ALLOCATOR;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::asm;
use core::ffi::CStr;
use crate::filesystem::file::{FileDescriptor, FileDescriptorTable, FileMode, PipeEnd};
use crate::filesystem::VFS;
use crate::pipe;
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
        crate::syscalls::SYS_EXECVE => {
            let params = read_exec_params(state.edx as *const ExecParamsUser);
            sys_execve(
                current_slot,
                state.ebx as *const u8,
                state.ecx as usize,
                params.stdin,
                params.stdout,
                params.stderr,
                &params.argv,
            )
        },
        crate::syscalls::SYS_WAIT   => sys_wait(current_slot, state.ebx as i32),
        crate::syscalls::SYS_PIPE   => sys_pipe(current_slot, state.ebx as *mut u32),
        crate::syscalls::SYS_DUP2   => sys_dup2(current_slot, state.ebx as usize, state.ecx as usize),
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

        // === Window manager ===
        crate::syscalls::SYS_WM_CREATE  => sys_wm_create(current_slot, state.ebx as *const WmCreateArgs),
        crate::syscalls::SYS_WM_DESTROY => sys_wm_destroy(state.ebx),
        crate::syscalls::SYS_WM_MOVE    => sys_wm_move(state.ebx, state.ecx as i32, state.edx as i32),
        crate::syscalls::SYS_WM_INFO    => sys_wm_info(state.ebx, state.ecx as *mut crate::drivers::wm::WindowInfo),
        crate::syscalls::SYS_WM_FLIP    => sys_wm_flip(state.ebx, state.ecx as *const u8, state.edx as usize),
        crate::syscalls::SYS_WM_FOCUS   => sys_wm_focus(state.ebx),
        crate::syscalls::SYS_WM_SCREEN  => sys_wm_screen(state.ebx as *mut u32),
        crate::syscalls::SYS_MOUSE_STATE => sys_mouse_state(state.ebx as *mut crate::drivers::mouse::MouseState),
        crate::syscalls::SYS_WM_POLL => sys_wm_poll(
            state.ebx,
            state.ecx as *mut crate::drivers::wm::WmEvent,
            state.edx as usize,
        ),

        _ => 0,
    };

    // println!("ret: 0x{:x}", ret);
    // КЛАДЁМ результат обратно в eax (чтобы пользовательская программа его получила)
    state.eax = ret as u32;

    // Deliver any signals that arrived during the syscall (e.g. SIGINT while in read/wait).
    // May switch away from this task if the default action is terminate.
    crate::signal::deliver_pending(esp)
}

// ====================== FILE DESCRIPTORS ======================

// open flags (subset of Linux)
const O_RDONLY: usize = 0;
const O_WRONLY: usize = 1;
const O_RDWR:   usize = 2;
const O_CREAT:  usize = 0x40;
const O_TRUNC:  usize = 0x200;
const O_APPEND: usize = 0x400;

fn sys_open(current_slot: usize, path_ptr: *const u8, flags: usize) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    if path.is_empty() {
        return usize::MAX;
    }

    let acc = flags & 3;
    let mode = match acc {
        O_WRONLY => FileMode::WriteOnly,
        O_RDWR => FileMode::ReadWrite,
        _ => FileMode::ReadOnly,
    };

    let mut inode = VFS.get().resolve_path(path);

    if inode.is_none() {
        if flags & O_CREAT != 0 {
            // create empty file
            if !VFS.get().create_file(path, &[]) {
                return usize::MAX;
            }
            inode = VFS.get().resolve_path(path);
        }
    } else if flags & O_TRUNC != 0 && (acc == O_WRONLY || acc == O_RDWR) {
        // truncate: rewrite as empty
        let _ = VFS.get().create_file(path, &[]);
        inode = VFS.get().resolve_path(path);
    }

    let Some(inode) = inode else {
        return usize::MAX;
    };

    let mut offset = 0u64;
    if flags & O_APPEND != 0 {
        // size via read_at probe is awkward; leave 0 and use write path size — for now
        // try list won't work; keep 0 (write_at extends)
        offset = 0;
    }
    let fs_id = (inode >> 24) as u8;
    let is_device = fs_id != 0; // fs_id > 0 значит это точка монтирования (DevFS и т.д.)

    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(fd) = current.fd_table.alloc_fd() {
                let desc = if is_device {
                    FileDescriptor::Device { inode, offset, mode }
                } else {
                    FileDescriptor::File { inode, offset, mode }
                };
                current.fd_table.insert(fd, desc);
                return fd;
            }
        }
    }
    usize::MAX
}

fn sys_read(current_slot: usize, fd: usize, buf_ptr: *mut u8, count: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            // Fallback: bare fd 0 with empty table still means console
            let desc = current.fd_table.get(fd).copied().or_else(|| {
                if fd == 0 { Some(FileDescriptor::ConsoleIn) } else { None }
            });

            match desc {
                Some(FileDescriptor::ConsoleIn) => {
                    return sys_read_stdin(buf_ptr, count);
                }
                Some(FileDescriptor::File { inode, offset, mode }) => {
                    if mode == FileMode::WriteOnly { return 0; }
                    let mut temp = alloc::vec![0u8; count];
                    let bytes = VFS.get().read_at(inode, offset, &mut temp);
                    if bytes > 0 {
                        core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes);
                        if let Some(FileDescriptor::File { offset: ref mut off, .. }) =
                            current.fd_table.get_mut(fd)
                        {
                            *off += bytes as u64;
                        }
                    }
                    return bytes;
                }
                Some(FileDescriptor::Pipe { pipe_id, end }) => {
                    if end != PipeEnd::Read { return 0; }
                    return pipe::pipe_read(pipe_id, buf_ptr, count);
                }
                Some(FileDescriptor::Socket { .. }) => return 0,
                Some(FileDescriptor::ConsoleOut) => return 0,
                None => return 0,
                Some(FileDescriptor::Device { inode, offset, mode }) => {
                    if mode == FileMode::WriteOnly { return 0; }

                    let mut temp = alloc::vec![0u8; count];
                    // VFS сам разберется, что это DevFS, и вызовет read_from_block_device или CharDevice::read
                    let bytes = VFS.get().read_at(inode, offset, &mut temp);

                    if bytes > 0 {
                        core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes);

                        // Обновляем offset в таблице дескрипторов
                        if let Some(FileDescriptor::Device { offset: ref mut off, .. }) =
                            current.fd_table.get_mut(fd)
                        {
                            *off += bytes as u64;
                        }
                    }
                    return bytes;
                }
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



    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            let desc = current.fd_table.get(fd).copied().or_else(|| {
                if fd == 1 || fd == 2 { Some(FileDescriptor::ConsoleOut) } else { None }
            });

            match desc {
                Some(FileDescriptor::ConsoleOut) | Some(FileDescriptor::ConsoleIn) => {
                    match core::str::from_utf8(buf) {
                        Ok(v) => print!("{}", v),
                        Err(_) => println!("{:02x?}", &buf),
                    }
                    return count;
                }
                Some(FileDescriptor::File { inode, offset, mode }) => {
                    if mode == FileMode::ReadOnly { return 0; }
                    let written = VFS.get().write_at(inode, offset, buf);
                    if let Some(FileDescriptor::File { offset: ref mut off, .. }) =
                        current.fd_table.get_mut(fd)
                    {
                        *off += written as u64;
                    }
                    return written;
                }
                Some(FileDescriptor::Pipe { pipe_id, end }) => {
                    if end != PipeEnd::Write { return 0; }
                    return pipe::pipe_write(pipe_id, buf_ptr, count);
                }
                Some(FileDescriptor::Socket { .. }) => return 0,
                Some(FileDescriptor::Device { inode, offset, mode }) => {
                    if mode == FileMode::ReadOnly { return 0; }

                    let mut temp = alloc::vec![0u8; count];
                    core::ptr::copy_nonoverlapping(buf_ptr, temp.as_mut_ptr(), count);

                    // VFS маршрутизирует в DevFS -> write_to_block_device (Read-Modify-Write)
                    let bytes = VFS.get().write_at(inode, offset, &temp);

                    if bytes > 0 {
                        if let Some(FileDescriptor::Device { offset: ref mut off, .. }) =
                            current.fd_table.get_mut(fd)
                        {
                            *off += bytes as u64;
                        }
                    }
                    return bytes;
                }
                None => {
                    // legacy: fd 1 without table entry
                    if fd == 1 || fd == 2 {
                        match core::str::from_utf8(buf) {
                            Ok(v) => print!("{}", v),
                            Err(_) => {}
                        }
                        return count;
                    }
                    return 0;
                }
            }
        }
    }
    0
}

fn close_descriptor(desc: FileDescriptor) {
    match desc {
        FileDescriptor::Socket { socket_id } => {
            SOCKET_TABLE.lock().free(socket_id);
        }
        FileDescriptor::Pipe { pipe_id, end } => {
            match end {
                PipeEnd::Read => pipe::pipe_close_reader(pipe_id),
                PipeEnd::Write => pipe::pipe_close_writer(pipe_id),
            }
        }
        _ => {}
    }
}

fn sys_close(current_slot: usize, fd: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(desc) = current.fd_table.close(fd) {
                close_descriptor(desc);
                return 0;
            }
        }
    }
    usize::MAX
}

fn sys_pipe(current_slot: usize, pipefd: *mut u32) -> usize {
    let pipe_id = match pipe::pipe_create() {
        Some(id) => id,
        None => return usize::MAX,
    };
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            let r = match current.fd_table.alloc_fd() {
                Some(f) => f,
                None => {
                    pipe::pipe_close_reader(pipe_id);
                    pipe::pipe_close_writer(pipe_id);
                    return usize::MAX;
                }
            };
            current.fd_table.insert(r, FileDescriptor::new_pipe(pipe_id, PipeEnd::Read));
            let w = match current.fd_table.alloc_fd() {
                Some(f) => f,
                None => {
                    let _ = current.fd_table.close(r);
                    pipe::pipe_close_reader(pipe_id);
                    pipe::pipe_close_writer(pipe_id);
                    return usize::MAX;
                }
            };
            current.fd_table.insert(w, FileDescriptor::new_pipe(pipe_id, PipeEnd::Write));
            *pipefd = r as u32;
            *pipefd.add(1) = w as u32;
            return 0;
        }
    }
    usize::MAX
}

fn sys_dup2(current_slot: usize, oldfd: usize, newfd: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if current.fd_table.get(oldfd).is_none() {
                return usize::MAX;
            }
            // bump pipe refcounts on duplicate
            if let Some(FileDescriptor::Pipe { pipe_id, end }) = current.fd_table.get(oldfd).copied() {
                match end {
                    PipeEnd::Read => pipe::pipe_add_reader(pipe_id),
                    PipeEnd::Write => pipe::pipe_add_writer(pipe_id),
                }
            }
            if oldfd != newfd {
                if let Some(old) = current.fd_table.close(newfd) {
                    close_descriptor(old);
                }
            }
            if current.fd_table.dup2(oldfd, newfd) {
                return newfd;
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
                // Close all fds so pipe writers release EOF to readers
                let fds: alloc::vec::Vec<_> = t.fd_table.take_all().collect();
                for desc in fds {
                    close_descriptor(desc);
                }
                t.running = false;
                t.zombie = true;
                t.exit_code = 0;
            }
        }
        TASK_MANAGER.schedule(esp as *mut CPUState) as u32
    }
}

// ====================== WAIT ======================
/// Block until a child matching `pid` becomes a zombie, then reap it.
/// `pid == -1` waits for any child.
/// Returns the reaped child's slot (pid), or usize::MAX if no such child can ever appear.
fn sys_wait(current_slot: usize, pid: i32) -> usize {
    let parent = current_slot as i8;

    // Child that we are waiting for becomes the terminal foreground process
    // so Ctrl+C (SIGINT) is delivered to it, not to the shell.
    if pid >= 0 {
        crate::signal::set_foreground(pid as i8);
    }

    loop {
        let found = unsafe { TASK_MANAGER.find_zombie_child(parent, pid) };
        if let Some((child_slot, _exit_code)) = found {
            crate::signal::clear_foreground();
            unsafe {
                TASK_MANAGER.reap(child_slot);
            }
            return child_slot;
        }

        // No zombie yet — sleep until the next interrupt (timer / keyboard),
        // same pattern as blocking stdin. Scheduler can run other tasks
        // while we are in hlt; when we are scheduled again we re-check.
        // SIGINT on the child will turn it into a zombie via deliver_pending.
        unsafe {
            asm!("sti");
            asm!("hlt");
            asm!("cli");
        }
    }
}

/// Userspace layout of `libfelix::syscall::ExecParams`.
#[repr(C)]
struct ExecParamsUser {
    stdin: i32,
    stdout: i32,
    stderr: i32,
    argc: u32,
    argv: *const *const u8,
}

struct ExecParamsKernel {
    stdin: i32,
    stdout: i32,
    stderr: i32,
    /// Owned C-string copies of argv.
    argv: alloc::vec::Vec<alloc::vec::Vec<u8>>,
}

fn read_exec_params(ptr: *const ExecParamsUser) -> ExecParamsKernel {
    if ptr.is_null() {
        return ExecParamsKernel {
            stdin: -1,
            stdout: -1,
            stderr: -1,
            argv: alloc::vec::Vec::new(),
        };
    }
    unsafe {
        let p = &*ptr;
        let mut argv = alloc::vec::Vec::new();
        let n = p.argc.min(64) as usize; // hard cap
        if !p.argv.is_null() {
            for i in 0..n {
                let sp = *p.argv.add(i);
                if sp.is_null() {
                    break;
                }
                // Copy C string (max 256 bytes each)
                let mut buf = alloc::vec::Vec::new();
                for j in 0..256 {
                    let b = *sp.add(j);
                    buf.push(b);
                    if b == 0 {
                        break;
                    }
                }
                if buf.last() != Some(&0) {
                    buf.push(0);
                }
                argv.push(buf);
            }
        }
        ExecParamsKernel {
            stdin: p.stdin,
            stdout: p.stdout,
            stderr: p.stderr,
            argv,
        }
    }
}

/// Write bytes to a user virtual address via the task's page tables.
fn copy_to_user_virt(page_dir: &crate::memory::paging::PageDirectory, mut vaddr: u32, data: &[u8]) {
    use crate::memory::paging::phys_to_virt;
    let mut offset = 0usize;
    while offset < data.len() {
        let page = vaddr & !0xFFF;
        let page_off = (vaddr & 0xFFF) as usize;
        let chunk = core::cmp::min(data.len() - offset, 0x1000 - page_off);
        let pd_idx = (page >> 22) as usize;
        let pt_idx = ((page >> 12) & 0x3FF) as usize;
        let pde = page_dir.entries[pd_idx];
        let pt_phys = pde & 0xFFFF_F000;
        let pte = unsafe { *((phys_to_virt(pt_phys) as *const u32).add(pt_idx)) };
        let frame_phys = pte & 0xFFFF_F000;
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr().add(offset),
                (phys_to_virt(frame_phys) as *mut u8).add(page_off),
                chunk,
            );
        }
        offset += chunk;
        vaddr += chunk as u32;
    }
}

fn write_u32_user(page_dir: &crate::memory::paging::PageDirectory, vaddr: u32, val: u32) {
    let bytes = val.to_le_bytes();
    copy_to_user_virt(page_dir, vaddr, &bytes);
}

/// Build Linux-style initial user stack with argc/argv/empty envp.
/// Returns the final ESP value (points at argc).
fn setup_user_argv(
    page_dir: &crate::memory::paging::PageDirectory,
    stack_top: u32,
    argv: &[alloc::vec::Vec<u8>],
) -> u32 {
    let mut sp = stack_top;
    let argc = argv.len();
    let mut arg_addrs: alloc::vec::Vec<u32> = alloc::vec::Vec::with_capacity(argc);

    // Place strings just below stack_top (highest addresses first).
    for s in argv.iter().rev() {
        let len = s.len();
        sp -= len as u32;
        copy_to_user_virt(page_dir, sp, s);
        arg_addrs.push(sp);
    }
    arg_addrs.reverse();

    // 16-byte align for ABI friendliness
    sp &= !0xF;

    // NULL envp terminator
    sp -= 4;
    write_u32_user(page_dir, sp, 0);
    // NULL argv terminator
    sp -= 4;
    write_u32_user(page_dir, sp, 0);
    // argv pointers (reverse order so argv[0] ends up at lowest address after pushes)
    for &addr in arg_addrs.iter().rev() {
        sp -= 4;
        write_u32_user(page_dir, sp, addr);
    }
    // argc
    sp -= 4;
    write_u32_user(page_dir, sp, argc as u32);
    sp
}

/// Copy parent fd into child slot, bumping pipe refcounts when needed.
fn install_child_fd(
    parent_slot: usize,
    child_table: &mut FileDescriptorTable,
    child_fd: usize,
    parent_fd: i32,
    default: FileDescriptor,
) {
    if parent_fd < 0 {
        child_table.set(child_fd, default);
        return;
    }
    unsafe {
        if let Some(ref parent) = TASK_MANAGER.tasks[parent_slot] {
            if let Some(desc) = parent.fd_table.get(parent_fd as usize).copied() {
                if let FileDescriptor::Pipe { pipe_id, end } = desc {
                    match end {
                        PipeEnd::Read => pipe::pipe_add_reader(pipe_id),
                        PipeEnd::Write => pipe::pipe_add_writer(pipe_id),
                    }
                }
                child_table.set(child_fd, desc);
                return;
            }
        }
    }
    child_table.set(child_fd, default);
}

// ====================== EXECVE ======================
/// Spawn a new task from an ELF image in memory.
/// Returns the new task's slot (pid) on success, or usize::MAX on failure.
/// `stdin_fd`/`stdout_fd`/`stderr_fd`: parent fd to install as child's 0/1/2,
/// or `-1` for default ConsoleIn / ConsoleOut.
pub fn sys_execve(
    parent_slot: usize,
    buf_ptr: *const u8,
    count: usize,
    stdin_fd: i32,
    stdout_fd: i32,
    stderr_fd: i32,
    argv: &[alloc::vec::Vec<u8>],
) -> usize {
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

            // argv on user stack (defaults to empty argc=0)
            let user_esp = setup_user_argv(&t.page_dir, USER_STACK_TOP, argv);

            *state_ptr = CPUState {
                eax: 0, ebx: 0, ecx: 0, edx: 0,
                esi: 0, edi: 0, ebp: 0,
                eip:    entry_point,
                cs:     0x1B,
                eflags: 0x202,
                esp:    user_esp,
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
            t.pending_signals = 0;

            // stdio + optional remap from parent fds
            t.fd_table = FileDescriptorTable::with_stdio();
            install_child_fd(parent_slot, &mut t.fd_table, 0, stdin_fd, FileDescriptor::ConsoleIn);
            install_child_fd(parent_slot, &mut t.fd_table, 1, stdout_fd, FileDescriptor::ConsoleOut);
            install_child_fd(parent_slot, &mut t.fd_table, 2, stderr_fd, FileDescriptor::ConsoleOut);

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

// ====================== WINDOW MANAGER ======================

#[repr(C)]
struct WmCreateArgs {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    title: *const u8,
}

fn sys_wm_create(current_slot: usize, args: *const WmCreateArgs) -> usize {
    if args.is_null() {
        return usize::MAX;
    }
    let a = unsafe { &*args };
    let title = if a.title.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(a.title as *const i8).to_str().unwrap_or("") }
    };
    match crate::drivers::wm::create_window(a.x, a.y, a.w, a.h, title, current_slot as i8) {
        Some(id) => id as usize,
        None => usize::MAX,
    }
}

fn sys_wm_destroy(id: u32) -> usize {
    if crate::drivers::wm::destroy_window(id) {
        0
    } else {
        usize::MAX
    }
}

fn sys_wm_move(id: u32, x: i32, y: i32) -> usize {
    if crate::drivers::wm::move_window(id, x, y) {
        0
    } else {
        usize::MAX
    }
}

fn sys_wm_info(id: u32, out: *mut crate::drivers::wm::WindowInfo) -> usize {
    if out.is_null() {
        return usize::MAX;
    }
    match crate::drivers::wm::window_info(id) {
        Some(info) => {
            unsafe { *out = info };
            0
        }
        None => usize::MAX,
    }
}

fn sys_wm_flip(id: u32, pixels: *const u8, len: usize) -> usize {
    if crate::drivers::wm::flip(id, pixels, len) {
        0
    } else {
        usize::MAX
    }
}

fn sys_wm_focus(id: u32) -> usize {
    if crate::drivers::wm::focus_window(id) {
        0
    } else {
        usize::MAX
    }
}

fn sys_wm_screen(out: *mut u32) -> usize {
    if out.is_null() {
        return usize::MAX;
    }
    let (w, h) = crate::drivers::wm::screen_size();
    unsafe {
        *out = w;
        *out.add(1) = h;
    }
    0
}

fn sys_mouse_state(out: *mut crate::drivers::mouse::MouseState) -> usize {
    if out.is_null() {
        return usize::MAX;
    }
    unsafe {
        *out = crate::drivers::mouse::state();
    }
    0
}

fn sys_wm_poll(id: u32, out: *mut crate::drivers::wm::WmEvent, max: usize) -> usize {
    crate::drivers::wm::poll_events(id, out, max.min(32))
}



