//! Cvar storage, parsing and the typed wrappers.

use alloc::string::{String, ToString};
use core::fmt;
use core::ops::{BitOr, BitOrAssign, Deref};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use marlin_std::sync::RwLock;
use marlin_text::name_hash;

use crate::error::{Bounds, Error, Expected};
use crate::registry;

static GENERATION: AtomicU64 = AtomicU64::new(0);
static CHEATS: AtomicBool = AtomicBool::new(false);
static STARTUP_FINISHED: AtomicBool = AtomicBool::new(false);

/// Bumped by every effective change of any cvar.
#[inline]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Lets [`Flags::CHEAT`] cvars be set from outside code. Off by default.
pub fn set_cheats_enabled(enabled: bool) {
    CHEATS.store(enabled, Ordering::Relaxed);
}

pub fn cheats_enabled() -> bool {
    CHEATS.load(Ordering::Relaxed)
}

/// Ends startup: from now on sets of [`Flags::REQUIRES_RESTART`] cvars only become pending.
/// Call after applying the config files and the command line.
pub fn finish_startup() {
    STARTUP_FINISHED.store(true, Ordering::Relaxed);
}

pub fn startup_finished() -> bool {
    STARTUP_FINISHED.load(Ordering::Relaxed)
}

/// Cvar flags; see the crate docs for their rules.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(u32);

impl Flags {
    pub const NONE: Self = Self(0);
    pub const READ_ONLY: Self = Self(1 << 0);
    pub const CHEAT: Self = Self(1 << 1);
    pub const ARCHIVE: Self = Self(1 << 2);
    pub const REQUIRES_RESTART: Self = Self(1 << 3);

    const NAMES: [(Self, &'static str); 4] = [
        (Self::READ_ONLY, "READ_ONLY"),
        (Self::CHEAT, "CHEAT"),
        (Self::ARCHIVE, "ARCHIVE"),
        (Self::REQUIRES_RESTART, "REQUIRES_RESTART"),
    ];

    /// `|` for const contexts.
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl BitOr for Flags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl BitOrAssign for Flags {
    fn bitor_assign(&mut self, other: Self) {
        *self = self.union(other);
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (flag, name) in Self::NAMES {
            if self.contains(flag) {
                f.write_str(if first { "" } else { " | " })?;
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("NONE")?;
        }
        Ok(())
    }
}

/// Who is setting a cvar; decides what [`Flags`] allow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Code,
    CommandLine,
    ConfigFile,
    Console,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bool,
    Int,
    Float,
    String,
    Enum,
}

#[derive(Clone, Copy)]
enum Spec {
    Bool(bool),
    Int {
        default: i64,
        min: i64,
        max: i64,
    },
    Float {
        default: f64,
        min: f64,
        max: f64,
    },
    Str(&'static str),
    Enum {
        default: u32,
        names: &'static [&'static str],
    },
}

impl Spec {
    const fn default_bits(self) -> u64 {
        match self {
            Self::Bool(b) => b as u64,
            Self::Int { default, .. } => default as u64,
            Self::Float { default, .. } => default.to_bits(),
            Self::Str(_) => 0,
            Self::Enum { default, .. } => default as u64,
        }
    }
}

/// String values; `None` means the default. `pending` is meaningful only with `has_pending`.
struct Text {
    live: Option<String>,
    pending: Option<String>,
}

/// A value as a typed source (a TOML document, or parsed text) gives it, before validation.
#[derive(Clone, Copy)]
pub(crate) enum Typed<'a> {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a str),
    /// Anything no cvar holds (a TOML array, table or datetime).
    Other,
}

/// A value to archive; enums by name.
pub(crate) enum Archived {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// A validated value, ready to store.
enum Value {
    Bits(u64),
    Text(String),
}

/// The type-erased cvar every typed cvar derefs to.
///
/// Scalars live in an atomic, so reads never lock. Writes are rare and serialized by the string
/// lock, which also keeps the pending value consistent.
pub struct Cvar {
    name: &'static str,
    /// `marlin_text::name_hash(name)`: the case-insensitive registry key.
    hash: u64,
    help: &'static str,
    flags: Flags,
    spec: Spec,
    bits: AtomicU64,
    pending: AtomicU64,
    has_pending: AtomicBool,
    text: RwLock<Text>,
    generation: AtomicU32,
}

impl Cvar {
    const fn new(name: &'static str, help: &'static str, spec: Spec) -> Self {
        Self {
            name,
            hash: name_hash(name),
            help,
            flags: Flags::NONE,
            spec,
            bits: AtomicU64::new(spec.default_bits()),
            pending: AtomicU64::new(0),
            has_pending: AtomicBool::new(false),
            text: RwLock::new(Text {
                live: None,
                pending: None,
            }),
            generation: AtomicU32::new(0),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) fn hash(&self) -> u64 {
        self.hash
    }

    pub fn help(&self) -> &'static str {
        self.help
    }

