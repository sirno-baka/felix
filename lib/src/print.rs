use core::arch::asm;
use core::fmt;

pub struct Printer {}

pub static mut PRINTER: Printer = Printer {};

// ====================== КОНВЕРТЕР UTF-8 → CP866 (без аллокации) ======================
fn char_to_cp866(c: char) -> u8 {
    match c {
        // Основная кириллица
        'А'..='Я' => (c as u32 - 'А' as u32 + 0x80) as u8,
        'а'..='п' => (c as u32 - 'а' as u32 + 0xA0) as u8,
        'р'..='я' => (c as u32 - 'р' as u32 + 0xE0) as u8,

        // Ё / ё
        'Ё' => 0xF0,
        'ё' => 0xF1,

        // Управляющие символы
        '\n' => b'\n',
        '\r' => b'\r',
        '\t' => b'\t',

        // ASCII как есть
        c if c.is_ascii() => c as u8,

        // Неизвестный символ → ?
        _ => b'?',
    }
}

impl fmt::Write for Printer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        unsafe { self.prints(s); }
        Ok(())
    }
}

impl Printer {
    // Печатаем один байт через syscall (самый безопасный способ)
    fn print_byte(&self, byte: u8) {
        unsafe {
            let ptr = &byte as *const u8;
            crate::syscall::write(1, ptr, 1);   // fd = 1 = stdout
        }
    }

    pub unsafe fn prints(&self, s: &str) {
        let ptr = s.as_ptr();

        crate::syscall::write(1, ptr, s.len());
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