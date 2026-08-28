use alloc::string::ToString;
use core::arch::asm;
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::drivers::pic::PICS;
use crate::filesystem::VFS;
use crate::filesystem::file::{FileDescriptor, FileDescriptorTable, FileMode, PipeEnd};
use crate::memory::allocator::ALLOCATOR;
use crate::memory::paging::{
    PAGE_SIZE, PAGING, PDEFlags, PTEFlags, PageDirectory, PhysAddr, VirtAddr, copy_kernel_mappings,
};
use crate::multitasking::task::{CPUState, TASK_MANAGER, Task};
use crate::net::{AF_INET, SOCK_DGRAM, SOCK_STREAM, SOCKET_TABLE, SockAddrIn, SocketState};
use crate::{pipe, utils};
use crate::{print, println};
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::arch::naked_asm;
use core::ffi::CStr;
use core::net::Ipv4Addr;

pub const SYSCALL_INT: u8 = 0x80;

#[unsafe(naked)]
pub extern "C" fn syscall() {
    unsafe {
        naked_asm!(
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
            "iretd"
        );
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn syscall_handler(esp: u32) -> u32 {
    let state = unsafe { &mut *(esp as *mut CPUState) };

    let syscall_num = state.eax;

    // Фиксируем текущий таск ОДИН раз (чтобы таймер не успел переключить)
    let current_slot = unsafe { TASK_MANAGER.get_current_slot() } as usize;

    // exit must switch to another task — never return to the dead one
    if syscall_num == crate::syscalls::SYS_EXIT || syscall_num == crate::syscalls::SYS_EXIT_GROUP {
        return sys_exit(current_slot, esp);
    }

    let ret = match syscall_num {
        // === File descriptors ===
        crate::syscalls::SYS_OPEN => {
            sys_open(current_slot, state.ebx as *const u8, state.ecx as usize)
        }
        crate::syscalls::SYS_READ => sys_read(
            current_slot,
            state.ebx as usize,
            state.ecx as *mut u8,
            state.edx as usize,
        ),
        crate::syscalls::SYS_WRITE => sys_write(
            current_slot,
            state.ebx as usize,
            state.ecx as *const u8,
            state.edx as usize,
        ),
        crate::syscalls::SYS_CLOSE => sys_close(current_slot, state.ebx as usize),
        crate::syscalls::SYS_LSEEK => sys_lseek(
            current_slot,
            state.ebx as usize,
            state.ecx as i32,
            state.edx as u32,
        ),
        crate::syscalls::SYS_BRK => sys_brk(current_slot, state.ebx),
        crate::syscalls::SYS_MMAP => sys_mmap_old(current_slot, state.ebx as *const MmapArgStruct),
        crate::syscalls::SYS_MMAP2 => sys_mmap2(
            current_slot,
            state.ebx,
            state.ecx as usize,
            state.edx,
            state.esi,
            state.edi as i32,
            state.ebp,
        ),
        crate::syscalls::SYS_MUNMAP => sys_munmap(current_slot, state.ebx, state.ecx as usize),
        crate::syscalls::SYS_IOCTL => {
            sys_ioctl(current_slot, state.ebx as usize, state.ecx, state.edx)
        }
        crate::syscalls::SYS_FSTAT64 => {
            sys_fstat64(current_slot, state.ebx as usize, state.ecx as *mut Stat64)
        }
        crate::syscalls::SYS_STAT64 => sys_stat64(state.ebx as *const u8, state.ecx as *mut Stat64),
        crate::syscalls::SYS_GETDENTS64 => sys_getdents64(
            current_slot,
            state.ebx as usize,
            state.ecx as *mut u8,
            state.edx as usize,
        ),

        // === Filesystem / process ===
        crate::syscalls::SYS_MKDIR => sys_mkdir(state.ebx as *const u8),
        crate::syscalls::SYS_RMDIR => sys_rmdir(state.ebx as *const u8),
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
        }
        crate::syscalls::SYS_WAIT => sys_wait(current_slot, state.ebx as i32, state.ecx as u32),
        crate::syscalls::SYS_KILL => sys_kill(state.ebx as i32, state.ecx as u32),
        crate::syscalls::SYS_SIGACTION => sys_sigaction(
            current_slot,
            state.ebx as u32,
            state.ecx as *const SigActionUser,
            state.edx as *mut SigActionUser,
        ),
        crate::syscalls::SYS_PIPE => sys_pipe(current_slot, state.ebx as *mut u32),
        crate::syscalls::SYS_DUP2 => sys_dup2(current_slot, state.ebx as usize, state.ecx as usize),
        crate::syscalls::SYS_FCNTL => sys_fcntl(
            current_slot,
            state.ebx as usize,
            state.ecx as u32,
            state.edx as u32,
        ),
        crate::syscalls::SYS_POLL => sys_poll(
            current_slot,
            state.ebx as *mut PollFd,
            state.ecx as usize,
            state.edx as i32,
        ),
        crate::syscalls::SYS_LS => sys_ls(
            state.ebx as *const u8,
            state.ecx as *mut u8,
            state.edx as usize,
        ),
        // === Memory ===
        crate::syscalls::SYS_MALLOC => {
            let size = state.ebx as usize;
            // Cap align: garbage/huge align would jump heap_next by megabytes per call.
            let align_raw = state.ecx as usize;
            let align = if align_raw == 0 {
                8
            } else {
                align_raw.next_power_of_two().max(8).min(4096)
            };

            // Use the slot fixed at syscall entry (not re-fetched) so a timer
            // cannot race us onto another task mid-handler.
            if current_slot == 0 || current_slot >= 8 {
                println!("[malloc] bad slot={}", current_slot);
                0
            } else if unsafe { TASK_MANAGER.tasks[current_slot].is_none() } {
                println!("[malloc] no task slot={}", current_slot);
                0
            } else {
                unsafe {
                    let task = TASK_MANAGER.tasks[current_slot].as_mut().unwrap();
                    let mut start = task.heap_next;
                    // Per-slot window: 0x4000_0000 + slot*0x1000_0000, size 256 MiB.
                    let heap_base = 0x4000_0000u32
                        .wrapping_add((current_slot as u32).wrapping_mul(0x1000_0000));
                    let heap_limit = heap_base.wrapping_add(128 * 1024 * 1024); // 128 MiB soft cap
                    if start < heap_base {
                        start = heap_base;
                        task.heap_next = heap_base;
                    }

                    let align_mask = (align - 1) as u32;
                    start = (start + align_mask) & !align_mask;

                    let used = start.saturating_sub(heap_base) as usize;
                    if size > 0
                        && (start.saturating_add(size as u32) > heap_limit
                            || used.saturating_add(size) > 128 * 1024 * 1024)
                    {
                        println!(
                            "[malloc] OOM slot={} size={} align={} heap_next={:#x} base={:#x} used={}",
                            current_slot, size, align, start, heap_base, used
                        );
                        0
                    } else if size > 0 {
                        // Log suspicious large single allocs (helps catch runaway growth).
                        if size >= 256 * 1024 {
                            println!(
                                "[malloc] large slot={} size={} used={} → {:#x}",
                                current_slot, size, used, start
                            );
                        }
                        let page_size = crate::memory::paging::PAGE_SIZE as u32;
                        let start_page = start & !(page_size - 1);
                        let end = start + size as u32;
                        let end_page = (end + page_size - 1) & !(page_size - 1);

                        let mut addr = start_page;
                        while addr < end_page {
                            let need_map = task.page_refcounts.inc(addr);
                            if need_map {
                                task.pd_mut().alloc_and_map_user_page(addr);
                            }
                            addr += page_size;
                        }
                        core::ptr::write_bytes(start as *mut u8, 0, size);
                        task.heap_next = start + size as u32;
                        start as usize
                    } else {
                        start as usize
                    }
                }
            }
        }

        crate::syscalls::SYS_REALLOC => {
            let old_ptr = state.ebx;
            let old_size = state.ecx as usize;
            let new_size = state.edx as usize;
            // Use slot fixed at syscall entry (same as SYS_MALLOC).
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
                                task.pd_mut().alloc_and_map_user_page(addr);
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

                    // Bump allocator: do NOT unmap the old range. Pages are shared
                    // by many live blocks; unmapping on realloc free corrupts them
                    // (user PF at low CR2 like 0xa0 / 0x20). Heap only grows.
                    new_start as usize
                }
            }
        }

        crate::syscalls::SYS_FREE => {
            // Bump allocator: pure no-op. Never unmap.
            // Unmapping on free was the root of PAGE_FAULT (user) CR2=0x20/0xa0:
            // pages are shared by many live Vec/String blocks; refcount hits 0
            // while other allocations still point into the same page.
            let _ = (state.ebx, state.ecx, state.edx);
            0
        }
        // === Sockets ===
        crate::syscalls::SYS_SOCKET => sys_socket(
            current_slot,
            state.ebx as u16,
            state.ecx as u16,
            state.edx as u8,
        ),
        crate::syscalls::SYS_BIND => sys_bind(
            current_slot,
            state.ebx as usize,
            state.ecx as *const u8,
            state.edx as usize,
        ),
        crate::syscalls::SYS_LISTEN => {
            sys_listen(current_slot, state.ebx as usize, state.ecx as usize)
        }
        crate::syscalls::SYS_ACCEPT4 => sys_accept4(
            current_slot,
            state.ebx as usize,
            state.ecx as *mut u8,
            state.edx as *mut u32,
            state.esi as u32,
        ),
        crate::syscalls::SYS_CONNECT => sys_connect(
            current_slot,
            state.ebx as usize,
            state.ecx as *const u8,
            state.edx as usize,
        ),
        crate::syscalls::SYS_SENDTO => sys_sendto(
            current_slot,
            state.ebx as usize,
            state.ecx as *const u8,
            state.edx as usize,
        ),
        crate::syscalls::SYS_RECVFROM => sys_recvfrom(
            current_slot,
            state.ebx as usize,
            state.ecx as *mut u8,
            state.edx as usize,
        ),
        crate::syscalls::SYS_SHUTDOWN => {
            sys_shutdown(current_slot, state.ebx as usize, state.ecx as u32)
        }

        // === Window manager ===
        crate::syscalls::SYS_WM_CREATE => {
            sys_wm_create(current_slot, state.ebx as *const WmCreateArgs)
        }
        crate::syscalls::SYS_WM_DESTROY => sys_wm_destroy(state.ebx),
        crate::syscalls::SYS_WM_MOVE => sys_wm_move(state.ebx, state.ecx as i32, state.edx as i32),
        crate::syscalls::SYS_WM_INFO => {
            sys_wm_info(state.ebx, state.ecx as *mut crate::drivers::wm::WindowInfo)
        }
        crate::syscalls::SYS_WM_FLIP => {
            sys_wm_flip(state.ebx, state.ecx as *const u8, state.edx as usize)
        }
        crate::syscalls::SYS_WM_FOCUS => sys_wm_focus(state.ebx),
        crate::syscalls::SYS_WM_SCREEN => sys_wm_screen(state.ebx as *mut u32),
        crate::syscalls::SYS_MOUSE_STATE => {
            sys_mouse_state(state.ebx as *mut crate::drivers::mouse::MouseState)
        }
        crate::syscalls::SYS_WM_POLL => sys_wm_poll(
            state.ebx,
            state.ecx as *mut crate::drivers::wm::WmEvent,
            state.edx as usize,
        ),

        // === PCI ===
        crate::syscalls::SYS_PCI_LIST => {
            sys_pci_list(state.ebx as *mut PciInfoUser, state.ecx as usize)
        }

        crate::syscalls::wasm::SYS_EXECVE_WASM => crate::syscalls::wasm::sys_execve_wasm(
            current_slot,
            state.ebx as *const u8,             // buf_ptr
            state.ecx as usize,                 // count
            state.edx as *const ExecParamsUser, // параметры (argc/argv и stdin/stdout/stderr)
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
const O_RDWR: usize = 2;
const O_CREAT: usize = 0x40;
const O_TRUNC: usize = 0x200;
const O_APPEND: usize = 0x400;
const O_DIRECTORY: usize = 0x10000;

const SEEK_SET: u32 = 0;
const SEEK_CUR: u32 = 1;
const SEEK_END: u32 = 2;

// Linux errno (returned as -errno in eax)
const EPERM: usize = (-1isize) as usize;
const ENOENT: usize = (-2isize) as usize;
const EBADF: usize = (-9isize) as usize;
const ENOMEM: usize = (-12isize) as usize;
const EFAULT: usize = (-14isize) as usize;
const EINVAL: usize = (-22isize) as usize;
const ENOTTY: usize = (-25isize) as usize;
const ENOTDIR: usize = (-20isize) as usize;

const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;

/// Linux i386 `struct stat64` (glibc layout).
#[repr(C, packed)]
pub struct Stat64 {
    st_dev: u64,
    __pad0: [u8; 4],
    __st_ino: u32,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad3: [u8; 4],
    st_size: i64,
    st_blksize: u32,
    st_blocks: u64,
    st_atime: u32,
    st_atime_nsec: u32,
    st_mtime: u32,
    st_mtime_nsec: u32,
    st_ctime: u32,
    st_ctime_nsec: u32,
    st_ino: u64,
}

fn probe_inode_size(inode: u32) -> u64 {
    let mut size = 0u64;
    let mut buf = [0u8; 1024];
    loop {
        let n = VFS.get().read_at(inode, size, &mut buf);
        if n == 0 {
            break;
        }
        size += n as u64;
        if n < buf.len() {
            break;
        }
        // safety cap ~16 MiB
        if size > 16 * 1024 * 1024 {
            break;
        }
    }
    size
}

fn path_is_dir(path: &str) -> bool {
    VFS.get().list_directory_entries(path).is_some()
}

fn fill_stat64(st: &mut Stat64, inode: u32, mode: u32, size: u64) {
    *st = Stat64 {
        st_dev: 1,
        __pad0: [0; 4],
        __st_ino: inode,
        st_mode: mode | 0o644,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad3: [0; 4],
        st_size: size as i64,
        st_blksize: 4096,
        st_blocks: (size + 511) / 512,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        st_ino: inode as u64,
    };
}

pub fn sys_open(current_slot: usize, path_ptr: *const u8, flags: usize) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    if path.is_empty() {
        return ENOENT;
    }

    // Directory open (explicit or path is a dir)
    if path_is_dir(path) {
        if flags & O_WRONLY != 0 || flags & O_RDWR != 0 {
            return EINVAL;
        }
        let pb = path.as_bytes();
        if pb.len() >= 96 {
            return ENAMETOOLONG_OR_EINVAL();
        }
        let mut path_buf = [0u8; 96];
        path_buf[..pb.len()].copy_from_slice(pb);
        unsafe {
            if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
                if let Some(fd) = current.fd_table.alloc_fd() {
                    current.fd_table.insert(
                        fd,
                        FileDescriptor::Dir {
                            path: path_buf,
                            path_len: pb.len() as u8,
                            cookie: 0,
                        },
                    );
                    return fd;
                }
            }
        }
        return EMFILE_OR_ENOMEM();
    }

    if flags & O_DIRECTORY != 0 {
        return ENOTDIR;
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
            if !VFS.get().create_file(path, &[]) {
                return ENOENT;
            }
            inode = VFS.get().resolve_path(path);
        }
    } else if flags & O_TRUNC != 0 && (acc == O_WRONLY || acc == O_RDWR) {
        let _ = VFS.get().create_file(path, &[]);
        inode = VFS.get().resolve_path(path);
    }

    let Some(inode) = inode else {
        return ENOENT;
    };

    let offset = if flags & O_APPEND != 0 {
        probe_inode_size(inode)
    } else {
        0
    };
    let fs_id = (inode >> 24) as u8;
    let is_device = fs_id != 0;

    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if let Some(fd) = current.fd_table.alloc_fd() {
                let desc = if is_device {
                    FileDescriptor::Device {
                        inode,
                        offset,
                        mode,
                    }
                } else {
                    FileDescriptor::File {
                        inode,
                        offset,
                        mode,
                    }
                };
                current.fd_table.insert(fd, desc);
                return fd;
            }
        }
    }
    ENOMEM
}