    pub fn flags(&self) -> Flags {
        self.flags
    }

    pub fn kind(&self) -> Kind {
        match self.spec {
            Spec::Bool(_) => Kind::Bool,
            Spec::Int { .. } => Kind::Int,
            Spec::Float { .. } => Kind::Float,
            Spec::Str(_) => Kind::String,
            Spec::Enum { .. } => Kind::Enum,
        }
    }

    pub fn int_range(&self) -> Option<(i64, i64)> {
        match self.spec {
            Spec::Int { min, max, .. } => Some((min, max)),
            _ => None,
        }
    }

    pub fn float_range(&self) -> Option<(f64, f64)> {
        match self.spec {
            Spec::Float { min, max, .. } => Some((min, max)),
            _ => None,
        }
    }

    /// The names of an enum cvar; empty for other kinds.
    pub fn enum_names(&self) -> &'static [&'static str] {
        match self.spec {
            Spec::Enum { names, .. } => names,
            _ => &[],
        }
    }

    /// Bumped by every effective change of this cvar.
    #[inline]
    pub fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    /// A set of this [`Flags::REQUIRES_RESTART`] cvar is waiting for a restart.
    pub fn needs_restart(&self) -> bool {
        self.has_pending.load(Ordering::Relaxed)
    }

    /// The current value as the config syntax writes it (unquoted).
    pub fn value_string(&self) -> String {
        match self.spec {
            Spec::Str(default) => self.text.read().live.as_deref().unwrap_or(default).into(),
            _ => self.format_bits(self.bits.load(Ordering::Relaxed)),
        }
    }

    pub fn default_string(&self) -> String {
        match self.spec {
            Spec::Str(default) => default.into(),
            spec => self.format_bits(spec.default_bits()),
        }
    }

    /// The value waiting for a restart, if any.
    pub fn pending_string(&self) -> Option<String> {
        let text = self.text.read();
        if !self.has_pending.load(Ordering::Relaxed) {
            return None;
        }
        Some(match self.spec {
            Spec::Str(default) => text.pending.as_deref().unwrap_or(default).into(),
            _ => self.format_bits(self.pending.load(Ordering::Relaxed)),
        })
    }

    pub fn is_default(&self) -> bool {
        match self.spec {
            Spec::Str(_) => self.text.read().live.is_none(),
            spec => self.bits.load(Ordering::Relaxed) == spec.default_bits(),
        }
    }

    /// What an archive should hold: the pending value, else the current one; `None` when that
    /// is the default.
    pub(crate) fn archive_value(&self) -> Option<Archived> {
        let text = self.text.read();
        let pending = self.has_pending.load(Ordering::Relaxed);
        let bits = if pending { &self.pending } else { &self.bits }.load(Ordering::Relaxed);
        if !matches!(self.spec, Spec::Str(_)) && bits == self.spec.default_bits() {
            return None;
        }
        Some(match self.spec {
            Spec::Bool(_) => Archived::Bool(bits != 0),
            Spec::Int { .. } => Archived::Int(bits as i64),
            Spec::Float { .. } => Archived::Float(f64::from_bits(bits)),
            Spec::Enum { names, .. } => Archived::Str(names[bits as usize].into()),
            Spec::Str(_) => {
                Archived::Str(if pending { &text.pending } else { &text.live }.clone()?)
            }
        })
    }

