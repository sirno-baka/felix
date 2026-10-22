#![no_std]
#![no_main]

extern crate alloc;

mod terminal;

use core::cmp::max;

use libfelix::embedded_graphics::prelude::Point;
use libfelix::prelude::*;
use libfelix::syscall::{
    close, kill, openpty, read, set_nonblock, spawn_path_env_pgid, tcsetwinsize, waitpid_status,
    write, WinSize, SIGTERM, WNOHANG,
};
use terminal::{Terminal, CELL_H, CELL_W};

const TERM_PAD: i32 = 8;

const SCAN_ESC: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_C: u8 = 0x2E;
const SCAN_Z: u8 = 0x2C;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PGUP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PGDN: u8 = 0x51;
const SCAN_DELETE: u8 = 0x53;

fn terminal_size(win: &Window) -> (usize, usize) {
    let w = win.client_width() as i32;
    let h = win.client_height() as i32;
    let cols = max(1, (w - TERM_PAD * 2) / CELL_W) as usize;
    let rows = max(1, (h - TERM_PAD * 2) / CELL_H) as usize;
    (cols, rows)
}

fn update_pty_size(master: u32, win: &Window, cols: usize, rows: usize) {
    let size = WinSize {
        rows: rows.min(u16::MAX as usize) as u16,
        cols: cols.min(u16::MAX as usize) as u16,
        xpixel: win.client_width().min(u16::MAX as u32) as u16,
        ypixel: win.client_height().min(u16::MAX as u32) as u16,
    };
    unsafe {
        let _ = tcsetwinsize(master, &size);
    }
}

fn draw(win: &mut Window, term: &Terminal) {
    term.draw(win, Point::new(TERM_PAD, TERM_PAD));
    let _ = win.flip();
}

fn key_bytes(scancode: u8, ch: u8, mods: u8) -> ([u8; 8], usize) {
    let ctrl = (mods & 2) != 0;

    if ctrl {
        let lower = if ch.is_ascii_uppercase() { ch + 32 } else { ch };
        if lower >= b'a' && lower <= b'z' {
            return ([lower - b'a' + 1, 0, 0, 0, 0, 0, 0, 0], 1);
        }
        if scancode == SCAN_C {
            return ([0x03, 0, 0, 0, 0, 0, 0, 0], 1);
        }
        if scancode == SCAN_Z {
            return ([0x1a, 0, 0, 0, 0, 0, 0, 0], 1);
        }
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
        SCAN_HOME => ([0x1b, b'[', b'H', 0, 0, 0, 0, 0], 3),
        SCAN_END => ([0x1b, b'[', b'F', 0, 0, 0, 0, 0], 3),
        SCAN_DELETE => ([0x1b, b'[', b'3', b'~', 0, 0, 0, 0], 4),
        _ if ch >= 0x20 && ch != 0x7f => ([ch, 0, 0, 0, 0, 0, 0, 0], 1),
        _ => ([0; 8], 0),
    }
}

fn spawn_shell(slave: u32) -> Option<i32> {
    static PATH: &[u8] = b"/bin/shell\0";
    static ARG0: &[u8] = b"/bin/shell\0";
    static ENV_PATH: &[u8] = b"PATH=/bin:.\0";
    static ENV_HOME: &[u8] = b"HOME=/home/user\0";
    static ENV_TERM: &[u8] = b"TERM=felix\0";
    static ENV_SHELL: &[u8] = b"SHELL=/bin/shell\0";

    let argv = [ARG0.as_ptr()];
    let envp = [
        ENV_PATH.as_ptr(),
        ENV_HOME.as_ptr(),
        ENV_TERM.as_ptr(),
        ENV_SHELL.as_ptr(),
    ];

    let pid = unsafe {
        spawn_path_env_pgid(
            PATH.as_ptr(),
            slave as i32,
            slave as i32,
            slave as i32,
            &argv,
            &envp,
            0,
            true,
        )
    };
    (pid != usize::MAX).then_some(pid as i32)
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut win = Window::create(30, 30, 640, 400, "Felix Terminal").unwrap_or_else(|| {
        Window::create(40, 40, 480, 320, "Felix Terminal").expect("wm_create failed")
    });

    let (cols, rows) = terminal_size(&win);
    let mut term = Terminal::new(cols, rows);

    let mut pty = [0u32; 2];
    if unsafe { openpty(pty.as_mut_ptr()) } == usize::MAX {
        term.process(b"terminal: openpty failed\r\n");
        draw(&mut win, &term);
        return 1;
    }

    let master = pty[0];
    let slave = pty[1];
    update_pty_size(master, &win, cols, rows);
    let _ = unsafe { set_nonblock(master) };

    let Some(shell_pid) = spawn_shell(slave) else {
        unsafe {
            close(slave);
            close(master);
        }
        term.process(b"terminal: cannot spawn /bin/shell\r\n");
        draw(&mut win, &term);
        return 1;
    };
    println!("terminal: spawned /bin/shell pid={}", shell_pid);
    unsafe {
        close(slave);
    }

    let mut dirty = true;
    let mut out = [0u8; 2048];

    loop {
        loop {
            let n = unsafe { read(master, out.as_mut_ptr(), out.len()) };
            if n == 0 || n == usize::MAX {
                break;
            }
            term.process(&out[..n]);
            dirty = true;
        }

        let mut status = 0i32;
        let waited = unsafe { waitpid_status(shell_pid, &mut status, WNOHANG) };
        if waited == shell_pid as usize || waited == usize::MAX {
            break;
        }

        let mut evbuf = [WmEvent::default(); 64];
        let n = win.poll_events(&mut evbuf);
        for e in &evbuf[..n] {
            if e.kind == EV_RESIZE {
                let (cols, rows) = terminal_size(&win);
                term.resize(cols, rows);
                update_pty_size(master, &win, cols, rows);
                dirty = true;
                continue;
            }
            if e.kind != EV_KEY_DOWN {
                continue;
            }

            let scancode = e.a as u8;
            if scancode == SCAN_PGUP {
                term.scroll(8);
                dirty = true;
                continue;
            }
            if scancode == SCAN_PGDN {
                term.scroll(-8);
                dirty = true;
                continue;
            }

            let (bytes, count) = key_bytes(scancode, e.b as u8, e.c as u8);
            if count != 0 {
                unsafe {
                    let _ = write(master, bytes.as_ptr(), count);
                }
            }
        }

        if dirty {
            draw(&mut win, &term);
            dirty = false;
        } else {
            unsafe {
                libfelix::syscall::sys_sleep(10);
            }
        }
    }

    unsafe {
        let _ = kill(shell_pid, SIGTERM);
        close(master);
    }
    0
}
