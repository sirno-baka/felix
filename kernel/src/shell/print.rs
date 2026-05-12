use alloc::vec::Vec;
use core::arch::asm;
use core::fmt;

pub struct Printer {}

pub static mut PRINTER: Printer = Printer {};

// ====================== КОНВЕРТЕР UTF-8 → CP866 ======================
fn to_cp866(s: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(s.len() + 1); // +1 на всякий случай

    for c in s.chars() {
        let byte = match c {
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

            // Всё остальное ASCII — как есть
            c if c.is_ascii() => c as u8,

            // Неизвестный символ → ?
            _ => b'?',
        };
        buf.push(byte);
    }
    buf
}

// ====================== ОСНОВНОЙ ПРИНТЕР ======================
impl fmt::Write for Printer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.prints(s);
        Ok(())
    }
}

impl Printer {
    pub fn prints(&self, s: &str) {
        let bytes = to_cp866(s);           // ← вот и вся магия

        unsafe {
            let ptr = bytes.as_ptr();
            let len = bytes.len();

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
    ($($arg:tt)*) => ($crate::shell::print::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => {
        unsafe { $crate::shell::print::PRINTER.prints("\n"); }
    };

    ($($arg:tt)*) => {
        $crate::print!("{}", format_args!($($arg)*));
        unsafe { $crate::shell::print::PRINTER.prints("\n"); }
    };
}

pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        PRINTER.write_fmt(args).unwrap();
    }
}