//! SecondaryMap keyed by SlotMap handles.

mod common;

use std::collections::HashMap;

use common::{Rng, Tracker};
use marlin_containers::{Handle, SecondaryMap, SlotMap};

#[test]
fn attaches_values_to_handles() {
    let mut slots = SlotMap::new();
    let a = slots.insert("a");
    let b = slots.insert("b");
    let mut extra = SecondaryMap::new();
    assert_eq!(extra.insert(a, 1), None);
    assert_eq!(extra.insert(a, 2), Some(1));
    assert_eq!(extra.get(b), None);
    *extra.get_or_insert_with(b, || 10).unwrap() += 1;
    assert_eq!(extra[b], 11);
    assert_eq!(extra.len(), 2);
    assert_eq!(extra.remove(a), Some(2));
    assert_eq!(extra.remove(a), None);
    assert_eq!(extra.handles().collect::<Vec<_>>(), [b]);
    assert_eq!(extra.insert(Handle::NULL, 5), None);
    assert!(extra.get_or_insert_with(Handle::NULL, || 5).is_none());
    assert_eq!(extra.len(), 1);
}

#[test]
fn stale_handles_never_see_newer_values() {
    let mut slots = SlotMap::new();
    let old = slots.insert(1);
    let mut extra = SecondaryMap::new();
    extra.insert(old, "old");
    slots.remove(old);
    let new = slots.insert(2);
    assert_eq!(new.index(), old.index());
    // The newer handle replaces the older occupant's value.
    assert_eq!(extra.get(new), None);
    assert_eq!(extra.insert(new, "new"), None);
    assert_eq!(extra.len(), 1);
    assert_eq!(extra.get(old), None);
    // An older handle cannot overwrite it.
    assert_eq!(extra.insert(old, "stale"), None);
    assert!(extra.get_or_insert_with(old, || "stale").is_none());
    assert_eq!(extra[new], "new");
    assert_eq!(extra.remove(old), None);
    assert_eq!(extra.len(), 1);
}

#[test]
fn matches_a_hash_map_model() {
    let t = Tracker::new();
    let mut rng = Rng::new(11);
    let mut slots = SlotMap::new();
    let mut handles = Vec::new();
    let mut extra = SecondaryMap::new();
    let mut model: HashMap<Handle<u32>, u32> = HashMap::new();
    for step in 0..5_000 {
        match rng.below(5) {
            0 => handles.push(slots.insert(0u32)),
            1 if !handles.is_empty() => {
                let h = handles.swap_remove(rng.below(handles.len()));
                slots.remove(h);
                // Values of removed handles linger until the slot is reused or removed here.
                if rng.below(2) == 0 {
                    assert_eq!(
                        extra.remove(h).map(|v: common::Tracked| v.value),
                        model.remove(&h)
                    );
                } else {
                    model.remove(&h);
                }
            }
            2 | 3 if !handles.is_empty() => {
                let h = handles[rng.below(handles.len())];
                let v = rng.next_u64() as u32;
                let old = extra.insert(h, t.make(v)).map(|x| x.value);
                assert_eq!(old, model.insert(h, v), "step {step}");
            }
            _ => {
                extra.retain(|h, _| slots.contains(h));
            }
        }
        for (h, v) in &model {
            assert_eq!(extra.get(*h).map(|x| x.value), Some(*v));
        }
    }
    extra.retain(|h, _| slots.contains(h));
    assert_eq!(extra.len(), model.len());
    drop(extra);
    t.assert_all_dropped();
}
