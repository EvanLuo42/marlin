//! BitArray and BitSet against BTreeSet.

use std::collections::{BTreeSet, HashSet};

use marlin_containers::{BitArray, BitSet};
use marlin_math::Rng;
use marlin_test_support::scale;

#[test]
fn bit_array_basics() {
    let mut a: BitArray<2> = BitArray::new();
    assert_eq!(BitArray::<2>::BITS, 128);
    assert!(a.insert(0));
    assert!(!a.insert(0));
    a.insert(63);
    a.insert(64);
    a.insert(127);
    assert!(a.contains(64));
    assert!(!a.contains(1000));
    assert!(!a.remove(1000));
    assert_eq!(a.len(), 4);
    assert_eq!(a.iter().collect::<Vec<_>>(), [0, 63, 64, 127]);
    assert_eq!((a.first(), a.last()), (Some(0), Some(127)));
    assert_eq!(format!("{a:?}"), "{0, 63, 64, 127}");
    a.toggle(63);
    a.set(5, true);
    assert_eq!(a.iter().collect::<Vec<_>>(), [0, 5, 64, 127]);

    let b: BitArray<2> = [5, 6, 64].into_iter().collect();
    assert_eq!((a | b).iter().collect::<Vec<_>>(), [0, 5, 6, 64, 127]);
    assert_eq!((a & b).iter().collect::<Vec<_>>(), [5, 64]);
    assert_eq!((a ^ b).iter().collect::<Vec<_>>(), [0, 6, 127]);
    assert_eq!((a - b).iter().collect::<Vec<_>>(), [0, 127]);
    assert_eq!((!BitArray::<1>::new()).len(), 64);
    assert_eq!(BitArray::<1>::full(), !BitArray::<1>::new());
    assert!((a & b).is_subset(&a));
    assert!(a.is_superset(&(a & b)));
    assert!((a - b).is_disjoint(&b));
    a.clear();
    assert!(a.is_empty());

    const C: BitArray<1> = {
        let mut c = BitArray::new();
        c.insert(3);
        c
    };
    assert!(C.contains(3));
}

#[test]
#[should_panic(expected = "out of range")]
fn bit_array_insert_past_end_panics() {
    BitArray::<1>::new().insert(64);
}

#[test]
fn bit_set_matches_btree_set() {
    let mut rng = Rng::new(7);
    let mut s = BitSet::new();
    let mut model = BTreeSet::new();
    for step in 0..scale(10_000) {
        let i = rng.below(700);
        match rng.below(4) {
            0 | 1 => assert_eq!(s.insert(i), model.insert(i)),
            2 => assert_eq!(s.remove(i), model.remove(&i)),
            _ => {
                s.toggle(i);
                if !model.remove(&i) {
                    model.insert(i);
                }
            }
        }
        if step % 97 == 0 {
            assert!(s.iter().eq(model.iter().copied()), "step {step}");
            assert_eq!(s.len(), model.len());
            assert_eq!(s.first(), model.first().copied());
            assert_eq!(s.last(), model.last().copied());
            assert_eq!(s.iter().len(), model.len());
        }
    }
}

#[test]
fn bit_set_operations() {
    let mut rng = Rng::new(3);
    for _ in 0..200 {
        let a: BTreeSet<usize> = (0..rng.below(40)).map(|_| rng.below(300)).collect();
        let b: BTreeSet<usize> = (0..rng.below(40)).map(|_| rng.below(500)).collect();
        let sa: BitSet = a.iter().copied().collect();
        let sb: BitSet = b.iter().copied().collect();
        let check = |s: BitSet, m: BTreeSet<usize>| assert!(s.iter().eq(m.iter().copied()));
        check(&sa | &sb, a.union(&b).copied().collect());
        check(&sa & &sb, a.intersection(&b).copied().collect());
        check(&sa ^ &sb, a.symmetric_difference(&b).copied().collect());
        check(&sa - &sb, a.difference(&b).copied().collect());
        let mut u = sa.clone();
        u |= &sb;
        assert_eq!(u, &sa | &sb);
        assert_eq!(sa.is_subset(&sb), a.is_subset(&b));
        assert_eq!(sa.is_disjoint(&sb), a.is_disjoint(&b));
    }
}

#[test]
fn bit_set_equality_ignores_capacity() {
    let mut a = BitSet::with_capacity(10_000);
    let mut b = BitSet::new();
    a.insert(3);
    b.insert(3);
    b.insert(5000);
    b.remove(5000);
    assert_eq!(a, b);
    let mut h = HashSet::new();
    h.insert(a.clone());
    assert!(h.contains(&b));
    b.shrink_to_fit();
    assert_eq!(b.capacity(), 64);
    assert_eq!(a, b);
    assert!(BitSet::new().is_empty());
    assert!(!BitSet::new().contains(1 << 40));
    let fixed: BitArray<1> = [1, 2].into_iter().collect();
    assert_eq!(BitSet::from(fixed), [2, 1].into_iter().collect::<BitSet>());
}
