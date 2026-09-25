// PRINTER + independent panic log ring (klog).
//
// klog is a lock-free (cli-only) ring of recent lines so fb_panic / exceptions
// can dump history even when PRINTER's Mutex is held by the faulting context.

use crate::sync::mutex::Mutex;
use crate::time::get_timestamp;
use alloc::fmt::format;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::fmt;
use core::fmt::Write;
use interrupt_sync::InterruptSpinMutex;

// Keep the whole pre-STI boot log in memory. 4096 * 150 bytes is ~600 KiB,
// large enough for verbose PCI/USB/network probing while still remaining a
// fixed panic-safe buffer (no allocation in the print path).
pub const LOG_LINES: usize = 8192;
pub const LOG_WIDTH: usize = 150;

// ---------------------------------------------------------------------------
// klog — independent of PRINTER lock
// ---------------------------------------------------------------------------

struct Klog {
    lines: [[u8; LOG_WIDTH]; LOG_LINES],
    lens: [u8; LOG_LINES],
    head: usize,
    count: usize,
    cur: [u8; LOG_WIDTH],
    cur_len: u8,
}

const fn klog_new() -> Klog {
    Klog {
        lines: [[0; LOG_WIDTH]; LOG_LINES],
        lens: [0; LOG_LINES],
        head: 0,
        count: 0,
        cur: [0; LOG_WIDTH],
        cur_len: 0,
    }
}

/// Independent SMP-safe ring. The lock also keeps local IRQs disabled for
/// the whole critical section, so IRQ logging cannot re-enter on the same CPU.
static KLOG: InterruptSpinMutex<Klog> = InterruptSpinMutex::new(klog_new());

fn klog_feed_char(k: &mut Klog, c: char) {
    if c == '\n' {
        klog_commit_line(k);
        return;
    }
    if c == '\r' {
        k.cur_len = 0;
        return;
    }
    if c == '\x08' {
        if k.cur_len > 0 {
            k.cur_len -= 1;
        }
        return;
    }
    if !c.is_ascii() {
        return;
    }
    let b = c as u8;
    if k.cur_len as usize >= LOG_WIDTH {
        klog_commit_line(k);
    }
    if (k.cur_len as usize) < LOG_WIDTH {
        k.cur[k.cur_len as usize] = b;
        k.cur_len += 1;
    }
}

fn klog_commit_line(k: &mut Klog) {
    let i = k.head % LOG_LINES;
    let n = k.cur_len as usize;
    k.lines[i] = [0; LOG_WIDTH];
    k.lines[i][..n].copy_from_slice(&k.cur[..n]);
    k.lens[i] = k.cur_len;
    k.head = (k.head + 1) % LOG_LINES;
    if k.count < LOG_LINES {
        k.count += 1;
    }
    k.cur_len = 0;
}

/// Append string to klog. Never wait here: panic/exception logging may
/// re-enter after a fault in a context that was already writing the log.
pub fn klog_write_str(s: &str) {
    let Some(mut k) = KLOG.try_lock() else {
        return;
    };
    for c in s.chars() {
        klog_feed_char(&mut k, c);
    }
}

/// Oldest → newest completed lines, then incomplete current line.
/// Does not take PRINTER lock.
pub fn klog_for_each_line(mut f: impl FnMut(&[u8])) {
    let Some(k) = KLOG.try_lock() else {
        return;
    };
    let count = k.count;
    if count == 0 && k.cur_len == 0 {
        return;
    }
    let start = if count < LOG_LINES {
        0
    } else {
        k.head % LOG_LINES
    };
    for idx in 0..count {
        let i = (start + idx) % LOG_LINES;
        let len = k.lens[i] as usize;
        f(&k.lines[i][..len]);
    }
    if k.cur_len > 0 {
        f(&k.cur[..k.cur_len as usize]);
    }
}

