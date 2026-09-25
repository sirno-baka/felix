use crate::spin::Mutex;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

pub mod mutex;

// ====================== InterruptLazy (lazy_static) ======================
pub struct MutexLazy<T> {
    init: UnsafeCell<Option<fn() -> T>>,
    data: UnsafeCell<Option<T>>,
    lock: Mutex<()>,
    initialized: AtomicBool,
}

unsafe impl<T: Send + Sync> Sync for MutexLazy<T> {}

impl<T> MutexLazy<T> {
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            init: UnsafeCell::new(Some(init)),
            data: UnsafeCell::new(None),
            lock: Mutex::new(()),
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
            let value = init_fn.take().expect("MutexLazy initializer missing")();
            *data = Some(value);
            self.initialized.store(true, Ordering::Release);
        }
        data.as_ref().unwrap()
    }
}
