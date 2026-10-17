//! Log levels, the compile-time and run-time filters, and per-target rules.

use core::fmt;
use core::hint::spin_loop;
use core::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering, fence,
};

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

    /// The level with discriminant `value`; `Error` below it, `Trace` past it.
    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            0 | 1 => Self::Error,
            2 => Self::Warn,
            3 => Self::Info,
            4 => Self::Debug,
            _ => Self::Trace,
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

const DEFAULT_LEVEL: u8 = LevelFilter::Info.min(STATIC_MAX_LEVEL) as u8;

static MAX_LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL);
/// The most verbose of `MAX_LEVEL` and every target rule: what the macros check first.
static MAX_ANY: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL);

/// The run-time filter for targets without a rule. Starts at `Info` (or [`STATIC_MAX_LEVEL`] if
/// lower).
#[inline]
pub fn max_level() -> LevelFilter {
    LevelFilter::from_u8(MAX_LEVEL.load(Ordering::Relaxed))
}

/// Sets the run-time filter for targets without a rule. Clamped to [`STATIC_MAX_LEVEL`]:
/// compiled-out levels stay out.
pub fn set_max_level(filter: LevelFilter) {
    with_rules(|_| MAX_LEVEL.store(filter.min(STATIC_MAX_LEVEL) as u8, Ordering::Relaxed));
}

/// Whether a record at `level` from a target without a rule would reach the sinks. See
/// [`enabled_for`].
#[inline]
pub fn enabled(level: Level) -> bool {
    STATIC_MAX_LEVEL.allows(level) && max_level().allows(level)
}

/// Whether a record at `level` from `target` (a module path) would reach the sinks, with the
/// target rules applied.
pub fn enabled_for(level: Level, target: &str) -> bool {
    if !STATIC_MAX_LEVEL.allows(level) {
        return false;
    }
    let filter = if RULE_COUNT.load(Ordering::Relaxed) == 0 {
        max_level()
    } else {
        target_rule(target).unwrap_or_else(max_level)
    };
    filter.allows(level)
}

/// The macros' run-time check: one relaxed load. Lets through anything the global filter or some
/// target rule allows; `__private::log` then applies the matching rule.
#[doc(hidden)]
#[inline]
pub fn may_log(level: Level) -> bool {
    LevelFilter::from_u8(MAX_ANY.load(Ordering::Relaxed)).allows(level)
}

// Target rules: a fixed table of `(prefix, filter)`, written under `RULE_LOCK` and read lock-free
// under the seqlock `RULE_SEQ`. Writers make the count odd, change the table, then make it even;
// readers copy the table and retry if the count was odd or changed meanwhile. The copy is
// validated before any prefix is dereferenced, so a torn `(ptr, len)` pair is never used, and the
// prefixes are `&'static str`, so a validated copy stays readable after its rule is removed.

/// Capacity of the target rule table.
pub const MAX_TARGET_LEVELS: usize = 16;

/// [`set_target_level`] failed: [`MAX_TARGET_LEVELS`] rules are already set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TargetTableFull;

impl fmt::Display for TargetTableFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("log target rule table is full")
    }
}

impl core::error::Error for TargetTableFull {}

struct Rule {
    ptr: AtomicPtr<u8>,
    len: AtomicUsize,
    filter: AtomicU8,
}

static RULES: [Rule; MAX_TARGET_LEVELS] = [const {
    Rule {
        ptr: AtomicPtr::new(core::ptr::null_mut()),
        len: AtomicUsize::new(0),
        filter: AtomicU8::new(0),
    }
}; MAX_TARGET_LEVELS];
static RULE_COUNT: AtomicUsize = AtomicUsize::new(0);
static RULE_SEQ: AtomicU32 = AtomicU32::new(0);
static RULE_LOCK: AtomicBool = AtomicBool::new(false);

/// Sets the filter for the targets (module paths) equal to `prefix` or below it at a `::`
/// boundary: `marlin_asset` covers `marlin_asset::server` but not `marlin_assets`. The longest
/// matching prefix wins.
///
/// A rule overrides [`max_level`] for its targets in both directions (`Off` silences them, `Trace`
/// lets everything through), but is clamped to [`STATIC_MAX_LEVEL`]. Replaces an existing rule
/// for the same prefix.
pub fn set_target_level(prefix: &'static str, filter: LevelFilter) -> Result<(), TargetTableFull> {
    let filter = filter.min(STATIC_MAX_LEVEL) as u8;
    with_rules(|count| {
        if let Some(rule) = RULES[..*count].iter().find(|rule| rule_is(rule, prefix)) {
            rule.filter.store(filter, Ordering::Relaxed);
            return Ok(());
        }
        let Some(rule) = RULES.get(*count) else {
            return Err(TargetTableFull);
        };
        rule.ptr
            .store(prefix.as_ptr().cast_mut(), Ordering::Relaxed);
        rule.len.store(prefix.len(), Ordering::Relaxed);
        rule.filter.store(filter, Ordering::Relaxed);
        *count += 1;
        Ok(())
    })
}

