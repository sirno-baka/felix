#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use libfelix::prelude::*;
use libfelix::mutex::Mutex;
use libfelix::syscall::{
    close, execve, getcwd, getpid, getppid, gettid, mount_list, openpty, pipe, read, task_list, write,
    MountInfo, TaskInfo,
};
use core::sync::atomic::{AtomicU32, Ordering};

static THREAD_COUNTER: AtomicU32 = AtomicU32::new(0);

const SHARED_MUTEX_PATH: &[u8] = b"/selftest-shared-mutex\0";
const SHARED_MUTEX_ITERS: u32 = 5_000;

#[repr(C)]
struct SharedMutexPage {
    ready: AtomicU32,
    start: AtomicU32,
    value: Mutex<u32>,
}

fn map_shared_mutex(create: bool) -> Option<*mut SharedMutexPage> {
    use libfelix::syscall::{
        close, mmap_old, open, write, MmapArgStruct, MAP_SHARED, O_CREAT, O_RDWR, O_TRUNC,
        PROT_READ, PROT_WRITE,
    };

    let mut flags = O_RDWR;
    if create {
        flags |= O_CREAT | O_TRUNC;
    }
    let fd = unsafe { open(SHARED_MUTEX_PATH.as_ptr(), flags) };
    if (fd as isize) < 0 {
        return None;
    }

    if create {
        let zero = [0u8; 4096];
        if unsafe { write(fd as u32, zero.as_ptr(), zero.len()) } != zero.len() {
            unsafe { close(fd as u32); }
            return None;
        }
    }

    let args = MmapArgStruct {
        addr: 0,
        len: 4096,
        prot: PROT_READ | PROT_WRITE,
        flags: MAP_SHARED,
        fd: fd as i32,
        offset: 0,
    };
    let mapped = unsafe { mmap_old(&args) };
    unsafe { close(fd as u32); }
    if (mapped as isize) < 0 {
        None
    } else {
        Some(mapped as *mut SharedMutexPage)
    }
}

fn shared_mutex_child() -> i32 {
    let Some(ptr) = map_shared_mutex(false) else {
        return 2;
    };
    let shared = unsafe { &*ptr };
    shared.ready.store(1, Ordering::Release);
    while shared.start.load(Ordering::Acquire) == 0 {
        unsafe { libfelix::syscall::sched_yield(); }
    }
    for _ in 0..SHARED_MUTEX_ITERS {
        *shared.value.lock() += 1;
    }
    let rc = unsafe { libfelix::syscall::munmap(ptr.cast::<u8>(), 4096) };
    if rc != 0 { 3 } else { 0 }
}

fn test_process_shared_mutex() -> bool {
    unsafe { let _ = libfelix::syscall::unlink(SHARED_MUTEX_PATH.as_ptr()); }
    let Some(ptr) = map_shared_mutex(true) else {
        return false;
    };
    unsafe {
        core::ptr::write(
            ptr,
            SharedMutexPage {
                ready: AtomicU32::new(0),
                start: AtomicU32::new(0),
                value: Mutex::new(0),
            },
        );
    }
    let shared = unsafe { &*ptr };

    let args = [
        b"/bin/selftest\0".as_ptr(),
        b"--shared-mutex-child\0".as_ptr(),
    ];
    let pid = unsafe {
        libfelix::syscall::spawn_path_env_pgid(args[0], 0, 1, 2, &args, &[], 0, false)
    };
    if pid == 0 || pid > i32::MAX as usize {
        unsafe {
            let _ = libfelix::syscall::munmap(ptr.cast::<u8>(), 4096);
            let _ = libfelix::syscall::unlink(SHARED_MUTEX_PATH.as_ptr());
        }
        return false;
    }

    let mut ready = false;
    for _ in 0..1000 {
        if shared.ready.load(Ordering::Acquire) != 0 {
            ready = true;
            break;
        }
        unsafe { libfelix::syscall::sys_sleep(1); }
    }
    if !ready {
        return false;
    }

    shared.start.store(1, Ordering::Release);
    for _ in 0..SHARED_MUTEX_ITERS {
        *shared.value.lock() += 1;
    }

    let mut status = 0;
    let waited = unsafe {
        libfelix::syscall::waitpid_status(pid as i32, &mut status, 0)
    };
    let value = *shared.value.lock();
    let unmap_ok = unsafe { libfelix::syscall::munmap(ptr.cast::<u8>(), 4096) } == 0;
    unsafe { let _ = libfelix::syscall::unlink(SHARED_MUTEX_PATH.as_ptr()); }

    waited == pid
        && status == 0
        && value == SHARED_MUTEX_ITERS * 2
        && unmap_ok
}

