//! Tests for the cooperative async runtime (`libfelix::async_rt`).
//!
//! Run from the Felix shell: `async_test`
//! Expected final line: `ALL TESTS PASSED`.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicUsize, Ordering};

use libfelix::async_rt::{async_read, async_write, block_on, yield_now, Executor};
use libfelix::prelude::*;
use libfelix::syscall;

static ORDER: AtomicUsize = AtomicUsize::new(0);

fn next_order() -> usize {
    ORDER.fetch_add(1, Ordering::SeqCst)
}

// -------------------- 1. yield interleaving --------------------

async fn task_a() {
    let o = next_order();
    println!("  task_a start order={}", o);
    yield_now().await;
    let o = next_order();
    println!("  task_a after yield order={}", o);
}

async fn task_b() {
    let o = next_order();
    println!("  task_b start order={}", o);
    yield_now().await;
    let o = next_order();
    println!("  task_b after yield order={}", o);
}

fn test_yield_order() -> bool {
    println!("[1] yield_order");
    ORDER.store(0, Ordering::SeqCst);

    let mut rt = Executor::new();
    rt.spawn(task_a());
    rt.spawn(task_b());
    rt.run_until_done();

    let steps = ORDER.load(Ordering::SeqCst);
    let ok = steps == 4;
    println!("  steps={} {}", steps, if ok { "OK" } else { "FAIL" });
    ok
}

// -------------------- 2. block_on + nested await --------------------

async fn chain() -> u32 {
    yield_now().await;
    42
}

fn test_block_on_chain() -> bool {
    println!("[2] block_on_chain");
    let v = block_on(async {
        let x = chain().await;
        yield_now().await;
        x + 1
    });
    let ok = v == 43;
    println!("  result={} {}", v, if ok { "OK" } else { "FAIL" });
    ok
}

// -------------------- 3. many concurrent tasks --------------------

async fn counter_task(id: usize, n: usize) {
    for i in 0..n {
        if i % 3 == 0 {
            yield_now().await;
        }
        let _ = id;
    }
}

fn test_many_tasks() -> bool {
    println!("[3] many_tasks");
    let mut rt = Executor::new();
    for i in 0..8 {
        rt.spawn(counter_task(i, 20));
    }
    rt.run_until_done();
    println!("  8 tasks finished OK");
    true
}

// -------------------- 4. async non-blocking pipe --------------------

fn test_async_pipe() -> bool {
    println!("[4] async_pipe");

    let mut fds = [0u32; 2];
    if unsafe { syscall::pipe(fds.as_mut_ptr()) } != 0 {
        println!("  pipe() failed");
        return false;
    }
    let (rfd, wfd) = (fds[0], fds[1]);

    unsafe {
        let _ = syscall::set_nonblock(rfd);
        let _ = syscall::set_nonblock(wfd);
    }

    let payload = b"hello async pipe\n";
    let mut rx = [0u8; 64];

    let result = block_on(async {
        let n = unsafe { async_write(wfd, payload.as_ptr(), payload.len()).await };
        if n != payload.len() {
            return Err(n);
        }
        // Close writer so the reader eventually sees EOF.
        unsafe {
            let _ = syscall::close(wfd);
        }

        let mut total = 0usize;
        loop {
            let n = unsafe { async_read(rfd, rx.as_mut_ptr().add(total), rx.len() - total).await };
            if n == 0 {
                break; // EOF
            }
            if n == usize::MAX {
                // Should be rare after close; just yield and retry.
                yield_now().await;
                continue;
            }
            total += n;
            if total >= payload.len() {
                break;
            }
        }
        Ok(total)
    });

    unsafe {
        let _ = syscall::close(rfd);
    }

    match result {
        Ok(n) if n == payload.len() && &rx[..n] == payload => {
            println!("  got {} bytes OK", n);
            true
        }
        Ok(n) => {
            println!("  got {} bytes (expected {}) FAIL", n, payload.len());
            false
        }
        Err(e) => {
            println!("  write error {} FAIL", e);
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    println!("=== async_test ===");

    let mut passed = 0usize;
    let mut failed = 0usize;

    for (name, f) in [
        ("yield_order", test_yield_order as fn() -> bool),
        ("block_on_chain", test_block_on_chain),
        ("many_tasks", test_many_tasks),
        ("async_pipe", test_async_pipe),
    ] {
        if f() {
            passed += 1;
        } else {
            failed += 1;
            println!("  !! {} failed", name);
        }
    }

    println!("---");
    println!("passed={} failed={}", passed, failed);
    if failed == 0 {
        println!("ALL TESTS PASSED");
        0
    } else {
        println!("SOME TESTS FAILED");
        1
    }
}
