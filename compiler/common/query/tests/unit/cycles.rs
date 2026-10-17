use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use marlin_lang_query::{Cancelled, Cx, Cycle, Db, Durability, Input, Query};
use marlin_test_support::{assert_panics_with, with_timeout};

struct Edges;
impl Input for Edges {
    type Key = u32;
    /// The node this one reads, if any.
    type Value = Option<u32>;
}

static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Follows `Edges` and counts the steps; a cycle yields `u32::MAX` at the read that closes it.
struct Depth;
impl Query for Depth {
    type Key = u32;
    type Value = u32;
    fn execute(cx: &Cx<'_>, node: &u32) -> Result<u32, Cancelled> {
        Ok(match cx.input::<Edges>(node)? {
            Some(next) => cx.get::<Depth>(&next)?.saturating_add(1),
            None => 0,
        })
    }
    fn cycle_fallback(cycle: &Cycle, _: &u32) -> Option<u32> {
        SEEN.lock().unwrap().push(cycle.to_string());
        Some(u32::MAX)
    }
}

#[test]
fn a_cycle_through_a_query_with_a_fallback_recovers() {
    let db = Db::new();
    db.update(|u| {
        u.set::<Edges>(0, Some(1), Durability::Low);
        u.set::<Edges>(1, Some(2), Durability::Low);
        u.set::<Edges>(2, Some(0), Durability::Low);
    });
    assert_eq!(db.snapshot().get::<Depth>(&0), Ok(u32::MAX));
    assert_eq!(
        SEEN.lock().unwrap().as_slice(),
        ["Depth(0) -> Depth(1) -> Depth(2) -> Depth(0)"]
    );

    // Breaking the cycle gives real answers again.
    db.set::<Edges>(2, None, Durability::Low);
    let snapshot = db.snapshot();
    assert_eq!(snapshot.get::<Depth>(&0), Ok(2));
    assert_eq!(snapshot.get::<Depth>(&1), Ok(1));
}

struct Ping;
impl Query for Ping {
    type Key = ();
    type Value = u32;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<u32, Cancelled> {
        cx.get::<Pong>(&())
    }
}

struct Pong;
impl Query for Pong {
    type Key = ();
    type Value = u32;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<u32, Cancelled> {
        cx.get::<Ping>(&())
    }
}

#[test]
fn a_cycle_without_a_fallback_is_a_bug_that_names_the_cycle() {
    let db = Db::new();
    assert_panics_with("query cycle: Ping(()) -> Pong(()) -> Ping(())", || {
        let _ = db.snapshot().get::<Ping>(&());
    });
    // The panic released its claims: the database is still usable.
    db.set::<Edges>(0, None, Durability::Low);
    assert_eq!(db.snapshot().get::<Depth>(&0), Ok(0));
}

static MEET: Barrier = Barrier::new(2);

/// `Left` and `Right` read each other, each on its own thread, after both have started.
struct Left;
impl Query for Left {
    type Key = ();
    type Value = &'static str;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<&'static str, Cancelled> {
        MEET.wait();
        cx.get::<Right>(&())
    }
    fn cycle_fallback(cycle: &Cycle, _: &()) -> Option<&'static str> {
        assert!(cycle.across_threads, "{cycle}");
        Some("left fallback")
    }
}

struct Right;
impl Query for Right {
    type Key = ();
    type Value = &'static str;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<&'static str, Cancelled> {
        MEET.wait();
        cx.get::<Left>(&())
    }
    fn cycle_fallback(cycle: &Cycle, _: &()) -> Option<&'static str> {
        assert!(cycle.across_threads, "{cycle}");
        Some("right fallback")
    }
}

#[test]
fn a_cycle_across_threads_is_detected_instead_of_deadlocking() {
    with_timeout(Duration::from_secs(30), "cross-thread cycle", || {
        let db = Arc::new(Db::new());
        let left = {
            let db = db.clone();
            std::thread::spawn(move || db.snapshot().get::<Left>(&()))
        };
        let right = {
            let db = db.clone();
            std::thread::spawn(move || db.snapshot().get::<Right>(&()))
        };
        let (left, right) = (left.join().unwrap(), right.join().unwrap());
        // Whichever thread closed the cycle got the other query's fallback; both agree.
        assert_eq!(left, right);
        assert!(
            matches!(left, Ok("left fallback" | "right fallback")),
            "{left:?}"
        );
    });
}
