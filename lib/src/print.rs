use alloc::vec::Vec;
use core::arch::asm;
use core::fmt;

pub struct Printer {}

pub static mut PRINTER: Printer = Printer {};

// ====================== КОНВЕРТЕР UTF-8 → CP866 ======================


// ====================== ОСНОВНОЙ ПРИНТЕР ======================
impl fmt::Write for Printer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.prints(s);
        Ok(())
    }
}

impl Printer {
    pub fn prints(&self, s: &str) {

        unsafe {
            let ptr = s.as_ptr();
            let len = s.len();

            asm!(
            "push eax",
            "push ebx",
            "push ecx",
            "int 0x80",
            "pop ecx",
            "pop ebx",
            "pop eax",
            in("eax") 0,
            in("ebx") ptr as u32,
            in("ecx") len as u32,
            );
        }
    }
}

// ====================== МАКРОСЫ ======================
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::print::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => {
        unsafe { $crate::print::PRINTER.prints("\n"); }
    };

    ($($arg:tt)*) => {
        $crate::print!("{}", format_args!($($arg)*));
        unsafe { $crate::print::PRINTER.prints("\n"); }
    };
}

pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        PRINTER.write_fmt(args).unwrap();
    }
}