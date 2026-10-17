use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use marlin_sync::Barrier;

static STATIC_BARRIER: Barrier = Barrier::new(2);

#[test]
fn single_thread_is_always_leader() {
    for n in [0, 1] {
        let b = Barrier::new(n);
        for _ in 0..3 {
            assert!(b.wait().is_leader());
        }
    }
}

#[test]
fn rounds_release_together_with_one_leader() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 200;
    let barrier = Arc::new(Barrier::new(THREADS as u32));
    let arrived = Arc::new(AtomicUsize::new(0));
    let leaders = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (barrier, arrived, leaders) = (barrier.clone(), arrived.clone(), leaders.clone());
            thread::spawn(move || {
                for round in 0..ROUNDS {
                    // Plain (Relaxed) work before the barrier must be visible to all after it.
                    arrived.fetch_add(1, Ordering::Relaxed);
                    if (t + round) % 50 == 0 {
                        thread::yield_now();
                    }
                    if barrier.wait().is_leader() {
                        leaders.fetch_add(1, Ordering::Relaxed);
                    }
                    // Everyone arrived for this round. Others may already be into the next one,
                    // but none past it, since that needs this thread to arrive too.
                    let seen = arrived.load(Ordering::Relaxed);
                    assert!(seen >= THREADS * (round + 1), "round {round}: {seen}");
                    assert!(seen <= THREADS * (round + 2), "round {round}: {seen}");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(leaders.load(Ordering::Relaxed), ROUNDS);
    assert_eq!(arrived.load(Ordering::Relaxed), THREADS * ROUNDS);
}

#[test]
fn waiter_sleeps_until_last_arrival() {
    let other = thread::spawn(|| STATIC_BARRIER.wait().is_leader());
    // Long enough for the first arrival to be asleep.
    thread::sleep(Duration::from_millis(30));
    assert!(!other.is_finished());
    let we_led = STATIC_BARRIER.wait().is_leader();
    let they_led = other.join().unwrap();
    assert!(we_led != they_led);
}
