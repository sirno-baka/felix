use core::cell::UnsafeCell;
use crate::spin::Mutex;

pub mod mutex;


// ====================== InterruptLazy (lazy_static) ======================
pub struct MutexLazy<T> {
    init: UnsafeCell<Option<fn() -> T>>,
    data: UnsafeCell<Option<T>>,
    lock: Mutex<()>,
}

unsafe impl<T: Send + Sync> Sync for MutexLazy<T> {}

impl<T> MutexLazy<T> {
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            init: UnsafeCell::new(Some(init)),
            data: UnsafeCell::new(None),
            lock: Mutex::new(()),
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
