//! SecondaryMap keyed by SlotMap handles.

use std::alloc::{AllocError, Allocator, Global, Layout};
use std::collections::HashMap;
use std::ptr::NonNull;

use marlin_containers::{Handle, SecondaryMap, SlotMap};
use marlin_math::Rng;
use marlin_test_support::{Tracker, scale};

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
    for step in 0..scale(5_000) {
        match rng.below(5) {
            0 => handles.push(slots.insert(0u32)),
            1 if !handles.is_empty() => {
                let h = handles.swap_remove(rng.below(handles.len()));
                slots.remove(h);
                // Values of removed handles linger until the slot is reused or removed here.
                if rng.below(2) == 0 {
                    assert_eq!(
                        extra
                            .remove(h)
                            .map(|v: marlin_test_support::Tracked| v.value),
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

/// Refuses blocks over 1 MiB, so a test asking for a huge slot array fails fast instead of
/// committing gigabytes.
#[derive(Clone, Copy)]
struct Capped;

// SAFETY: forwards to `Global`, which upholds the contract.
unsafe impl Allocator for Capped {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() > 1 << 20 {
            return Err(AllocError);
        }
        Global.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded from the caller; the block came from `Global`.
        unsafe { Global.deallocate(ptr, layout) }
    }
}

#[test]
fn foreign_handles_never_abort() {
    let bogus = |index: u32| Handle::<&str>::from_bits(1 << 32 | u64::from(index));
    // `u32::MAX` is never issued: nothing is allocated for it, even with the global heap.
    let mut global: SecondaryMap<&str, u64> = SecondaryMap::new();
    assert_eq!(global.insert(bogus(u32::MAX), 1), None);
    assert!(
        global
            .get_or_insert_with(bogus(u32::MAX), || unreachable!())
            .is_none()
    );
    assert!(global.is_empty());

    // An index too far to allocate for inserts nothing; the map keeps working.
    let mut slots = SlotMap::new();
    let a = slots.insert("a");
    let mut capped: SecondaryMap<&str, u64, Capped> = SecondaryMap::new_in(Capped);
    assert_eq!(capped.insert(a, 1), None);
    for index in [u32::MAX - 1, 1 << 28] {
        assert_eq!(capped.insert(bogus(index), 2), None);
        assert!(
            capped
                .get_or_insert_with(bogus(index), || unreachable!())
                .is_none()
        );
        assert_eq!(capped.get(bogus(index)), None);
    }
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[a], 1);
}
