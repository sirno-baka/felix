//PRINTER
//Manages text output by directly writing to VGA video memory

use core::arch::asm;
use core::fmt;
use core::fmt::Write;
use interrupt_sync::InterruptSpinMutex;
use crate::sync::mutex::Mutex;

pub const LOG_LINES: usize = 20;
pub const LOG_WIDTH: usize = 80;

pub const fn printer_new() -> Printer {
    Printer {
        x: 0,
        y: 0,
        foreground: 0x7,
        background: 0,
        log_lines: [[0; LOG_WIDTH]; LOG_LINES],
        log_lens: [0; LOG_LINES],
        log_head: 0,
        log_count: 0,
        cur: [0; LOG_WIDTH],
        cur_len: 0,
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
    /// Ring of the last `LOG_LINES` completed lines (no trailing '\n').
    log_lines: [[u8; LOG_WIDTH]; LOG_LINES],
    log_lens: [u8; LOG_LINES],
    /// Next slot to overwrite.
    log_head: u8,
    log_count: u8,
    /// Current incomplete line being built.
    cur: [u8; LOG_WIDTH],
    cur_len: u8,
}

impl Printer {
    //copy given char to memory pointed to vga_pointer
    pub fn printc(&mut self, c: char) {
        // Always mirror to QEMU debug port
        unsafe {
            asm!("out dx, al", in("dx") 0xe9 as u16, in("al") c as u8);
        }

        // Feed the panic log ring (independent of VGA).
        self.log_feed(c);

        if c == '\n' {
            self.new_line();
            return;
        }

        // Backspace: move left, erase character, stay there
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

        // Carriage return
        if c == '\r' {
            self.x = 0;
            self.set_cursor_position();
            return;
        }

        //calculate target from coords
        let target = (VGA_START + ((self.y * WIDTH + self.x) * 2) as u32) as *mut u8;

        unsafe {
            if self.y == HEIGHT {
                self.y -= 1;
                self.scroll();
                self.set_cursor_position();
            }

            //copy char byte to target
            *target = c as u8;

            //calculate color byte and move it to target + 1 byte
            let color = self.background << 4 | self.foreground;
            *target.byte_add(1) = color;

            //increment x coord
            self.x += 1;

            //if x coord overflow go to new line
            if self.x > WIDTH {
                self.x = 0;
                self.y += 1;
            }
        }
    }

    //print a string by printing one char at the time
    pub fn prints(&mut self, s: &str) {
        //set coords to current cursor position
        let cursor = self.get_cursor_position();
        self.x = cursor.0;
        self.y = cursor.1;

        for c in s.chars() {
            self.printc(c);
        }

        //set cursors position to new coords
        self.set_cursor_position();
    }

    pub fn delete(&mut self) {
        // Re-use the same logic as printc('\x08')
        self.printc('\x08');
    }

    //get cursor position directly talking to vga hardware
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

        let x: u16 = index % WIDTH;
        let y: u16 = index / WIDTH;

        (x, y)
    }

    //set cursor position directly talking to vga hardware
    pub fn set_cursor_position(&self) {
        let index: u16 = self.y * WIDTH + self.x;

        unsafe {
            asm!("out dx, al", in("dx") 0x3d4 as u16, in("al") 0x0f as u8);
            asm!("out dx, al", in("dx") 0x3d5 as u16, in("al") (index & 0xff) as u8);
            asm!("out dx, al", in("dx") 0x3d4 as u16, in("al") 0x0e as u8);
            asm!("out dx, al", in("dx") 0x3d5 as u16, in("al") ((index >> 8) & 0xff) as u8);
        }
    }

    //copy content of each row to upper row
    pub fn scroll(&mut self) {
        // Сдвигаем строки 1..24 → 0..23 (правильно!)
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

        // Очищаем последнюю (24-ю) строку
        let last_line_start = (VGA_START + 24 * 80 * 2) as *mut u8;
        for i in 0..80 {
            unsafe {
                *last_line_start.add(i * 2) = b' ';                    // пробел
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

    fn log_feed(&mut self, c: char) {
        if c == '\n' {
            self.log_commit_line();
            return;
        }
        if c == '\r' {
            self.cur_len = 0;
            return;
        }
        if c == '\x08' {
            if self.cur_len > 0 {
                self.cur_len -= 1;
            }
            return;
        }
        if !c.is_ascii() {
            return;
        }
        let b = c as u8;
        if self.cur_len as usize >= LOG_WIDTH {
            // Soft-wrap: commit full line and continue.
            self.log_commit_line();
        }
        if (self.cur_len as usize) < LOG_WIDTH {
            self.cur[self.cur_len as usize] = b;
            self.cur_len += 1;
        }
    }

    fn log_commit_line(&mut self) {
        let i = self.log_head as usize % LOG_LINES;
        let n = self.cur_len as usize;
        self.log_lines[i] = [0; LOG_WIDTH];
        self.log_lines[i][..n].copy_from_slice(&self.cur[..n]);
        self.log_lens[i] = self.cur_len;
        self.log_head = self.log_head.wrapping_add(1);
        if (self.log_count as usize) < LOG_LINES {
            self.log_count += 1;
        }
        self.cur_len = 0;
    }

    /// Oldest → newest completed lines, then the incomplete current line if any.
    pub fn for_each_log_line(&self, mut f: impl FnMut(&[u8])) {
        let count = self.log_count as usize;
        if count == 0 && self.cur_len == 0 {
            return;
        }
        let start = if count < LOG_LINES {
            0
        } else {
            self.log_head as usize % LOG_LINES
        };
        for k in 0..count {
            let i = (start + k) % LOG_LINES;
            let len = self.log_lens[i] as usize;
            f(&self.log_lines[i][..len]);
        }
        if self.cur_len > 0 {
            f(&self.cur[..self.cur_len as usize]);
        }
    }
}
impl fmt::Write for Printer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.prints(s);
        // for byte in s.bytes() {
        //     e9_write_byte(byte);
        // }
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
        $crate::print::print!("\n");
    };

    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*));
    }};
}

pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;

    // After VESA / WM: no VGA text, no kernel FB console — logs go to E9 only.
    // Userspace apps create windows and draw themselves.
    let mut p = super::PRINTER.lock();
    p.write_fmt(args).unwrap();
}

/// Пишет один байт в debug-port 0xE9 (QEMU/Bochs)
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

/// Writer, который реализует `core::fmt::Write`
struct E9Writer;

impl Write for E9Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            e9_write_byte(byte);
        }
        Ok(())
    }
}

/// Низкоуровневая функция, которую вызывает макрос
#[doc(hidden)]
pub fn _printd(args: fmt::Arguments) {
    let mut writer = E9Writer;
    let _ = writer.write_fmt(args);
}

/// Макрос `print!`
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::print::_printd(format_args!($($arg)*))
    };
}

/// Макрос `println!`
#[macro_export]
macro_rules! debugln {
    () => {
        $crate::debug!("\n")
    };
    ($($arg:tt)*) => {
        $crate::debug!("{}\n", format_args!($($arg)*))
    };
}