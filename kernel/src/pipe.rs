//! In-kernel anonymous pipes for shell pipelines.
//!
//! Each pipe has a fixed ring buffer, reader/writer refcounts, and blocking
//! read/write via sti/hlt (same pattern as stdin).

use core::arch::asm;
use crate::println;

pub const PIPE_BUF_SIZE: usize = 4096;
const MAX_PIPES: usize = 16;

#[derive(Copy)]
#[derive(Clone)]
pub struct Pipe {
    buf: [u8; PIPE_BUF_SIZE],
    head: usize, // next write index
    tail: usize, // next read index
    len: usize,
    readers: u32,
    writers: u32,
    in_use: bool,
}

impl Pipe {
    const fn empty() -> Self {
        Self {
            buf: [0; PIPE_BUF_SIZE],
            head: 0,
            tail: 0,
            len: 0,
            readers: 0,
            writers: 0,
            in_use: false,
        }
    }
}

static mut PIPES: [Pipe; MAX_PIPES] = [Pipe::empty(); MAX_PIPES];

/// Allocate a new pipe. Returns pipe_id. Initial refcounts are 1 reader + 1 writer
/// (caller must create two FileDescriptor ends).
pub fn pipe_create() -> Option<usize> {
    unsafe {
        for i in 0..MAX_PIPES {
            if !PIPES[i].in_use {
                PIPES[i] = Pipe::empty();
                PIPES[i].in_use = true;
                PIPES[i].readers = 1;
                PIPES[i].writers = 1;
                return Some(i);
            }
        }
    }
    None
}

pub fn pipe_add_reader(id: usize) {
    unsafe {
        if id < MAX_PIPES && PIPES[id].in_use {
            PIPES[id].readers += 1;
        }
    }
}

pub fn pipe_add_writer(id: usize) {
    unsafe {
        if id < MAX_PIPES && PIPES[id].in_use {
            PIPES[id].writers += 1;
        }
    }
}

pub fn pipe_close_reader(id: usize) {
    unsafe {
        if id >= MAX_PIPES || !PIPES[id].in_use {
            return;
        }
        if PIPES[id].readers > 0 {
            PIPES[id].readers -= 1;
        }
        maybe_free(id);
    }
}

pub fn pipe_close_writer(id: usize) {
    unsafe {
        if id >= MAX_PIPES || !PIPES[id].in_use {
            return;
        }
        if PIPES[id].writers > 0 {
            PIPES[id].writers -= 1;
        }
        maybe_free(id);
    }
}

fn maybe_free(id: usize) {
    unsafe {
        if PIPES[id].readers == 0 && PIPES[id].writers == 0 {
            PIPES[id].in_use = false;
        }
    }
}

/// True if a non-blocking read would not block (data available or EOF).
pub fn pipe_readable(id: usize) -> bool {
    unsafe {
        if id >= MAX_PIPES || !PIPES[id].in_use {
            return true; // treat as ready (EOF/error)
        }
        PIPES[id].len > 0 || PIPES[id].writers == 0
    }
}

/// True if a non-blocking write would not block.
pub fn pipe_writable(id: usize) -> bool {
    unsafe {
        if id >= MAX_PIPES || !PIPES[id].in_use {
            return true;
        }
        PIPES[id].readers == 0 || PIPES[id].len < PIPE_BUF_SIZE
    }
}

/// Non-blocking read. Returns:
/// - `n > 0` bytes read
/// - `0` EOF (no writers, empty)
/// - `usize::MAX` would block (no data, writers still open)
pub fn pipe_try_read(id: usize, buf: *mut u8, count: usize) -> usize {
    if id >= MAX_PIPES || count == 0 {
        return 0;
    }
    unsafe {
        if !PIPES[id].in_use {
            return 0;
        }
        if PIPES[id].len == 0 {
            return if PIPES[id].writers == 0 { 0 } else { usize::MAX };
        }
        let mut read = 0usize;
        while read < count && PIPES[id].len > 0 {
            let byte = PIPES[id].buf[PIPES[id].tail];
            PIPES[id].tail = (PIPES[id].tail + 1) % PIPE_BUF_SIZE;
            PIPES[id].len -= 1;
            *buf.add(read) = byte;
            read += 1;
        }
        read
    }
}

/// Non-blocking write. Returns:
/// - `n` bytes written (may be partial)
/// - `0` no readers
/// - `usize::MAX` would block (buffer full, readers exist)
pub fn pipe_try_write(id: usize, buf: *const u8, count: usize) -> usize {
    if id >= MAX_PIPES || count == 0 {
        return 0;
    }
    unsafe {
        if !PIPES[id].in_use {
            return 0;
        }
        if PIPES[id].readers == 0 {
            return 0;
        }
        if PIPES[id].len == PIPE_BUF_SIZE {
            return usize::MAX;
        }
        let mut written = 0usize;
        while written < count && PIPES[id].len < PIPE_BUF_SIZE {
            let byte = *buf.add(written);
            PIPES[id].buf[PIPES[id].head] = byte;
            PIPES[id].head = (PIPES[id].head + 1) % PIPE_BUF_SIZE;
            PIPES[id].len += 1;
            written += 1;
        }
        written
    }
}

/// Blocking read from pipe. Returns 0 on EOF (no writers left and empty).
pub fn pipe_read(id: usize, buf: *mut u8, count: usize) -> usize {
    if id >= MAX_PIPES || count == 0 {
        return 0;
    }
    let mut read = 0usize;
    unsafe {
        if !PIPES[id].in_use {
            return 0;
        }
        asm!("sti");
        while read < count {
            if PIPES[id].len == 0 {
                if PIPES[id].writers == 0 {
                    break; // EOF
                }
                asm!("hlt");
                continue;
            }
            let byte = PIPES[id].buf[PIPES[id].tail];
            PIPES[id].tail = (PIPES[id].tail + 1) % PIPE_BUF_SIZE;
            PIPES[id].len -= 1;
            *buf.add(read) = byte;
            read += 1;
        }
        asm!("cli");
    }
    read
}

/// Blocking write to pipe. Returns bytes written (0 if no readers).
pub fn pipe_write(id: usize, buf: *const u8, count: usize) -> usize {
    if id >= MAX_PIPES || count == 0 {
        return 0;
    }
    let mut written = 0usize;
    unsafe {
        if !PIPES[id].in_use {
            return 0;
        }
        asm!("sti");
        while written < count {
            if PIPES[id].readers == 0 {
                break; // SIGPIPE-ish: stop
            }
            if PIPES[id].len == PIPE_BUF_SIZE {
                asm!("hlt");
                continue;
            }
            let byte = *buf.add(written);
            PIPES[id].buf[PIPES[id].head] = byte;
            PIPES[id].head = (PIPES[id].head + 1) % PIPE_BUF_SIZE;
            PIPES[id].len += 1;
            written += 1;
        }
        asm!("cli");
    }
    written
}
