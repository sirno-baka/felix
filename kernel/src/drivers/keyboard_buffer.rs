// kernel/src/drivers/keyboard_buffer.rs
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use interrupt_sync::without_interrupts;

const BUFFER_SIZE: usize = 256;

pub struct KeyboardBuffer {
    buffer: UnsafeCell<[u8; BUFFER_SIZE]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// ←←← ЭТО САМОЕ ВАЖНОЕ ИСПРАВЛЕНИЕ
unsafe impl Sync for KeyboardBuffer {}

impl KeyboardBuffer {
    pub const fn new() -> Self {
        KeyboardBuffer {
            buffer: UnsafeCell::new([0; BUFFER_SIZE]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, byte: u8) {
        let head = self.head.load(Ordering::Relaxed);
        let next_head = (head + 1) % BUFFER_SIZE;

        if next_head == self.tail.load(Ordering::Acquire) {
            return; // буфер полон
        }

        // Доступ через сырой указатель (без создания &mut)
        unsafe {
            let buf_ptr = self.buffer.get() as *mut u8;
            *buf_ptr.add(head) = byte;
        }

        self.head.store(next_head, Ordering::Release);
    }

    pub fn pop(&self) -> Option<u8> {
        without_interrupts(|| {
            let tail = self.tail.load(Ordering::Relaxed);
            if tail == self.head.load(Ordering::Acquire) {
                return None;
            }

            let byte = unsafe {
                let buf_ptr = self.buffer.get() as *const u8;
                *buf_ptr.add(tail)
            };

            self.tail.store((tail + 1) % BUFFER_SIZE, Ordering::Release);
            Some(byte)
        })
    }
}

pub static KEYBOARD_BUFFER: KeyboardBuffer = KeyboardBuffer::new();