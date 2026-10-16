use std::hash::BuildHasher;

use marlin_containers::{HashMap, HashMapExt, HashSet, HashSetExt, HashTable, hash_map::Entry};
use marlin_hash::FastState;

#[test]
fn map_and_set() {
    let mut m: HashMap<&str, u32> = HashMap::new();
    m.insert("a", 1);
    *m.entry("a").or_insert(0) += 1;
    match m.entry("b") {
        Entry::Vacant(v) => {
            v.insert(5);
        }
        Entry::Occupied(_) => unreachable!(),
    }
    assert_eq!(m["a"], 2);
    assert_eq!(m.len(), 2);

    let mut s: HashSet<u32> = HashSet::with_capacity(100);
    assert!(s.capacity() >= 100);
    for i in 0..1000 {
        s.insert(i * 3);
    }
    assert!(s.contains(&(999 * 3)));
    assert_eq!(s.len(), 1000);

    let d: HashMap<u32, u32> = HashMap::default();
    assert!(d.is_empty());
}

#[test]
fn hash_table_with_external_hash() {
    let state = FastState::new();
    let mut t: HashTable<(String, u32)> = HashTable::new();
    for (i, name) in ["x", "y", "z"].into_iter().enumerate() {
        let h = state.hash_one(name);
        t.insert_unique(h, (name.to_owned(), i as u32), |(k, _)| state.hash_one(k));
    }
    let h = state.hash_one("y");
    assert_eq!(t.find(h, |(k, _)| k == "y").map(|e| e.1), Some(1));
}
