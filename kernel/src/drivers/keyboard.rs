// KEYBOARD DRIVER
// Нормальная, высокоуровневая версия без хаков

use crate::drivers::pic::PICS;
use crate::shell::shell::SHELL;
use core::arch::asm;

// Warning! Mutable static here
// TODO: заменить на spin::Mutex или lock-free структуру
pub static mut KEYBOARD: Keyboard = Keyboard { shift: false };

pub const KEYBOARD_INT: u8 = 33;
const KEYBOARD_PORT: u16 = 0x60;

// ===================================================================
// ДВЕ ТАБЛИЦЫ ДЛЯ US QWERTY (unshifted + shifted)
// ===================================================================
const UNSHIFTED: [u8; 122] = [
    0, 0, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0, 0,
    b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', 0, 0,
    b'a', b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0,
    b'\\', b'z', b'x', b'c', b'v', b'b', b'n', b'm', b',', b'.', b'/', 0, 0, 0, b' ',
    // F1-F12, стрелки, NumPad и т.д. можно добавить позже
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
];

const SHIFTED: [u8; 122] = [
    0, 0, b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', 0, 0,
    b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', 0, 0,
    b'A', b'S', b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', b'~', 0,
    b'|', b'Z', b'X', b'C', b'V', b'B', b'N', b'M', b'<', b'>', b'?', 0, 0, 0, b' ',
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
];

// ===================================================================
// Структура клавиатуры (теперь один флаг shift для Left + Right)
// ===================================================================
pub struct Keyboard {
    shift: bool,
}

// ===================================================================
// naked-функция остаётся без изменений
// ===================================================================
#[naked]
pub extern "C" fn keyboard() {
    unsafe {
        asm!(
        "call keyboard_handler",
        "iretd",
        options(noreturn)
        );
    }
}

// ===================================================================
// Главный обработчик
// ===================================================================
#[no_mangle]
pub extern "C" fn keyboard_handler() {
    // Читаем scancode
    let scancode: u8 = unsafe {
        let mut sc: u8;
        asm!("in al, dx", out("al") sc, in("dx") KEYBOARD_PORT);
        sc
    };

    // Уведомляем PIC
    PICS.end_interrupt(KEYBOARD_INT);

    unsafe {
        match scancode {
            // Left Shift press / release
            0x2a => { KEYBOARD.shift = true; return; }
            0xaa => { KEYBOARD.shift = false; return; }

            // Right Shift press / release (добавлено для полноты)
            0x36 => { KEYBOARD.shift = true; return; }
            0xb6 => { KEYBOARD.shift = false; return; }

            // Backspace
            0x0e => {
                SHELL.backspace();
                return;
            }
            // Enter
            0x1c => {
                SHELL.enter();
                return;
            }
            _ => {}
        }
    }

    // Получаем символ с учётом Shift
    let key_byte = scancode_to_char(scancode);

    if key_byte != 0 {
        unsafe {
            SHELL.add(char::from(key_byte));
        }
    }
}

// ===================================================================
// НОВАЯ функция — супер-чистая и расширяемая
// ===================================================================
fn scancode_to_char(scancode: u8) -> u8 {
    if scancode >= 128 {
        return 0; // release-коды игнорируем
    }

    unsafe {
        if KEYBOARD.shift {
            SHIFTED[scancode as usize]
        } else {
            UNSHIFTED[scancode as usize]
        }
    }
}