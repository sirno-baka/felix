//PRINTER
//Manages text output by directly writing to VGA video memory

use core::arch::asm;
use core::fmt;
use crate::syscall::write;

pub const fn printer_new() -> Printer {
    Printer {
       
    }
}

pub static mut PRINTER: Printer = printer_new();


pub struct Printer {

}

impl Printer {
    //print a string by printing one char at the time
    pub fn prints(&mut self, s: &str) {
        //set coords to current cursor position
        unsafe {
            write(0, s.as_ptr(), s.len());
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
        $crate::print::print!("\n");
    };

    ($($arg:tt)*) => {{
        $crate::print!("{}\n", format_args!($($arg)*));
    }};
}

pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        PRINTER.write_fmt(args).unwrap();
    }


}