/// Up to the newest 80 completed lines, then the incomplete current line.
/// Does not take PRINTER lock.
pub fn klog_for_each_line_last(mut f: impl FnMut(&[u8])) {
    let Some(k) = KLOG.try_lock() else {
        return;
    };
    let count = k.count;
    if count == 0 && k.cur_len == 0 {
        return;
    }
    let shown = count.min(50);
    let oldest = if count < LOG_LINES {
        0
    } else {
        k.head % LOG_LINES
    };
    let start = (oldest + count - shown) % LOG_LINES;
    for idx in 0..shown {
        let i = (start + idx) % LOG_LINES;
        let len = k.lens[i] as usize;
        f(&k.lines[i][..len]);
    }
    if k.cur_len > 0 {
        f(&k.cur[..k.cur_len as usize]);
    }
}

/// Copy the current kernel log oldest -> newest, adding a newline after every
/// stored line. Intended for the one-shot pre-STI dump to /var/system.log.
/// Allocation happens only here, never in the normal print/panic path.
pub fn klog_snapshot() -> Vec<u8> {
    let mut out = Vec::new();
    klog_for_each_line(|line| {
        out.extend_from_slice(line);
        out.push(b'\n');
    });
    out
}

// ---------------------------------------------------------------------------
// VGA text printer (best-effort; may be locked)
// ---------------------------------------------------------------------------

pub const fn printer_new() -> Printer {
    Printer {
        x: 0,
        y: 0,
        foreground: 0x7,
        background: 0,
    }
}

pub static PRINTER: Mutex<Printer> = Mutex::new(printer_new());

const WIDTH: u16 = 80;
const HEIGHT: u16 = 25;
const VGA_START: u32 = 0xC00B_8000;

pub struct Printer {
    x: u16,
    y: u16,
    foreground: u8,
    background: u8,
}

impl Printer {
    pub fn printc(&mut self, c: char) {
        if c == '\n' {
            self.new_line();
            return;
        }
        if c == '\x08' {
            if self.x > 0 {
                self.x -= 1;
                let target = (VGA_START + ((self.y * WIDTH + self.x) * 2) as u32) as *mut u8;
                unsafe {
                    *target = b' ';
                    let color = self.background << 4 | self.foreground;
                    *target.byte_add(1) = color;
                }
                self.set_cursor_position();
            }
            return;
        }
        if c == '\r' {
            self.x = 0;
            self.set_cursor_position();
            return;
        }

        let target = (VGA_START + ((self.y * WIDTH + self.x) * 2) as u32) as *mut u8;
        unsafe {
            if self.y == HEIGHT {
                self.y -= 1;
                self.scroll();
                self.set_cursor_position();
            }
            *target = c as u8;
            let color = self.background << 4 | self.foreground;
            *target.byte_add(1) = color;
            self.x += 1;
            if self.x > WIDTH {
                self.x = 0;
                self.y += 1;
            }
        }
    }

    pub fn prints(&mut self, s: &str) {
        let cursor = self.get_cursor_position();
        self.x = cursor.0;
        self.y = cursor.1;
        for c in s.chars() {
            self.printc(c);
        }
        self.set_cursor_position();
    }

    pub fn delete(&mut self) {
        self.printc('\x08');
    }

    pub fn get_cursor_position(&self) -> (u16, u16) {
        let mut index: u16 = 0;
        unsafe {
            asm!("out dx, al", in("dx") 0x3d4 as u16, in("al") 0x0f as u8);
            let mut a: u8;
            asm!("in al, dx", out("al") a, in("dx") 0x3d5);
            index |= a as u16;
            asm!("out dx, al", in("dx") 0x3d4 as u16, in("al") 0x0e as u8);
            let b: u8;
            asm!("in al, dx", out("al") b, in("dx") 0x3d5);
            index |= (b as u16) << 8;
        }
        (index % WIDTH, index / WIDTH)
    }

