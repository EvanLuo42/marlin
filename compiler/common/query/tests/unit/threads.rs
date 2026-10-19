use std::sync::{Arc, Barrier};
use std::time::Duration;

use marlin_lang_query::{Cancelled, Cx, Db, Durability, Input, Query};
use marlin_math::Rng;
use marlin_test_support::{scale, seed, with_timeout};

use crate::model::{INPUTS, NODES, Node, random_db, reference};

struct Seed;
impl Input for Seed {
    type Key = ();
    type Value = u64;
}

/// Deliberately slow, so that the other threads ask while it runs.
struct Slow;
impl Query for Slow {
    type Key = ();
    type Value = u64;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<u64, Cancelled> {
        let mut x = cx.input::<Seed>(&())?;
        for _ in 0..200_000 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
        }
        Ok(x)
    }
}

#[test]
fn a_key_asked_by_many_threads_is_computed_once() {
    with_timeout(Duration::from_secs(30), "threads sharing one query", || {
        let db = Arc::new(Db::new());
        db.set::<Seed>((), 7, Durability::Low);
        let start = Arc::new(Barrier::new(8));
        let results: Vec<u64> = (0..8)
            .map(|_| {
                let (db, start) = (db.clone(), start.clone());
                std::thread::spawn(move || {
                    let snapshot = db.snapshot();
                    start.wait();
                    snapshot.get::<Slow>(&()).unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        assert!(results.windows(2).all(|w| w[0] == w[1]), "{results:?}");
        assert_eq!(db.stats().executions, 1);
    });
}

#[test]
fn concurrent_readers_agree_with_direct_evaluation() {
    let seed = seed(0x0de1_0002);
    with_timeout(
        Duration::from_secs(60),
        "concurrent model readers",
        move || {
            let mut rng = Rng::new(seed);
            // Many rounds: a thread that finishes a key and moves on to one its waiter holds
            // must not see a cycle through that waiter's stale wait.
            for round in 0..scale(40) {
                let (db, inputs) = random_db(&mut rng);
                let db = Arc::new(db);
                let expected: Vec<u64> = (0..NODES).map(|i| reference(&inputs, i)).collect();
                let start = Arc::new(Barrier::new(8));
                let threads: Vec<_> = (0..8)
                    .map(|t| {
                        let (db, start) = (db.clone(), start.clone());
                        std::thread::spawn(move || {
                            let snapshot = db.snapshot();
                            start.wait();
                            // Each thread walks the nodes in a different order.
                            (0..NODES)
                                .map(|n| {
                                    let i = (n * (2 * t + 1) + t) % NODES;
                                    (i, snapshot.get::<Node>(&i).unwrap())
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                for t in threads {
                    for (i, value) in t.join().unwrap() {
                        assert_eq!(
                            value, expected[i as usize],
                            "seed {seed}, round {round}, node {i}"
                        );
                    }
                }
                assert!(
                    db.stats().executions <= u64::from(NODES),
                    "each node computed at most once: {:?} ({INPUTS} inputs)",
                    db.stats()
                );
            }
        },
    );
}
