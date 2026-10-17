use core::fmt;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};

use marlin_pal::futex;

/// Blocks a fixed number of threads until all of them have arrived, then releases them together.
/// Reusable: once released, the same threads can [`wait`](Self::wait) again for the next round.
///
/// Two 32-bit words: the arrivals in the current round, and a generation counter that waiters
/// sleep on and the last arrival bumps. Exactly `n` threads must take part in each round; more
/// would spill into the next round's count.
pub struct Barrier {
    arrived: AtomicU32,
    generation: AtomicU32,
    n: u32,
}

/// Returned by [`Barrier::wait`]; exactly one thread per round is the leader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarrierWaitResult(bool);

impl BarrierWaitResult {
    /// Whether this thread was the last to arrive, the one that released the others.
    #[inline]
    pub fn is_leader(self) -> bool {
        self.0
    }
}

impl Barrier {
    /// A barrier for `n` threads. `n` of 0 behaves like 1: every wait returns at once as leader.
    #[inline]
    pub const fn new(n: u32) -> Self {
        Self {
            arrived: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            n,
        }
    }

    /// Blocks until `n` threads (this one included) have called `wait` in this round. Everything
    /// each thread did before its `wait` is visible to all of them afterwards.
    pub fn wait(&self) -> BarrierWaitResult {
        // Read before arriving: the round cannot end until we arrive, so this is our round's
        // generation. (After arriving, the leader could already have moved it on.)
        let generation = self.generation.load(Acquire);
        // AcqRel chains every arrival's writes through to the leader, whose Release bump below
        // then publishes them all to the waiters.
        let arrived = self.arrived.fetch_add(1, AcqRel) + 1;
        if arrived >= self.n {
            // Nobody touches `arrived` until the bump below releases the next round.
            self.arrived.store(0, Relaxed);
            self.generation.store(generation.wrapping_add(1), Release);
            futex::wake_all(&self.generation);
            return BarrierWaitResult(true);
        }
        loop {
            futex::wait(&self.generation, generation, None);
            if self.generation.load(Acquire) != generation {
                return BarrierWaitResult(false);
            }
        }
    }
}

impl fmt::Debug for Barrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Barrier")
            .field("n", &self.n)
            .finish_non_exhaustive()
    }
}
