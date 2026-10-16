#![allow(clippy::undocumented_unsafe_blocks)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use marlin_pal::thread::{self, ThreadStart};

#[repr(C)]
struct Job {
    start: ThreadStart,
    ran: AtomicBool,
    id: AtomicU64,
}

impl Job {
    fn new() -> Self {
        Self {
            start: ThreadStart { entry: run },
            ran: AtomicBool::new(false),
            id: AtomicU64::new(0),
        }
    }

    fn start_ptr(&self) -> NonNull<ThreadStart> {
        // Derived from the whole Job so `run` may cast back.
        NonNull::from(self).cast()
    }
}

unsafe fn run(start: NonNull<ThreadStart>) {
    let job = unsafe { start.cast::<Job>().as_ref() };
    thread::set_current_name("marlin-pal-test-thread-with-a-long-name");
    job.id.store(thread::current_id(), Ordering::Relaxed);
    job.ran.store(true, Ordering::Release);
}

#[test]
fn spawn_and_join() {
    let job = Job::new();
    let handle = unsafe { thread::spawn(0, job.start_ptr()) }.unwrap();
    handle.join().unwrap();
    assert!(job.ran.load(Ordering::Acquire));
    assert_ne!(job.id.load(Ordering::Relaxed), thread::current_id());
}

#[test]
fn custom_stack_size() {
    let job = Job::new();
    let handle = unsafe { thread::spawn(256 * 1024, job.start_ptr()) }.unwrap();
    handle.join().unwrap();
    assert!(job.ran.load(Ordering::Acquire));
}

#[test]
fn detach() {
    // Leaked so it outlives the detached thread.
    let job: &'static Job = Box::leak(Box::new(Job::new()));
    unsafe { thread::spawn(0, job.start_ptr()) }
        .unwrap()
        .detach();
    while !job.ran.load(Ordering::Acquire) {
        thread::yield_now();
    }
}

#[test]
fn parallelism_is_positive() {
    assert!(thread::available_parallelism() >= 1);
}
