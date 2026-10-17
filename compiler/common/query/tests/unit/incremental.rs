use marlin_lang_query::{Cancelled, Cx, Db, Durability, Input, Query, Stats};

/// `Text(file)`: a source file's text.
struct Text;
impl Input for Text {
    type Key = u32;
    type Value = String;
}

/// `Len(file)`: the text's length.
struct Len;
impl Query for Len {
    type Key = u32;
    type Value = usize;
    fn execute(cx: &Cx<'_>, file: &u32) -> Result<usize, Cancelled> {
        Ok(cx.input::<Text>(file)?.len())
    }
}

/// `Total`: the summed length of files 0..3.
struct Total;
impl Query for Total {
    type Key = ();
    type Value = usize;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<usize, Cancelled> {
        let mut total = 0;
        for file in 0..3 {
            total += cx.get::<Len>(&file)?;
        }
        Ok(total)
    }
}

fn db_with_files() -> Db {
    let db = Db::new();
    db.update(|u| {
        for (file, text) in ["a", "bb", "ccc"].into_iter().enumerate() {
            u.set::<Text>(file as u32, text.into(), Durability::Low);
        }
    });
    db
}

fn total(db: &Db) -> usize {
    db.snapshot().get::<Total>(&()).unwrap()
}

#[test]
fn values_are_memoized_within_a_revision() {
    let db = db_with_files();
    assert_eq!(total(&db), 6);
    assert_eq!(db.stats().executions, 4, "Total and three Len");
    assert_eq!(total(&db), 6);
    assert_eq!(db.stats().executions, 4, "nothing recomputed");
}

#[test]
fn a_change_recomputes_only_what_depends_on_it() {
    let db = db_with_files();
    total(&db);
    db.set::<Text>(1, "bbbb".into(), Durability::Low);
    let before = db.stats();
    assert_eq!(total(&db), 8);
    assert_eq!(
        db.stats().executions - before.executions,
        2,
        "Len(1) and Total; Len(0) and Len(2) are verified, not recomputed"
    );
}

#[test]
fn an_equal_result_stops_recomputation_from_spreading() {
    let db = db_with_files();
    total(&db);
    // Same length: Len(1) is recomputed, finds the same value, and Total stays green.
    db.set::<Text>(1, "xy".into(), Durability::Low);
    let before = db.stats();
    assert_eq!(total(&db), 6);
    assert_eq!(db.stats().executions - before.executions, 1, "only Len(1)");
}

#[test]
fn setting_an_equal_value_changes_nothing() {
    let db = db_with_files();
    total(&db);
    let revision = db.revision();
    db.set::<Text>(0, "a".into(), Durability::Low);
    assert_eq!(db.revision(), revision, "no new revision");
    let before = db.stats();
    total(&db);
    assert_eq!(db.stats(), before, "no work at all");
}

/// `Std(name)`: a rarely changing library input.
struct Std;
impl Input for Std {
    type Key = u32;
    type Value = u32;
}

/// Reads only `Std` inputs, through one more query.
struct StdSum;
impl Query for StdSum {
    type Key = ();
    type Value = u32;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<u32, Cancelled> {
        Ok(cx.get::<StdItem>(&0)? + cx.get::<StdItem>(&1)?)
    }
}

struct StdItem;
impl Query for StdItem {
    type Key = u32;
    type Value = u32;
    fn execute(cx: &Cx<'_>, key: &u32) -> Result<u32, Cancelled> {
        cx.input::<Std>(key)
    }
}

#[test]
fn memos_of_high_durability_inputs_skip_verification_after_low_changes() {
    let db = db_with_files();
    db.update(|u| {
        u.set::<Std>(0, 10, Durability::High);
        u.set::<Std>(1, 20, Durability::High);
    });
    assert_eq!(db.snapshot().get::<StdSum>(&()).unwrap(), 30);

    db.set::<Text>(0, "changed".into(), Durability::Low);
    let before = db.stats();
    assert_eq!(db.snapshot().get::<StdSum>(&()).unwrap(), 30);
    assert_eq!(
        db.stats(),
        before,
        "a Low change does not even walk the dependencies of a High memo"
    );

    db.set::<Std>(1, 21, Durability::High);
    assert_eq!(db.snapshot().get::<StdSum>(&()).unwrap(), 31);
}

#[test]
fn lowering_an_input_durability_invalidates_its_readers() {
    let db = Db::new();
    db.update(|u| {
        u.set::<Std>(0, 1, Durability::High);
        u.set::<Std>(1, 2, Durability::High);
    });
    assert_eq!(db.snapshot().get::<StdSum>(&()).unwrap(), 3);
    // Same value, now Low. StdSum stays green when verified, but must take over the lower
    // durability, or the next Low change would skip it.
    db.set::<Std>(0, 1, Durability::Low);
    assert_eq!(db.snapshot().get::<StdSum>(&()).unwrap(), 3);
    db.set::<Std>(0, 5, Durability::Low);
    assert_eq!(db.snapshot().get::<StdSum>(&()).unwrap(), 7);
}

/// Reads `Text(1)` only when `Text(0)` is "a": dependencies can differ between revisions.
struct Conditional;
impl Query for Conditional {
    type Key = ();
    type Value = String;
    fn execute(cx: &Cx<'_>, _: &()) -> Result<String, Cancelled> {
        let first = cx.input::<Text>(&0)?;
        if first == "a" {
            Ok(first + &cx.input::<Text>(&1)?)
        } else {
            Ok(first)
        }
    }
}

#[test]
fn dependencies_follow_what_the_last_execution_read() {
    let db = db_with_files();
    let get = |db: &Db| db.snapshot().get::<Conditional>(&()).unwrap();
    assert_eq!(get(&db), "abb");
    db.set::<Text>(0, "z".into(), Durability::Low);
    assert_eq!(get(&db), "z");
    // Text(1) is no longer read, so changing it reuses the memo.
    db.set::<Text>(1, "qq".into(), Durability::Low);
    let before = db.stats();
    assert_eq!(get(&db), "z");
    assert_eq!(db.stats().executions, before.executions);
    db.set::<Text>(0, "a".into(), Durability::Low);
    assert_eq!(get(&db), "aqq");
}

#[test]
fn stats_start_at_zero() {
    assert_eq!(Db::new().stats(), Stats::default());
}

#[test]
fn reading_an_unset_input_is_a_bug() {
    let db = Db::new();
    marlin_test_support::assert_panics_with("input Text(7) was read before it was set", || {
        let _ = db.snapshot().input::<Text>(&7);
    });
}

#[test]
fn updating_while_holding_a_snapshot_is_a_bug() {
    let db = db_with_files();
    let snapshot = db.snapshot();
    marlin_test_support::assert_panics_with("while this thread holds a Snapshot", || {
        db.set::<Text>(0, "x".into(), Durability::Low);
    });
    drop(snapshot);
    db.set::<Text>(0, "x".into(), Durability::Low);
}
