use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use marlin_lang_query::{Cancelled, Cx, Db, Durability, Input, Query};
use marlin_test_support::{wait_until, with_timeout};

struct Mode;
impl Input for Mode {
    type Key = ();
    type Value = &'static str;
}

static SPINNING: AtomicUsize = AtomicUsize::new(0);

/// While `Mode` is "spin", runs until cancelled, as a long computation that checks in its loop.
struct Spin;
impl Query for Spin {
    type Key = ();
    type Value = u32;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<u32, Cancelled> {
        if cx.input::<Mode>(&())? != "spin" {
            return Ok(42);
        }
        SPINNING.fetch_add(1, Ordering::SeqCst);
        loop {
            cx.check_cancelled()?;
            std::hint::spin_loop();
        }
    }
}

/// Reads `Spin` through another query, so that cancellation has to travel up with `?`.
struct Outer;
impl Query for Outer {
    type Key = ();
    type Value = u32;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<u32, Cancelled> {
        Ok(cx.get::<Spin>(&())? + 1)
    }
}

#[test]
fn an_input_change_cancels_running_and_waiting_reads() {
    with_timeout(Duration::from_secs(30), "cancellation", || {
        let db = Arc::new(Db::new());
        db.set::<Mode>((), "spin", Durability::Low);

        // One thread runs the query; a second waits for the same key.
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let db = db.clone();
                std::thread::spawn(move || db.snapshot().get::<Outer>(&()))
            })
            .collect();
        wait_until(
            Duration::from_secs(10),
            "the query to start spinning",
            || SPINNING.load(Ordering::SeqCst) == 1,
        );

        // Returns only after both readers have given up their snapshots.
        db.set::<Mode>((), "done", Durability::Low);
        for reader in readers {
            assert_eq!(reader.join().unwrap(), Err(Cancelled));
        }
        assert_eq!(SPINNING.load(Ordering::SeqCst), 1, "computed once");

        // The next request sees the new input.
        assert_eq!(db.snapshot().get::<Outer>(&()), Ok(43));
    });
}

#[test]
fn snapshots_taken_after_a_change_see_it() {
    let db = Db::new();
    db.set::<Mode>((), "a", Durability::Low);
    assert_eq!(db.snapshot().input::<Mode>(&()), Ok("a"));
    db.set::<Mode>((), "b", Durability::Low);
    let snapshot = db.snapshot();
    assert_eq!(snapshot.input::<Mode>(&()), Ok("b"));
    assert_eq!(snapshot.check_cancelled(), Ok(()));
}
