// kernel/src/drivers/keyboard_buffer.rs
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

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
            return; // буфер полный
        }

        let buf = unsafe { &mut *self.buffer.get() };
        buf[head] = byte;

        self.head.store(next_head, Ordering::Release);
    }

    pub fn pop(&self) -> Option<u8> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }

        let buf = unsafe { &mut *self.buffer.get() };
        let byte = buf[tail];

        self.tail.store((tail + 1) % BUFFER_SIZE, Ordering::Release);
        Some(byte)
    }
}

pub static KEYBOARD_BUFFER: KeyboardBuffer = KeyboardBuffer::new();