    fn format_bits(&self, bits: u64) -> String {
        match self.spec {
            Spec::Bool(_) => (if bits != 0 { "true" } else { "false" }).into(),
            Spec::Int { .. } => (bits as i64).to_string(),
            Spec::Float { .. } => f64::from_bits(bits).to_string(),
            Spec::Enum { names, .. } => names[bits as usize].into(),
            Spec::Str(_) => unreachable!("strings have no bits"),
        }
    }

    /// Parses `text` and sets the value if `source` may; see the crate docs.
    pub fn set_str(&'static self, text: &str, source: Source) -> Result<(), Error> {
        self.check(source)?;
        let value = self.parse(text)?;
        self.store(value);
        Ok(())
    }

    /// Restores the default if `source` may.
    pub fn reset(&'static self, source: Source) -> Result<(), Error> {
        self.check(source)?;
        self.store(match self.spec {
            Spec::Str(default) => Value::Text(default.into()),
            spec => Value::Bits(spec.default_bits()),
        });
        Ok(())
    }

    fn check(&self, source: Source) -> Result<(), Error> {
        if source == Source::Code {
            return Ok(());
        }
        if self.flags.contains(Flags::READ_ONLY) && source != Source::CommandLine {
            return Err(Error::ReadOnly);
        }
        if self.flags.contains(Flags::CHEAT) && !cheats_enabled() {
            return Err(Error::Cheat);
        }
        Ok(())
    }

    /// Sets a value from a typed source (a TOML document) if `source` may.
    pub(crate) fn set_typed(&'static self, value: Typed<'_>, source: Source) -> Result<(), Error> {
        self.check(source)?;
        let value = self.validate(value)?;
        self.store(value);
        Ok(())
    }

    fn parse(&self, text: &str) -> Result<Value, Error> {
        let t = text.trim();
        let typed = match self.spec {
            Spec::Bool(_) => Typed::Bool(parse_bool(t).ok_or(Error::Invalid(Expected::Bool))?),
            Spec::Int { .. } => Typed::Int(parse_int(t).ok_or(Error::Invalid(Expected::Int))?),
            Spec::Float { .. } => {
                Typed::Float(t.parse().map_err(|_| Error::Invalid(Expected::Float))?)
            }
            Spec::Str(_) => Typed::Str(text),
            Spec::Enum { .. } => Typed::Str(t),
        };
        self.validate(typed)
    }

    /// Type and range checks.
    fn validate(&self, value: Typed<'_>) -> Result<Value, Error> {
        match (self.spec, value) {
            (Spec::Bool(_), Typed::Bool(b)) => Ok(Value::Bits(b as u64)),
            (Spec::Bool(_), _) => Err(Error::Invalid(Expected::Bool)),
            (Spec::Int { min, max, .. }, Typed::Int(v)) => {
                if v < min || v > max {
                    return Err(Error::OutOfRange(Bounds::Int(min, max)));
                }
                Ok(Value::Bits(v as u64))
            }
            (Spec::Int { .. }, _) => Err(Error::Invalid(Expected::Int)),
            (Spec::Float { min, max, .. }, Typed::Float(_) | Typed::Int(_)) => {
                let v = match value {
                    Typed::Int(i) => i as f64,
                    Typed::Float(f) => f,
                    _ => unreachable!(),
                };
                if !v.is_finite() {
                    return Err(Error::Invalid(Expected::Float));
                }
                if v < min || v > max {
                    return Err(Error::OutOfRange(Bounds::Float(min, max)));
                }
                Ok(Value::Bits(v.to_bits()))
            }
            (Spec::Float { .. }, _) => Err(Error::Invalid(Expected::Float)),
            (Spec::Str(_), Typed::Str(s)) => Ok(Value::Text(s.into())),
            (Spec::Str(_), _) => Err(Error::Invalid(Expected::String)),
            (Spec::Enum { names, .. }, value) => match value {
                Typed::Str(s) => names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(s))
                    .or_else(|| s.parse::<usize>().ok().filter(|&i| i < names.len())),
                Typed::Int(i) => usize::try_from(i).ok().filter(|&i| i < names.len()),
                _ => None,
            }
            .map(|i| Value::Bits(i as u64))
            .ok_or(Error::Invalid(Expected::Enum(names))),
        }
    }