fn ENAMETOOLONG_OR_EINVAL() -> usize {
    EINVAL
}
fn EMFILE_OR_ENOMEM() -> usize {
    ENOMEM
}

pub fn sys_lseek(current_slot: usize, fd: usize, offset: i32, whence: u32) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            match current.fd_table.get_mut(fd) {
                Some(FileDescriptor::File {
                    inode,
                    offset: off,
                    ..
                })
                | Some(FileDescriptor::Device {
                    inode,
                    offset: off,
                    ..
                }) => {
                    let size = probe_inode_size(*inode);
                    let base = match whence {
                        SEEK_SET => 0i64,
                        SEEK_CUR => *off as i64,
                        SEEK_END => size as i64,
                        _ => return EINVAL,
                    };
                    let new = base + offset as i64;
                    if new < 0 {
                        return EINVAL;
                    }
                    *off = new as u64;
                    return *off as usize;
                }
                Some(FileDescriptor::Dir { cookie, .. }) => {
                    if whence != SEEK_SET || offset != 0 {
                        return EINVAL;
                    }
                    *cookie = 0;
                    return 0;
                }
                _ => return EBADF,
            }
        }
    }
    EBADF
}

pub fn sys_brk(current_slot: usize, addr: u32) -> usize {
    unsafe {
        if current_slot == 0 || current_slot >= 8 {
            return 0;
        }
        let Some(ref mut task) = TASK_MANAGER.tasks[current_slot] else {
            return 0;
        };
        // Region: 0x4000_0000 + slot * 0x1000_0000
        let heap_base =
            0x4000_0000u32.wrapping_add((current_slot as u32).wrapping_mul(0x1000_0000));
        if task.heap_next < heap_base {
            task.heap_next = heap_base;
        }
        // brk(0) → current break
        if addr == 0 {
            return task.heap_next as usize;
        }
        if addr < heap_base {
            return task.heap_next as usize;
        }
        // Cap growth: 32 MiB per process
        if addr.saturating_sub(heap_base) > 32 * 1024 * 1024 {
            return task.heap_next as usize;
        }
        let page_size = PAGE_SIZE as u32;
        if addr > task.heap_next {
            let start_page = task.heap_next & !(page_size - 1);
            let end_page = (addr + page_size - 1) & !(page_size - 1);
            let mut p = start_page;
            while p < end_page {
                let need = task.page_refcounts.inc(p);
                if need {
                    task.pd_mut().alloc_and_map_user_page(p);
                }
                p += page_size;
            }
        }
        task.heap_next = addr;
        task.heap_next as usize
    }
}

