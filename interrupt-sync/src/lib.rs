#![no_std]
use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use lock_api::{Mutex as ApiMutex, RawMutex};
use spinning_top::RawSpinlock;

const CPU_SLOTS: usize = 256;
static INTERRUPT_NESTING: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static RESTORE_INTERRUPTS: [AtomicBool; CPU_SLOTS] =
    [const { AtomicBool::new(false) }; CPU_SLOTS];

#[inline(always)]
fn current_cpu_slot() -> usize {
    #[cfg(target_arch = "x86")]
    return ((core::arch::x86::__cpuid(1).ebx >> 24) & 0xff) as usize;
    #[cfg(target_arch = "x86_64")]
    return ((core::arch::x86_64::__cpuid(1).ebx >> 24) & 0xff) as usize;
    #[allow(unreachable_code)]
    0
}

/// Disable local interrupts and remember the incoming IF state per CPU.
#[inline(always)]
fn enter_interrupt_guard() -> usize {
    let flags: usize;
    unsafe {
        asm!("pushfd", "pop {0}", out(reg) flags, options(nomem, preserves_flags));
        asm!("cli", options(nomem, nostack));
    }
    let cpu = current_cpu_slot();
    if INTERRUPT_NESTING[cpu].fetch_add(1, Ordering::Relaxed) == 0 {
        RESTORE_INTERRUPTS[cpu].store(flags & (1 << 9) != 0, Ordering::Relaxed);
    }
    cpu
}

#[inline(always)]
fn leave_interrupt_guard(cpu: usize) {
    let previous = INTERRUPT_NESTING[cpu].fetch_sub(1, Ordering::Relaxed);
    debug_assert!(previous > 0);
    if previous == 1 && RESTORE_INTERRUPTS[cpu].swap(false, Ordering::Relaxed) {
        unsafe { asm!("sti", options(nomem, nostack)) };
    }
}

/// Отключает прерывания на текущем CPU и восстанавливает предыдущее состояние.
#[inline(always)]
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let cpu = enter_interrupt_guard();
    let result = f();
    leave_interrupt_guard(cpu);
    result
}
// ====================== RawInterruptMutex ======================
pub struct RawInterruptMutex<R: RawMutex> {
    inner: R,
}

unsafe impl<R: RawMutex> RawMutex for RawInterruptMutex<R> {
    type GuardMarker = R::GuardMarker;

    const INIT: Self = RawInterruptMutex { inner: R::INIT };

    /// cli for the whole hold, not just the spin. Previously without_interrupts()
    /// only wrapped lock() so sti ran before the critical section — useless.
    #[inline(always)]
    fn lock(&self) {
        enter_interrupt_guard();
        self.inner.lock();
    }

    #[inline(always)]
    fn try_lock(&self) -> bool {
        let cpu = enter_interrupt_guard();
        if self.inner.try_lock() {
            true
        } else {
            leave_interrupt_guard(cpu);
            false
        }
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        self.inner.unlock();
        leave_interrupt_guard(current_cpu_slot());
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
    lock: SpinMutex<()>, // используем новый алиас
    initialized: AtomicBool,
}

unsafe impl<T: Send + Sync> Sync for InterruptLazy<T> {}

impl<T> InterruptLazy<T> {
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            init: UnsafeCell::new(Some(init)),
            data: UnsafeCell::new(None),
            lock: SpinMutex::new(()),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn get(&self) -> &T {
        if self.initialized.load(Ordering::Acquire) {
            return unsafe { (&*self.data.get()).as_ref().unwrap_unchecked() };
        }

        let _guard = self.lock.lock();
        let data = unsafe { &mut *self.data.get() };
        if data.is_none() {
            let init_fn = unsafe { &mut *self.init.get() };
            let value = init_fn.take().expect("InterruptLazy initializer missing")();
            *data = Some(value);
            self.initialized.store(true, Ordering::Release);
        }
        data.as_ref().unwrap()
    }
}

// Удобные ре-экспорты
pub use lock_api::MutexGuard; // если где-то явно нужен общий тип
