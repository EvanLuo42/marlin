use marlin_containers::SmallVec;
use marlin_math::Rng;
use marlin_test_support::{Tracker, scale};

#[test]
fn stays_inline_then_spills() {
    let mut v: SmallVec<u32, 4> = SmallVec::new();
    assert_eq!(v.capacity(), 4);
    for i in 0..4 {
        v.push(i);
    }
    assert!(!v.spilled());
    v.push(4);
    assert!(v.spilled());
    assert!(v.capacity() >= 5);
    assert_eq!(v.as_slice(), &[0, 1, 2, 3, 4]);
    assert_eq!(v.pop(), Some(4));
    v.shrink_to_fit();
    assert!(!v.spilled(), "4 elements fit inline again");
    assert_eq!(v.as_slice(), &[0, 1, 2, 3]);
}

#[test]
fn zero_inline_capacity_and_zsts() {
    let mut v: SmallVec<u64, 0> = SmallVec::new();
    assert_eq!(v.capacity(), 0);
    v.push(7);
    assert!(v.spilled());
    assert_eq!(v[0], 7);

    let mut z: SmallVec<(), 2> = SmallVec::new();
    for _ in 0..10_000 {
        z.push(());
    }
    assert_eq!(z.len(), 10_000);
    assert!(!z.spilled());
    assert_eq!(z.capacity(), usize::MAX);
    assert_eq!(z.into_iter().count(), 10_000);
}

#[test]
fn matches_vec_under_random_operations() {
    fn run<const N: usize>(seed: u64) {
        let mut rng = Rng::new(seed);
        let mut v: SmallVec<u64, N> = SmallVec::new();
        let mut model = Vec::new();
        for step in 0..scale(5_000) {
            match rng.below(9) {
                0..=2 => {
                    let x = rng.next_u64();
                    v.push(x);
                    model.push(x);
                }
                3 => assert_eq!(v.pop(), model.pop()),
                4 => {
                    let i = rng.below(model.len() + 1);
                    let x = rng.next_u64();
                    v.insert(i, x);
                    model.insert(i, x);
                }
                5 if !model.is_empty() => {
                    let i = rng.below(model.len());
                    assert_eq!(v.remove(i), model.remove(i));
                }
                6 if !model.is_empty() => {
                    let i = rng.below(model.len());
                    assert_eq!(v.swap_remove(i), model.swap_remove(i));
                }
                7 => {
                    let n = rng.below(model.len() + 2);
                    v.truncate(n);
                    model.truncate(n);
                }
                8 => {
                    let m = rng.next_u64() % 3;
                    v.retain(|x| x % 3 != m);
                    model.retain(|x| x % 3 != m);
                    if step % 7 == 0 {
                        v.shrink_to_fit();
                    }
                }
                _ => {}
            }
            assert_eq!(v.as_slice(), model.as_slice(), "step {step}");
        }
    }
    run::<0>(1);
    run::<1>(2);
    run::<4>(3);
    run::<16>(4);
}

#[test]
fn drops_every_element_exactly_once() {
    let t = Tracker::new();
    {
        // Dropped while inline and while spilled.
        let mut a: SmallVec<_, 4> = SmallVec::new();
        a.push(t.make(1));
        a.push(t.make(2));
        let mut b: SmallVec<_, 4> = SmallVec::new();
        for i in 0..20 {
            b.push(t.make(i));
        }
        assert_eq!(t.live(), 22);
    }
    t.assert_all_dropped();

    let mut v: SmallVec<_, 4> = (0..10).map(|i| t.make(i)).collect();
    v.truncate(6);
    assert_eq!(t.live(), 6);
    v.retain(|x| x.value % 2 == 0);
    assert_eq!(t.live(), 3);
    drop(v.remove(0));
    drop(v.swap_remove(0));
    assert_eq!(t.live(), 1);
    v.insert(0, t.make(100));
    let c = v.clone();
    assert_eq!(c, v);
    v.clear();
    assert_eq!(t.live(), 2);
    drop(c);
    drop(v);
    t.assert_all_dropped();

    // Spill, then shrink back inline: elements move, none are dropped.
    let mut v: SmallVec<_, 4> = (0..8).map(|i| t.make(i)).collect();
    v.truncate(3);
    v.shrink_to_fit();
    assert!(!v.spilled());
    assert_eq!(t.live(), 3);
    drop(v);
    t.assert_all_dropped();
}

#[test]
fn into_iter_drops_the_rest() {
    let t = Tracker::new();
    for n in [2, 10] {
        let v: SmallVec<_, 4> = (0..n).map(|i| t.make(i)).collect();
        let mut it = v.into_iter();
        let first = it.next().unwrap();
        let last = it.next_back().unwrap();
        assert_eq!((first.value, last.value), (0, n - 1));
        assert_eq!(it.len() as u32, n - 2);
        drop(it);
        assert_eq!(t.live(), 2);
        drop((first, last));
        t.assert_all_dropped();
    }
}

#[test]
fn vec_conversions_move_without_dropping() {
    let t = Tracker::new();
    let v: SmallVec<_, 2> = (0..9).map(|i| t.make(i)).collect();
    let vec = v.into_vec();
    assert_eq!(vec.len(), 9);
    assert_eq!(t.live(), 9);
    let back: SmallVec<_, 2> = SmallVec::from(vec);
    assert!(back.spilled());
    let values: Vec<u32> = back.iter().map(|x| x.value).collect();
    assert_eq!(values, (0..9).collect::<Vec<_>>());
    let inline: SmallVec<_, 16> = SmallVec::from(back.into_vec());
    assert!(!inline.spilled());
    let small: SmallVec<_, 4> = (0..3).map(|i| t.make(i)).collect();
    assert_eq!(small.into_vec().len(), 3);
    drop(inline);
    t.assert_all_dropped();
}

#[test]
fn traits() {
    use std::collections::HashSet;
    let a: SmallVec<i32, 2> = SmallVec::from([1, 2, 3]);
    let b: SmallVec<i32, 8> = [1, 2, 3].as_slice().into();
    assert_eq!(a, b);
    assert!(a < SmallVec::<i32, 2>::from([1, 2, 4]));
    let mut set = HashSet::new();
    set.insert(a.clone());
    assert!(set.contains(&a));
    let mut c = a.clone();
    c.extend([4, 5]);
    c.extend(&[6]);
    c.extend_from_slice(&[7]);
    assert_eq!(&c[..], &[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(format!("{c:?}"), "[1, 2, 3, 4, 5, 6, 7]");
    for x in &mut c {
        *x *= 2;
    }
    assert_eq!(c.iter().sum::<i32>(), 56);
}

#[test]
#[should_panic]
fn insert_out_of_bounds_panics() {
    let mut v: SmallVec<u8, 2> = SmallVec::new();
    v.insert(1, 0);
}
