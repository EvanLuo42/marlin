use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Counts drops of the [`DropToken`]s it hands out. Cheap to clone and `'static`, so tokens can
/// move into spawned threads and jobs.
#[derive(Clone, Default, Debug)]
pub struct DropCounter(Arc<AtomicUsize>);

impl DropCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token that counts once when dropped.
    pub fn token(&self) -> DropToken {
        DropToken(self.0.clone())
    }

    pub fn dropped(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// See [`DropCounter`].
#[derive(Debug)]
pub struct DropToken(Arc<AtomicUsize>);

impl Drop for DropToken {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Hands out [`Tracked`] values and records which are alive: dropping one twice panics at once,
/// and [`Tracker::assert_all_dropped`] catches leaks. Thread-safe, for containers and schedulers
/// that move values across threads.
#[derive(Default)]
pub struct Tracker {
    alive: Mutex<Vec<bool>>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn make(&self, value: u32) -> Tracked<'_> {
        let mut alive = self.alive.lock().unwrap();
        alive.push(true);
        Tracked {
            id: alive.len() - 1,
            value,
            tracker: self,
        }
    }

    pub fn live(&self) -> usize {
        self.alive.lock().unwrap().iter().filter(|&&a| a).count()
    }

    pub fn created(&self) -> usize {
        self.alive.lock().unwrap().len()
    }

    pub fn assert_all_dropped(&self) {
        let (live, created) = (self.live(), self.created());
        assert_eq!(live, 0, "leaked {live} of {created} values");
    }
}

/// A value from a [`Tracker`]; compares and prints by `value`.
pub struct Tracked<'a> {
    id: usize,
    pub value: u32,
    tracker: &'a Tracker,
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        let mut alive = self.tracker.alive.lock().unwrap();
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

impl Eq for Tracked<'_> {}

impl fmt::Debug for Tracked<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.value)
    }
}
