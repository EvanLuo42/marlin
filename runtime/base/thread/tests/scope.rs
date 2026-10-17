use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use marlin_thread::{Builder, ThreadPriority, current, scope, sleep};

#[test]
fn borrows_immutably() {
    let data = [1u64, 2, 3, 4];
    let name = String::from("shared");
    let (a, b) = scope(|s| {
        let a = s.spawn(|| data.iter().sum::<u64>());
        let b = s.spawn(|| data.len() + name.len());
        (a.join(), b.join())
    });
    assert_eq!((a, b), (10, 10));
    // Still ours after the scope.
    assert_eq!(data.len(), 4);
}

#[test]
fn borrows_mutably() {
    let mut numbers = vec![1, 2, 3];
    let mut total = 0;
    scope(|s| {
        s.spawn(|| numbers.push(4));
        s.spawn(|| total += 10);
    });
    assert_eq!(numbers, [1, 2, 3, 4]);
    assert_eq!(total, 10);
}

#[test]
fn disjoint_chunks_in_parallel() {
    let mut buf = vec![0u32; 64 * 16];
    scope(|s| {
        for (i, chunk) in buf.chunks_mut(16).enumerate() {
            s.spawn(move || chunk.fill(i as u32));
        }
    });
    for (i, chunk) in buf.chunks(16).enumerate() {
        assert!(chunk.iter().all(|&v| v == i as u32), "chunk {i}");
    }
}

#[test]
fn unjoined_threads_are_joined_at_scope_end() {
    let done = AtomicUsize::new(0);
    scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                sleep(Duration::from_millis(10));
                done.fetch_add(1, Ordering::SeqCst);
            });
        }
    });
    assert_eq!(done.load(Ordering::SeqCst), 8);
}

#[test]
fn returns_the_closure_value() {
    let words = ["a", "bb", "ccc"];
    let lens: Vec<usize> = scope(|s| {
        let handles: Vec<_> = words.iter().map(|w| s.spawn(move || w.len())).collect();
        handles.into_iter().map(|h| h.join()).collect()
    });
    assert_eq!(lens, [1, 2, 3]);
    // Results may borrow from the environment too.
    let text = String::from("hello world");
    let first: &str = scope(|s| s.spawn(|| text.split(' ').next().unwrap()).join());
    assert_eq!(first, "hello");
}

#[test]
fn nested_spawns() {
    let counter = AtomicUsize::new(0);
    let mut slots = [0usize; 4];
    scope(|s| {
        for slot in &mut slots {
            let counter = &counter;
            s.spawn(move || {
                // Grandchildren join the same scope; nobody joins them explicitly.
                for _ in 0..4 {
                    s.spawn(move || counter.fetch_add(1, Ordering::SeqCst));
                }
                *slot = s.spawn(|| 7).join();
            });
        }
    });
    assert_eq!(counter.load(Ordering::SeqCst), 16);
    assert_eq!(slots, [7; 4]);
}

#[test]
fn nested_scopes() {
    let mut outer = 0;
    scope(|s| {
        s.spawn(|| {
            let mut inner = [0; 2];
            scope(|t| {
                for (i, v) in inner.iter_mut().enumerate() {
                    t.spawn(move || *v = i + 1);
                }
            });
            outer = inner.iter().sum();
        });
    });
    assert_eq!(outer, 3);
}

#[test]
fn many_threads() {
    let values: Vec<u64> = (0..64).collect();
    let sum = scope(|s| {
        let handles: Vec<_> = values.iter().map(|v| s.spawn(move || v * v)).collect();
        handles.into_iter().map(|h| h.join()).sum::<u64>()
    });
    assert_eq!(sum, (0..64u64).map(|i| i * i).sum());
}

#[test]
fn builder_settings_apply() {
    let input = [1u8; 16];
    let result = scope(|s| {
        // Larger than the 1 MiB Windows default and the 512 KiB macOS secondary-thread default.
        let big = Builder::new()
            .name("scoped-big")
            .stack_size(8 << 20)
            .spawn_scoped(s, || {
                let mut buf = [0u8; 2 << 20];
                buf[..input.len()].copy_from_slice(&input);
                black_box(&mut buf);
                buf.iter().map(|&b| b as usize).sum::<usize>()
            })
            .unwrap();
        let low = Builder::new()
            .name("scoped-low")
            .priority(ThreadPriority::Low)
            .spawn_scoped(s, || input.len())
            .unwrap();
        big.join() + low.join()
    });
    assert_eq!(result, 32);
}

#[cfg(not(target_vendor = "apple"))]
#[test]
fn failed_setup_is_not_counted() {
    use marlin_thread::{CpuSet, ErrorKind};
    let ran = AtomicBool::new(false);
    scope(|s| {
        // No machine has CPU 1023 available to this process.
        let result = Builder::new()
            .affinity(CpuSet::single(CpuSet::CAPACITY as u32 - 1))
            .spawn_scoped(s, || ran.store(true, Ordering::SeqCst));
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
    });
    // The scope returned, so the failed thread did not leave the count raised.
    assert!(!ran.load(Ordering::SeqCst));
}

#[test]
fn threads_are_distinct() {
    let main = current::id();
    let other = scope(|s| s.spawn(current::id).join());
    assert_ne!(main, other);
}

#[test]
fn handle_joined_from_another_scoped_thread() {
    let text = String::from("inner");
    let joined = scope(|s| {
        let inner = s.spawn(|| text.as_str());
        s.spawn(move || inner.join()).join()
    });
    assert_eq!(joined, "inner");
}

struct CountDrop<'a>(&'a AtomicUsize);

impl Drop for CountDrop<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn unjoined_results_are_dropped_inside_the_scope() {
    let drops = AtomicUsize::new(0);
    scope(|s| {
        for _ in 0..4 {
            let _ = s.spawn(|| CountDrop(&drops));
        }
        // Dropped before the threads finish.
        let _ = s.spawn(|| {
            sleep(Duration::from_millis(10));
            CountDrop(&drops)
        });
    });
    assert_eq!(drops.load(Ordering::SeqCst), 5);
}

#[test]
fn unwinding_scope_waits_for_threads() {
    let finished = AtomicBool::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| {
        scope(|s| {
            s.spawn(|| {
                sleep(Duration::from_millis(20));
                finished.store(true, Ordering::SeqCst);
            });
            panic!("scope body panicked");
        })
    }));
    assert!(result.is_err());
    // The unwind did not leave the scope until the thread was done with `finished`.
    assert!(finished.load(Ordering::SeqCst));
}

#[test]
fn empty_scope_returns() {
    assert_eq!(scope(|_| 5), 5);
}
