//! Cooperative single-threaded async runtime for Felix userspace.
//!
//! Designed to feel familiar if you know `smol`, `futures::executor` or `async-std`.
//!
//! # Quick start
//!
//! ```ignore
//! use libfelix::async_rt::{block_on, yield_now, Executor};
//!
//! // One-shot future:
//! let v = block_on(async {
//!     yield_now().await;
//!     42
//! });
//!
//! // Multiple concurrent tasks:
//! let mut rt = Executor::new();
//! rt.spawn(async { /* ... */ });
//! rt.spawn(async { /* ... */ });
//! rt.run_until_done();   // or .run()
//! ```
//!
//! # I/O
//!
//! Use non-blocking fds (`syscall::set_nonblock`) together with
//! [`async_read`] / [`async_write`].  When the kernel returns EAGAIN the
//! future yields; the executor (or `block_on`) parks via `poll` until the
//! next timer tick / event.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::syscall::{self, PollFd, POLLIN, POLLOUT};

// ---------------------------------------------------------------------------
// Waker (noop — we simply re-queue Pending tasks)
// ---------------------------------------------------------------------------

fn raw_waker() -> RawWaker {
    fn clone(_: *const ()) -> RawWaker {
        raw_waker()
    }
    fn wake(_: *const ()) {}
    fn wake_by_ref(_: *const ()) {}
    fn drop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    RawWaker::new(core::ptr::null(), &VTABLE)
}

fn dummy_waker() -> Waker {
    // SAFETY: vtable functions are pure no-ops and never dereference the data pointer.
    unsafe { Waker::from_raw(raw_waker()) }
}

/// Park the current task for ~1 ms (one timer tick) so we don't busy-spin
/// when every future is Pending.
fn idle_once() {
    let mut dummy = PollFd {
        fd: -1,
        events: 0,
        revents: 0,
    };
    // SAFETY: dummy is stack-local and only used for the duration of the call.
    unsafe {
        let _ = syscall::poll(&mut dummy as *mut PollFd, 0, 1);
    }
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

type BoxFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// Single-threaded cooperative executor.
///
/// Tasks are polled in FIFO order.  When a task returns `Pending` it is
/// pushed back onto the queue; when the whole queue stalls the executor
/// sleeps briefly via kernel `poll`.
///
/// # Example
/// ```ignore
/// let mut rt = Executor::new();
/// rt.spawn(async { println!("http-client"); });
/// rt.run_until_done();
/// ```
pub struct Executor {
    tasks: VecDeque<BoxFuture>,
}

impl Executor {
    /// Create an empty executor.
    pub fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
        }
    }

    /// Schedule a future that produces no value.
    ///
    /// The future must be `'static` (it may outlive the call that spawned it).
    pub fn spawn<F>(&mut self, fut: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.tasks.push_back(Box::pin(fut));
    }

    /// Number of tasks still in the queue (including those that have not
    /// yet been polled to completion).
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Returns `true` when no tasks remain.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Run until every spawned task has completed (`Poll::Ready`).
    ///
    /// This is the primary entry-point, analogous to `smol::Executor::run`
    /// or `futures::executor::LocalPool::run_until_stalled` + wait.
    pub fn run_until_done(&mut self) {
        self.run()
    }

    /// Alias for [`run_until_done`](Self::run_until_done).
    pub fn run(&mut self) {
        let waker = dummy_waker();
        let mut cx = Context::from_waker(&waker);

        while !self.tasks.is_empty() {
            let n = self.tasks.len();
            let mut progressed = false;

            for _ in 0..n {
                let Some(mut task) = self.tasks.pop_front() else {
                    break;
                };
                match task.as_mut().poll(&mut cx) {
                    Poll::Ready(()) => {
                        progressed = true;
                        // task dropped here
                    }
                    Poll::Pending => {
                        self.tasks.push_back(task);
                    }
                }
            }

            if !progressed && !self.tasks.is_empty() {
                idle_once();
            }
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// block_on
// ---------------------------------------------------------------------------

/// Drive a future to completion on the current (and only) thread.
///
/// Equivalent to the free functions found in `futures`, `smol`, `async-std`
/// and `tokio`.
///
/// ```ignore
/// let answer = block_on(async { 42 });
/// ```
pub fn block_on<T>(fut: impl Future<Output = T>) -> T {
    let waker = dummy_waker();
    let mut cx = Context::from_waker(&waker);
    let mut fut = core::pin::pin!(fut);

    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => idle_once(),
        }
    }
}

