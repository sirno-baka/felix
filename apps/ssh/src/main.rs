#![no_std]
#![no_main]

use libfelix::prelude::*;
use libfelix::net::edge_adapter::FelixStack;
use edge_nal::{TcpConnect, TcpSplit}; // TcpSplit для разделения Read/Write
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use core::str::from_utf8;

use sunset_async::{SSHClient, ProgressHolder};
use sunset::event::CliEvent;
use embassy_futures::select::{select, Either};
use embassy_futures::join::{join, join3};
use getrandom::Error;
use sunset_async::embedded_io_async::{Read, Write};
use sunset::Pty;

#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    block_on(async {
        // 1. TCP подключение
        let ip = Ipv4Addr::new(10, 0, 2, 2);
        let addr = SocketAddr::V4(SocketAddrV4::new(ip, 2222));
        let stack = FelixStack;
        let mut tcp_stream = stack.connect(addr).await.expect("TCP connect failed");

        // Разделяем поток, чтобы передать &mut Read и &mut Write в run()
        let (mut rx, mut tx) = tcp_stream.split();

        // 2. Инициализация Sunset с правильными буферами
        // new_owned() сам выделит ~32KB через аллокатор Felix (требует фичу "alloc")
        let ssh = SSHClient::new_owned();

        // Auth + session I/O must share the select with ssh.run().
        // If run() is dropped, CHANNEL_OPEN never leaves and sunset panics BadChannel(0).
        let app_fut = async {
            loop {
                let mut ph = ProgressHolder::new();
                match ssh.progress(&mut ph).await {
                    Ok(CliEvent::Authenticated) => {
                        println!("Authenticated!");
                        break;
                    }
                    Ok(CliEvent::Hostkey(k)) => {
                        match k.hostkey() {
                            Ok(pk) => println!("hostkey: {:?}", pk),
                            Err(e) => println!("hostkey peek: {:?}", e),
                        }
                        if let Err(e) = k.accept() {
                            println!("hostkey accept failed: {:?}", e);
                            return 1;
                        }
                    }
                    Ok(CliEvent::Username(u)) => {
                        u.username("linux").expect("username");
                    }
                    Ok(CliEvent::Password(p)) => {
                        p.password("password").expect("password");
                    }
                    Ok(CliEvent::Pubkey(pk)) => {
                        pk.skip().expect("pubkey skip");
                    }
                    Ok(CliEvent::AgentSign(s)) => {
                        s.skip().expect("agent skip");
                    }
                    Ok(CliEvent::PollAgain) => {
                        yield_now().await;
                    }
                    Ok(e) => {
                        println!("CliEvent({:?})", e);
                        yield_now().await;
                    }
                    Err(e) => {
                        println!("Auth error: {:?}", e);
                        return 1;
                    }
                }
            }

            // CHANNEL_OPEN is queued here; channel stays Opening until SessionOpened.
            // Reading or writing before that unwraps BadChannel(0) in sunset.
            let open_fut = ssh.open_session_pty();
            let wait_opened = async {
                loop {
                    let mut ph = ProgressHolder::new();
                    match ssh.progress(&mut ph).await {
                        Ok(CliEvent::SessionOpened(mut op)) => {
                            println!("SessionOpened");
                            let mut term = heapless::String::new();
                            let _ = term.push_str("xterm");
                            let pty = Pty {
                                term,
                                cols: 80,
                                rows: 24,
                                width: 640,
                                height: 480,
                                modes: heapless::Vec::new(),
                            };
                            if let Err(e) = op.pty(pty) {
                                println!("pty request failed: {:?}", e);
                            }
                            if let Err(e) = op.shell() {
                                println!("shell request failed: {:?}", e);
                            }
                            return;
                        }
                        Ok(CliEvent::PollAgain) | Ok(CliEvent::Authenticated) => {
                            yield_now().await;
                        }
                        Ok(e) => {
                            println!("CliEvent({:?})", e);
                            yield_now().await;
                        }
                        Err(e) => {
                            println!("progress error: {:?}", e);
                            return;
                        }
                    }
                }
            };

            let (sess_res, _) = join(open_fut, wait_opened).await;
            let session = match sess_res {
                Ok(s) => s,
                Err(e) => {
                    println!("open session failed: {:?}", e);
                    return 1;
                }
            };
            let (mut chan_in, mut chan_out) = session.split();

            let io_fut = async {
                let mut buf = [0u8; 1024];
                loop {
                    match chan_in.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(text) = from_utf8(&buf[..n]) {
                                print!("{}", text);
                            }
                        }
                        Err(e) => {
                            println!("Read error: {:?}", e);
                            break;
                        }
                    }
                }
            };
            let kb_fut = async {
                // Shell now forwards keys into our stdin pipe (fd 0).
                let _ = unsafe { libfelix::syscall::set_nonblock(0) };
                let mut buf = [0u8; 64];
                loop {
                    let n = unsafe {
                        libfelix::async_rt::async_read(0, buf.as_mut_ptr(), buf.len()).await
                    };
                    if n == 0 {
                        break;
                    }
                    if n == usize::MAX {
                        yield_now().await;
                        continue;
                    }
                    if chan_out.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            };
            let pump_fut = async {
                loop {
                    let mut stop = false;
                    {
                        let mut ph = ProgressHolder::new();
                        match ssh.progress(&mut ph).await {
                            Ok(CliEvent::SessionExit(_)) => {
                                println!("session exit");
                                stop = true;
                            }
                            Ok(CliEvent::Defunct) => stop = true,
                            Err(e) => {
                                println!("progress error: {:?}", e);
                                stop = true;
                            }
                            _ => {}
                        }
                    }
                    if stop {
                        break;
                    }
                    yield_now().await;
                }
            };
            join3(io_fut, kb_fut, pump_fut).await;
            0
        };

        let net_fut = ssh.run(&mut rx, &mut tx);

        match select(app_fut, net_fut).await {
            Either::First(code) => code,
            Either::Second(res) => {
                println!("SSH transport closed: {:?}", res);
                1
            }
        }
    })
}


