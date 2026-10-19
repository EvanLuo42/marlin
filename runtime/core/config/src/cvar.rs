//! Cvar storage, layers, parsing and the typed wrappers.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::{self, Write as _};
use core::ops::{BitOr, BitOrAssign, Deref};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use marlin_std::sync::RwLock;
use marlin_text::name_hash;

use crate::declared::Declare;
use crate::error::{Bounds, Error, Expected};
use crate::registry::{self, Entry};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static CHEATS: AtomicBool = AtomicBool::new(false);
static STARTUP_FINISHED: AtomicBool = AtomicBool::new(false);
static REPLICATED_LOCK: AtomicBool = AtomicBool::new(false);

/// Whether [`Flags::DEV_ONLY`] cvars and commands may be used from outside code: in every build
/// but `marlin_build = "shipping"`.
pub const DEV_ENABLED: bool = !cfg!(marlin_build = "shipping");

/// Bumped by every effective change of any cvar.
#[inline]
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Lets [`Flags::CHEAT`] cvars and commands be used from outside code. Off by default.
pub fn set_cheats_enabled(enabled: bool) {
    CHEATS.store(enabled, Ordering::Relaxed);
}

pub fn cheats_enabled() -> bool {
    CHEATS.load(Ordering::Relaxed)
}

/// While locked (connected to a server), [`Flags::REPLICATED`] cvars take values only from
/// [`Source::Server`] and code; other sets fail with [`Error::Replicated`]. Off by default.
pub fn set_replicated_lock(locked: bool) {
    REPLICATED_LOCK.store(locked, Ordering::Relaxed);
}

pub fn replicated_locked() -> bool {
    REPLICATED_LOCK.load(Ordering::Relaxed)
}

/// Ends startup: from now on sets of [`Flags::REQUIRES_RESTART`] cvars only become pending.
/// Call after applying the config files and the command line.
pub fn finish_startup() {
    STARTUP_FINISHED.store(true, Ordering::Relaxed);
}

pub fn startup_finished() -> bool {
    STARTUP_FINISHED.load(Ordering::Relaxed)
}

/// Cvar and command flags; see the crate docs for their rules.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(u32);

impl Flags {
    pub const NONE: Self = Self(0);
    pub const READ_ONLY: Self = Self(1 << 0);
    pub const CHEAT: Self = Self(1 << 1);
    pub const ARCHIVE: Self = Self(1 << 2);
    pub const REQUIRES_RESTART: Self = Self(1 << 3);
    pub const DEV_ONLY: Self = Self(1 << 4);
    pub const REPLICATED: Self = Self(1 << 5);

