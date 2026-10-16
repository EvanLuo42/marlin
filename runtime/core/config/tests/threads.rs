//! Concurrent reads and writes from std threads.

use std::thread;

use marlin_config::{CvarI64, CvarString, Source};

static COUNTER: CvarI64 = CvarI64::new("mt.counter", 0, "");
static TEXT: CvarString = CvarString::new("mt.text", "", "");

const WRITERS: usize = 4;
const WRITES: i64 = 2000;

#[test]
fn concurrent_access() {
    thread::scope(|s| {
        for w in 0..WRITERS as i64 {
            s.spawn(move || {
                for i in 0..WRITES {
                    // Writers own disjoint value ranges, so every read is one a writer stored.
                    COUNTER.set(w * WRITES + i);
                    TEXT.set_str(&format!("{w}:{i}"), Source::Console).unwrap();
                }
            });
        }
        for _ in 0..4 {
            s.spawn(|| {
                let mut last = 0;
                for _ in 0..WRITES {
                    let v = COUNTER.get();
                    assert!((0..WRITERS as i64 * WRITES).contains(&v));
                    TEXT.with(|t| {
                        assert!(t.is_empty() || t.split_once(':').is_some(), "torn: {t}");
                    });
                    let g = COUNTER.generation();
                    assert!(g >= last, "generation went backwards");
                    last = g;
                }
            });
        }
    });
    let v = COUNTER.get();
    assert_eq!(v % WRITES, WRITES - 1, "each writer's last value");
    assert!(COUNTER.generation() > 0);
}