const MAP_SHARED: u32 = 0x01;
const MAP_PRIVATE: u32 = 0x02;
const MAP_FIXED: u32 = 0x10;
const MAP_ANONYMOUS: u32 = 0x20;

/// Linux i386 old mmap arg block (syscall 90).
#[repr(C)]
pub struct MmapArgStruct {
    addr: u32,
    len: u32,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: u32, // byte offset
}

pub fn sys_mmap_old(current_slot: usize, argp: *const MmapArgStruct) -> usize {
    if argp.is_null() {
        return EFAULT;
    }
    let a = unsafe { &*argp };
    // offset is in bytes; mmap2 wants page offset
    let pgoff = a.offset >> 12;
    sys_mmap2(
        current_slot,
        a.addr,
        a.len as usize,
        a.prot,
        a.flags,
        a.fd,
        pgoff,
    )
}

/// mmap2: offset is in pages (4 KiB).
pub fn sys_mmap2(
    current_slot: usize,
    addr: u32,
    len: usize,
    _prot: u32,
    flags: u32,
    fd: i32,
    pgoff: u32,
) -> usize {
    if len == 0 {
        return EINVAL;
    }
    // Cap single mapping at 64 MiB
    if len > 64 * 1024 * 1024 {
        return ENOMEM;
    }
    let page_size = PAGE_SIZE as u32;
    let len_u = ((len as u32) + page_size - 1) & !(page_size - 1);

    let anonymous = flags & MAP_ANONYMOUS != 0 || fd < 0;

    unsafe {
        if current_slot == 0 || current_slot >= 8 {
            return ENOMEM;
        }
        let Some(ref mut task) = TASK_MANAGER.tasks[current_slot] else {
            return ENOMEM;
        };

        // Pick VA
        let mut va = if flags & MAP_FIXED != 0 {
            if addr == 0 {
                return EINVAL;
            }
            addr & !(page_size - 1)
        } else if addr != 0 {
            // Hint — we honour it if non-zero and not FIXED (simple)
            addr & !(page_size - 1)
        } else {
            // Auto: bump allocator from mmap_next
            if task.mmap_next < 0x6000_0000 {
                task.mmap_next = 0x6000_0000;
            }
            // Keep below user stack (~0xBFFF_F000)
            if task.mmap_next.saturating_add(len_u) >= 0xB000_0000 {
                return ENOMEM;
            }
            let v = task.mmap_next;
            task.mmap_next = task.mmap_next.saturating_add(len_u);
            v
        };

        // Don't map into kernel half
        if va >= 0xC000_0000 || va.saturating_add(len_u) > 0xC000_0000 {
            return EINVAL;
        }

        // Map pages (zero-filled)
        let mut p = va;
        let end = va + len_u;
        while p < end {
            let need = task.page_refcounts.inc(p);
            if need {
                task.pd_mut().alloc_and_map_user_page(p);
            }
            // Clear page
            core::ptr::write_bytes(p as *mut u8, 0, PAGE_SIZE);
            p += page_size;
        }

        // File-backed: copy file contents into mapping
        if !anonymous {
            let inode = match task.fd_table.get(fd as usize) {
                Some(FileDescriptor::File { inode, .. })
                | Some(FileDescriptor::Device { inode, .. }) => *inode,
                _ => return EBADF,
            };
            let file_off = (pgoff as u64) * (PAGE_SIZE as u64);
            let mut remaining = len;
            let mut dst = va as *mut u8;
            let mut off = file_off;
            while remaining > 0 {
                let chunk = remaining.min(4096);
                let mut buf = alloc::vec![0u8; chunk];
                let n = VFS.get().read_at(inode, off, &mut buf);
                if n == 0 {
                    break;
                }
                core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, n);
                remaining -= n;
                off += n as u64;
                dst = dst.add(n);
            }
        }

        va as usize
    }
}