/// Removes the rule for exactly `prefix`. Returns whether there was one.
pub fn clear_target_level(prefix: &str) -> bool {
    with_rules(|count| {
        let Some(index) = RULES[..*count]
            .iter()
            .position(|rule| rule_is(rule, prefix))
        else {
            return false;
        };
        // Move the last rule into the hole.
        let (last, hole) = (&RULES[*count - 1], &RULES[index]);
        hole.ptr
            .store(last.ptr.load(Ordering::Relaxed), Ordering::Relaxed);
        hole.len
            .store(last.len.load(Ordering::Relaxed), Ordering::Relaxed);
        hole.filter
            .store(last.filter.load(Ordering::Relaxed), Ordering::Relaxed);
        *count -= 1;
        true
    })
}

/// Called under `RULE_LOCK`, so the rule is not changing.
fn rule_is(rule: &Rule, prefix: &str) -> bool {
    let ptr = rule.ptr.load(Ordering::Relaxed);
    let len = rule.len.load(Ordering::Relaxed);
    // SAFETY: under the lock, `ptr` and `len` are the parts of one `&'static str`.
    len == prefix.len() && unsafe { core::slice::from_raw_parts(ptr, len) } == prefix.as_bytes()
}

/// Runs `f` on the rule count under the lock and an odd seqlock count, then recomputes `MAX_ANY`.
fn with_rules<R>(f: impl FnOnce(&mut usize) -> R) -> R {
    // Changes are rare and short, so a spin lock is enough.
    while RULE_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        spin_loop();
    }
    let seq = RULE_SEQ.load(Ordering::Relaxed);
    RULE_SEQ.store(seq.wrapping_add(1), Ordering::Relaxed);
    fence(Ordering::Release);
    let mut count = RULE_COUNT.load(Ordering::Relaxed);
    let result = f(&mut count);
    RULE_COUNT.store(count, Ordering::Relaxed);
    RULE_SEQ.store(seq.wrapping_add(2), Ordering::Release);

    let any = RULES[..count]
        .iter()
        .map(|rule| rule.filter.load(Ordering::Relaxed))
        .fold(MAX_LEVEL.load(Ordering::Relaxed), u8::max);
    MAX_ANY.store(any, Ordering::Relaxed);
    RULE_LOCK.store(false, Ordering::Release);
    result
}

/// The filter of the longest rule matching `target`, if any.
fn target_rule(target: &str) -> Option<LevelFilter> {
    let mut copy = [(core::ptr::null::<u8>(), 0usize, 0u8); MAX_TARGET_LEVELS];
    let count = loop {
        let seq = RULE_SEQ.load(Ordering::Acquire);
        if seq & 1 != 0 {
            spin_loop();
            continue;
        }
        let count = RULE_COUNT.load(Ordering::Relaxed).min(MAX_TARGET_LEVELS);
        for (slot, rule) in copy.iter_mut().zip(&RULES[..count]) {
            *slot = (
                rule.ptr.load(Ordering::Relaxed).cast_const(),
                rule.len.load(Ordering::Relaxed),
                rule.filter.load(Ordering::Relaxed),
            );
        }
        fence(Ordering::Acquire);
        if RULE_SEQ.load(Ordering::Relaxed) == seq {
            break count;
        }
    };

    let target = target.as_bytes();
    let mut best: Option<(usize, u8)> = None;
    for &(ptr, len, filter) in &copy[..count] {
        if target.len() < len || best.is_some_and(|(longest, _)| longest >= len) {
            continue;
        }
        // SAFETY: the seqlock validated the copy, so `ptr` and `len` are the parts of one
        // `&'static str`.
        let prefix = unsafe { core::slice::from_raw_parts(ptr, len) };
        let at_boundary = target.len() == len || target[len..].starts_with(b"::");
        if at_boundary && target.starts_with(prefix) {
            best = Some((len, filter));
        }
    }
    best.map(|(_, filter)| LevelFilter::from_u8(filter))
}
