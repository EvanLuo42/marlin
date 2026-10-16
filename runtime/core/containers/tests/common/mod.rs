//! Shared test helpers: a drop tracker and a deterministic RNG.
#![allow(dead_code)]

use std::cell::RefCell;
use std::fmt;

/// Hands out [`Tracked`] values and records which are alive: dropping one twice panics at once,
/// and [`Tracker::assert_all_dropped`] catches leaks.
#[derive(Default)]
pub struct Tracker {
    alive: RefCell<Vec<bool>>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn make(&self, value: u32) -> Tracked<'_> {
        let mut alive = self.alive.borrow_mut();
        alive.push(true);
        Tracked {
            id: alive.len() - 1,
            value,
            tracker: self,
        }
    }

    pub fn live(&self) -> usize {
        self.alive.borrow().iter().filter(|&&a| a).count()
    }

    pub fn created(&self) -> usize {
        self.alive.borrow().len()
    }

    pub fn assert_all_dropped(&self) {
        assert_eq!(
            self.live(),
            0,
            "leaked {} of {} values",
            self.live(),
            self.created()
        );
    }
}

pub struct Tracked<'a> {
    id: usize,
    pub value: u32,
    tracker: &'a Tracker,
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        let mut alive = self.tracker.alive.borrow_mut();
        assert!(
            alive[self.id],
            "value #{} ({}) dropped twice",
            self.id, self.value
        );
        alive[self.id] = false;
    }
}

impl Clone for Tracked<'_> {
    fn clone(&self) -> Self {
        self.tracker.make(self.value)
    }
}

impl PartialEq for Tracked<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl fmt::Debug for Tracked<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.value)
    }
}

/// xorshift64*: deterministic, so failures reproduce.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
