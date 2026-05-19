#![no_std]

use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr::read;
use core::sync::atomic::{AtomicUsize, Ordering};
use lock_api::{RawMutex, Mutex as ApiMutex};
use spinning_top::RawSpinlock;

/// Отключает прерывания на x86 (32-bit) и восстанавливает предыдущее состояние

static INTERRUPT_NESTING: AtomicUsize = AtomicUsize::new(0);
#[inline(always)]
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let flags: u32;
    unsafe {
        asm!("pushfd", "pop {0}", out(reg) flags, options(nomem, nostack, preserves_flags));
        asm!("cli", options(nomem, nostack));
    }

    INTERRUPT_NESTING.fetch_add(1, Ordering::SeqCst);

    let result = f();

    let nesting = INTERRUPT_NESTING.fetch_sub(1, Ordering::SeqCst);

    unsafe {
        // Включаем прерывания только если это был самый внешний вызов
        if nesting == 1 && (flags & (1 << 9)) != 0 {
            asm!("sti", options(nomem, nostack));
        }
    }

    result
}
// ====================== RawInterruptMutex ======================
pub struct RawInterruptMutex<R: RawMutex> {
    inner: R,
}

unsafe impl<R: RawMutex> RawMutex for RawInterruptMutex<R> {
    type GuardMarker = R::GuardMarker;

    const INIT: Self = RawInterruptMutex { inner: R::INIT };

    #[inline(always)]
    fn lock(&self) {
        without_interrupts(|| self.inner.lock());
    }

    #[inline(always)]
    fn try_lock(&self) -> bool {
        without_interrupts(|| self.inner.try_lock())
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        self.inner.unlock();
    }
}

// ====================== RawSpinMutex (без cli/sti) ======================
pub struct RawSpinMutex<R: RawMutex> {
    inner: R,
}

unsafe impl<R: RawMutex> RawMutex for RawSpinMutex<R> {
    type GuardMarker = R::GuardMarker;

    const INIT: Self = RawSpinMutex { inner: R::INIT };

    #[inline(always)]
    fn lock(&self) {
        self.inner.lock();
    }

    #[inline(always)]
    fn try_lock(&self) -> bool {
        self.inner.try_lock()
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        self.inner.unlock();
    }
}

// ====================== Основные типы ======================
pub type InterruptSpinMutex<T> = ApiMutex<RawInterruptMutex<RawSpinlock>, T>;
pub type SpinMutex<T> = ApiMutex<RawSpinMutex<RawSpinlock>, T>;
pub type SpinMutexGuard<'a, T> = lock_api::MutexGuard<'a, RawSpinMutex<RawSpinlock>, T>;

// ====================== InterruptLazy (lazy_static) ======================
pub struct InterruptLazy<T> {
    init: UnsafeCell<Option<fn() -> T>>,
    data: UnsafeCell<Option<T>>,
    lock: SpinMutex<()>,          // используем новый алиас
}

unsafe impl<T: Send + Sync> Sync for InterruptLazy<T> {}

impl<T> InterruptLazy<T> {
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            init: UnsafeCell::new(Some(init)),
            data: UnsafeCell::new(None),
            lock: SpinMutex::new(()),
        }
    }

    pub fn get(&self) -> &T {
        let data = unsafe { &*self.data.get() };
        if let Some(val) = data {
            return val;
        }

        let _guard = self.lock.lock();
        let data = unsafe { &mut *self.data.get() };

        if data.is_none() {
            let init_fn = unsafe { &mut *self.init.get() };
            if let Some(f) = init_fn.take() {
                *data = Some(f());
            }
        }
        data.as_ref().unwrap()
    }
}

// Удобные ре-экспорты
pub use lock_api::MutexGuard; // если где-то явно нужен общий тип