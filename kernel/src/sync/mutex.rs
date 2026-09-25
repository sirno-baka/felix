use core::arch::asm;
use core::ops::{Deref, DerefMut};

use crate::print::{printer_new, PRINTER};
use crate::println;
use interrupt_sync::{without_interrupts, SpinMutex};

pub struct Mutex<T: ?Sized> {
    inner: SpinMutex<T>,
}

impl<T: ?Sized> Mutex<T> {
    pub const fn new(data: T) -> Self
    where
        T: Sized,
    {
        Self {
            inner: SpinMutex::new(data),
        }
    }

    /// Non-blocking lock for panic/exception paths (must not sleep).
    pub fn try_lock_nb(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock().map(|guard| MutexGuard { guard })
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        // Сохраняем исходное состояние Interrupt Flag (IF)
        let eflags: u32;
        unsafe {
            asm!("pushfd; pop {}", out(reg) eflags);
        }
        let was_enabled = (eflags & (1 << 9)) != 0; // бит IF

        unsafe { asm!("cli") };

        loop {
            if let Some(guard) = self.inner.try_lock() {
                // Восстанавливаем то состояние, которое было ДО вызова lock()
                if was_enabled {
                    unsafe { asm!("sti") };
                }
                return MutexGuard { guard };
            }

            // A mutex wait is not a scheduler context switch. Marking this task
            // runnable from another CPU while it still executes on its kernel
            // stack would let two CPUs run the same task and corrupt its saved
            // CPUState. Keep interrupts serviceable, but retain ownership of
            // the current execution context until the lock becomes available.
            if was_enabled {
                unsafe {
                    asm!("sti");
                    asm!("pause");
                    asm!("cli");
                }
            } else {
                core::hint::spin_loop();
            }
        }
    }

    pub unsafe fn yield_current(&mut self) {
        core::arch::asm!("hlt");
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Неблокирующая попытка взять лок (безопасно из IRQ)
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let eflags: u32;
        unsafe {
            asm!("pushfd; pop {}", out(reg) eflags);
        }
        let was_enabled = (eflags & (1 << 9)) != 0;

        unsafe { asm!("cli") };

        if let Some(guard) = self.inner.try_lock() {
            if was_enabled {
                unsafe { asm!("sti") };
            }
            Some(MutexGuard { guard })
        } else {
            // не получилось — восстанавливаем IF и уходим
            if was_enabled {
                unsafe { asm!("sti") };
            }
            None
        }
    }
}

pub struct MutexGuard<'a, T: ?Sized> {
    guard: interrupt_sync::SpinMutexGuard<'a, T>,
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}
