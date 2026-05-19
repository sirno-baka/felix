use alloc::collections::VecDeque;
use core::ops::{Deref, DerefMut};

use interrupt_sync::SpinMutex;
use crate::multitasking::task::TASK_MANAGER;

pub struct Mutex<T> {
    inner: SpinMutex<T>,
    waiters: SpinMutex<VecDeque<i8>>,
}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: SpinMutex::new(data),
            waiters: SpinMutex::new(VecDeque::new()),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.inner.try_lock() {
                return MutexGuard {
                    guard,
                    parent: self,
                };
            }

            unsafe {
                let current = TASK_MANAGER.get_current_slot();
                if current >= 0 {
                    // === Сохраняем текущий контекст перед засыпанием ===
                    let current_esp: u32;
                    core::arch::asm!("mov {}, esp", out(reg) current_esp);

                    // Обновляем указатель на стек таска
                    TASK_MANAGER.tasks[current as usize].cpu_state_ptr = current_esp;
                    TASK_MANAGER.tasks[current as usize].sleep();

                    // Добавляем таск в очередь ожидающих
                    self.waiters.lock().push_back(current);
                }
            }

            // Останавливаемся. Таймер потом разбудит нас через schedule()
            unsafe {
                core::arch::asm!("hlt");
            }
        }
    }

    fn wake_one(&self) {
        if let Some(id) = self.waiters.lock().pop_front() {
            unsafe {
                if id >= 0 && (id as usize) < TASK_MANAGER.tasks.len() {
                    TASK_MANAGER.tasks[id as usize].wake();
                }
            }
        }
    }
    pub unsafe fn yield_current(&mut self) {
        // Можно вызвать из кода таска, если нужно добровольно отдать CPU
        core::arch::asm!("hlt");
    }
}

pub struct MutexGuard<'a, T> {
    guard: interrupt_sync::SpinMutexGuard<'a, T>,
    parent: &'a Mutex<T>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T { &self.guard }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T { &mut self.guard }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.parent.wake_one();
    }
}