pub fn sys_munmap(current_slot: usize, addr: u32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let page_size = PAGE_SIZE as u32;
    let start = addr & !(page_size - 1);
    let end = (addr.saturating_add(len as u32) + page_size - 1) & !(page_size - 1);
    if start >= 0xC000_0000 {
        return EINVAL;
    }
    unsafe {
        if let Some(ref mut task) = TASK_MANAGER.tasks[current_slot] {
            let mut p = start;
            while p < end && p < 0xC000_0000 {
                let should = task.page_refcounts.dec(p);
                if should {
                    task.pd_mut().unmap(p);
                }
                p += page_size;
            }
            return 0;
        }
    }
    EINVAL
}

pub fn sys_ioctl(_current_slot: usize, _fd: usize, _req: u32, _arg: u32) -> usize {
    // Most tty ioctls → ENOTTY is fine for non-interactive tools
    ENOTTY
}

pub fn sys_fstat64(current_slot: usize, fd: usize, st_ptr: *mut Stat64) -> usize {
    if st_ptr.is_null() {
        return EFAULT;
    }
    unsafe {
        if let Some(ref current) = TASK_MANAGER.tasks[current_slot] {
            let desc = current.fd_table.get(fd).copied().or_else(|| {
                if fd == 0 {
                    Some(FileDescriptor::ConsoleIn)
                } else if fd == 1 || fd == 2 {
                    Some(FileDescriptor::ConsoleOut)
                } else {
                    None
                }
            });
            match desc {
                Some(FileDescriptor::File { inode, .. })
                | Some(FileDescriptor::Device { inode, .. }) => {
                    let size = probe_inode_size(inode);
                    fill_stat64(&mut *st_ptr, inode, S_IFREG, size);
                    return 0;
                }
                Some(FileDescriptor::Dir { .. }) => {
                    fill_stat64(&mut *st_ptr, 1, S_IFDIR, 0);
                    return 0;
                }
                Some(FileDescriptor::ConsoleIn) | Some(FileDescriptor::ConsoleOut) => {
                    fill_stat64(&mut *st_ptr, 0, S_IFCHR, 0);
                    return 0;
                }
                _ => return EBADF,
            }
        }
    }
    EBADF
}

pub fn sys_stat64(path_ptr: *const u8, st_ptr: *mut Stat64) -> usize {
    if st_ptr.is_null() || path_ptr.is_null() {
        return EFAULT;
    }
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    if path.is_empty() {
        return ENOENT;
    }
    if path_is_dir(path) {
        unsafe {
            fill_stat64(&mut *st_ptr, 1, S_IFDIR, 0);
        }
        return 0;
    }
    let Some(inode) = VFS.get().resolve_path(path) else {
        return ENOENT;
    };
    let size = probe_inode_size(inode);
    unsafe {
        fill_stat64(&mut *st_ptr, inode, S_IFREG, size);
    }
    0
}

pub fn sys_getdents64(current_slot: usize, fd: usize, dirp: *mut u8, count: usize) -> usize {
    if dirp.is_null() || count < 24 {
        return EINVAL;
    }
    unsafe {
        let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] else {
            return EBADF;
        };
        let (path_str, cookie) = match current.fd_table.get(fd) {
            Some(FileDescriptor::Dir {
                path,
                path_len,
                cookie,
            }) => {
                let s = core::str::from_utf8(&path[..*path_len as usize]).unwrap_or("/");
                (s, *cookie)
            }
            _ => return ENOTDIR,
        };
        let entries = match VFS.get().list_directory_entries(path_str) {
            Some(e) => e,
            None => return ENOTDIR,
        };
        let mut written = 0usize;
        let mut idx = cookie as usize;
        while idx < entries.len() {
            let e = &entries[idx];
            let name = e.name.as_bytes();
            // reclen = 19 + name + NUL, aligned to 8
            let base = 19 + name.len() + 1;
            let reclen = (base + 7) & !7;
            if written + reclen > count {
                break;
            }
            let p = dirp.add(written);
            // d_ino u64
            core::ptr::write_unaligned(p as *mut u64, e.inode as u64);
            // d_off i64 = next index
            core::ptr::write_unaligned(p.add(8) as *mut i64, (idx + 1) as i64);
            // d_reclen u16
            core::ptr::write_unaligned(p.add(16) as *mut u16, reclen as u16);
            // d_type u8: DT_DIR=4, DT_REG=8
            *p.add(18) = if e.file_type == 2 { 4 } else { 8 };
            // d_name
            core::ptr::copy_nonoverlapping(name.as_ptr(), p.add(19), name.len());
            *p.add(19 + name.len()) = 0;
            written += reclen;
            idx += 1;
        }
        if let Some(FileDescriptor::Dir { cookie, .. }) = current.fd_table.get_mut(fd) {
            *cookie = idx as u32;
        }
        written
    }
}

