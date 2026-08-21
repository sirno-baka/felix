#![no_std]
#![no_main]

extern crate alloc;

use core::mem::size_of;
use libfelix::prelude::*;
use libfelix::syscall::{
    socket, bind, connect, recvfrom, sendto, close, read,
    AF_INET, SOCK_DGRAM, IPPROTO_UDP,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct InAddr {
    s_addr: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: InAddr,
    sin_zero: [u8; 8],
}

impl SockAddrIn {
    fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: InAddr {
                s_addr: u32::from_be_bytes(ip),
            },
            sin_zero: [0; 8],
        }
    }

    fn any(port: u16) -> Self {
        Self::new([0, 0, 0, 0], port)
    }
}

fn parse_u16(s: &str) -> Option<u16> {
    let mut n: u32 = 0;
    if s.is_empty() {
        return None;
    }
    for b in s.bytes() {
        if !(b'0'..=b'9').contains(&b) {
            return None;
        }
        n = n * 10 + (b - b'0') as u32;
        if n > 65535 {
            return None;
        }
    }
    Some(n as u16)
}

fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut part = 0usize;
    let mut val: u16 = 0;
    let mut digits = 0u8;
    for b in s.bytes() {
        match b {
            b'0'..=b'9' => {
                digits += 1;
                if digits > 3 {
                    return None;
                }
                val = val * 10 + (b - b'0') as u16;
                if val > 255 {
                    return None;
                }
            }
            b'.' => {
                if digits == 0 || part >= 3 {
                    return None;
                }
                out[part] = val as u8;
                part += 1;
                val = 0;
                digits = 0;
            }
            _ => return None,
        }
    }
    if part != 3 || digits == 0 {
        return None;
    }
    out[3] = val as u8;
    Some(out)
}

fn usage() {
    println!("usage:");
    println!("  hello -l <port>              UDP listen (print + echo)");
    println!("  hello <ip> <port>            UDP client (send line, print reply)");
}

fn run_server(port: u16) -> i32 {
    let sock = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP) };
    if sock == usize::MAX {
        println!("socket failed");
        return 1;
    }

    let addr = SockAddrIn::any(port);
    let ret = unsafe {
        bind(
            sock as u32,
            &addr as *const _ as *const u8,
            size_of::<SockAddrIn>() as u32,
        )
    };
    if ret == usize::MAX {
        println!("bind failed");
        unsafe { close(sock as u32) };
        return 1;
    }
    println!("udp listen 0.0.0.0:{}", port);

    println!("send \"quit\" to stop");
    let mut buf = [0u8; 1500];
    loop {
        let n = unsafe { recvfrom(sock as u32, buf.as_mut_ptr(), buf.len()) };
        if n == 0 {
            continue;
        }
        let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
        let trimmed = text.trim();
        if trimmed == "quit" || trimmed == "exit" {
            println!("bye");
            unsafe { close(sock as u32) };
            return 0;
        }
        match core::str::from_utf8(&buf[..n]) {
            Ok(s) => println!("{}", s),
            Err(_) => println!("<{} binary bytes>", n),
        }
        let _ = unsafe { sendto(sock as u32, buf.as_ptr(), n) };
    }
}

fn run_client(ip: [u8; 4], port: u16) -> i32 {
    let sock = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP) };
    if sock == usize::MAX {
        println!("socket failed");
        return 1;
    }

    let addr = SockAddrIn::new(ip, port);
    let ret = unsafe {
        connect(
            sock as u32,
            &addr as *const _ as *const u8,
            size_of::<SockAddrIn>() as u32,
        )
    };
    if ret == usize::MAX {
        println!("connect failed");
        unsafe { close(sock as u32) };
        return 1;
    }
    println!(
        "udp client {}.{}.{}.{}:{}",
        ip[0], ip[1], ip[2], ip[3], port
    );

    let mut line = [0u8; 512];
    let mut pos = 0usize;
    let mut one = [0u8; 1];
    let mut rx = [0u8; 1500];

    loop {
        // non-blocking-ish: poll recv, then read one stdin byte
        let n = unsafe { recvfrom(sock as u32, rx.as_mut_ptr(), rx.len()) };
        if n > 0 {
            match core::str::from_utf8(&rx[..n]) {
                Ok(s) => println!("{}", s.trim_end_matches(&['\n', '\r'][..])),
                Err(_) => println!("<{} binary bytes>", n),
            }
        }

        // blocking read of 1 byte from stdin
        let r = unsafe { read(0, one.as_mut_ptr(), 1) };
        if r == 0 {
            continue;
        }
        let c = one[0];
        if c == b'\n' || c == b'\r' {
            if pos > 0 {
                // append newline so remote can treat as line
                if pos < line.len() {
                    line[pos] = b'\n';
                    pos += 1;
                }
                let sent = unsafe { sendto(sock as u32, line.as_ptr(), pos) };
                if sent == 0 {
                    println!("send failed");
                }
                pos = 0;
            }
        } else if pos < line.len() {
            line[pos] = c;
            pos += 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    // hello -l <port>
    if arg(1) == Some("-l") {
        let port = match arg(2).and_then(parse_u16) {
            Some(p) => p,
            None => {
                usage();
                return 1;
            }
        };
        return run_server(port);
    }

    // hello <ip> <port>
    if let (Some(ip_s), Some(port_s)) = (arg(1), arg(2)) {
        let ip = match parse_ip(ip_s) {
            Some(ip) => ip,
            None => {
                println!("bad ip: {}", ip_s);
                usage();
                return 1;
            }
        };
        let port = match parse_u16(port_s) {
            Some(p) => p,
            None => {
                println!("bad port: {}", port_s);
                usage();
                return 1;
            }
        };
        return run_client(ip, port);
    }

    usage();
    1
}
