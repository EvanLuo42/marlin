//! ArrayDeque and Deque against std's VecDeque.

mod common;

use std::collections::VecDeque;

use common::{Rng, Tracker};
use marlin_containers::{ArrayDeque, CapacityError, Deque};

/// Pushes and pops at both ends, comparing contents and both slice views.
macro_rules! check_model {
    ($d:expr, $cap:expr, $seed:expr) => {{
        let mut d = $d;
        let cap: usize = $cap;
        let mut rng = Rng::new($seed);
        let mut model = VecDeque::new();
        for step in 0..5_000 {
            match rng.below(7) {
                0 | 1 if model.len() < cap => {
                    let x = rng.next();
                    d.push_back(x);
                    model.push_back(x);
                }
                2 if model.len() < cap => {
                    let x = rng.next();
                    d.push_front(x);
                    model.push_front(x);
                }
                3 => assert_eq!(d.pop_front(), model.pop_front(), "step {step}"),
                4 => assert_eq!(d.pop_back(), model.pop_back(), "step {step}"),
                5 if !model.is_empty() => {
                    let i = rng.below(model.len());
                    *d.get_mut(i).unwrap() ^= 1;
                    model[i] ^= 1;
                }
                6 if step % 50 == 0 => {
                    d.make_contiguous();
                    assert!(d.as_slices().1.is_empty());
                }
                _ => {}
            }
            assert_eq!(d.len(), model.len());
            assert!(d.iter().eq(model.iter()), "step {step}");
            assert!(d.iter().rev().eq(model.iter().rev()), "step {step}");
            let (a, b) = d.as_slices();
            assert_eq!(a.len() + b.len(), model.len());
            assert_eq!(d.front(), model.front());
            assert_eq!(d.back(), model.back());
        }
    }};
}

#[test]
fn array_deque_matches_vec_deque() {
    check_model!(ArrayDeque::<u64, 1>::new(), 1, 1);
    check_model!(ArrayDeque::<u64, 7>::new(), 7, 2);
    check_model!(ArrayDeque::<u64, 64>::new(), 64, 3);
}

#[test]
fn deque_matches_vec_deque() {
    check_model!(Deque::<u64>::new(), usize::MAX, 4);
    check_model!(Deque::<u64>::with_capacity(3), usize::MAX, 5);
}

#[test]
fn array_deque_capacity() {
    let mut d: ArrayDeque<u32, 3> = ArrayDeque::new();
    d.push_back(1);
    d.push_back(2);
    d.push_front(0);
    assert!(d.is_full());
    assert_eq!(d.try_push_back(9), Err(CapacityError(9)));
    assert_eq!(d.try_push_front(9), Err(CapacityError(9)));
    assert_eq!(d.push_back_overwrite(3), Some(0));
    assert!(d.iter().copied().eq([1, 2, 3]));
    assert_eq!(d[0], 1);
    assert_eq!(format!("{d:?}"), "[1, 2, 3]");
    let c = d.clone();
    assert_eq!(c, d);
    assert!(d.into_iter().rev().eq([3, 2, 1]));

    let mut none: ArrayDeque<u32, 0> = ArrayDeque::new();
    assert_eq!(none.try_push_back(1), Err(CapacityError(1)));
    assert_eq!(none.push_back_overwrite(1), Some(1));
    assert_eq!(none.pop_front(), None);
    assert_eq!(none.as_slices(), (&[][..], &[][..]));
}

#[test]
#[should_panic(expected = "capacity 2 exceeded")]
fn array_deque_push_when_full_panics() {
    let mut d: ArrayDeque<u8, 2> = ArrayDeque::new();
    d.extend([1, 2, 3]);
}

#[test]
fn deque_grows_across_the_wrap() {
    // Wrap the ring, then force growth: the wrapped part must move.
    for front in 0..9 {
        let mut d: Deque<u32> = Deque::with_capacity(8);
        let cap = d.capacity();
        for i in 0..cap as u32 {
            d.push_back(i);
        }
        for _ in 0..front {
            let x = d.pop_front().unwrap();
            d.push_back(x);
        }
        d.push_back(100);
        let mut expected: Vec<u32> = (0..cap as u32).collect();
        expected.rotate_left(front % cap);
        expected.push(100);
        assert!(
            d.iter().copied().eq(expected.iter().copied()),
            "front {front}"
        );
    }
    let mut z: Deque<()> = Deque::new();
    for _ in 0..1000 {
        z.push_front(());
    }
    assert_eq!(z.len(), 1000);
    assert_eq!(z.capacity(), usize::MAX);
    assert_eq!(z.into_iter().count(), 1000);
}

#[test]
fn drops_every_element_exactly_once() {
    let t = Tracker::new();
    {
        let mut a: ArrayDeque<_, 4> = ArrayDeque::new();
        let mut d = Deque::new();
        for i in 0..20 {
            drop(a.push_back_overwrite(t.make(i)));
            d.push_front(t.make(i));
            if i % 3 == 0 {
                drop(d.pop_back());
            }
        }
        let c = d.clone();
        d.make_contiguous();
        d.truncate(5);
        let mut it = c.into_iter();
        drop(it.next());
        drop(it);
        assert!(t.live() > 0);
    }
    t.assert_all_dropped();
}
