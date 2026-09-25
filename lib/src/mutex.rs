//! Blocking userspace mutex for Felix native threads.
//!
//! State is Linux-style: 0 = unlocked, 1 = locked without known waiters,
//! 2 = contended. The fast path is entirely userspace; only contention enters
//! the kernel through FUTEX_WAIT/FUTEX_WAKE.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

pub struct Mutex<T: ?Sized> {
    state: AtomicU32,
    value: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(0),
            value: UnsafeCell::new(value),
        }
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if let Some(guard) = self.try_lock() {
            return guard;
        }

        let mut state = self.state.load(Ordering::Relaxed);
        loop {
            if state == 0 {
                match self
                    .state
                    .compare_exchange_weak(0, 2, Ordering::Acquire, Ordering::Relaxed)
                {
                    Ok(_) => return MutexGuard { mutex: self },
                    Err(current) => {
                        state = current;
                        continue;
                    }
                }
            }

            if state != 2 {
                state = self.state.swap(2, Ordering::Acquire);
                if state == 0 {
                    return MutexGuard { mutex: self };
                }
            }

            unsafe {
                let _ = crate::syscall::futex_wait(self.state.as_ptr(), 2);
            }
            state = self.state.load(Ordering::Relaxed);
        }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard { mutex: self })
    }
}

pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if self.mutex.state.swap(0, Ordering::Release) == 2 {
            unsafe {
                let _ = crate::syscall::futex_wake(self.mutex.state.as_ptr(), 1);
            }
        }
    }
}