    /// Stores a valid value, or makes it pending for a restart.
    fn store(&'static self, value: Value) {
        let restart = self.flags.contains(Flags::REQUIRES_RESTART) && startup_finished();
        let changed = {
            let mut text = self.text.write();
            match value {
                Value::Bits(bits) => {
                    let live = self.bits.load(Ordering::Relaxed);
                    if restart {
                        self.pending.store(bits, Ordering::Relaxed);
                        self.has_pending.store(bits != live, Ordering::Relaxed);
                        false
                    } else {
                        self.bits.store(bits, Ordering::Relaxed);
                        self.has_pending.store(false, Ordering::Relaxed);
                        bits != live
                    }
                }
                Value::Text(s) => {
                    let Spec::Str(default) = self.spec else {
                        unreachable!("only string cvars parse to text")
                    };
                    let value = (s != default).then_some(s);
                    if restart {
                        let differs = value != text.live;
                        text.pending = if differs { value } else { None };
                        self.has_pending.store(differs, Ordering::Relaxed);
                        false
                    } else {
                        let changed = value != text.live;
                        text.live = value;
                        text.pending = None;
                        self.has_pending.store(false, Ordering::Relaxed);
                        changed
                    }
                }
            }
        };
        if changed {
            self.generation.fetch_add(1, Ordering::Release);
            GENERATION.fetch_add(1, Ordering::Release);
            registry::notify(self);
        }
    }
}

impl fmt::Debug for Cvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cvar")
            .field("name", &self.name)
            .field("kind", &self.kind())
            .field("value", &self.value_string())
            .field("flags", &self.flags)
            .finish()
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    const TRUE: [&str; 4] = ["true", "1", "on", "yes"];
    const FALSE: [&str; 4] = ["false", "0", "off", "no"];
    if TRUE.iter().any(|t| t.eq_ignore_ascii_case(s)) {
        Some(true)
    } else if FALSE.iter().any(|f| f.eq_ignore_ascii_case(s)) {
        Some(false)
    } else {
        None
    }
}

/// Decimal or `0x` hexadecimal, with an optional sign.
fn parse_int(s: &str) -> Option<i64> {
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = match digits.get(..2) {
        Some("0x" | "0X") => (16, &digits[2..]),
        _ => (10, digits),
    };
    // `from_str_radix` would accept a second sign.
    if digits.is_empty() || digits.starts_with(['+', '-']) {
        return None;
    }
    let magnitude = u64::from_str_radix(digits, radix).ok()?;
    if negative {
        0i64.checked_sub_unsigned(magnitude)
    } else {
        i64::try_from(magnitude).ok()
    }
}

macro_rules! typed_cvar {
    ($(#[$doc:meta])* $ty:ident) => {
        $(#[$doc])*
        #[repr(transparent)]
        pub struct $ty(Cvar);

        impl $ty {
            /// Adds `flags`.
            pub const fn with_flags(mut self, flags: Flags) -> Self {
                self.0.flags = self.0.flags.union(flags);
                self
            }

            pub const fn read_only(self) -> Self {
                self.with_flags(Flags::READ_ONLY)
            }

            pub const fn cheat(self) -> Self {
                self.with_flags(Flags::CHEAT)
            }

            pub const fn archive(self) -> Self {
                self.with_flags(Flags::ARCHIVE)
            }

            pub const fn requires_restart(self) -> Self {
                self.with_flags(Flags::REQUIRES_RESTART)
            }
        }

        impl Deref for $ty {
            type Target = Cvar;

            #[inline]
            fn deref(&self) -> &Cvar {
                &self.0
            }
        }

        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Debug::fmt(&self.0, f)
            }
        }
    };
}

typed_cvar!(
    /// A boolean cvar. Parses `true`/`false`, `1`/`0`, `on`/`off`, `yes`/`no`, in any case.
    CvarBool
);
typed_cvar!(
    /// An integer cvar with an optional inclusive range. Parses decimal and `0x` hex.
    CvarI64
);
typed_cvar!(
    /// A floating-point cvar with an optional inclusive range. Rejects NaN and infinities.
    CvarF64
);
typed_cvar!(
    /// A string cvar. Reads take a read lock; not for per-frame use in hot loops.
    CvarString
);
typed_cvar!(
    /// One of a fixed list of names, stored as its index. Parses a name (any case) or an index.
    CvarEnum
);

