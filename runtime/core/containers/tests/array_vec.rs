mod common;

use common::{Rng, Tracker};
use marlin_containers::{ArrayVec, CapacityError};

#[test]
fn fixed_capacity() {
    let mut v: ArrayVec<u32, 3> = ArrayVec::new();
    assert_eq!(v.capacity(), 3);
    assert!(v.try_push(1).is_ok());
    v.push(2);
    v.push(3);
    assert!(v.is_full());
    assert_eq!(v.try_push(4), Err(CapacityError(4)));
    assert_eq!(v.try_insert(0, 5), Err(CapacityError(5)));
    assert_eq!(v.as_slice(), &[1, 2, 3]);
    assert_eq!(v.into_array().ok(), Some([1, 2, 3]));
    assert!(ArrayVec::<u32, 3>::new().into_array().is_err());
}

#[test]
#[should_panic(expected = "capacity 2 exceeded")]
fn push_when_full_panics() {
    let mut v: ArrayVec<u8, 2> = ArrayVec::new();
    for i in 0..3 {
        v.push(i);
    }
}

#[test]
fn extend_from_slice_is_all_or_nothing() {
    let mut v: ArrayVec<u8, 4> = ArrayVec::new();
    v.extend_from_slice(&[1, 2]);
    assert!(v.try_extend_from_slice(&[3, 4, 5]).is_err());
    assert_eq!(v.as_slice(), &[1, 2]);
    assert!(v.try_extend_from_slice(&[3, 4]).is_ok());
    assert!(v.is_full());
}

#[test]
fn matches_vec_under_random_operations() {
    let mut rng = Rng::new(7);
    let mut v: ArrayVec<u64, 32> = ArrayVec::new();
    let mut model: Vec<u64> = Vec::new();
    for step in 0..10_000u64 {
        match rng.below(7) {
            0 | 1 => {
                let x = rng.next();
                assert_eq!(v.try_push(x).is_ok(), model.len() < 32);
                if model.len() < 32 {
                    model.push(x);
                }
            }
            2 => assert_eq!(v.pop(), model.pop()),
            3 if model.len() < 32 => {
                let i = rng.below(model.len() + 1);
                v.insert(i, step);
                model.insert(i, step);
            }
            4 if !model.is_empty() => {
                let i = rng.below(model.len());
                assert_eq!(v.remove(i), model.remove(i));
            }
            5 if !model.is_empty() => {
                let i = rng.below(model.len());
                assert_eq!(v.swap_remove(i), model.swap_remove(i));
            }
            6 => {
                let m = rng.next() % 4;
                v.retain(|x| x % 4 != m);
                model.retain(|x| x % 4 != m);
            }
            _ => {}
        }
        assert_eq!(v.as_slice(), model.as_slice(), "step {step}");
    }
}

#[test]
fn drops_every_element_exactly_once() {
    let t = Tracker::new();
    let mut v: ArrayVec<_, 8> = (0..8).map(|i| t.make(i)).collect();
    let rejected = v.try_push(t.make(99)).unwrap_err().into_inner();
    assert_eq!(rejected.value, 99);
    drop(rejected);
    assert_eq!(t.live(), 8);
    v.truncate(6);
    v.retain(|x| x.value != 3);
    drop(v.remove(0));
    drop(v.swap_remove(1));
    assert_eq!(t.live(), 3);
    let c = v.clone();
    v.clear();
    assert_eq!(t.live(), 3);
    drop(c);
    drop(v);
    t.assert_all_dropped();

    let full: ArrayVec<_, 3> = ArrayVec::from([t.make(1), t.make(2), t.make(3)]);
    let arr = full.into_array().unwrap();
    assert_eq!(t.live(), 3);
    drop(arr);
    t.assert_all_dropped();

    let v: ArrayVec<_, 5> = (0..5).map(|i| t.make(i)).collect();
    let mut it = v.into_iter();
    let x = it.next_back().unwrap();
    drop(it);
    assert_eq!(t.live(), 1);
    drop(x);
    t.assert_all_dropped();
}

#[test]
fn const_construction() {
    static EMPTY: ArrayVec<u32, 4> = ArrayVec::new();
    assert!(EMPTY.is_empty());
    const FILLED: ArrayVec<u32, 2> = {
        let mut v = ArrayVec::new();
        assert!(v.try_push(1).is_ok());
        v
    };
    assert_eq!(FILLED.as_slice(), &[1]);
}
