use core::fmt;
use core::sync::atomic::{AtomicU8, Ordering};

#[repr(transparent)]
#[derive(Default, Clone, Copy)]
pub struct Flags<T>(pub T);

/// To use this trait the type T has to allow interior mutability
/// This is ok in the example since AtomicU8 is thread safe and allow interior mutability
pub trait FlagOp<T> {
    fn toggle(&self, bit: u8);
    fn enable(&self, bit: u8);
    fn disable(&self, bit: u8);
    fn is_enable(&self, bit: u8) -> bool;
    fn is_disable(&self, bit: u8) -> bool;
    fn as_ptr(&self) -> *const u8;
    unsafe fn from_ptr<'a>(ptr: *const u8) -> &'a Self;
}

impl FlagOp<AtomicU8> for Flags<AtomicU8> {
    /// Toggle bit.
    ///
    /// # Safety
    /// No check performed, will produce panic if trying to toggle bit outside of range
    ///
    /// # Examples
    ///
    /// ```
    /// let flag = Flags::<AtomicU8>::default();
    /// flag.toggle(1); // Toggle the first bit of the inner AtomicU8
    /// flag.toggle(9); // Panic there is no 9th bit in an u8
    /// ```
    fn toggle(&self, bit: u8) {
        self.0.fetch_xor(1 << bit, Ordering::Relaxed);
    }

    /// enable x bit.
    /// # Safety
    /// No check performed, will produce panic if trying to enable bit outside of range
    ///
    /// # Examples
    ///
    /// ```
    /// let flag = Flags::<AtomicU8>::default();
    /// flag.disable(1); // Enable the first bit of the inner AtomicU8
    /// flag.disable(9); // Panic there is no 9th bit in an u8
    /// ```
    fn enable(&self, bit: u8) {
        self.0.fetch_or(1 << bit, Ordering::Relaxed);
    }

    /// disable x bit.
    /// # Safety
    /// No check performed, will produce panic if trying to disable bit outside of range
    ///
    /// # Examples
    ///
    /// ```
    /// let flag = Flags::<AtomicU8>::default();
    /// flag.disable(1); // Disable the first bit of the inner AtomicU8
    /// flag.disable(9); // Panic there is no 9th bit in an u8
    /// ```
    fn disable(&self, bit: u8) {
        self.0.fetch_and(!(1 << bit), Ordering::Relaxed);
    }

    /// check if the x bit is set.
    /// safety: no check performed, will produce panic if trying to check bit outside of range
    ///
    /// # Examples
    ///
    /// ```
    /// let flag = flags::<atomicu8>::default();
    /// flag.is_enable(1); // return true/false depending of the firt bit state
    /// flag.is_enable(9); // panic there is no 9th bit in an u8
    /// ```
    fn is_enable(&self, bit: u8) -> bool {
        (self.0.load(Ordering::Relaxed) & (1 << bit)) == (1 << bit)
    }

    /// check if the x bit is unset.
    /// safety: no check performed, will produce panic if trying to check bit outside of range
    ///
    /// # Examples
    ///
    /// ```
    /// let flag = flags::<atomicu8>::default();
    /// flag.is_disable(1); // return true/false depending of the firt bit state
    /// flag.is_disable(9); // panic there is no 9th bit in an u8
    /// ```
    fn is_disable(&self, bit: u8) -> bool {
        !self.is_enable(bit)
    }

    /// Возвращает сырой указатель на данные.
    fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr()
    }

    /// Восстанавливает ссылку на Flags из сырого указателя.
    ///
    /// # Safety
    /// Указатель должен быть валидным, выровненным и указывать на корректный `AtomicU8`.
    /// Структура должна быть помечена как `#[repr(transparent)]`.
    unsafe fn from_ptr<'a>(ptr: *const u8) -> &'a Self {
        &*(ptr as *const AtomicU8 as *const Self)
    }
}

impl Flags<AtomicU8> {
    pub const fn new(base: u8) -> Self {
        Self(AtomicU8::new(base))
    }
}

impl fmt::Display for Flags<AtomicU8> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:#010b}", self.0.load(Ordering::Relaxed))
    }
}



use core::ops::Deref;
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