impl CvarBool {
    pub const fn new(name: &'static str, default: bool, help: &'static str) -> Self {
        Self(Cvar::new(name, help, Spec::Bool(default)))
    }

    #[inline]
    pub fn get(&self) -> bool {
        self.0.bits.load(Ordering::Relaxed) != 0
    }

    pub fn set(&'static self, value: bool) {
        self.0.store(Value::Bits(value as u64));
    }
}

impl CvarI64 {
    pub const fn new(name: &'static str, default: i64, help: &'static str) -> Self {
        Self(Cvar::new(
            name,
            help,
            Spec::Int {
                default,
                min: i64::MIN,
                max: i64::MAX,
            },
        ))
    }

    /// Limits the value to `min..=max`, which must contain the default.
    pub const fn range(mut self, min: i64, max: i64) -> Self {
        let Spec::Int { default, .. } = self.0.spec else {
            unreachable!()
        };
        assert!(
            min <= default && default <= max,
            "default outside the range"
        );
        self.0.spec = Spec::Int { default, min, max };
        self
    }

    #[inline]
    pub fn get(&self) -> i64 {
        self.0.bits.load(Ordering::Relaxed) as i64
    }

    /// Clamps to the range.
    pub fn set(&'static self, value: i64) {
        let Spec::Int { min, max, .. } = self.0.spec else {
            unreachable!()
        };
        self.0.store(Value::Bits(value.clamp(min, max) as u64));
    }
}

impl CvarF64 {
    pub const fn new(name: &'static str, default: f64, help: &'static str) -> Self {
        assert!(default.is_finite(), "default not finite");
        Self(Cvar::new(
            name,
            help,
            Spec::Float {
                default,
                min: f64::MIN,
                max: f64::MAX,
            },
        ))
    }

    /// Limits the value to `min..=max`, which must contain the default.
    pub const fn range(mut self, min: f64, max: f64) -> Self {
        let Spec::Float { default, .. } = self.0.spec else {
            unreachable!()
        };
        assert!(
            min <= default && default <= max,
            "default outside the range"
        );
        self.0.spec = Spec::Float { default, min, max };
        self
    }

    #[inline]
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.bits.load(Ordering::Relaxed))
    }

    /// Clamps to the range; ignores NaN.
    pub fn set(&'static self, value: f64) {
        let Spec::Float { min, max, .. } = self.0.spec else {
            unreachable!()
        };
        if !value.is_nan() {
            self.0.store(Value::Bits(value.clamp(min, max).to_bits()));
        }
    }
}

impl CvarString {
    pub const fn new(name: &'static str, default: &'static str, help: &'static str) -> Self {
        Self(Cvar::new(name, help, Spec::Str(default)))
    }

    pub fn get(&self) -> String {
        self.0.value_string()
    }

    /// Calls `f` with the value, under a read lock, without copying it.
    pub fn with<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        let Spec::Str(default) = self.0.spec else {
            unreachable!()
        };
        f(self.0.text.read().live.as_deref().unwrap_or(default))
    }

    pub fn set(&'static self, value: &str) {
        self.0.store(Value::Text(value.into()));
    }
}

impl CvarEnum {
    /// `default` indexes `names`, which must not be empty.
    pub const fn new(
        name: &'static str,
        default: u32,
        names: &'static [&'static str],
        help: &'static str,
    ) -> Self {
        assert!(
            (default as usize) < names.len(),
            "default outside the names"
        );
        Self(Cvar::new(name, help, Spec::Enum { default, names }))
    }

    /// The index into the names.
    #[inline]
    pub fn get(&self) -> u32 {
        self.0.bits.load(Ordering::Relaxed) as u32
    }

    pub fn get_name(&self) -> &'static str {
        self.enum_names()[self.get() as usize]
    }

    /// Clamps to the last name.
    pub fn set(&'static self, index: u32) {
        let last = self.enum_names().len() as u32 - 1;
        self.0.store(Value::Bits(u64::from(index.min(last))));
    }
}