    const NAMES: [(Self, &'static str); 6] = [
        (Self::READ_ONLY, "READ_ONLY"),
        (Self::CHEAT, "CHEAT"),
        (Self::ARCHIVE, "ARCHIVE"),
        (Self::REQUIRES_RESTART, "REQUIRES_RESTART"),
        (Self::DEV_ONLY, "DEV_ONLY"),
        (Self::REPLICATED, "REPLICATED"),
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
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
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

/// Who sets a value, which decides what [`Flags`] allow, and the config layer it goes to.
///
/// A cvar holds at most one value per layer; the highest layer that holds one decides its
/// value, else the default does. The layers, lowest first, are those of [`Source::LAYERS`]:
/// [`Engine`](Self::Engine) < [`Platform`](Self::Platform) <
/// [`Scalability`](Self::Scalability) < [`Project`](Self::Project) <
/// [`DeviceProfile`](Self::DeviceProfile) < [`User`](Self::User) <
/// [`CommandLine`](Self::CommandLine) < [`Console`](Self::Console) < [`Server`](Self::Server).
/// [`Code`](Self::Code) writes the console's layer: whichever of the two set last wins.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Source {
    /// The engine's config files.
    Engine,
    /// The current platform's section of the config (see [`load_platform`](crate::load_platform)).
    Platform,
    /// Values a scalability group applies (see [`load_scalability`](crate::load_scalability)).
    Scalability,
    /// The project's config files (`boot.toml`).
    Project,
    /// The device profile chosen at boot (see
    /// [`load_device_profile`](crate::load_device_profile)).
    DeviceProfile,
    /// The user's settings, saved by [`write_archive`](crate::write_archive).
    User,
    CommandLine,
    Console,
    /// Engine code: may set anything, and writes the console's layer.
    Code,
    /// The server of a networked session, for [`Flags::REPLICATED`] cvars; trusted like code.
    Server,
}

impl Source {
    /// The layers, lowest priority first. [`Source::Code`] is not one: it writes the console's.
    pub const LAYERS: [Self; 9] = [
        Self::Engine,
        Self::Platform,
        Self::Scalability,
        Self::Project,
        Self::DeviceProfile,
        Self::User,
        Self::CommandLine,
        Self::Console,
        Self::Server,
    ];

    /// The layer's priority; higher wins.
    pub(crate) const fn layer(self) -> u8 {
        match self {
            Self::Code => Self::Console as u8,
            other => other as u8,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Engine => "engine",
            Self::Platform => "platform",
            Self::Scalability => "scalability",
            Self::Project => "project",
            Self::DeviceProfile => "device profile",
            Self::User => "user",
            Self::CommandLine => "command line",
            Self::Console => "console",
            Self::Code => "code",
            Self::Server => "server",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bool,
    Int,
    Float,
    String,
    Enum,
    /// Two `f32` components.
    Vec2,
    Vec3,
    Vec4,
    /// Linear RGBA, `f32` components.
    Color,
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
    /// `n` (2 to 4) components; the rest of `default` is zero.
    Vec {
        n: u8,
        default: [f32; 4],
    },
    Color([f32; 4]),
}

impl Spec {
    const fn default_bits(self) -> [u64; 2] {
        match self {
            Self::Bool(b) => [b as u64, 0],
            Self::Int { default, .. } => [default as u64, 0],
            Self::Float { default, .. } => [default.to_bits(), 0],
            Self::Str(_) => [0, 0],
            Self::Enum { default, .. } => [default as u64, 0],
            Self::Vec { default, .. } | Self::Color(default) => pack(default),
        }
    }

    /// The number of `f32` components, for vectors and colors.
    const fn components(self) -> usize {
        match self {
            Self::Vec { n, .. } => n as usize,
            Self::Color(_) => 4,
            _ => 0,
        }
    }
}

/// Four `f32`s in two words, two per word.
const fn pack(v: [f32; 4]) -> [u64; 2] {
    [
        v[0].to_bits() as u64 | (v[1].to_bits() as u64) << 32,
        v[2].to_bits() as u64 | (v[3].to_bits() as u64) << 32,
    ]
}

fn unpack([lo, hi]: [u64; 2]) -> [f32; 4] {
    [
        f32::from_bits(lo as u32),
        f32::from_bits((lo >> 32) as u32),
        f32::from_bits(hi as u32),
        f32::from_bits((hi >> 32) as u32),
    ]
}

/// A value as a typed source (a TOML document, or parsed text) gives it, before validation.
#[derive(Clone, Copy)]
pub(crate) enum Typed<'a> {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a str),
    /// The first `.1` of up to four numbers (a TOML array).
    Numbers([f64; 4], usize),
    /// Anything no cvar holds (a longer or mixed array, a table or a datetime).
    Other,
}

/// A value to archive; enums by name.
pub(crate) enum Archived {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// The first `.1` components.
    Floats([f32; 4], usize),
}

/// A validated value, ready to store.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Value {
    Bits([u64; 2]),
    Text(String),
}

/// A value one layer holds.
struct Held {
    layer: u8,
    source: Source,
    value: Value,
}

struct State {
    /// By ascending layer; the last one is in effect.
    layers: Vec<Held>,
    /// The live value of a string cvar; `None` is the default.
    text: Option<String>,
    /// What the layers say while a restart is due, when that differs from the live value.
    pending: Option<Value>,
}

/// The type-erased cvar every typed cvar derefs to.
///
/// The live value of a scalar lives in an atomic, so reads never lock; three- and
/// four-component vectors and colors take two words behind a seqlock, so reads never lock
/// either and never see half a write. Writes are rare and serialized by the state lock, which
/// also holds each layer's value and the pending one.
pub struct Cvar {
    name: &'static str,
    /// `marlin_text::name_hash(name)`: the case-insensitive registry key.
    hash: u64,
    help: &'static str,
    flags: Flags,
    spec: Spec,
    bits: AtomicU64,
    bits_hi: AtomicU64,
    /// Odd while a two-word value is being written.
    seq: AtomicU32,
    state: RwLock<State>,
    generation: AtomicU32,
}

impl Cvar {
    const fn new(name: &'static str, help: &'static str, spec: Spec) -> Self {
        let [lo, hi] = spec.default_bits();
        Self {
            name,
            hash: name_hash(name),
            help,
            flags: Flags::NONE,
            spec,
            bits: AtomicU64::new(lo),
            bits_hi: AtomicU64::new(hi),
            seq: AtomicU32::new(0),
            state: RwLock::new(State {
                layers: Vec::new(),
                text: None,
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
            Spec::Vec { n: 2, .. } => Kind::Vec2,
            Spec::Vec { n: 3, .. } => Kind::Vec3,
            Spec::Vec { .. } => Kind::Vec4,
            Spec::Color(_) => Kind::Color,
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
        self.state.read().pending.is_some()
    }

    /// Whether this cvar's live value takes two words.
    fn wide(&self) -> bool {
        self.spec.components() > 2
    }

    /// The live bits.
    #[inline]
    fn load(&self) -> [u64; 2] {
        if !self.wide() {
            return [self.bits.load(Ordering::Relaxed), 0];
        }
        loop {
            let seq = self.seq.load(Ordering::Acquire);
            if seq & 1 == 0 {
                let lo = self.bits.load(Ordering::Relaxed);
                let hi = self.bits_hi.load(Ordering::Relaxed);
                // Orders the data loads before the second sequence load.
                fence(Ordering::Acquire);
                if self.seq.load(Ordering::Relaxed) == seq {
                    return [lo, hi];
                }
            }
            core::hint::spin_loop();
        }
    }

    /// Stores the live bits; only under the state's write lock, which serializes writers.
    fn store_live(&self, [lo, hi]: [u64; 2]) {
        if !self.wide() {
            self.bits.store(lo, Ordering::Relaxed);
            return;
        }
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        // Orders the odd sequence before the data stores.
        fence(Ordering::Release);
        self.bits.store(lo, Ordering::Relaxed);
        self.bits_hi.store(hi, Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// The current value as the config syntax writes it (unquoted; vector components separated
    /// by spaces).
    pub fn value_string(&self) -> String {
        match self.spec {
            Spec::Str(default) => self.state.read().text.as_deref().unwrap_or(default).into(),
            _ => self.format_bits(self.load()),
        }
    }

    pub fn default_string(&self) -> String {
        self.format(&self.default_value())
    }

    /// The value waiting for a restart, if any.
    pub fn pending_string(&self) -> Option<String> {
        self.state.read().pending.as_ref().map(|v| self.format(v))
    }

    pub fn is_default(&self) -> bool {
        match self.spec {
            Spec::Str(_) => self.state.read().text.is_none(),
            spec => self.load() == spec.default_bits(),
        }
    }

    /// Who set the value in effect (the highest layer that holds one); `None` for the default.
    /// While a restart is due, that is who set the pending value.
    pub fn set_by(&self) -> Option<Source> {
        self.state.read().layers.last().map(|h| h.source)
    }

    /// The value `source`'s layer holds, if any.
    pub fn layer_string(&self, source: Source) -> Option<String> {
        self.layer_value(source).map(|v| self.format(&v))
    }

    pub(crate) fn layer_value(&self, source: Source) -> Option<Value> {
        let layer = source.layer();
        let state = self.state.read();
        state
            .layers
            .iter()
            .find(|h| h.layer == layer)
            .map(|h| h.value.clone())
    }

    /// The live value, as an integer: the level of a bool, int or enum scalability group.
    pub(crate) fn level(&self) -> Option<i64> {
        match self.spec {
            Spec::Bool(_) | Spec::Int { .. } | Spec::Enum { .. } => Some(self.load()[0] as i64),
            _ => None,
        }
    }

    /// Parses a scalability level of this group cvar: what [`Cvar::level`] would return after
    /// setting `text`.
    pub(crate) fn parse_level(&self, text: &str) -> Result<i64, Error> {
        match (self.spec, self.parse(text)?) {
            (Spec::Bool(_) | Spec::Int { .. } | Spec::Enum { .. }, Value::Bits([bits, _])) => {
                Ok(bits as i64)
            }
            _ => Err(Error::Invalid(Expected::Int)),
        }
    }

    fn default_value(&self) -> Value {
        match self.spec {
            Spec::Str(default) => Value::Text(default.into()),
            spec => Value::Bits(spec.default_bits()),
        }
    }

    /// The live value; under the state lock.
    fn live_value(&self, state: &State) -> Value {
        match self.spec {
            Spec::Str(default) => Value::Text(state.text.as_deref().unwrap_or(default).into()),
            _ => Value::Bits(self.load()),
        }
    }

    pub(crate) fn format(&self, value: &Value) -> String {
        match value {
            Value::Bits(bits) => self.format_bits(*bits),
            Value::Text(s) => s.clone(),
        }
    }

    fn format_bits(&self, bits: [u64; 2]) -> String {
        let [word, _] = bits;
        match self.spec {
            Spec::Bool(_) => (if word != 0 { "true" } else { "false" }).into(),
            Spec::Int { .. } => (word as i64).to_string(),
            Spec::Float { .. } => f64::from_bits(word).to_string(),
            Spec::Enum { names, .. } => names[word as usize].into(),
            Spec::Vec { .. } | Spec::Color(_) => {
                let mut out = String::new();
                for (i, c) in unpack(bits)[..self.spec.components()].iter().enumerate() {
                    // Writing to a String cannot fail.
                    let _ = write!(out, "{}{c}", if i > 0 { " " } else { "" });
                }
                out
            }
            Spec::Str(_) => unreachable!("strings have no bits"),
        }
    }

    /// `value` as an archive writes it.
    pub(crate) fn archived(&self, value: &Value) -> Archived {
        let bits = match value {
            Value::Text(s) => return Archived::Str(s.clone()),
            Value::Bits(bits) => *bits,
        };
        match self.spec {
            Spec::Bool(_) => Archived::Bool(bits[0] != 0),
            Spec::Int { .. } => Archived::Int(bits[0] as i64),
            Spec::Float { .. } => Archived::Float(f64::from_bits(bits[0])),
            Spec::Enum { names, .. } => Archived::Str(names[bits[0] as usize].into()),
            Spec::Vec { .. } | Spec::Color(_) => {
                Archived::Floats(unpack(bits), self.spec.components())
            }
            Spec::Str(_) => unreachable!("strings have no bits"),
        }
    }

    /// Parses `text` and sets the value in `source`'s layer, if `source` may; see the crate
    /// docs.
    pub fn set_str(&'static self, text: &str, source: Source) -> Result<(), Error> {
        self.check(source)?;
        let value = self.parse(text)?;
        self.put(source, Some(value));
        Ok(())
    }

    /// Sets the default in `source`'s layer, if `source` may: the cvar is at its default until
    /// that layer's value is removed.
    pub fn reset(&'static self, source: Source) -> Result<(), Error> {
        self.check(source)?;
        self.put(source, Some(self.default_value()));
        Ok(())
    }

    /// Removes the value of `source`'s layer, if `source` may: the cvar falls back to the
    /// layers below, or to its default.
    pub fn unset(&'static self, source: Source) -> Result<(), Error> {
        self.check(source)?;
        self.put(source, None);
        Ok(())
    }

    /// `text` as this cvar would hold it, without setting it.
    pub fn normalize(&self, text: &str) -> Result<String, Error> {
        self.parse(text).map(|v| self.format(&v))
    }

    pub(crate) fn check(&self, source: Source) -> Result<(), Error> {
        if matches!(source, Source::Code | Source::Server) {
            return Ok(());
        }
        if self.flags.contains(Flags::REPLICATED) && replicated_locked() {
            return Err(Error::Replicated);
        }
        if self.flags.contains(Flags::READ_ONLY) && source != Source::CommandLine {
            return Err(Error::ReadOnly);
        }
        if self.flags.contains(Flags::CHEAT) && !cheats_enabled() {
            return Err(Error::Cheat);
        }
        if self.flags.contains(Flags::DEV_ONLY) && !DEV_ENABLED {
            return Err(Error::DevOnly);
        }
        Ok(())
    }

    /// Checks and validates a value from a typed source (a TOML document) for `source`.
    pub(crate) fn prepare(&self, value: Typed<'_>, source: Source) -> Result<Value, Error> {
        self.check(source)?;
        self.validate(value)
    }

    pub(crate) fn parse(&self, text: &str) -> Result<Value, Error> {
        let t = text.trim();
        let typed = match self.spec {
            Spec::Bool(_) => Typed::Bool(parse_bool(t).ok_or(Error::Invalid(Expected::Bool))?),
            Spec::Int { .. } => Typed::Int(parse_int(t).ok_or(Error::Invalid(Expected::Int))?),
            Spec::Float { .. } => {
                Typed::Float(t.parse().map_err(|_| Error::Invalid(Expected::Float))?)
            }
            Spec::Str(_) => Typed::Str(text),
            Spec::Enum { .. } | Spec::Vec { .. } | Spec::Color(_) => Typed::Str(t),
        };
        self.validate(typed)
    }

    /// Type and range checks.
    fn validate(&self, value: Typed<'_>) -> Result<Value, Error> {
        let bits = |word: u64| Value::Bits([word, 0]);
        match (self.spec, value) {
            (Spec::Bool(_), Typed::Bool(b)) => Ok(bits(b as u64)),
            (Spec::Bool(_), _) => Err(Error::Invalid(Expected::Bool)),
            (Spec::Int { min, max, .. }, Typed::Int(v)) => {
                if v < min || v > max {
                    return Err(Error::OutOfRange(Bounds::Int(min, max)));
                }
                Ok(bits(v as u64))
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
                Ok(bits(v.to_bits()))
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
            .map(|i| bits(i as u64))
            .ok_or(Error::Invalid(Expected::Enum(names))),
            (Spec::Vec { n, .. }, value) => numbers(value)
                .filter(|&(_, len)| len == n as usize)
                .map(|(v, _)| Value::Bits(pack(v)))
                .ok_or(Error::Invalid(Expected::Vector(n))),
            (Spec::Color(_), value) => match value {
                Typed::Str(s) if s.starts_with('#') => parse_hex_color(s),
                value => numbers(value).and_then(|(mut v, len)| match len {
                    3 => {
                        v[3] = 1.0;
                        Some(v)
                    }
                    4 => Some(v),
                    _ => None,
                }),
            }
            .map(|v| Value::Bits(pack(v)))
            .ok_or(Error::Invalid(Expected::Color)),
        }
    }

    /// Sets (`Some`) or removes (`None`) the value of `source`'s layer, then makes the live
    /// value what the layers say, or pending for a restart.
    pub(crate) fn put(&'static self, source: Source, value: Option<Value>) {
        let layer = source.layer();
        let changed = {
            let mut state = self.state.write();
            let i = state.layers.partition_point(|h| h.layer < layer);
            let held = state.layers.get(i).is_some_and(|h| h.layer == layer);
            match (value, held) {
                (Some(value), true) => {
                    state.layers[i] = Held {
                        layer,
                        source,
                        value,
                    }
                }
                (Some(value), false) => state.layers.insert(
                    i,
                    Held {
                        layer,
                        source,
                        value,
                    },
                ),
                (None, true) => {
                    state.layers.remove(i);
                }
                (None, false) => return,
            }
            self.settle(&mut state)
        };
        if changed {
            self.generation.fetch_add(1, Ordering::Release);
            GENERATION.fetch_add(1, Ordering::Release);
            registry::notify(self);
        }
    }

    /// Makes the live value what the layers say, or pending until a restart. Returns whether
    /// the live value changed.
    fn settle(&self, state: &mut State) -> bool {
        let wanted = state
            .layers
            .last()
            .map_or_else(|| self.default_value(), |h| h.value.clone());
        let live = self.live_value(state);
        if self.flags.contains(Flags::REQUIRES_RESTART) && startup_finished() {
            state.pending = (wanted != live).then_some(wanted);
            return false;
        }
        state.pending = None;
        if wanted == live {
            return false;
        }
        match wanted {
            Value::Bits(bits) => self.store_live(bits),
            Value::Text(s) => {
                let Spec::Str(default) = self.spec else {
                    unreachable!("only string cvars hold text")
                };
                state.text = (s != default).then_some(s);
            }
        }
        true
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

/// Up to four finite `f32`s from an array or text, with how many there are.
fn numbers(value: Typed<'_>) -> Option<([f32; 4], usize)> {
    let mut out = [0.0; 4];
    let len = match value {
        Typed::Numbers(v, len) => {
            for (o, v) in out.iter_mut().zip(&v[..len]) {
                *o = *v as f32;
            }
            len
        }
        Typed::Str(s) => parse_numbers(s, &mut out)?,
        _ => return None,
    };
    out.iter().all(|c| c.is_finite()).then_some((out, len))
}

/// `1 2 3`, `1, 2, 3`, `(1, 2, 3)` or `[1 2 3]`: up to four numbers into `out`.
fn parse_numbers(s: &str, out: &mut [f32; 4]) -> Option<usize> {
    let s = s.trim();
    let s = match (s.as_bytes().first(), s.as_bytes().last()) {
        (Some(b'('), Some(b')')) | (Some(b'['), Some(b']')) => s[1..s.len() - 1].trim(),
        _ => s,
    };
    let mut len = 0;
    let mut push = |part: &str| -> Option<()> {
        *out.get_mut(len)? = part.parse().ok()?;
        len += 1;
        Some(())
    };
    if s.contains(',') {
        for part in s.split(',') {
            push(part.trim())?;
        }
    } else {
        for part in s.split_ascii_whitespace() {
            push(part)?;
        }
    }
    Some(len)
}

/// `#rrggbb` (opaque) or `#rrggbbaa`, each byte mapped to 0..=1.
fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let digits = s.strip_prefix('#')?;
    if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [1.0; 4];
    for (i, o) in out.iter_mut().enumerate().take(digits.len() / 2) {
        let byte = u8::from_str_radix(&digits[2 * i..2 * i + 2], 16).ok()?;
        *o = f32::from(byte) / 255.0;
    }
    Some(out)
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

            pub const fn dev_only(self) -> Self {
                self.with_flags(Flags::DEV_ONLY)
            }

            pub const fn replicated(self) -> Self {
                self.with_flags(Flags::REPLICATED)
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

        impl Declare for $ty {
            fn entry(&'static self) -> Entry {
                Entry::Cvar(&self.0)
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
typed_cvar!(
    /// Two `f32`s. Parses `1 2`, `1, 2`, `(1, 2)` or `[1 2]`; TOML takes an array of numbers.
    /// Rejects NaN and infinities.
    CvarVec2
);
typed_cvar!(
    /// Three `f32`s; see [`CvarVec2`]. Reads never lock.
    CvarVec3
);
typed_cvar!(
    /// Four `f32`s; see [`CvarVec2`]. Reads never lock.
    CvarVec4
);
typed_cvar!(
    /// A linear RGBA color of `f32`s. Parses `#rrggbb` (opaque) and `#rrggbbaa`, each byte
    /// divided by 255, or 3 (opaque) or 4 numbers as [`CvarVec2`] does; TOML takes such a
    /// string or an array. Unclamped, for HDR; rejects NaN and infinities.
    CvarColor
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
        self.0
            .put(Source::Code, Some(Value::Bits([value as u64, 0])));
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
        let value = value.clamp(min, max) as u64;
        self.0.put(Source::Code, Some(Value::Bits([value, 0])));
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
            let bits = value.clamp(min, max).to_bits();
            self.0.put(Source::Code, Some(Value::Bits([bits, 0])));
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
        f(self.0.state.read().text.as_deref().unwrap_or(default))
    }

    pub fn set(&'static self, value: &str) {
        self.0.put(Source::Code, Some(Value::Text(value.into())));
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
        let index = u64::from(index.min(last));
        self.0.put(Source::Code, Some(Value::Bits([index, 0])));
    }
}

/// `N` finite components into four, zero-padded; panics (at compile time, in a const) on a
/// non-finite one.
const fn widen<const N: usize>(v: [f32; N]) -> [f32; 4] {
    let mut out = [0.0; 4];
    let mut i = 0;
    while i < N {
        assert!(v[i].is_finite(), "default not finite");
        out[i] = v[i];
        i += 1;
    }
    out
}

macro_rules! vector_cvar {
    ($ty:ident, $n:literal, |$default:ident| $spec:expr) => {
        impl $ty {
            pub const fn new(name: &'static str, default: [f32; $n], help: &'static str) -> Self {
                let $default = widen(default);
                Self(Cvar::new(name, help, $spec))
            }

            #[inline]
            pub fn get(&self) -> [f32; $n] {
                let all = unpack(self.0.load());
                let mut v = [0.0; $n];
                v.copy_from_slice(&all[..$n]);
                v
            }

            /// Ignores a value with a non-finite component.
            pub fn set(&'static self, value: [f32; $n]) {
                if value.iter().all(|c| c.is_finite()) {
                    let mut all = [0.0; 4];
                    all[..$n].copy_from_slice(&value);
                    self.0.put(Source::Code, Some(Value::Bits(pack(all))));
                }
            }
        }
    };
}

vector_cvar!(CvarVec2, 2, |default| Spec::Vec { n: 2, default });
vector_cvar!(CvarVec3, 3, |default| Spec::Vec { n: 3, default });
vector_cvar!(CvarVec4, 4, |default| Spec::Vec { n: 4, default });
vector_cvar!(CvarColor, 4, |default| Spec::Color(default));
