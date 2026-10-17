mod common;

use std::collections::HashMap;

use common::{Rng, Tracker};
use marlin_containers::SparseSet;

#[test]
fn insert_get_remove() {
    let mut s = SparseSet::new();
    assert_eq!(s.insert(5, "five"), None);
    assert_eq!(s.insert(1_000_000, "big"), None);
    assert_eq!(s.insert(5, "FIVE"), Some("five"));
    assert_eq!(s.len(), 2);
    assert_eq!(s.get(5), Some(&"FIVE"));
    assert_eq!(s.get(1_000_000), Some(&"big"));
    assert_eq!(s.get(6), None);
    assert_eq!(s.get(u32::MAX - 1), None);
    assert_eq!(s.remove(5), Some("FIVE"));
    assert_eq!(s.remove(5), None);
    assert_eq!(s.keys(), &[1_000_000]);
}

#[test]
fn removal_keeps_values_dense() {
    let mut s = SparseSet::new();
    for k in [10, 20, 30, 40] {
        s.insert(k, k * 2);
    }
    s.remove(20);
    // The last value moved into the hole.
    assert_eq!(s.keys(), &[10, 40, 30]);
    assert_eq!(s.values(), &[20, 80, 60]);
    assert_eq!(s.dense_index(40), Some(1));
    s.swap_dense(0, 2);
    assert_eq!(s.keys(), &[30, 40, 10]);
    assert_eq!(s.get(10), Some(&20));
    assert_eq!(s.get(30), Some(&60));
    let pairs: Vec<_> = s.iter().map(|(k, &v)| (k, v)).collect();
    assert_eq!(pairs, vec![(30, 60), (40, 80), (10, 20)]);
    s.clear();
    assert!(s.is_empty());
    assert!(!s.contains(10));
}

#[test]
fn matches_model_under_random_operations() {
    let mut rng = Rng::new(3);
    let mut s = SparseSet::new();
    let mut model = HashMap::new();
    for step in 0..20_000u64 {
        // Small key space so inserts and removes collide often; a few far-away keys.
        let key = if rng.below(50) == 0 {
            rng.next_u64() as u32 % 5_000_000
        } else {
            rng.below(500) as u32
        };
        if rng.below(2) == 0 {
            assert_eq!(s.insert(key, step), model.insert(key, step));
        } else {
            assert_eq!(s.remove(key), model.remove(&key));
        }
        assert_eq!(s.len(), model.len());
    }
    for (&k, v) in &model {
        assert_eq!(s.get(k), Some(v));
    }
    for (k, v) in s.iter() {
        assert_eq!(model.get(&k), Some(v));
    }
}

#[test]
fn drops_every_value_exactly_once() {
    let t = Tracker::new();
    {
        let mut s = SparseSet::new();
        for k in 0..100 {
            s.insert(k * 7, t.make(k));
        }
        drop(s.insert(0, t.make(1000)));
        drop(s.remove(14));
        assert_eq!(t.live(), 99);
        s.clear();
        t.assert_all_dropped();
        s.insert(3, t.make(3));
    }
    t.assert_all_dropped();
}
