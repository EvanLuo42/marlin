//! Log levels and the compile-time and run-time filters.

use core::fmt;
use core::sync::atomic::{AtomicU8, Ordering};

/// Severity of a log record, from most to least severe.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Level {
    Error = 1,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// The most verbose [`Level`] let through, or `Off`.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum LevelFilter {
    Off = 0,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LevelFilter {
    #[inline]
    pub const fn allows(self, level: Level) -> bool {
        level as u8 <= self as u8
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Off,
            1 => Self::Error,
            2 => Self::Warn,
            3 => Self::Info,
            4 => Self::Debug,
            _ => Self::Trace,
        }
    }

    const fn min(self, other: Self) -> Self {
        if (self as u8) < other as u8 {
            self
        } else {
            other
        }
    }
}

impl From<Level> for LevelFilter {
    fn from(level: Level) -> Self {
        Self::from_u8(level as u8)
    }
}

/// Levels above this are compiled out: the macros still type-check their arguments but generate
/// no code.
///
/// | `marlin_build` | max level |
/// |---|---|
/// | `debug`, `development` | `Trace` |
/// | `profile` | `Info` |
/// | `shipping` | `Warn` |
/// | unset (plain cargo) | `Trace` with debug assertions, else `Info` |
pub const STATIC_MAX_LEVEL: LevelFilter = if cfg!(marlin_build = "shipping") {
    LevelFilter::Warn
} else if cfg!(any(marlin_build = "debug", marlin_build = "development")) {
    LevelFilter::Trace
} else if cfg!(marlin_build = "profile") {
    LevelFilter::Info
} else if cfg!(debug_assertions) {
    LevelFilter::Trace
} else {
    LevelFilter::Info
};

static MAX_LEVEL: AtomicU8 = AtomicU8::new(LevelFilter::Info.min(STATIC_MAX_LEVEL) as u8);

/// The run-time filter. Starts at `Info` (or [`STATIC_MAX_LEVEL`] if lower).
#[inline]
pub fn max_level() -> LevelFilter {
    LevelFilter::from_u8(MAX_LEVEL.load(Ordering::Relaxed))
}

/// Sets the run-time filter. Clamped to [`STATIC_MAX_LEVEL`]: compiled-out levels stay out.
pub fn set_max_level(filter: LevelFilter) {
    MAX_LEVEL.store(filter.min(STATIC_MAX_LEVEL) as u8, Ordering::Relaxed);
}

/// Whether a record at `level` would reach the sinks.
#[inline]
pub fn enabled(level: Level) -> bool {
    STATIC_MAX_LEVEL.allows(level) && max_level().allows(level)
}