fn fail(name: &str) -> i32 {
    println!("selftest: FAIL {}", name);
    1
}

fn test_process_identity() -> bool {
    let pid = unsafe { getpid() };
    let ppid = unsafe { getppid() };
    if pid <= 1 || ppid != 1 {
        println!("selftest: process identity pid={} ppid={}", pid, ppid);
        return false;
    }

    let total = unsafe { task_list(core::ptr::null_mut(), 0) };
    if total == 0 || (total as isize) < 0 {
        return false;
    }
    let mut tasks = alloc::vec![TaskInfo::default(); total];
    let n = unsafe { task_list(tasks.as_mut_ptr(), tasks.len()) };
    if (n as isize) < 0 || n > tasks.len() {
        return false;
    }
    tasks[..n].iter().any(|t| t.pid == pid && t.ppid == 1)
}

fn test_procfs() -> bool {
    let mut f = match File::open_ro("/proc/1/status") {
        Ok(f) => f,
        Err(_) => return false,
    };
    let data = match f.read_to_end() {
        Ok(data) => data,
        Err(_) => return false,
    };
    let text = core::str::from_utf8(&data).unwrap_or("");
    text.contains("Pid:\t1\n") && text.contains("Name:\tinit\n")
}

fn test_cwd() -> bool {
    let mut buf = [0u8; 128];
    let n = unsafe { getcwd(buf.as_mut_ptr(), buf.len()) };
    if n == 0 || (n as isize) < 0 || n > buf.len() {
        return false;
    }
    buf[0] == b'/'
}

fn test_pipe() -> bool {
    let mut fds = [0u32; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        return false;
    }
    let payload = b"pipe-ok";
    let wrote = unsafe { write(fds[1], payload.as_ptr(), payload.len()) };
    let mut out = [0u8; 7];
    let got = unsafe { read(fds[0], out.as_mut_ptr(), out.len()) };
    unsafe {
        close(fds[0]);
        close(fds[1]);
    }
    wrote == payload.len() && got == payload.len() && &out == payload
}

fn test_pty() -> bool {
    let mut fds = [u32::MAX; 2];
    let pty = unsafe { openpty(fds.as_mut_ptr()) };
    if (pty as isize) < 0 || fds[0] == u32::MAX || fds[1] == u32::MAX {
        return false;
    }

    // PTY defaults to canonical mode, so terminate the input record with LF.
    let payload = b"pty-ok\n";
    let wrote = unsafe { write(fds[0], payload.as_ptr(), payload.len()) };
    let mut out = [0u8; 7];
    let got = unsafe { read(fds[1], out.as_mut_ptr(), out.len()) };
    unsafe {
        close(fds[1]);
        close(fds[0]);
    }
    wrote == payload.len() && got == payload.len() && &out == payload
}

fn mount_path(item: &MountInfo) -> &str {
    let end = item.path.iter().position(|b| *b == 0).unwrap_or(item.path.len());
    core::str::from_utf8(&item.path[..end]).unwrap_or("")
}

fn test_mounts() -> bool {
    let total = unsafe { mount_list(core::ptr::null_mut(), 0) };
    if total < 3 || (total as isize) < 0 {
        return false;
    }
    let mut mounts = alloc::vec![MountInfo::default(); total];
    let n = unsafe { mount_list(mounts.as_mut_ptr(), mounts.len()) };
    if (n as isize) < 0 || n > mounts.len() {
        return false;
    }
    let mounts = &mounts[..n];
    ["/", "/dev", "/proc"]
        .iter()
        .all(|want| mounts.iter().any(|m| mount_path(m) == *want))
}

fn irq_stats_for_owner(owner: &str) -> Option<(u8, u32, u32)> {
    let mut file = File::open_ro("/proc/interrupts").ok()?;
    let data = file.read_to_end().ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    for line in text.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let Some(irq) = fields.next().and_then(|v| v.parse::<u8>().ok()) else {
            continue;
        };
        let Some(handled) = fields.next().and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        let Some(unhandled) = fields.next().and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        let _owner_count = fields.next();
        let names = fields.next().unwrap_or("-");
        if names.split(',').any(|name| name == owner) {
            return Some((irq, handled, unhandled));
        }
    }
    None
}

