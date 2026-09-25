use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

thread_local! {
    static LOCAL: Cell<usize> = const { Cell::new(0) };
}

fn main() {
    println!("thread-smoke: stage start");
    let sum = Arc::new(Mutex::new(0usize));
    let barrier = Arc::new(Barrier::new(4));
    let mut threads = Vec::new();

    for value in 1..=3 {
        let sum = Arc::clone(&sum);
        let barrier = Arc::clone(&barrier);
        threads.push(thread::spawn(move || {
            LOCAL.set(value);
            barrier.wait();
            thread::yield_now();
            *sum.lock().unwrap() += LOCAL.get();
            (thread::current().id(), LOCAL.get())
        }));
    }
    barrier.wait();

    let results: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
    assert_eq!(*sum.lock().unwrap(), 6);
    assert_eq!(results.iter().map(|(_, value)| *value).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(results.windows(2).all(|pair| pair[0].0 != pair[1].0));
    println!("thread-smoke: stage joined");

    // Dropping JoinHandle must detach instead of leaking a scheduler slot.
    // Exceed MAX_TASKS over time to prove exited detached tasks are reclaimed.
    for _ in 0..36 {
        let done = Arc::new(AtomicBool::new(false));
        let child_done = Arc::clone(&done);
        drop(thread::spawn(move || child_done.store(true, Ordering::Release)));
        while !done.load(Ordering::Acquire) {
            thread::yield_now();
        }
    }
    println!("thread-smoke: stage detached");
    println!("std::thread smoke: PASS ({results:?})");
}
