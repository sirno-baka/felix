// KEYBOARD DRIVER
// Нормальная, высокоуровневая версия без хаков

use crate::drivers::pic::PICS;
use core::arch::asm;
use crate::drivers::keyboard_buffer::KEYBOARD_BUFFER;
use crate::println;

// Warning! Mutable static here
// TODO: заменить на spin::Mutex или lock-free структуру
pub static mut KEYBOARD: Keyboard = Keyboard { shift: false, ctrl: false };

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
    ctrl: bool,
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
// ИСПРАВЛЕННАЯ версия — правильное сопоставление сканкодов
// ===================================================================
fn scancode_to_char(scancode: u8, shift: bool) -> u8 {
    if scancode >= 128 {
        return 0; // release-коды игнорируем
    }

    let index = match scancode {
        // Пробел
        0x39 => return b' ',
        // Backspace
        0x0e => return 0x08,
        //enter
        0x1c => return b'\n',
        // Цифры и символы в верхнем ряду
        0x02..=0x0d => (scancode - 0) as usize,      // 1..0
        // Буквы
        0x10..=0x21 => (scancode - 0) as usize,      // q..p
        0x1e..=0x30 => (scancode - 0) as usize,     // a..l
        0x2c..=0x35 => (scancode - 0) as usize,     // z..m



        _ => return 0,
    };

    if index >= UNSHIFTED.len() {
        return 0;
    }

    if shift {
        SHIFTED[index]
    } else {
        UNSHIFTED[index]
    }
}

// ===================================================================
// Главный обработчик (исправленный)
// ===================================================================
#[no_mangle]
pub extern "C" fn keyboard_handler() {
    let scancode: u8 = unsafe {
        let mut sc: u8;
        asm!("in al, dx", out("al") sc, in("dx") KEYBOARD_PORT);
        sc
    };

    // Modifier keys (press / release)
    unsafe {
        match scancode {
            // Left/Right Shift
            0x2a | 0x36 => { KEYBOARD.shift = true; }
            0xaa | 0xb6 => { KEYBOARD.shift = false; }
            // Left Ctrl (Right Ctrl is E0 1D — ignored for now)
            0x1d => { KEYBOARD.ctrl = true; }
            0x9d => { KEYBOARD.ctrl = false; }
            _ => {}
        }
    }

    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7F;

    // Ctrl+C → ETX to focused window (shell kills its own children).
    if code == 0x2e && unsafe { KEYBOARD.ctrl } && !released {
        match &mut *KEYBOARD_BUFFER.lock() {
            Some(buffer) => buffer.push(0x03),
            None => {}
        }
        let mods = 2u8;
        crate::drivers::wm::push_key(true, code, 0x03, mods);
        PICS.end_interrupt(KEYBOARD_INT);
        return;
    }

    let key_byte = if released {
        0
    } else {
        scancode_to_char(code, unsafe { KEYBOARD.shift })
    };

    // Suppress ordinary characters while Ctrl is held (except handled above)
    if key_byte != 0 && !unsafe { KEYBOARD.ctrl } {
        match &mut *KEYBOARD_BUFFER.lock() {
            Some(buffer) => buffer.push(key_byte),
            None => {}
        }
    }

    // Window events (focused window)
    let mods = (if unsafe { KEYBOARD.shift } { 1 } else { 0 })
        | (if unsafe { KEYBOARD.ctrl } { 2 } else { 0 });
    // Skip pure modifier scancodes for KeyDown/Up noise reduction? still useful.
    match scancode {
        0x2a | 0x36 | 0xaa | 0xb6 | 0x1d | 0x9d => {}
        _ => {
            // While Ctrl held, don't inject the underlying letter into WM either
            let ch = if unsafe { KEYBOARD.ctrl } { 0 } else { key_byte };
            crate::drivers::wm::push_key(!released, code, ch, mods);
        }
    }

    // EOI в самом конце
    PICS.end_interrupt(KEYBOARD_INT);
}