fn test_device_irqs() -> bool {
    // DHCP has already completed before init starts selftest. If a NIC has a
    // registered INTx owner, at least one interrupt must have been handled.
    let mut nic_seen = false;
    for owner in ["rtl8139", "e1000e", "i8255x"] {
        if let Some((irq, handled, _)) = irq_stats_for_owner(owner) {
            nic_seen = true;
            if handled == 0 {
                println!("selftest: {} IRQ{} never handled", owner, irq);
                return false;
            }
        }
    }
    if nic_seen {
        println!("selftest: NIC shared IRQ observed");
    }

    // Audio may legitimately run in polling fallback on hardware whose IRQ is
    // not one of the legacy shared PIC lines. Always test DMA start when the
    // device exists; require an IRQ count increase only for registered INTx.
    let before = irq_stats_for_owner("audio");
    let mut audio = match File::open("/dev/audio") {
        Ok(file) => file,
        Err(_) => return true,
    };
    let pcm = alloc::vec![0u8; 32 * 1024];
    if audio.write_all(&pcm).is_err() {
        return false;
    }
    unsafe { libfelix::syscall::sys_sleep(150); }

    if let Some((irq, handled_before, _)) = before {
        let Some((irq_after, handled_after, _)) = irq_stats_for_owner("audio") else {
            return false;
        };
        if irq_after != irq || handled_after <= handled_before {
            println!(
                "selftest: audio IRQ{} did not advance {} -> {}",
                irq, handled_before, handled_after
            );
            return false;
        }
        println!(
            "selftest: audio IRQ{} handled {} -> {}",
            irq, handled_before, handled_after
        );
    }
    true
}

fn test_threads() -> bool {
    let pid = unsafe { getpid() };
    let leader_tid = unsafe { gettid() };
    if pid != leader_tid {
        return false;
    }
    THREAD_COUNTER.store(0, Ordering::SeqCst);
    let protected = Arc::new(Mutex::new(0u32));
    let mut shared_pipe = [0u32; 2];
    if unsafe { pipe(shared_pipe.as_mut_ptr()) } != 0 {
        return false;
    }
    let pipe_writer = shared_pipe[1];
    let mut handles = Vec::new();
    for worker in 0..3u32 {
        let protected = protected.clone();
        println!("selftest: spawn worker {}", worker);
        let handle = match thread::spawn(move || {
            // Exercise shared heap allocation as well as shared atomics.
            let data = alloc::vec![worker as u8; 2048 + worker as usize * 31];
            for _ in 0..1000 {
                THREAD_COUNTER.fetch_add(1, Ordering::SeqCst);
                *protected.lock() += 1;
            }
            if worker == 0 {
                let byte = [b'T'];
                let _ = unsafe { write(pipe_writer, byte.as_ptr(), byte.len()) };
            }
            (unsafe { getpid() }, unsafe { gettid() }, data.len())
        }) {
            Ok(handle) => handle,
            Err(error) => {
                println!("selftest: thread spawn failed {:?}", error);
                return false;
            }
        };
        println!("selftest: spawned worker {} tid={}", worker, handle.tid());
        handles.push(handle);
    }
    // TIDs consume scheduler slots but must not appear as duplicate processes.
    let total = unsafe { task_list(core::ptr::null_mut(), 0) };
    let mut tasks = alloc::vec![TaskInfo::default(); total];
    let listed = unsafe { task_list(tasks.as_mut_ptr(), tasks.len()) };
    if listed > tasks.len() || tasks[..listed].iter().filter(|t| t.pid == pid).count() != 1 {
        return false;
    }
    for handle in handles {
        let expected_tid = handle.tid();
        println!("selftest: join tid={}", expected_tid);
        let Ok((thread_pid, thread_tid, len)) = handle.join() else {
            return false;
        };
        println!("selftest: joined tid={}", expected_tid);
        if thread_pid != pid || thread_tid != expected_tid || thread_tid == leader_tid || len < 2048 {
            return false;
        }
    }
    let mut byte = [0u8; 1];
    let read_count = unsafe { read(shared_pipe[0], byte.as_mut_ptr(), byte.len()) };
    unsafe {
        close(shared_pipe[0]);
        close(shared_pipe[1]);
    }
    THREAD_COUNTER.load(Ordering::SeqCst) == 3000
        && *protected.lock() == 3000
        && read_count == 1
        && byte[0] == b'T'
}

