#![allow(clippy::undocumented_unsafe_blocks)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use marlin_pal::thread::{self, ThreadStart};
use marlin_test_support::wait_until;

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
    wait_until(
        Duration::from_secs(10),
        "the detached thread to run",
        || job.ran.load(Ordering::Acquire),
    );
}

#[test]
fn parallelism_is_positive() {
    assert!(thread::available_parallelism() >= 1);
}

#[cfg(marlin_ext = "hybrid_cores")]
#[test]
fn efficiency_modes_apply() {
    use marlin_pal::ext::hybrid_cores::{EfficiencyMode, HybridCores};
    let Some(hybrid) = HybridCores::get() else {
        marlin_test_support::skip!("hybrid-core control is unavailable on this machine");
    };
    std::thread::spawn(move || {
        for mode in [
            EfficiencyMode::Efficient,
            EfficiencyMode::Performance,
            EfficiencyMode::Default,
        ] {
            hybrid.set_current_mode(mode).unwrap();
        }
    })
    .join()
    .unwrap();
}
