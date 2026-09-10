#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libfelix::prelude::*;
use libfelix::syscall::{
    execve_env, execve_wasm_env, getpid, sys_sleep, waitpid_status, wifexited,
    wifsignaled, wexitstatus, wtermsig, WNOHANG,
};

const SELFTEST: &str = "/selftest";
const DEFAULT_SHELL: &str = "/shell";
const POLL_MS: u32 = 50;

fn exit_code_from_wait(status: i32) -> i32 {
    if wifexited(status) {
        wexitstatus(status)
    } else if wifsignaled(status) {
        128 + wtermsig(status) as i32
    } else {
        status
    }
}

struct Service {
    argv: Vec<String>,
    image: Vec<u8>,
    respawn: bool,
    pid: Option<i32>,
}

impl Service {
    fn path(&self) -> &str {
        self.argv.first().map(|s| s.as_str()).unwrap_or("")
    }
}

fn read_file(path: &str) -> Option<Vec<u8>> {
    let mut file = File::open_ro(path).ok()?;
    file.read_to_end().ok()
}

fn parse_config() -> Vec<(bool, Vec<String>)> {
    let data = read_file("/init.conf").or_else(|| read_file("/etc/init.conf"));
    let Some(data) = data else { return Vec::new(); };
    let text = core::str::from_utf8(&data).unwrap_or("");
    let mut specs = Vec::new();

    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue; };
        let (respawn, path) = match first {
            "respawn" => (true, words.next().unwrap_or("")),
            "once" => (false, words.next().unwrap_or("")),
            // A bare command is a once service.
            other => (false, other),
        };
        if path.is_empty() || !path.starts_with('/') { continue; }

        let mut argv = Vec::new();
        argv.push(path.to_string());
        for arg in words {
            argv.push(arg.to_string());
        }
        specs.push((respawn, argv));
    }
    specs
}

fn load_services() -> Vec<Service> {
    let mut specs = parse_config();
    if !specs.iter().any(|(_, argv)| argv.first().map(|s| s.as_str()) == Some(DEFAULT_SHELL)) {
        specs.push((true, alloc::vec![DEFAULT_SHELL.to_string()]));
    }

    let mut services = Vec::new();
    for (respawn, argv) in specs {
        let Some(path) = argv.first() else { continue; };
        match read_file(path) {
            Some(image) if image.len() >= 4 => services.push(Service {
                argv,
                image,
                respawn,
                pid: None,
            }),
            _ => println!("init: cannot load {}", path),
        }
    }
    services
}

fn run_boot_selftest() {
    let Some(image) = read_file(SELFTEST) else {
        println!("init: {} not found", SELFTEST);
        return;
    };
    if image.len() < 4 {
        println!("init: {} is invalid", SELFTEST);
        return;
    }

    let mut service = Service {
        argv: alloc::vec![SELFTEST.to_string()],
        image,
        respawn: false,
        pid: None,
    };
    if !spawn_service(&mut service) {
        println!("init: failed to start boot selftest");
        return;
    }

    let Some(pid) = service.pid else { return; };
    let mut status = 0i32;
    let ret = unsafe { waitpid_status(pid, &mut status, 0) };
    if ret == pid as usize {
        println!("init: boot selftest pid={} status={}", pid, exit_code_from_wait(status));
    } else {
        println!("init: boot selftest wait failed pid={} ret={}", pid, ret as i32);
    }
}

fn spawn_service(service: &mut Service) -> bool {
    if service.image.len() < 4 || service.argv.is_empty() { return false; }

    let mut argv_store = Vec::new();
    for arg in &service.argv {
        let mut s = arg.clone();
        s.push('\0');
        argv_store.push(s);
    }
    let argv: Vec<*const u8> = argv_store.iter().map(|s| s.as_ptr()).collect();

    let env_store = [String::from("PATH=/\0"), String::from("HOME=/\0")];
    let envp: Vec<*const u8> = env_store.iter().map(|s| s.as_ptr()).collect();

    let pid = unsafe {
        match &service.image[..4] {
            b"\x7fELF" => execve_env(
                service.image.as_ptr(), service.image.len(), -1, -1, -1, &argv, &envp,
            ),
            b"\0asm" => execve_wasm_env(
                service.image.as_ptr(), service.image.len(), -1, -1, -1, &argv, &envp,
            ),
            _ => usize::MAX,
        }
    };

    if pid == usize::MAX {
        println!("init: exec {} failed", service.path());
        false
    } else {
        service.pid = Some(pid as i32);
        println!(
            "init: started pid={} mode={} {}",
            pid,
            if service.respawn { "respawn" } else { "once" },
            service.path(),
        );
        true
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let pid = unsafe { getpid() };
    println!("Felix init: service manager pid={}", pid);
    if pid != 1 {
        println!("init: expected PID 1, got {}", pid);
    }

    run_boot_selftest();

    let mut services = load_services();
    if services.is_empty() {
        println!("init: no runnable services");
        loop { unsafe { sys_sleep(1000); } }
    }

    for service in &mut services {
        let _ = spawn_service(service);
    }

    loop {
        for service in &mut services {
            if let Some(pid) = service.pid {
                let mut status = 0i32;
                let ret = unsafe { waitpid_status(pid, &mut status, WNOHANG) };
                if ret == pid as usize || ret == usize::MAX {
                    if ret == pid as usize {
                        println!("init: pid={} exited status={} {}", pid, exit_code_from_wait(status), service.path());
                    } else {
                        println!("init: lost child pid={} {}", pid, service.path());
                    }
                    service.pid = None;
                }
            }

            if service.pid.is_none() && service.respawn {
                let _ = spawn_service(service);
            }
        }

        // Reap children adopted from terminated services/processes. If one of
        // our tracked services happened to exit between the checks above and
        // this pass, update its state here as well.
        loop {
            let mut status = 0i32;
            let pid = unsafe { waitpid_status(-1, &mut status, WNOHANG) };
            if pid == 0 || pid == usize::MAX { break; }
            let pid = pid as i32;
            if let Some(service) = services.iter_mut().find(|s| s.pid == Some(pid)) {
                println!("init: pid={} exited status={} {}", pid, exit_code_from_wait(status), service.path());
                service.pid = None;
            } else {
                println!("init: reaped orphan pid={} status={}", pid, exit_code_from_wait(status));
            }
        }

        unsafe { sys_sleep(POLL_MS); }
    }
}