fn now_ms() -> u64 {
    let mut t = libfelix::syscall::TimeSpec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libfelix::syscall::clock_gettime(1, &mut t); }
    t.tv_sec as u64 * 1000 + t.tv_nsec as u64 / 1_000_000
}

fn test_parallel_timeouts() -> bool {
    let mut workers = Vec::new();
    for delay in [25, 75, 150] {
        let Ok(worker) = thread::spawn(move || {
            let start = now_ms();
            let result = unsafe { libfelix::syscall::poll(core::ptr::null_mut(), 0, delay) };
            result == 0 && now_ms() >= start + delay as u64
        }) else { return false; };
        workers.push(worker);
    }
    unsafe { libfelix::syscall::sys_sleep(100); }
    workers.into_iter().all(|w| w.join() == Ok(true))
}

fn test_process_reclaim() -> bool {
    for _ in 0..8 {
        let args = [b"/bin/selftest\0".as_ptr(), b"--fault-child\0".as_ptr()];
        let pid = unsafe { libfelix::syscall::spawn_path_env_pgid(args[0], 0, 1, 2, &args, &[], 0, false) };
        if pid == 0 || pid > i32::MAX as usize { return false; }
        let mut status = 0;
        let waited = unsafe { libfelix::syscall::waitpid_status(pid as i32, &mut status, 0) };
        if waited != pid || status != 132 << 8 { return false; }
    }
    true
}

fn fault_child() -> ! {
    for _ in 0..3 {
        let handle = thread::spawn(|| loop {
            core::hint::black_box(unsafe { getpid() });
        }).unwrap();
        core::mem::forget(handle);
    }
    unsafe { libfelix::syscall::sys_sleep(10); core::arch::asm!("ud2", options(noreturn)); }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if libfelix::rt::arg(1) == Some("--fault-child") { fault_child(); }
    if libfelix::rt::arg(1) == Some("--shared-mutex-child") {
        return shared_mutex_child();
    }
    // The first image execs itself at the end of the suite. Reaching this
    // branch proves that execve replaced the image instead of spawning a
    // child or returning to the caller.
    if libfelix::rt::arg(1) == Some("--exec-target") {
        println!("selftest: PASS (execve)");
        return 0;
    }

    println!("selftest: begin pid={} ppid={}", unsafe { getpid() }, unsafe { getppid() });

    println!("selftest: START process-identity");
    if !test_process_identity() { return fail("process-identity"); }
    println!("selftest: OK process-identity");
    println!("selftest: START procfs");
    if !test_procfs() { return fail("procfs"); }
    println!("selftest: OK procfs");
    println!("selftest: START cwd");
    if !test_cwd() { return fail("cwd"); }
    println!("selftest: OK cwd");
    println!("selftest: START pipe");
    if !test_pipe() { return fail("pipe"); }
    println!("selftest: OK pipe");
    println!("selftest: START pty");
    if !test_pty() { return fail("pty"); }
    println!("selftest: OK pty");
    println!("selftest: START mounts");
    if !test_mounts() { return fail("mounts"); }
    println!("selftest: OK mounts");
    println!("selftest: START device-irqs");
    if !test_device_irqs() { return fail("device-irqs"); }
    println!("selftest: OK device-irqs");
    println!("selftest: START threads");
    if !test_threads() { return fail("threads"); }
    println!("selftest: OK threads");

    println!("selftest: START parallel-timeouts");
    if !test_parallel_timeouts() { return fail("parallel-timeouts"); }
    println!("selftest: OK parallel-timeouts");
    println!("selftest: START process-shared-mutex");
    if !test_process_shared_mutex() { return fail("process-shared-mutex"); }
    println!("selftest: OK process-shared-mutex");
    println!("selftest: START process-reclaim");
    if !test_process_reclaim() { return fail("process-reclaim"); }
    println!("selftest: OK process-reclaim");
    println!("selftest: START execve");
    let path = b"/bin/selftest\0";
    let arg0 = b"/bin/selftest\0";
    let arg1 = b"--exec-target\0";
    let argv = [arg0.as_ptr(), arg1.as_ptr(), core::ptr::null()];
    let ret = unsafe { execve(path.as_ptr(), argv.as_ptr(), core::ptr::null()) };
    println!("selftest: FAIL execve returned {}", ret);
    1
}
