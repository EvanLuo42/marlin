mod common;

use std::collections::HashMap;

use common::{Rng, Tracker};
use marlin_containers::{Handle, SlotMap};

#[test]
fn insert_get_remove() {
    let mut m = SlotMap::new();
    let a = m.insert("a");
    let b = m.insert("b");
    assert_eq!(m.len(), 2);
    assert_eq!(m.get(a), Some(&"a"));
    assert_eq!(m[b], "b");
    *m.get_mut(b).unwrap() = "B";
    assert_eq!(m.remove(b), Some("B"));
    assert_eq!(m.remove(b), None);
    assert_eq!(m.get(b), None);
    assert!(!m.contains(b));
    assert_eq!(m.len(), 1);
}

#[test]
fn stale_handles_miss_after_slot_reuse() {
    let mut m = SlotMap::new();
    let old = m.insert(1);
    m.remove(old);
    let new = m.insert(2);
    assert_eq!(new.index(), old.index(), "freed slot is reused");
    assert_ne!(new.generation(), old.generation());
    assert_eq!(m.get(old), None);
    assert_eq!(m.get_mut(old), None);
    assert_eq!(m.remove(old), None);
    assert_eq!(m.get(new), Some(&2));
}

#[test]
fn null_and_foreign_handles_never_match() {
    let mut m = SlotMap::new();
    let h = m.insert(5u8);
    assert!(Handle::<u8>::NULL.is_null());
    assert_eq!(Handle::<u8>::default(), Handle::NULL);
    assert_eq!(m.get(Handle::NULL), None);
    assert_eq!(m.get(Handle::from_bits(h.to_bits() + (1 << 32))), None);
    assert_eq!(m.get(Handle::from_bits(12345)), None);
    assert_eq!(Handle::<u8>::from_bits(h.to_bits()), h);
    assert_eq!(format!("{h:?}"), "Handle(0v1)");
}

#[test]
fn insert_with_handle_sees_its_own_handle() {
    struct Node {
        me: Handle<Node>,
    }
    let mut m = SlotMap::new();
    let a = m.insert_with_handle(|me| Node { me });
    m.remove(a);
    let b = m.insert_with_handle(|me| Node { me });
    assert!(m.get(a).is_none());
    assert!(m[b].me == b);
}

#[test]
fn clear_and_retain_invalidate() {
    let mut m = SlotMap::new();
    let hs: Vec<_> = (0..10).map(|i| m.insert(i)).collect();
    m.retain(|_, v| *v % 2 == 0);
    assert_eq!(m.len(), 5);
    for (i, &h) in hs.iter().enumerate() {
        assert_eq!(m.get(h).is_some(), i % 2 == 0);
    }
    m.clear();
    assert!(m.is_empty());
    assert!(hs.iter().all(|&h| m.get(h).is_none()));
    // All slots are reused, all with new generations.
    let again: Vec<_> = (0..10).map(|i| m.insert(i)).collect();
    assert!(again.iter().all(|h| h.index() < 10 && h.generation() > 1));
}

#[test]
fn iteration_visits_live_values() {
    let mut m = SlotMap::new();
    let hs: Vec<_> = (0..6).map(|i| m.insert(i * 10)).collect();
    m.remove(hs[1]);
    m.remove(hs[4]);
    let got: Vec<_> = m.iter().map(|(h, &v)| (h, v)).collect();
    assert_eq!(got, vec![(hs[0], 0), (hs[2], 20), (hs[3], 30), (hs[5], 50)]);
    for v in m.values_mut() {
        *v += 1;
    }
    assert_eq!(m.values().copied().collect::<Vec<_>>(), vec![1, 21, 31, 51]);
    assert_eq!(m.handles().count(), 4);
}

#[test]
#[should_panic(expected = "stale")]
fn indexing_with_stale_handle_panics() {
    let mut m = SlotMap::new();
    let h = m.insert(1);
    m.remove(h);
    let _ = m[h];
}

#[test]
fn matches_model_under_random_operations() {
    let mut rng = Rng::new(11);
    let mut m = SlotMap::new();
    let mut model: HashMap<u64, u64> = HashMap::new();
    let mut issued: Vec<Handle<u64>> = Vec::new();
    for step in 0..20_000u64 {
        if rng.below(3) != 0 || issued.is_empty() {
            let h = m.insert(step);
            assert!(model.insert(h.to_bits(), step).is_none(), "handle reissued");
            issued.push(h);
        } else {
            // Mix live and stale handles.
            let h = issued[rng.below(issued.len())];
            assert_eq!(m.remove(h), model.remove(&h.to_bits()));
        }
        assert_eq!(m.len(), model.len());
    }
    for h in issued {
        assert_eq!(m.get(h), model.get(&h.to_bits()));
    }
}

#[test]
fn drops_every_value_exactly_once() {
    let t = Tracker::new();
    {
        let mut m = SlotMap::new();
        let hs: Vec<_> = (0..50).map(|i| m.insert(t.make(i))).collect();
        drop(m.remove(hs[3]));
        assert!(m.remove(hs[3]).is_none());
        m.retain(|_, v| v.value % 3 != 0);
        assert_eq!(t.live(), m.len());
        let c = m.clone();
        assert_eq!(c.get(hs[1]).map(|v| v.value), Some(1));
        drop(c);
        m.insert(t.make(100));
        assert_eq!(t.live(), m.len());
    }
    t.assert_all_dropped();

    let mut m = SlotMap::new();
    for i in 0..10 {
        m.insert(t.make(i));
    }
    m.clear();
    t.assert_all_dropped();
}