const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;

/// WM key → bytes for a remote PTY.
fn map_key(scancode: u8, ch: u8, mods: u8) -> ([u8; 8], usize) {
    let ctrl = (mods & 2) != 0;
    if ch == 0x03 || (scancode == 0x2e && ctrl) {
        return ([0x03, 0, 0, 0, 0, 0, 0, 0], 1);
    }
    if ctrl && ch >= b'a' && ch <= b'z' {
        return ([ch - b'a' + 1, 0, 0, 0, 0, 0, 0, 0], 1);
    }
    match scancode {
        SCAN_ENTER => ([b'\r', 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_BACKSPACE => ([0x7f, 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_TAB => ([b'\t', 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_ESC => ([0x1b, 0, 0, 0, 0, 0, 0, 0], 1),
        SCAN_UP => ([0x1b, b'[', b'A', 0, 0, 0, 0, 0], 3),
        SCAN_DOWN => ([0x1b, b'[', b'B', 0, 0, 0, 0, 0], 3),
        SCAN_RIGHT => ([0x1b, b'[', b'C', 0, 0, 0, 0, 0], 3),
        SCAN_LEFT => ([0x1b, b'[', b'D', 0, 0, 0, 0, 0], 3),
        _ if ch >= 0x20 && ch < 0x7f => ([ch, 0, 0, 0, 0, 0, 0, 0], 1),
        _ => ([0; 8], 0),
    }
}

fn felix_entropy(buf: &mut [u8]) -> Result<(), Error> {
    // Вариант А: Если в ядре Felix есть syscall для RNG, используйте его:
    // unsafe { libfelix::syscall::getrandom(buf.as_mut_ptr(), buf.len()) };
    // return Ok(());

    // Вариант Б: Fallback через RDTSC (x86) и псевдо-шум
    #[cfg(target_arch = "x86")]
    unsafe {
        // Простой LCG (Linear Congruential Generator) на базе TSC
        // ВНИМАНИЕ: Для продакшена лучше собирать энтропию из событий мыши/клавиатуры
        // или добавить драйвер аппаратного RNG в ядро Felix.
        let mut lo: u32;
        let mut hi: u32;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
        let mut state = ((hi as u64) << 32) | (lo as u64);
        state ^= buf.as_ptr() as u64;
        for byte in buf.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *byte = (state >> 32) as u8;
        }
    }

    #[cfg(not(target_arch = "x86"))]
    {
        // Заглушка для других архитектур
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = i as u8;
        }
    }

    Ok(())
}

#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), Error> {
    let buf = unsafe {
        // fill the buffer with zeros
        core::ptr::write_bytes(dest, 0, len);
        // create mutable byte slice
        core::slice::from_raw_parts_mut(dest, len)
    };
    felix_entropy(buf)
}