pub fn sys_read(current_slot: usize, fd: usize, buf_ptr: *mut u8, count: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            // Fallback: bare fd 0 with empty table still means console
            let desc = current.fd_table.get(fd).copied().or_else(|| {
                if fd == 0 {
                    Some(FileDescriptor::ConsoleIn)
                } else {
                    None
                }
            });

            match desc {
                Some(FileDescriptor::ConsoleIn) => {
                    return sys_read_stdin(buf_ptr, count);
                }
                Some(FileDescriptor::File {
                    inode,
                    offset,
                    mode,
                }) => {
                    if mode == FileMode::WriteOnly {
                        return 0;
                    }
                    let mut temp = alloc::vec![0u8; count];
                    let bytes = VFS.get().read_at(inode, offset, &mut temp);
                    if bytes > 0 {
                        core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes);
                        if let Some(FileDescriptor::File {
                            offset: off,
                            ..
                        }) = current.fd_table.get_mut(fd)
                        {
                            *off += bytes as u64;
                        }
                    }
                    return bytes;
                }
                Some(FileDescriptor::Pipe { pipe_id, end }) => {
                    if end != PipeEnd::Read {
                        return 0;
                    }
                    if current.fd_table.is_nonblock(fd) {
                        return pipe::pipe_try_read(pipe_id, buf_ptr, count);
                    }
                    return pipe::pipe_read(pipe_id, buf_ptr, count);
                }
                Some(FileDescriptor::Socket { .. }) => return 0,
                Some(FileDescriptor::ConsoleOut) => return 0,
                None => return 0,
                Some(FileDescriptor::Device {
                    inode,
                    offset,
                    mode,
                }) => {
                    if mode == FileMode::WriteOnly {
                        return 0;
                    }

                    let mut temp = alloc::vec![0u8; count];
                    // VFS сам разберется, что это DevFS, и вызовет read_from_block_device или CharDevice::read
                    let bytes = VFS.get().read_at(inode, offset, &mut temp);

                    if bytes > 0 {
                        core::ptr::copy_nonoverlapping(temp.as_ptr(), buf_ptr, bytes);

                        // Обновляем offset в таблице дескрипторов
                        if let Some(FileDescriptor::Device {
                            offset: off,
                            ..
                        }) = current.fd_table.get_mut(fd)
                        {
                            *off += bytes as u64;
                        }
                    }
                    return bytes;
                }
                _ => {}
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
pub fn sys_read_stdin(buf_ptr: *mut u8, count: usize) -> usize {
    let mut read = 0;

    // Включаем прерывания — обработчик клавиатуры сможет наполнять буфер
    unsafe {
        asm!("sti");
    }

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
                unsafe {
                    *buf_ptr.add(read) = b;
                }
                read += 1;
            }
            None => {
                // Буфер пуст — спим до следующего прерывания
                unsafe {
                    asm!("hlt");
                }
            }
        }
    }

    // Восстанавливаем состояние (syscall entry сделал cli)
    unsafe {
        asm!("cli");
    }
    read
}

pub fn sys_write(current_slot: usize, fd: usize, buf_ptr: *const u8, count: usize) -> usize {
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

    // println!("KERNEL WRITE: fd={} len={} task={} {:02x?}", fd,  count, current_slot, &buf[0..buf.len().min(32)]);

    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            let desc = current.fd_table.get(fd).copied().or_else(|| {
                if fd == 1 || fd == 2 {
                    Some(FileDescriptor::ConsoleOut)
                } else {
                    None
                }
            });

            match desc {
                Some(FileDescriptor::ConsoleOut) | Some(FileDescriptor::ConsoleIn) => {
                    match core::str::from_utf8(buf) {
                        Ok(v) => {
                            print!("{}", v);
                        }
                        Err(_) => println!("{:02x?}", &buf),
                    }
                    return count;
                }
                Some(FileDescriptor::File {
                    inode,
                    offset,
                    mode,
                }) => {
                    if mode == FileMode::ReadOnly {
                        return 0;
                    }
                    let written = VFS.get().write_at(inode, offset, buf);
                    if let Some(FileDescriptor::File {
                        offset: off,
                        ..
                    }) = current.fd_table.get_mut(fd)
                    {
                        *off += written as u64;
                    }
                    return written;
                }
                Some(FileDescriptor::Pipe { pipe_id, end }) => {
                    if end != PipeEnd::Write {
                        return 0;
                    }
                    if current.fd_table.is_nonblock(fd) {
                        return pipe::pipe_try_write(pipe_id, buf_ptr, count);
                    }
                    return pipe::pipe_write(pipe_id, buf_ptr, count);
                }
                Some(FileDescriptor::Socket { .. }) => return 0,
                Some(FileDescriptor::Device {
                    inode,
                    offset,
                    mode,
                }) => {
                    if mode == FileMode::ReadOnly {
                        return 0;
                    }

                    let mut temp = alloc::vec![0u8; count];
                    core::ptr::copy_nonoverlapping(buf_ptr, temp.as_mut_ptr(), count);

                    // VFS маршрутизирует в DevFS -> write_to_block_device (Read-Modify-Write)
                    let bytes = VFS.get().write_at(inode, offset, &temp);

                    if bytes > 0 {
                        if let Some(FileDescriptor::Device {
                            offset: off,
                            ..
                        }) = current.fd_table.get_mut(fd)
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
                _ => {}
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
        FileDescriptor::Pipe { pipe_id, end } => match end {
            PipeEnd::Read => pipe::pipe_close_reader(pipe_id),
            PipeEnd::Write => pipe::pipe_close_writer(pipe_id),
        },
        _ => {}
    }
}

pub fn sys_close(current_slot: usize, fd: usize) -> usize {
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

pub fn sys_pipe(current_slot: usize, pipefd: *mut u32) -> usize {
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
            current
                .fd_table
                .insert(r, FileDescriptor::new_pipe(pipe_id, PipeEnd::Read));
            let w = match current.fd_table.alloc_fd() {
                Some(f) => f,
                None => {
                    let _ = current.fd_table.close(r);
                    pipe::pipe_close_reader(pipe_id);
                    pipe::pipe_close_writer(pipe_id);
                    return usize::MAX;
                }
            };
            current
                .fd_table
                .insert(w, FileDescriptor::new_pipe(pipe_id, PipeEnd::Write));
            *pipefd = r as u32;
            *pipefd.add(1) = w as u32;
            return 0;
        }
    }
    usize::MAX
}

pub fn sys_dup2(current_slot: usize, oldfd: usize, newfd: usize) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if current.fd_table.get(oldfd).is_none() {
                return usize::MAX;
            }
            // bump pipe refcounts on duplicate
            if let Some(FileDescriptor::Pipe { pipe_id, end }) =
                current.fd_table.get(oldfd).copied()
            {
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

pub fn sys_mkdir(path_ptr: *const u8) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    let success = VFS.get().mkdir(path);
    if success { 0 } else { usize::MAX }
}

pub fn sys_rmdir(path_ptr: *const u8) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    let success = VFS.get().rmdir(path);
    if success { 0 } else { usize::MAX }
}

