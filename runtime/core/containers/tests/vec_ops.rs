//! drain, splice, dedup, resize and extend_from_within on SmallVec and ArrayVec, against Vec.

mod common;

use common::{Rng, Tracker};
use marlin_containers::{ArrayVec, SmallVec};

fn range(rng: &mut Rng, len: usize) -> (usize, usize) {
    let a = rng.below(len + 1);
    let b = rng.below(len + 1);
    (a.min(b), a.max(b))
}

#[test]
fn small_vec_matches_vec() {
    fn run<const N: usize>(seed: u64) {
        let mut rng = Rng::new(seed);
        let mut v: SmallVec<u64, N> = SmallVec::new();
        let mut model: Vec<u64> = Vec::new();
        for step in 0..3_000 {
            match rng.below(8) {
                0 | 1 => {
                    let x = rng.next_u64() % 4;
                    v.push(x);
                    model.push(x);
                }
                2 => {
                    let (a, b) = range(&mut rng, model.len());
                    let taken: Vec<u64> = v.drain(a..b).collect();
                    assert_eq!(taken, model.drain(a..b).collect::<Vec<_>>(), "step {step}");
                }
                3 => {
                    let (a, b) = range(&mut rng, model.len());
                    let new: Vec<u64> = (0..rng.below(6) as u64).map(|i| 100 + i).collect();
                    let removed: Vec<u64> = v.splice(a..b, new.iter().copied()).collect();
                    let expected: Vec<u64> = model.splice(a..b, new).collect();
                    assert_eq!(removed, expected, "step {step}");
                }
                4 => {
                    v.dedup();
                    model.dedup();
                }
                5 => {
                    let n = rng.below(model.len() + 8);
                    let x = rng.next_u64() % 4;
                    v.resize(n, x);
                    model.resize(n, x);
                }
                6 if !model.is_empty() => {
                    let (a, b) = range(&mut rng, model.len().min(6));
                    v.extend_from_within(a..b);
                    model.extend_from_within(a..b);
                }
                7 => {
                    v.dedup_by_key(|x| *x / 2);
                    model.dedup_by_key(|x| *x / 2);
                    let mut i = 0;
                    v.resize_with(model.len() + 1, || {
                        i += 1;
                        i
                    });
                    model.push(1);
                }
                _ => {}
            }
            assert_eq!(v.as_slice(), model.as_slice(), "step {step}");
        }
    }
    run::<0>(1);
    run::<3>(2);
    run::<16>(3);
}

#[test]
fn array_vec_matches_vec() {
    let mut rng = Rng::new(9);
    let mut v: ArrayVec<u64, 64> = ArrayVec::new();
    let mut model: Vec<u64> = Vec::new();
    for step in 0..3_000 {
        match rng.below(6) {
            0 | 1 if model.len() < 64 => {
                let x = rng.next_u64() % 4;
                v.push(x);
                model.push(x);
            }
            2 => {
                let (a, b) = range(&mut rng, model.len());
                let mut d = v.drain(a..b);
                let mut m = model.drain(a..b);
                // Take from both ends, then drop the rest.
                assert_eq!(d.next(), m.next());
                assert_eq!(d.next_back(), m.next_back());
                assert_eq!(d.len(), m.len());
            }
            3 => {
                v.dedup_by(|a, b| a == b);
                model.dedup();
            }
            4 => {
                let n = rng.below(65);
                v.resize(n, 7);
                model.resize(n, 7);
            }
            5 if model.len() <= 32 => {
                v.extend_from_within(..);
                model.extend_from_within(..);
            }
            _ => {}
        }
        assert_eq!(v.as_slice(), model.as_slice(), "step {step}");
    }
}

#[test]
#[should_panic(expected = "capacity 4 exceeded")]
fn array_vec_resize_past_capacity_panics() {
    let mut v: ArrayVec<u8, 4> = ArrayVec::new();
    v.resize(5, 0);
}

#[test]
#[should_panic(expected = "out of bounds")]
fn drain_out_of_bounds_panics() {
    let mut v: SmallVec<u8, 4> = SmallVec::from([1, 2, 3]);
    v.drain(1..4);
}

#[test]
fn drains_drop_exactly_once() {
    let t = Tracker::new();
    // Partly consumed, then dropped: the rest are dropped and the tail closes the gap.
    let mut v: SmallVec<_, 4> = (0..10).map(|i| t.make(i)).collect();
    let mut d = v.drain(2..7);
    let first = d.next().unwrap();
    assert_eq!(first.value, 2);
    drop(d);
    drop(first);
    assert_eq!(t.live(), 5);
    let values: Vec<u32> = v.iter().map(|x| x.value).collect();
    assert_eq!(values, [0, 1, 7, 8, 9]);

    // Leaked: the vector keeps only the head; nothing is dropped twice.
    std::mem::forget(v.drain(1..2));
    assert_eq!(v.len(), 1);
    drop(v);
    assert_eq!(t.live(), 4, "the leaked range and tail stay leaked");

    let t = Tracker::new();
    let mut a: ArrayVec<_, 8> = (0..8).map(|i| t.make(i)).collect();
    drop(a.drain(..));
    assert!(a.is_empty());
    a.extend((0..4).map(|i| t.make(i)));
    a.dedup_by_key(|x| x.value / 2);
    assert_eq!(t.live(), 2);
    a.resize_with(5, || t.make(9));
    a.truncate(1);
    drop(a);
    t.assert_all_dropped();

    let mut s: SmallVec<_, 2> = (0..6).map(|i| t.make(i)).collect();
    let removed: Vec<_> = s.splice(1..3, (10..13).map(|i| t.make(i))).collect();
    assert_eq!(removed.len(), 2);
    let values: Vec<u32> = s.iter().map(|x| x.value).collect();
    assert_eq!(values, [0, 10, 11, 12, 3, 4, 5]);
    drop(s.splice(.., std::iter::empty()));
    assert!(s.is_empty());
    drop((removed, s));
    t.assert_all_dropped();
}
