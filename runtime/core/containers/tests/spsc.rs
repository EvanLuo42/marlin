//! SpscQueue: halves, statics, drops and a two-thread stress run.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use marlin_containers::{CapacityError, SpscQueue};

#[test]
fn push_pop_and_capacity() {
    let q: SpscQueue<u32, 3> = SpscQueue::new();
    let (mut tx, mut rx) = q.split().unwrap();
    assert!(q.split().is_none());
    assert!(q.producer().is_none());
    assert_eq!(rx.pop(), None);
    for i in 0..3 {
        tx.push(i).unwrap();
    }
    assert!(tx.is_full());
    assert_eq!(tx.push(9), Err(CapacityError(9)));
    assert_eq!(q.len(), 3);
    assert_eq!(rx.peek(), Some(&0));
    assert_eq!(rx.pop(), Some(0));
    assert_eq!(tx.free(), 1);
    tx.push(3).unwrap();
    // Wraps around the ring several times.
    for i in 4..100 {
        assert_eq!(rx.pop(), Some(i - 3));
        tx.push(i).unwrap();
    }
    assert_eq!(rx.len(), 3);
    assert!(!rx.is_empty());
    assert_eq!(format!("{q:?}"), "SpscQueue { len: 3, capacity: 3 }");
}

#[test]
fn halves_can_be_taken_again_after_drop() {
    let q: SpscQueue<String, 4> = SpscQueue::new();
    let mut tx = q.producer().unwrap();
    tx.push("a".into()).unwrap();
    drop(tx);
    let mut tx = q.producer().unwrap();
    tx.push("b".into()).unwrap();
    let mut rx = q.consumer().unwrap();
    assert!(q.consumer().is_none());
    assert_eq!(rx.pop().as_deref(), Some("a"));
    drop((tx, rx));
    let (_, mut rx) = q.split().unwrap();
    assert_eq!(rx.pop().as_deref(), Some("b"));
}

#[test]
fn in_a_static() {
    static Q: SpscQueue<u64, 8> = SpscQueue::new();
    let mut tx = Q.producer().unwrap();
    let consumer = thread::spawn(|| {
        let mut rx = Q.consumer().unwrap();
        let mut sum = 0;
        let mut n = 0;
        while n < 1000 {
            if let Some(x) = rx.pop() {
                sum += x;
                n += 1;
            } else {
                thread::yield_now();
            }
        }
        sum
    });
    for i in 0..1000 {
        let mut v = i;
        while let Err(CapacityError(back)) = tx.push(v) {
            v = back;
            thread::yield_now();
        }
    }
    assert_eq!(consumer.join().unwrap(), (0..1000).sum::<u64>());
}

#[test]
fn drops_what_was_never_popped() {
    let drops = Arc::new(AtomicUsize::new(0));
    struct D(Arc<AtomicUsize>);
    impl Drop for D {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    {
        let q: SpscQueue<D, 4> = SpscQueue::new();
        let (mut tx, mut rx) = q.split().unwrap();
        for _ in 0..4 {
            assert!(tx.push(D(drops.clone())).is_ok());
        }
        drop(rx.pop());
        drop(rx.pop());
        assert!(tx.push(D(drops.clone())).is_ok());
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
    assert_eq!(drops.load(Ordering::Relaxed), 5);
}

/// Two threads, a fixed number of items, every item checked in order.
fn stress<const N: usize>(items: u64) {
    let q: SpscQueue<u64, N> = SpscQueue::new();
    let (mut tx, mut rx) = q.split().unwrap();
    thread::scope(|s| {
        let producer = s.spawn(move || {
            for i in 0..items {
                let mut v = i;
                loop {
                    match tx.push(v) {
                        Ok(()) => break,
                        Err(CapacityError(back)) => {
                            v = back;
                            std::hint::spin_loop();
                        }
                    }
                }
            }
        });
        let consumer = s.spawn(move || {
            let mut expected = 0;
            let mut spins = 0u64;
            while expected < items {
                match rx.pop() {
                    Some(v) => {
                        assert_eq!(v, expected);
                        expected += 1;
                    }
                    None => {
                        spins += 1;
                        if spins.is_multiple_of(1024) {
                            thread::yield_now();
                        }
                    }
                }
            }
            assert_eq!(rx.pop(), None);
        });
        producer.join().unwrap();
        consumer.join().unwrap();
    });
    assert!(q.is_empty());
}

#[test]
fn two_thread_stress() {
    stress::<1>(20_000);
    stress::<3>(100_000);
    stress::<64>(300_000);
}
