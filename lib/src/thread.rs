//! Native Felix userspace threads.
//!
//! Threads have Linux-like identity and lifetime rules: all threads in a
//! process share its address space and file table, `getpid()` returns the
//! thread-group id, and each thread has a distinct `gettid()`.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};

const RUNNING: u8 = 0;
const FINISHED: u8 = 1;

struct Shared<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

struct Start<F, T> {
    function: Option<F>,
    shared: Arc<Shared<T>>,
}

extern "C" fn trampoline<F, T>(raw: *mut u8) -> !
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let mut start = unsafe { Box::from_raw(raw as *mut Start<F, T>) };
    let function = start
        .function
        .take()
        .expect("thread entry already consumed");
    let value = function();
    unsafe {
        (*start.shared.value.get()).write(value);
    }
    start.shared.state.store(FINISHED, Ordering::Release);
    drop(start);
    unsafe { crate::syscall::thread_exit(0) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnError(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinError {
    Syscall(i32),
    ThreadTerminated,
}

pub struct JoinHandle<T: Send + 'static> {
    tid: i32,
    shared: Arc<Shared<T>>,
    joined: bool,
}

impl<T: Send + 'static> JoinHandle<T> {
    pub fn tid(&self) -> i32 {
        self.tid
    }

    pub fn join(mut self) -> Result<T, JoinError> {
        let ret = unsafe { crate::syscall::thread_join(self.tid, core::ptr::null_mut()) };
        if ret < 0 {
            return Err(JoinError::Syscall(ret));
        }
        self.joined = true;
        if self.shared.state.load(Ordering::Acquire) != FINISHED {
            return Err(JoinError::ThreadTerminated);
        }
        Ok(unsafe { (*self.shared.value.get()).assume_init_read() })
    }
}

impl<T: Send + 'static> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if self.joined {
            return;
        }
        // Until detached-thread ownership is added, joining on drop prevents a
        // lost handle from leaking a scheduler slot and its kernel stack.
        let ret = unsafe { crate::syscall::thread_join(self.tid, core::ptr::null_mut()) };
        if ret >= 0 && self.shared.state.load(Ordering::Acquire) == FINISHED {
            unsafe {
                (*self.shared.value.get()).assume_init_drop();
            }
        }
        self.joined = true;
    }
}

pub fn spawn<F, T>(function: F) -> Result<JoinHandle<T>, SpawnError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let shared = Arc::new(Shared {
        state: AtomicU8::new(RUNNING),
        value: UnsafeCell::new(MaybeUninit::uninit()),
    });
    let start = Box::new(Start {
        function: Some(function),
        shared: shared.clone(),
    });
    let raw = Box::into_raw(start) as *mut u8;
    let entry = trampoline::<F, T> as *const () as u32;
    let tid = unsafe { crate::syscall::thread_create(entry, raw) };
    if tid < 0 {
        unsafe {
            drop(Box::from_raw(raw as *mut Start<F, T>));
        }
        return Err(SpawnError(tid));
    }
    Ok(JoinHandle {
        tid,
        shared,
        joined: false,
    })
}

pub fn current_id() -> i32 {
    unsafe { crate::syscall::gettid() }
}

pub fn yield_now() {
    unsafe { crate::syscall::sched_yield() }
}