pub fn sys_unlink(path_ptr: *const u8) -> usize {
    let path = unsafe { CStr::from_ptr(path_ptr as *const i8).to_str().unwrap_or("") };
    let success = VFS.get().remove_file(path);
    if success { 0 } else { usize::MAX }
}

/// Читает содержимое директории и записывает имена файлов
/// (разделённые '\n') в пользовательский буфер.
/// Возвращает количество записанных байт или 0 при ошибке.
pub fn sys_ls(path_ptr: *const u8, buf_ptr: *mut u8, buf_size: usize) -> usize {
    let path = unsafe {
        CStr::from_ptr(path_ptr as *const i8)
            .to_str()
            .unwrap_or("/")
    };
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
pub fn sys_exit(current_slot: usize, esp: u32) -> u32 {
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
pub const WNOHANG: u32 = 1;

/// kill(pid, sig) — queue `sig` for task slot `pid`.
pub fn sys_kill(pid: i32, sig: u32) -> usize {
    if crate::signal::send_signal(pid as i8, sig) {
        0
    } else {
        usize::MAX
    }
}

#[repr(C)]
pub struct SigActionUser {
    sa_handler: u32,
    sa_mask: u32,
    sa_flags: u32,
}

/// sigaction(sig, act, oldact)
pub fn sys_sigaction(
    current_slot: usize,
    sig: u32,
    act: *const SigActionUser,
    oldact: *mut SigActionUser,
) -> usize {
    if sig == 0 || sig > 31 || sig == crate::signal::SIGKILL {
        return usize::MAX;
    }
    let idx = (sig - 1) as usize;
    unsafe {
        if let Some(ref mut t) = TASK_MANAGER.tasks[current_slot] {
            if !oldact.is_null() {
                *oldact = SigActionUser {
                    sa_handler: t.signal_handlers[idx],
                    sa_mask: 0,
                    sa_flags: 0,
                };
            }
            if !act.is_null() {
                t.signal_handlers[idx] = (*act).sa_handler;
            }
            return 0;
        }
    }
    usize::MAX
}

pub fn sys_wait(current_slot: usize, pid: i32, options: u32) -> usize {
    let parent = current_slot as i8;
    let nohang = options & WNOHANG != 0;

    loop {
        let found = unsafe { TASK_MANAGER.find_zombie_child(parent, pid) };
        if let Some((child_slot, _exit_code)) = found {
            unsafe {
                TASK_MANAGER.reap(child_slot);
            }
            return child_slot;
        }

        if nohang {
            return 0;
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

const F_GETFL: u32 = 3;
const F_SETFL: u32 = 4;

pub fn sys_fcntl(current_slot: usize, fd: usize, cmd: u32, arg: u32) -> usize {
    unsafe {
        if let Some(ref mut current) = TASK_MANAGER.tasks[current_slot] {
            if current.fd_table.get(fd).is_none() {
                return usize::MAX;
            }
            match cmd {
                F_GETFL => return current.fd_table.get_flags(fd) as usize,
                F_SETFL => {
                    // Only O_NONBLOCK is meaningful for now.
                    let flags = arg & crate::filesystem::file::O_NONBLOCK;
                    if current.fd_table.set_flags(fd, flags) {
                        return 0;
                    }
                    return usize::MAX;
                }
                _ => return usize::MAX,
            }
        }
    }
    usize::MAX
}

/// Linux-ish pollfd (32-bit).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

pub const POLLIN: i16 = 0x0001;
pub const POLLOUT: i16 = 0x0004;
pub const POLLERR: i16 = 0x0008;
pub const POLLHUP: i16 = 0x0010;

fn fd_poll_revents(current_slot: usize, fd: i32, events: i16) -> i16 {
    if fd < 0 {
        return 0;
    }
    let fd = fd as usize;
    unsafe {
        let Some(ref current) = TASK_MANAGER.tasks[current_slot] else {
            return POLLERR;
        };
        match current.fd_table.get(fd).copied() {
            Some(FileDescriptor::Pipe { pipe_id, end }) => {
                let mut rev = 0i16;
                match end {
                    PipeEnd::Read => {
                        if events & POLLIN != 0 && pipe::pipe_readable(pipe_id) {
                            rev |= POLLIN;
                        }
                    }
                    PipeEnd::Write => {
                        if events & POLLOUT != 0 && pipe::pipe_writable(pipe_id) {
                            rev |= POLLOUT;
                        }
                    }
                }
                rev
            }
            Some(FileDescriptor::ConsoleIn) => {
                // Always report readable for simplicity (stdin may still block on read).
                if events & POLLIN != 0 { POLLIN } else { 0 }
            }
            Some(FileDescriptor::ConsoleOut)
            | Some(FileDescriptor::File { .. })
            | Some(FileDescriptor::Device { .. }) => {
                let mut rev = 0i16;
                if events & POLLIN != 0 {
                    rev |= POLLIN;
                }
                if events & POLLOUT != 0 {
                    rev |= POLLOUT;
                }
                rev
            }
            Some(FileDescriptor::Socket { socket_id }) => {
                let mut rev = 0i16;
                if let Some(stack_guard) = crate::net::stack::NET_STACK.try_lock() {
                    if let Some(ref stack) = *stack_guard {
                        if let Some((handle, is_tcp)) = stack.get_handle(socket_id) {
                            if is_tcp {
                                let socket = stack.sockets.get::<tcp::Socket>(handle);
                                if events & POLLIN != 0 && socket.can_recv() {
                                    rev |= POLLIN;
                                }
                                if events & POLLOUT != 0 && socket.can_send() {
                                    rev |= POLLOUT;
                                }
                            } else {
                                // UDP: всегда можно писать, читать если есть данные
                                if events & POLLOUT != 0 { rev |= POLLOUT; }
                                if events & POLLIN != 0 {
                                    let socket = stack.sockets.get::<udp::Socket>(handle);
                                    if socket.can_recv() { rev |= POLLIN; }
                                }
                            }
                        }
                    }
                }
                rev
            }
            None => POLLERR,
            _ => 0,
        }
    }
}

pub fn sys_poll(current_slot: usize, fds: *mut PollFd, nfds: usize, timeout_ms: i32) -> usize {
    if fds.is_null() || nfds == 0 {
        return 0;
    }
    let nfds = nfds.min(64);
    let start = crate::time::jiffies();

    loop {
        let mut ready = 0usize;
        for i in 0..nfds {
            unsafe {
                let p = fds.add(i);
                let fd = (*p).fd;
                let events = (*p).events;
                let rev = fd_poll_revents(current_slot, fd, events);
                (*p).revents = rev;
                if rev != 0 {
                    ready += 1;
                }
            }
        }
        if ready > 0 {
            return ready;
        }
        // timeout_ms: -1 = block forever, 0 = return immediately, >0 = ms
        if timeout_ms == 0 {
            return 0;
        }
        if timeout_ms > 0 {
            let elapsed = crate::time::jiffies().wrapping_sub(start);
            // jiffies ~1ms on this kernel
            if elapsed as i32 >= timeout_ms {
                return 0;
            }
        }
        // Sleep until next timer/keyboard interrupt so other tasks can run.
        unsafe {
            asm!("sti");
            asm!("hlt");
            asm!("cli");
        }
    }
}

/// Userspace layout of `libfelix::syscall::ExecParams`.
#[repr(C)]
pub struct ExecParamsUser {
    stdin: i32,
    stdout: i32,
    stderr: i32,
    argc: u32,
    argv: *const *const u8,
}

pub struct ExecParamsKernel {
    pub(crate) stdin: i32,
    pub(crate) stdout: i32,
    pub(crate) stderr: i32,
    /// Owned C-string copies of argv.
    argv: alloc::vec::Vec<alloc::vec::Vec<u8>>,
}

pub fn read_exec_params(ptr: *const ExecParamsUser) -> ExecParamsKernel {
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
pub fn install_child_fd(
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
        let pd_phys = task.page_dir_phys;

        // Kernel mappings в PD задачи
        copy_kernel_mappings(task.pd_mut(), pd_phys);

        // User stack
        for i in 0..USER_STACK_PAGES {
            let page = USER_STACK_TOP - (i + 1) * PAGE_SIZE as u32;
            task.pd_mut().alloc_and_map_user_page(page);
        }
        // Pre-map a generous user heap so bump-malloc does not fault after a few
        // format!/String allocations in the shell (was 8 pages = 32 KiB).
        // Also seed page_refcounts so a stray FREE cannot unmap these pages.
        const USER_HEAP_PAGES: u32 = 512; // 2 MiB
        for i in 0..USER_HEAP_PAGES {
            let va = heap_start + i * PAGE_SIZE as u32;
            task.pd_mut().alloc_and_map_user_page(va);
            let _ = task.page_refcounts.inc(va);
        }

        // Переключаемся на PD задачи и грузим ELF по его p_vaddr
        // task.page_dir.switch();

        let entry_point = match crate::elf::load_elf(buf, task.pd_mut()) {
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
            let kernel_stack_top = t.stack_base + crate::multitasking::task::STACK_SIZE as u32;
            t.kernel_stack = kernel_stack_top;

            let state_ptr = (kernel_stack_top as usize
                - crate::multitasking::task::HEADROOM
                - core::mem::size_of::<CPUState>()) as *mut CPUState;
            t.cpu_state_ptr = state_ptr as u32;

            // argv on user stack (defaults to empty argc=0)
            let user_esp = setup_user_argv(t.pd(), USER_STACK_TOP, argv);

            *state_ptr = CPUState {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
                esi: 0,
                edi: 0,
                ebp: 0,
                eip: entry_point,
                cs: 0x1B,
                eflags: 0x202,
                esp: user_esp,
                ss: 0x23,
            };

            let pd_phys = t.page_dir_phys;
            t.pd_mut().entries[1023] = pd_phys
                | crate::memory::paging::PDEFlags::PRESENT
                | crate::memory::paging::PDEFlags::WRITABLE;

            t.running = true;
            t.heap_next = heap_start;
            t.mmap_next = 0x6000_0000;
            t.parent = parent_slot as i8;
            t.zombie = false;
            t.exit_code = 0;
            t.pending_signals = 0;
            t.signal_handlers = [0; 32];

            // stdio + optional remap from parent fds
            t.fd_table = FileDescriptorTable::with_stdio();
            install_child_fd(
                parent_slot,
                &mut t.fd_table,
                0,
                stdin_fd,
                FileDescriptor::ConsoleIn,
            );
            install_child_fd(
                parent_slot,
                &mut t.fd_table,
                1,
                stdout_fd,
                FileDescriptor::ConsoleOut,
            );
            install_child_fd(
                parent_slot,
                &mut t.fd_table,
                2,
                stderr_fd,
                FileDescriptor::ConsoleOut,
            );

            println!(
                "[execve] OK pid={} entry={:#x} stack={:#x} pd_phys={:#x} parent={}",
                slot, entry_point, USER_STACK_TOP, pd_phys, parent_slot
            );
        }

        asm!("sti");
        slot // return pid to caller
    }
}

use crate::net::stack::{NET_STACK, poll_stack};
use crate::print::klog_write_str;
use smoltcp::socket::{tcp, udp};
use smoltcp::socket::tcp::ConnectError;
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address};
use crate::time::sleep;
use crate::utils::rand_int;

pub fn sys_socket(current_slot: usize, domain: u16, ty: u16, protocol: u8) -> usize {
    let mut stack_guard = match NET_STACK.try_lock() {
        Some(g) => g,
        None => return usize::MAX,
    };
    let stack = match stack_guard.as_mut() {
        Some(s) => s,
        None => return usize::MAX,
    };

    let (socket_id, handle) = match stack.create_socket(domain, ty, protocol) {
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

pub fn sys_bind(current_slot: usize, fd: usize, addr_ptr: *const u8, addrlen: usize) -> usize {
    println!(
        "current_slot: {}, fd: {} addr_ptr: {:x} addrlen: {}",
        current_slot, fd, addr_ptr as usize, addrlen
    );
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
            addr: Some(IpAddress::Ipv4(Ipv4Addr::from_bits(
                addr.sin_addr.s_addr,
            ))),
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

pub fn sys_listen(current_slot: usize, fd: usize, backlog: usize) -> usize {
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

pub fn sys_accept4(
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


pub fn sys_connect(current_slot: usize, fd: usize, addr_ptr: *const u8, addrlen: usize) -> usize {
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

    let endpoint = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Addr::from_bits(addr.sin_addr.s_addr)),
        port: u16::from_be(addr.sin_port),
    };
    let local_port = 49152u16 + (socket_id as u16 & 0x3FFF);
    // --- UDP: простой bind, без retry ---
    {
        let mut guard = NET_STACK.lock();
        let stack = match guard.as_mut() {
            Some(s) => s,
            None => return usize::MAX,
        };
        let (handle, is_tcp) = match stack.get_handle(socket_id) {
            Some(h) => h,
            None => return usize::MAX,
        };

        if !is_tcp {
            let socket = stack.sockets.get_mut::<udp::Socket>(handle);
            let listen = IpListenEndpoint {
                addr: None,
                port: local_port,
            };
            match socket.bind(listen) {
                Ok(()) => {
                    let mut table = SOCKET_TABLE.lock();
                    if let Some(sock) = table.get_mut(socket_id) {
                        sock.peer_addr = Some(addr);
                        sock.state = SocketState::Connected;
                    }
                    return 0;
                }
                Err(_) => return usize::MAX,
            }
        }
    }

    // --- TCP: retry loop с поллингом ---
    let mut connect_issued = false;

    // Явный эфемерный порт (49152 + socket_id чтобы не было коллизий)
    let local_port = 49152u16 + (socket_id as u16 & 0x3FFF);
    let local = IpListenEndpoint {
        addr: Some(IpAddress::v4(10, 0, 2, 15)),
        port: local_port,
    };

    for attempt in 0..200 {
        {
            let mut guard = NET_STACK.lock();
            let stack = match guard.as_mut() {
                Some(s) => s,
                None => return usize::MAX,
            };

            stack.poll(crate::time::jiffies() as i64);

            let (handle, _is_tcp) = match stack.get_handle(socket_id) {
                Some(h) => h,
                None => return usize::MAX,
            };

            let socket = stack.sockets.get_mut::<tcp::Socket>(handle);

            if !connect_issued {
                match socket.connect(stack.iface.context(), endpoint, local) {
                    Ok(()) => {
                        connect_issued = true;
                        // Поллим ещё раз чтобы smoltcp отправил ARP + SYN
                        stack.poll(crate::time::jiffies() as i64);
                    }
                    Err(smoltcp::socket::tcp::ConnectError::Unaddressable) => {
                        // ARP ещё не прошёл — отпустим лок и попробуем снова
                    }
                    Err(e) => {
                        println!("connect: fatal err {:?} (attempt {})", e, attempt);
                        return usize::MAX;
                    }
                }
            } else {
                match socket.state() {
                    smoltcp::socket::tcp::State::Established => {
                        let mut table = SOCKET_TABLE.lock();
                        if let Some(sock) = table.get_mut(socket_id) {
                            sock.peer_addr = Some(addr);
                            sock.state = SocketState::Connected;
                        }
                        return 0;
                    }
                    smoltcp::socket::tcp::State::Closed
                    | smoltcp::socket::tcp::State::CloseWait => {
                        return usize::MAX;
                    }
                    _ => {} // SynSent / SynReceived — продолжаем поллить
                }
            }
        } // лок отпущен

        // Короткий сон, даём таймеру поллить сеть
        unsafe {
            asm!("sti");
            asm!("hlt");
            asm!("cli");
        }
    }

    usize::MAX // timeout
}
pub fn sys_sendto(current_slot: usize, fd: usize, buf: *const u8, len: usize) -> usize {
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
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        match socket.send_slice(data) {
            Ok(n) => return n,
            Err(_) => return usize::MAX,  // буфер полон
        }
    }

    let peer = {
        let table = SOCKET_TABLE.lock();
        table.get(socket_id).and_then(|s| s.peer_addr)
    };

    let Some(peer) = peer else { return 0 };

    let endpoint = IpEndpoint {
        addr: IpAddress::Ipv4(Ipv4Addr::from_bits(
            peer.sin_addr.s_addr,
        )),
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

pub fn sys_recvfrom(current_slot: usize, fd: usize, buf: *mut u8, len: usize) -> usize {
    if buf.is_null() || len == 0 {
        return 0;
    }

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

    let ts = crate::time::jiffies() as i64;
    stack.poll(ts);

    let (handle, is_tcp) = match stack.get_handle(socket_id) {
        Some(h) => h,
        None => return 0,
    };

    // Прямой slice из пользовательского буфера — без аллокации
    let user_buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };

    if is_tcp {
        let socket = stack.sockets.get_mut::<tcp::Socket>(handle);
        match socket.recv_slice(user_buf) {
            Ok(n) if n > 0 => n,
            Ok(_) => {
                // 0 байт — проверяем состояние
                if socket.state() == tcp::State::CloseWait
                    || socket.state() == tcp::State::Closed
                {
                    0  // настоящий EOF
                } else {
                    usize::MAX  // данных нет, попробовать позже
                }
            }
            Err(_) => usize::MAX,
        }
    } else {
        let socket = stack.sockets.get_mut::<udp::Socket>(handle);
        match socket.recv_slice(user_buf) {
            Ok((size, ep)) => {
                // сохраняем peer для последующего sendto
                let mut table = SOCKET_TABLE.lock();
                if let Some(sock) = table.get_mut(socket_id) {
                    let ip = match ep.endpoint.addr {
                        IpAddress::Ipv4(a) => a.to_bits(),
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
            Err(smoltcp::socket::udp::RecvError::Truncated) => {
                // Буфер был меньше UDP-пакета — пакет потерян
                0
            }
            Err(_) => 0,
        }
    }
}

pub fn sys_shutdown(current_slot: usize, fd: usize, _how: u32) -> usize {
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
pub struct WmCreateArgs {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    title: *const u8,
}

pub fn sys_wm_create(current_slot: usize, args: *const WmCreateArgs) -> usize {
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

pub fn sys_wm_destroy(id: u32) -> usize {
    if crate::drivers::wm::destroy_window(id) {
        0
    } else {
        usize::MAX
    }
}

pub fn sys_wm_move(id: u32, x: i32, y: i32) -> usize {
    if crate::drivers::wm::move_window(id, x, y) {
        0
    } else {
        usize::MAX
    }
}

pub fn sys_wm_info(id: u32, out: *mut crate::drivers::wm::WindowInfo) -> usize {
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

pub fn sys_wm_flip(id: u32, pixels: *const u8, len: usize) -> usize {
    if crate::drivers::wm::flip(id, pixels, len) {
        0
    } else {
        usize::MAX
    }
}

pub fn sys_wm_focus(id: u32) -> usize {
    if crate::drivers::wm::focus_window(id) {
        0
    } else {
        usize::MAX
    }
}

pub fn sys_wm_screen(out: *mut u32) -> usize {
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

pub fn sys_mouse_state(out: *mut crate::drivers::mouse::MouseState) -> usize {
    if out.is_null() {
        return usize::MAX;
    }
    unsafe {
        *out = crate::drivers::mouse::state();
    }
    0
}

pub fn sys_wm_poll(id: u32, out: *mut crate::drivers::wm::WmEvent, max: usize) -> usize {
    crate::drivers::wm::poll_events(id, out, max.min(32))
}

/// Compact PCI device record for userspace (`lspci`). Must match libfelix.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PciInfoUser {
    bus: u8,
    device: u8,
    function: u8,
    _pad: u8,
    vendor_id: u16,
    device_id: u16,
    class_code: u8,
    subclass: u8,
    prog_if: u8,
    interrupt_line: u8,
}

/// `buf` may be null with `max == 0` to query the number of devices only.
pub fn sys_pci_list(buf: *mut PciInfoUser, max: usize) -> usize {
    let devices = crate::pci::enumerate();
    let n = devices.len();
    if max == 0 || buf.is_null() {
        return n;
    }
    let write_n = n.min(max);
    for (i, d) in devices.iter().take(write_n).enumerate() {
        unsafe {
            *buf.add(i) = PciInfoUser {
                bus: d.bus,
                device: d.device,
                function: d.function,
                _pad: 0,
                vendor_id: d.vendor_id,
                device_id: d.device_id,
                class_code: d.class_code,
                subclass: d.subclass,
                prog_if: d.prog_if,
                interrupt_line: d.interrupt_line,
            };
        }
    }
    write_n
}