// ---------------------------------------------------------------------------
// yield_now
// ---------------------------------------------------------------------------

/// Future that yields once, allowing other tasks to run.
///
/// ```ignore
/// yield_now().await;
/// ```
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            Poll::Pending
        }
    }
}

/// Create a future that yields control once.
pub fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}

// ---------------------------------------------------------------------------
// Async I/O (non-blocking fds)
// ---------------------------------------------------------------------------

/// Future that performs a non-blocking `read` syscall.
///
/// Returns the number of bytes read, or `0` on EOF.
/// When the fd would block (`EAGAIN`) the future returns `Pending`.
///
/// # Safety
/// `buf` must remain valid for the whole lifetime of the future.
pub struct AsyncRead {
    fd: u32,
    buf: *mut u8,
    len: usize,
}

/// Start an asynchronous read.
///
/// The fd should be in non-blocking mode (`syscall::set_nonblock`).
///
/// # Safety
/// `buf` must stay valid while the returned future is alive.
pub unsafe fn async_read(fd: u32, buf: *mut u8, len: usize) -> AsyncRead {
    AsyncRead { fd, buf, len }
}

impl Future for AsyncRead {
    type Output = usize;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<usize> {
        // SAFETY: caller guaranteed the buffer is valid for the duration of this future.
        let n = unsafe { syscall::read(self.fd, self.buf, self.len) };
        if n == usize::MAX {
            // Would block.  Touch poll so the kernel knows we care; the
            // executor / block_on will sleep.  Never sleep *inside* a future.
            let mut pfd = PollFd {
                fd: self.fd as i32,
                events: POLLIN,
                revents: 0,
            };
            unsafe {
                let _ = syscall::poll(&mut pfd as *mut PollFd, 1, 0);
            }
            Poll::Pending
        } else {
            Poll::Ready(n)
        }
    }
}

/// Future that performs a non-blocking `write` syscall (handles partial writes).
///
/// # Safety
/// `buf` must remain valid for the whole lifetime of the future.
pub struct AsyncWrite {
    fd: u32,
    buf: *const u8,
    len: usize,
    offset: usize,
}

/// Start an asynchronous write.
///
/// The fd should be in non-blocking mode (`syscall::set_nonblock`).
///
/// # Safety
/// `buf` must stay valid while the returned future is alive.
pub unsafe fn async_write(fd: u32, buf: *const u8, len: usize) -> AsyncWrite {
    AsyncWrite {
        fd,
        buf,
        len,
        offset: 0,
    }
}

impl Future for AsyncWrite {
    type Output = usize;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<usize> {
        while self.offset < self.len {
            // SAFETY: caller guaranteed the buffer is valid.
            let n = unsafe {
                syscall::write(self.fd, self.buf.add(self.offset), self.len - self.offset)
            };
            if n == usize::MAX {
                let mut pfd = PollFd {
                    fd: self.fd as i32,
                    events: POLLOUT,
                    revents: 0,
                };
                unsafe {
                    let _ = syscall::poll(&mut pfd as *mut PollFd, 1, 0);
                }
                return Poll::Pending;
            }
            if n == 0 {
                // Peer closed / no progress
                return Poll::Ready(self.offset);
            }
            self.offset += n;
        }
        Poll::Ready(self.offset)
    }
}

// ---------------------------------------------------------------------------
// Convenience: wait until readable
// ---------------------------------------------------------------------------

/// Future that completes when `fd` becomes readable (or has an error/hangup).
pub struct WaitReadable {
    fd: u32,
    done: bool,
}

/// Wait until `fd` is ready for reading.
pub fn wait_readable(fd: u32) -> WaitReadable {
    WaitReadable { fd, done: false }
}

impl Future for WaitReadable {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            return Poll::Ready(());
        }
        let mut pfd = PollFd {
            fd: self.fd as i32,
            events: POLLIN,
            revents: 0,
        };
        // Non-blocking probe; executor sleeps if we return Pending.
        let n = unsafe { syscall::poll(&mut pfd as *mut PollFd, 1, 0) };
        if n > 0 && (pfd.revents & (POLLIN | syscall::POLLERR | syscall::POLLHUP)) != 0 {
            self.done = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
