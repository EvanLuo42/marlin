use marlin_lang_arena::{Arena, ArenaMap, Idx, Interner};

#[test]
fn arena_indices_address_values_in_allocation_order() {
    let mut arena = Arena::new();
    let a = arena.alloc("a");
    let b = arena.alloc("b");
    assert_eq!((a.into_raw(), b.into_raw()), (0, 1));
    assert_eq!((arena[a], arena[b]), ("a", "b"));
    arena[b] = "B";
    assert_eq!(arena.get(b), Some(&"B"));
    assert_eq!(arena.get(Idx::from_raw(9)), None);
    let range = arena.alloc_many(["c", "d"]);
    assert_eq!(range.len(), 2);
    assert_eq!(range.map(|i| arena[i]).collect::<Vec<_>>(), ["c", "d"]);
    assert_eq!(
        arena
            .iter()
            .map(|(i, v)| (i.into_raw(), *v))
            .collect::<Vec<_>>(),
        [(0, "a"), (1, "B"), (2, "c"), (3, "d")]
    );
    assert_eq!(format!("{a:?}"), "Idx::<&str>(0)");
}

#[test]
fn arena_map_holds_values_for_some_indices() {
    let mut arena = Arena::new();
    let ids: Vec<Idx<u8>> = (0..4).map(|i| arena.alloc(i)).collect();
    let mut map = ArenaMap::new();
    assert_eq!(map.insert(ids[2], "two"), None);
    assert_eq!(map.insert(ids[2], "TWO"), Some("two"));
    map.insert(ids[0], "zero");
    assert!(map.contains(ids[0]) && !map.contains(ids[1]) && !map.contains(ids[3]));
    assert_eq!(map[ids[2]], "TWO");
    assert_eq!(
        map.iter()
            .map(|(i, v)| (i.into_raw(), *v))
            .collect::<Vec<_>>(),
        [(0, "zero"), (2, "TWO")]
    );
    assert_eq!(map.remove(ids[0]), Some("zero"));
    assert_eq!(map.get(ids[0]), None);
}

#[test]
fn interner_gives_equal_values_one_index() {
    let mut names: Interner<String> = Interner::new();
    let a = names.intern("Dot");
    let b = names.intern("Lerp");
    assert_eq!(names.intern("Dot"), a);
    assert_ne!(a, b);
    assert_eq!(names[b], "Lerp");
    assert_eq!(names.lookup("Lerp"), Some(b));
    assert_eq!(names.lookup("Saturate"), None);
    assert_eq!(names.len(), 2);
}
