use std::collections::HashSet;
use std::mem::size_of;

use marlin_hash::xxh3_64;
use marlin_text::{Name, name_hash};

#[test]
fn interning_is_idempotent() {
    let a = Name::new("Hero");
    let b = Name::new("Hero");
    assert_eq!(a, b);
    assert!(a.eq_exact(b));
    assert_eq!(a.base_str(), "Hero");
    assert_eq!(a.to_string(), "Hero");
    assert_eq!(size_of::<Name>(), 12);
}

#[test]
fn case_insensitive_but_case_preserving() {
    let lower = Name::new("player_controller");
    let mixed = Name::new("Player_Controller");
    let upper = Name::new("PLAYER_CONTROLLER");
    assert_eq!(lower, mixed);
    assert_eq!(mixed, upper);
    assert!(!lower.eq_exact(mixed));
    assert_eq!(lower.to_string(), "player_controller");
    assert_eq!(mixed.to_string(), "Player_Controller");
    assert_eq!(upper.to_string(), "PLAYER_CONTROLLER");
    let mut set = HashSet::new();
    set.extend([lower, mixed, upper]);
    assert_eq!(set.len(), 1);
    assert_ne!(Name::new("player"), Name::new("player_"));
}

#[test]
fn numeric_suffix_is_split() {
    let n = Name::new("Enemy_12");
    assert_eq!(n.number(), Some(12));
    assert_eq!(n.base_str(), "Enemy");
    assert_eq!(n.base(), Name::new("Enemy"));
    assert_eq!(n, Name::with_number("Enemy", 12));
    assert_eq!(n.to_string(), "Enemy_12");
    assert_ne!(n, Name::new("Enemy_13"));
    assert_eq!(Name::new("Enemy_0").number(), Some(0));
    assert_eq!(Name::new("Enemy").number(), None);
    assert_ne!(Name::new("Enemy"), Name::new("Enemy_0"));
    assert_eq!(Name::new("enemy_12"), n);
}

#[test]
fn non_canonical_numbers_stay_in_the_base() {
    for s in [
        "Enemy_007",
        "Enemy_",
        "Enemy12",
        "12",
        "Enemy_4294967295",
        "Enemy_99999999999",
        "A_1x",
    ] {
        let n = Name::new(s);
        assert_eq!(n.number(), None, "{s}");
        assert_eq!(n.to_string(), s);
    }
    // The largest number that splits.
    let max = Name::new("Enemy_4294967294");
    assert_eq!(max.number(), Some(u32::MAX - 1));
    assert_eq!(max.to_string(), "Enemy_4294967294");
    // An empty base with a number.
    let bare = Name::new("_5");
    assert_eq!((bare.base_str(), bare.number()), ("", Some(5)));
    assert!(!bare.is_none());
    // Nested suffixes round-trip.
    let nested = Name::with_number("Wave_3", 7);
    assert_eq!(nested.to_string(), "Wave_3_7");
    assert_eq!(Name::new("Wave_3_7"), nested);
}

#[test]
fn every_name_round_trips_through_display() {
    for s in [
        "a",
        "A_0",
        "x_1_2",
        "_",
        "__",
        "_0",
        "Ünïcødé_3",
        "名前_42",
        "sp ace_9",
        "end_",
    ] {
        assert_eq!(Name::new(s).to_string(), s);
        assert_eq!(Name::new(&Name::new(s).to_string()), Name::new(s));
    }
}

#[test]
fn none() {
    assert!(Name::NONE.is_none());
    assert_eq!(Name::new(""), Name::NONE);
    assert_eq!(Name::default(), Name::NONE);
    assert_eq!(Name::NONE.to_string(), "");
    assert_eq!(Name::NONE.base_str(), "");
    assert_ne!(Name::new("x"), Name::NONE);
    assert_eq!(Name::find(""), Some(Name::NONE));
}

#[test]
fn find_does_not_intern() {
    let probe = "find_does_not_intern_probe";
    assert_eq!(Name::find(probe), None);
    assert_eq!(Name::find(probe), None);
    let n = Name::new("Find_Does_Not_Intern_Probe");
    let found = Name::find(probe).unwrap();
    assert_eq!(found, n);
    // Found through another spelling, so it displays that spelling.
    assert_eq!(found.to_string(), "Find_Does_Not_Intern_Probe");
    assert_eq!(
        Name::find("find_does_not_intern_probe_3").unwrap().number(),
        Some(3)
    );
}

#[test]
fn compares_with_str_without_interning() {
    let n = Name::new("Weapon_2");
    assert!(n == "weapon_2");
    assert!(n == "WEAPON_2");
    assert!(n != "Weapon_3");
    assert!(n != "Weapon");
    assert!(Name::new("Weapon_02") == "weapon_02");
}

#[test]
fn stable_hash_is_the_lowercased_string_hash() {
    for s in [
        "Hero",
        "hero",
        "HERO",
        "Enemy_12",
        "enemy_12",
        "",
        "_5",
        "Ünïcødé_3",
    ] {
        let expected = xxh3_64(s.to_ascii_lowercase().as_bytes());
        assert_eq!(Name::new(s).stable_hash(), expected, "{s}");
        assert_eq!(name_hash(s), expected, "{s}");
    }
    // Pinned to the reference XXH3 of the lowercased strings: cooked data stores these.
    assert_eq!(name_hash("Player_12"), 0xfde61fa1451ca54a);
    const HERO: u64 = name_hash("Hero");
    assert_eq!(HERO, 0x5a3384393157acf1);
    assert_eq!(HERO, Name::new("HERO").stable_hash());
}

#[test]
fn lexical_order() {
    let mut names: Vec<Name> = ["b", "A_10", "a_9", "B_1", "a"]
        .into_iter()
        .map(Name::new)
        .collect();
    names.sort_by(|a, b| a.cmp_lexical(*b));
    let shown: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    assert_eq!(shown, ["a", "a_9", "A_10", "b", "B_1"]);
}

#[test]
fn long_names_and_many_names() {
    let long = "x".repeat(10_000);
    let n = Name::new(&long);
    assert_eq!(n.base_str(), long);
    assert_eq!(Name::new(&long.to_uppercase()), n);
    let names: Vec<Name> = (0..20_000)
        .map(|i| Name::new(&format!("bulk_name_{i:x}x")))
        .collect();
    for (i, n) in names.iter().enumerate() {
        assert_eq!(n.to_string(), format!("bulk_name_{i:x}x"));
    }
    assert_eq!(names.iter().collect::<HashSet<_>>().len(), names.len());
}