    pub fn set_cursor_position(&self) {
        let index: u16 = self.y * WIDTH + self.x;
        unsafe {
            asm!("out dx, al", in("dx") 0x3d4 as u16, in("al") 0x0f as u8);
            asm!("out dx, al", in("dx") 0x3d5 as u16, in("al") (index & 0xff) as u8);
            asm!("out dx, al", in("dx") 0x3d4 as u16, in("al") 0x0e as u8);
            asm!("out dx, al", in("dx") 0x3d5 as u16, in("al") ((index >> 8) & 0xff) as u8);
        }
    }

    pub fn scroll(&mut self) {
        for a in 0..24 {
            for i in 0..80 {
                let pos = (a * 80 + i) as u32;
                let new = (VGA_START + pos * 2) as *mut u8;
                let old = (VGA_START + (pos + 80) * 2) as *const u8;
                unsafe {
                    *new = *old;
                    *new.byte_add(1) = *old.byte_add(1);
                }
            }
        }
        let last_line_start = (VGA_START + 24 * 80 * 2) as *mut u8;
        for i in 0..80 {
            unsafe {
                *last_line_start.add(i * 2) = b' ';
                *last_line_start.add(i * 2 + 1) = (self.background << 4) | self.foreground;
            }
        }
    }

    pub fn set_colors(&mut self, foreground: u8, background: u8) {
        self.foreground = foreground;
        self.background = background;
    }

    pub fn reset_colors(&mut self) {
        self.foreground = 0x7;
        self.background = 0;
    }

    pub fn new_line(&mut self) {
        self.x = 0;
        self.y += 1;
        if self.y >= HEIGHT {
            self.y = HEIGHT - 1;
            self.scroll();
        }
        self.set_cursor_position();
    }
}

impl fmt::Write for Printer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.prints(s);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fan-out writer: klog + E9 always; VGA if PRINTER is free
// ---------------------------------------------------------------------------

struct FanoutWriter<'a> {
    printer: Option<&'a mut Printer>,
}

impl Write for FanoutWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // 1) Independent ring (panic-safe)
        klog_write_str(s);
        // 2) QEMU/Bochs debug port (no lock)
        for byte in s.bytes() {
            e9_write_byte(byte);
        }
        // 3) Best-effort VGA text
        if let Some(p) = self.printer.as_mut() {
            p.prints(s);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::print::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print::print!("{} | \n", $crate::time::get_timestamp().as_f64());
    };
    ($($arg:tt)*) => {{
        $crate::print!("{} | {}\n", $crate::time::get_timestamp().as_f64(), format_args!($($arg)*));
    }};
}

pub fn _print(args: fmt::Arguments) {
    // Prefer non-blocking lock so a nested println (or IRQ) never sleeps on
    // PRINTER. klog + E9 still get the message either way.
    if let Some(mut p) = PRINTER.try_lock_nb() {
        let _ = FanoutWriter {
            printer: Some(&mut *p),
        }
        .write_fmt(args);
    } else {
        let _ = FanoutWriter { printer: None }.write_fmt(args);
    }
}

#[inline]
fn e9_write_byte(byte: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") 0xe9u16,
            in("al") byte,
            options(nostack, preserves_flags)
        );
    }
}

struct E9Writer;

impl Write for E9Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            e9_write_byte(byte);
        }
        Ok(())
    }
}

#[doc(hidden)]
pub fn _printd(args: fmt::Arguments) {
    // debug/debugln: E9 + klog, never touch PRINTER
    struct Dbg;
    impl Write for Dbg {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            klog_write_str(s);
            for byte in s.bytes() {
                e9_write_byte(byte);
            }
            Ok(())
        }
    }
    let _ = Dbg.write_fmt(args);
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::print::_printd(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! debugln {
    () => {
        $crate::debug!("\n")
    };
    ($($arg:tt)*) => {
        $crate::debug!("{}\n", format_args!($($arg)*))
    };
}
