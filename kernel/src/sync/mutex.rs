use alloc::collections::VecDeque;
use core::arch::asm;
use core::ops::{Deref, DerefMut};

use crate::multitasking::task::TASK_MANAGER;
use crate::print::{PRINTER, printer_new};
use crate::println;
use interrupt_sync::{SpinMutex, without_interrupts};

pub struct Mutex<T: ?Sized> {
    waiters: SpinMutex<VecDeque<i8>>,
    inner: SpinMutex<T>,
}

impl<T: ?Sized> Mutex<T> {
    pub const fn new(data: T) -> Self
    where
        T: Sized,
    {
        Self {
            inner: SpinMutex::new(data),
            waiters: SpinMutex::new(VecDeque::new()),
        }
    }

    /// Non-blocking lock for panic/exception paths (must not sleep).
    pub fn try_lock_nb(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock().map(|guard| MutexGuard {
            guard,
            parent: self,
        })
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
                return MutexGuard {
                    guard,
                    parent: self,
                };
            }

            // === contended path ===
            unsafe {
                let current = TASK_MANAGER.get_current_slot();
                if current >= 0 {
                    let current_esp: u32;
                    core::arch::asm!("mov {}, esp", out(reg) current_esp);

                    if let Some(ref mut task) = TASK_MANAGER.tasks[current as usize] {
                        task.cpu_state_ptr = current_esp;
                        task.running = false;
                    }

                    self.waiters.lock().push_back(current);
                }
            }

            // Важно: sti + hlt только если прерывания должны быть включены
            if was_enabled {
                unsafe {
                    asm!("sti");
                    asm!("hlt");
                }
            } else {
                // Во время boot (прерывания выключены) — просто спин
                core::hint::spin_loop();
            }
        }
    }

    fn wake_one(&self) {
        if let Some(id) = self.waiters.lock().pop_front() {
            unsafe {
                if id >= 0 && (id as usize) < TASK_MANAGER.tasks.len() {
                    if let Some(ref mut task) = TASK_MANAGER.tasks[id as usize] {
                        task.running = true;
                    }
                }
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
            Some(MutexGuard {
                guard,
                parent: self,
            })
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
    parent: &'a Mutex<T>,
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

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.parent.wake_one();
    }
}
