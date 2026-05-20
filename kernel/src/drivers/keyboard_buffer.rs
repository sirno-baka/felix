use alloc::collections::VecDeque;
// kernel/src/drivers/keyboard_buffer.rs
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use interrupt_sync::without_interrupts;
use crate::spin::KMutex;
use crate::utils::queue::Queue;

const BUFFER_SIZE: usize = 256;
pub static KEYBOARD_BUFFER: KMutex<Option<Queue<u8>>> = KMutex::new(None);
