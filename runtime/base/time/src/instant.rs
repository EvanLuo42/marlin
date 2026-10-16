use core::fmt;
use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::time::Duration;

use marlin_pal::time;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Frequency of [`Instant::as_ticks`]; constant for the life of the process.
#[inline]
pub fn ticks_per_second() -> u64 {
    time::ticks_per_second()
}

/// Whether `x * NANOS_PER_SEC` fits in a u64 for every `x < tps`. True for every current backend
/// (Windows QPC is 10 MHz, Unix ticks are nanoseconds); the u128 path is for exotic clocks.
#[inline]
fn fits_u64(tps: u64) -> bool {
    tps <= u64::MAX / NANOS_PER_SEC
}

/// Floors to whole nanoseconds. Splits into seconds and a sub-second remainder first, so
/// `ticks * 10^9` is never formed.
#[inline]
fn ticks_to_duration(ticks: u64) -> Duration {
    let tps = time::ticks_per_second();
    let rem = ticks % tps;
    let nanos = if fits_u64(tps) {
        rem * NANOS_PER_SEC / tps
    } else {
        (u128::from(rem) * u128::from(NANOS_PER_SEC) / u128::from(tps)) as u64
    };
    Duration::new(ticks / tps, nanos as u32)
}

/// Rounds up to whole ticks, so a deadline built from a duration is never early. `None` if the
/// tick count overflows u64.
#[inline]
fn duration_to_ticks(duration: Duration) -> Option<u64> {
    let tps = time::ticks_per_second();
    let nanos = u64::from(duration.subsec_nanos());
    let sub = if fits_u64(tps) {
        (nanos * tps).div_ceil(NANOS_PER_SEC)
    } else {
        (u128::from(nanos) * u128::from(tps)).div_ceil(u128::from(NANOS_PER_SEC)) as u64
    };
    duration.as_secs().checked_mul(tps)?.checked_add(sub)
}

/// A point on the monotonic clock. Never goes backwards, ignores wall-clock changes, and does not
/// advance while the system is suspended.
///
/// Durations are converted to ticks rounding up, so `(t + d).duration_since(t)` can exceed `d` by
/// less than one tick (100 ns on Windows, nothing on Unix).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    #[inline]
    pub fn now() -> Self {
        Self(time::ticks())
    }

    /// The raw tick count, in units of [`ticks_per_second`]. For profilers that convert lazily;
    /// the origin is unspecified.
    #[inline]
    pub fn as_ticks(self) -> u64 {
        self.0
    }

    /// Inverse of [`as_ticks`](Self::as_ticks), within the same process.
    #[inline]
    pub fn from_ticks(ticks: u64) -> Self {
        Self(ticks)
    }

    /// Time from `earlier` to `self`, or zero if `earlier` is later.
    #[inline]
    pub fn duration_since(self, earlier: Instant) -> Duration {
        self.saturating_duration_since(earlier)
    }

    /// Time from `earlier` to `self`, or `None` if `earlier` is later.
    #[inline]
    pub fn checked_duration_since(self, earlier: Instant) -> Option<Duration> {
        self.0.checked_sub(earlier.0).map(ticks_to_duration)
    }

    #[inline]
    pub fn saturating_duration_since(self, earlier: Instant) -> Duration {
        ticks_to_duration(self.0.saturating_sub(earlier.0))
    }

    /// Time since `self`.
    #[inline]
    pub fn elapsed(self) -> Duration {
        Instant::now().saturating_duration_since(self)
    }

    #[inline]
    pub fn checked_add(self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration_to_ticks(duration)?).map(Self)
    }

    #[inline]
    pub fn checked_sub(self, duration: Duration) -> Option<Instant> {
        self.0.checked_sub(duration_to_ticks(duration)?).map(Self)
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    /// Panics on overflow; see [`Instant::checked_add`].
    #[inline]
    fn add(self, rhs: Duration) -> Instant {
        self.checked_add(rhs)
            .expect("overflow when adding duration to instant")
    }
}

impl AddAssign<Duration> for Instant {
    #[inline]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    /// Panics on underflow; see [`Instant::checked_sub`].
    #[inline]
    fn sub(self, rhs: Duration) -> Instant {
        self.checked_sub(rhs)
            .expect("overflow when subtracting duration from instant")
    }
}

impl SubAssign<Duration> for Instant {
    #[inline]
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    /// Saturates to zero, like [`Instant::duration_since`].
    #[inline]
    fn sub(self, rhs: Instant) -> Duration {
        self.duration_since(rhs)
    }
}

impl fmt::Debug for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Instant")
            .field(&ticks_to_duration(self.0))
            .finish()
    }
}
