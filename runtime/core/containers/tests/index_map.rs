//! IndexMap / IndexSet against an ordered Vec model.

mod common;

use common::{Rng, Tracker};
use marlin_containers::{IndexMap, IndexSet};

#[test]
fn keeps_insertion_order() {
    let mut m = IndexMap::new();
    assert_eq!(m.insert("c", 1), None);
    m.insert("a", 2);
    m.insert("b", 3);
    assert_eq!(m.insert("a", 20), Some(2), "replacing keeps the position");
    assert_eq!(m.keys().copied().collect::<Vec<_>>(), ["c", "a", "b"]);
    assert_eq!(m.get_index(1), Some((&"a", &20)));
    assert_eq!(m.get_index_of("b"), Some(2));
    assert_eq!(m.get_full("b"), Some((2, &"b", &3)));
    assert_eq!(m["c"], 1);
    m["c"] += 5;
    assert_eq!(m.first(), Some((&"c", &6)));
    assert_eq!(m.last(), Some((&"b", &3)));
    assert_eq!(format!("{m:?}"), r#"{"c": 6, "a": 20, "b": 3}"#);

    assert_eq!(m.shift_remove("c"), Some(6));
    assert_eq!(m.keys().copied().collect::<Vec<_>>(), ["a", "b"]);
    m.insert("d", 4);
    assert_eq!(m.swap_remove("a"), Some(20));
    assert_eq!(m.keys().copied().collect::<Vec<_>>(), ["d", "b"]);
    assert_eq!(m.get("a"), None);
    assert_eq!(m.pop(), Some(("b", 3)));
    assert_eq!(m.insert_full("e", 5), (1, None));
    *m.get_or_insert_with("f", || 0) += 1;
    assert_eq!(m.values().copied().collect::<Vec<_>>(), [4, 5, 1]);

    let mut other: IndexMap<&str, i32> = [("f", 1), ("e", 5), ("d", 4)].into_iter().collect();
    assert_eq!(m, other, "equality ignores order");
    other.sort_keys();
    assert_eq!(other.keys().copied().collect::<Vec<_>>(), ["d", "e", "f"]);
    other.reverse();
    assert_eq!(other.get_index_of("d"), Some(2));
    other.sort_unstable_by(|_, a, _, b| a.cmp(b));
    assert_eq!(other.values().copied().collect::<Vec<_>>(), [1, 4, 5]);
    assert_eq!(other.get("e"), Some(&5));
    other.retain(|k, _| *k != "d");
    assert_eq!(other.into_iter().collect::<Vec<_>>(), [("f", 1), ("e", 5)]);
}

#[test]
fn matches_an_ordered_model() {
    let t = Tracker::new();
    let mut rng = Rng::new(5);
    let mut m = IndexMap::new();
    let mut model: Vec<(u32, u32)> = Vec::new();
    for step in 0..6_000 {
        let k = rng.below(300) as u32;
        let pos = model.iter().position(|(mk, _)| *mk == k);
        match rng.below(6) {
            0 | 1 => {
                let v = rng.next_u64() as u32;
                let old = m.insert(k, t.make(v)).map(|x| x.value);
                match pos {
                    Some(i) => assert_eq!(old, Some(std::mem::replace(&mut model[i].1, v))),
                    None => {
                        assert_eq!(old, None);
                        model.push((k, v));
                    }
                }
            }
            2 => {
                let got = m.swap_remove_full(&k).map(|(i, k, v)| (i, k, v.value));
                let expected = pos.map(|i| {
                    let (k, v) = model.swap_remove(i);
                    (i, k, v)
                });
                assert_eq!(got, expected, "step {step}");
            }
            3 => {
                let got = m.shift_remove_full(&k).map(|(i, k, v)| (i, k, v.value));
                let expected = pos.map(|i| {
                    let (k, v) = model.remove(i);
                    (i, k, v)
                });
                assert_eq!(got, expected, "step {step}");
            }
            4 if !model.is_empty() => {
                let i = rng.below(model.len());
                if rng.below(2) == 0 {
                    let (k, v) = m.shift_remove_index(i).unwrap();
                    assert_eq!((k, v.value), model.remove(i));
                } else {
                    let (k, v) = m.swap_remove_index(i).unwrap();
                    assert_eq!((k, v.value), model.swap_remove(i));
                }
            }
            _ => {
                if step % 500 == 0 {
                    m.retain(|k, _| k % 7 != 0);
                    model.retain(|(k, _)| k % 7 != 0);
                }
            }
        }
        if step % 50 == 0 {
            let got: Vec<(u32, u32)> = m.iter().map(|(k, v)| (*k, v.value)).collect();
            assert_eq!(got, model, "step {step}");
            for (i, (k, _)) in model.iter().enumerate() {
                assert_eq!(m.get_index_of(k), Some(i));
            }
        }
    }
    let c = m.clone();
    drop(m);
    drop(c);
    t.assert_all_dropped();
}

#[test]
fn index_set() {
    let mut s = IndexSet::new();
    assert!(s.insert("x"));
    assert!(s.insert("y"));
    assert!(!s.insert("x"));
    assert_eq!(s.insert_full("z"), (2, true));
    assert_eq!(s.get_index_of("y"), Some(1));
    assert!(s.contains("z"));
    assert_eq!(s.get("z"), Some(&"z"));
    assert!(s.shift_remove("x"));
    assert_eq!(s.iter().copied().collect::<Vec<_>>(), ["y", "z"]);
    s.extend(["a", "y"]);
    assert!(s.swap_remove("y"));
    assert_eq!(s.iter().copied().collect::<Vec<_>>(), ["a", "z"]);
    s.sort();
    assert_eq!(s.first(), Some(&"a"));
    let t: IndexSet<&str> = ["z", "a"].into_iter().collect();
    assert_eq!(s, t);
    assert_eq!(format!("{s:?}"), r#"{"a", "z"}"#);
    assert_eq!(s.into_iter().collect::<Vec<_>>(), ["a", "z"]);
}
