
use core::ops::Deref;
use core::sync::atomic::AtomicU8;
use crate::utils::flags::{FlagOp, Flags};

pub struct WindowFlags(Flags<AtomicU8>);

impl WindowFlags {
    pub const WINDOW_TITLE_HINT: u8 = 0;
    pub const WINDOW_CLOSE_BUTTON_HINT: u8 = 1;
    pub const WINDOW_FULLSCREEN_BUTTON_HINT: u8 = 2;
    pub const FRAMELESS_WINDOW_HINT: u8 = 3;

    pub fn new() -> Self {
        let flags = Flags::new(0);
        flags.enable(Self::WINDOW_CLOSE_BUTTON_HINT);
        flags.enable(Self::WINDOW_TITLE_HINT);
        Self(flags)
    }

    pub fn from_base(base: u8) -> Self {
        Self(Flags::new(base))
    }

    // Если нужны методы as_ptr / from_ptr из прошлого шага, продублируйте их здесь:
    pub fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }

    pub unsafe fn from_ptr(ptr: *const u8) -> &'static Self {
        // Безопасно, так как WindowFlags - это transparent обёртка над Flags
        &*(ptr as *const Self)
    }
}

// Делегируем вызовы к внутреннему Flags<AtomicU8>
impl Deref for WindowFlags {
    type Target = Flags<AtomicU8>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}