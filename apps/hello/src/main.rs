#![no_std]
#![no_main]

extern crate alloc;

use core::mem::size_of;
use libfelix::prelude::*;
use libfelix::syscall::{
    socket, bind, recvfrom, sendto, close,
    AF_INET, SOCK_DGRAM, IPPROTO_UDP,
};

/// Минимальные сетевые типы (как в Linux)
#[repr(C)]
#[derive(Clone, Copy)]
struct InAddr {
    s_addr: u32, // network byte order
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrIn {
    sin_family: u16,
    sin_port:   u16, // network byte order
    sin_addr:   InAddr,
    sin_zero:   [u8; 8],
}

impl SockAddrIn {
    fn new(ip: [u8; 4], port: u16) -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port:   port.to_be(),
            sin_addr:   InAddr {
                s_addr: u32::from_be_bytes(ip),
            },
            sin_zero: [0; 8],
        }
    }

    fn any(port: u16) -> Self {
        Self::new([0, 0, 0, 0], port)
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    println!("=== UDP Echo Server ===");

    // 1. Создаём UDP-сокет
    let sock = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP) };
    if sock == usize::MAX {
        println!("socket() failed");
        return 1;
    }
    println!("socket fd = {}", sock);

    // 2. bind на 0.0.0.0:1234
    let addr = SockAddrIn::any(1234);
    let ret = unsafe {
        bind(
            sock as u32,
            &addr as *const _ as *const u8,
            size_of::<SockAddrIn>() as u32,
        )
    };
    if ret == usize::MAX {
        println!("bind() failed");
        unsafe { close(sock as u32) };
        return 1;
    }
    println!("bound to port {}", u16::from_be(addr.sin_port));

    // 3. Главный цикл: recv → echo → send
    let mut buf = [0u8; 1000];

    loop {
        let n = unsafe {
            recvfrom(
                sock as u32,
                buf.as_mut_ptr(),
                buf.len(),
            )
        };

        if n == 0 {
            // пока нет данных — крутимся (позже сделаем блокирующий recv)
            continue;
        }

        println!("recv {} {:?} bytes", n, &buf[..n]);

        // echo обратно (пока sendto без peer-адреса — упрощённая версия)
        let sent = unsafe {
            sendto(
                sock as u32,
                buf.as_ptr(),
                n,
            )
        };
        println!("sent {} bytes", sent);
    }
}
