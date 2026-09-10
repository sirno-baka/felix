#![no_std]
#![no_main]

extern crate alloc;

use libfelix::prelude::*;
use libfelix::syscall::{
    close, execve, getcwd, getpid, getppid, mount_list, openpty, pipe, read, task_list, write,
    MountInfo, TaskInfo,
};

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

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // The first image execs itself at the end of the suite. Reaching this
    // branch proves that execve replaced the image instead of spawning a
    // child or returning to the caller.
    if libfelix::rt::arg(1) == Some("--exec-target") {
        println!("selftest: PASS (execve)");
        return 0;
    }

    println!("selftest: begin pid={} ppid={}", unsafe { getpid() }, unsafe { getppid() });

    if !test_process_identity() { return fail("process-identity"); }
    if !test_procfs() { return fail("procfs"); }
    if !test_cwd() { return fail("cwd"); }
    if !test_pipe() { return fail("pipe"); }
    if !test_pty() { return fail("pty"); }
    if !test_mounts() { return fail("mounts"); }

    let path = b"/selftest\0";
    let arg0 = b"/selftest\0";
    let arg1 = b"--exec-target\0";
    let argv = [arg0.as_ptr(), arg1.as_ptr(), core::ptr::null()];
    let ret = unsafe { execve(path.as_ptr(), argv.as_ptr(), core::ptr::null()) };
    println!("selftest: FAIL execve returned {}", ret);
    1
}
