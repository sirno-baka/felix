// Printer → stdout (fd 1)
use crate::syscall::write;
use core::fmt;

pub const fn printer_new() -> Printer {
    Printer {}
}

pub static mut PRINTER: Printer = printer_new();

pub struct Printer {}

impl Printer {
    pub fn prints(&mut self, s: &str) {
        // fd 1 = stdout (kernel maps both 0 and 1 to VGA console)
        unsafe {
            write(1, s.as_ptr(), s.len());
        }
    }
}

impl fmt::Write for Printer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.prints(s);
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
        $crate::print::print!("\n")
    };
    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*))
    }};
}

pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        let _ = PRINTER.write_fmt(args);
    }
}
