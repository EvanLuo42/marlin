//! Log records, the sink registry and the console sinks.

use core::fmt::{self, Write};
use core::hint::spin_loop;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering, fence};
use core::time::Duration;

use marlin_pal::debug;

use crate::{FixedBuf, Level};

/// Longest formatted line the built-in sinks write, newline included. Longer lines are truncated
/// and end in `...`.
pub const LINE_LEN: usize = 1024;

/// One log message, handed to every sink by reference. Borrowed and formatted lazily: sinks
/// that drop a record never pay for formatting it.
///
/// Displays as the line the built-in sinks write, without the newline:
/// `[   1.234] LEVEL [thread] target: message`, the timestamp in seconds since [`init`](crate::init).
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    level: Level,
    target: &'a str,
    file: &'a str,
    line: u32,
    thread_id: u64,
    timestamp: u64,
    args: fmt::Arguments<'a>,
}

impl<'a> Record<'a> {
    /// A record stamped with the current tick.
    pub fn new(
        level: Level,
        target: &'a str,
        file: &'a str,
        line: u32,
        thread_id: u64,
        args: fmt::Arguments<'a>,
    ) -> Self {
        let timestamp = marlin_pal::time::ticks();
        crate::clock::start(timestamp);
        Self {
            level,
            target,
            file,
            line,
            thread_id,
            timestamp,
            args,
        }
    }

    /// Replaces the timestamp, in `marlin_pal::time::ticks` units.
    pub fn with_timestamp(self, ticks: u64) -> Self {
        Self {
            timestamp: ticks,
            ..self
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// The module path of the call site.
    pub fn target(&self) -> &'a str {
        self.target
    }

    pub fn file(&self) -> &'a str {
        self.file
    }

    pub fn line(&self) -> u32 {
        self.line
    }

    /// OS id of the logging thread.
    pub fn thread_id(&self) -> u64 {
        self.thread_id
    }

    /// When the record was made, in `marlin_pal::time::ticks` units (monotonic).
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// Time from the log clock's start ([`init`](crate::init), else the first record) to the
    /// record; zero for records stamped before it.
    pub fn since_start(&self) -> Duration {
        crate::clock::since_start(self.timestamp)
    }

    /// The message. Format it with `write!`; `to_string` would allocate.
    pub fn args(&self) -> fmt::Arguments<'a> {
        self.args
    }
}

impl fmt::Display for Record<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let time = self.since_start();
        write!(
            f,
            "[{:>4}.{:03}] {:<5} [{}] {}: {}",
            time.as_secs(),
            time.subsec_millis(),
            self.level,
            self.thread_id,
            self.target,
            self.args
        )
    }
}

/// `record` as one line, ending in `'\n'`, at most [`LINE_LEN`] bytes.
pub(crate) fn format_line(record: &Record<'_>) -> FixedBuf<LINE_LEN> {
    let mut line = FixedBuf::new();
    let _ = write!(line, "{record}");
    line.end_line();
    line
}

/// A log destination. Called concurrently from any thread, so it must synchronize its own state.
///
/// Must not log itself: that would recurse into every sink. Must not panic: the panic handler
/// logs through it.
pub trait Sink: Sync {
    fn log(&self, record: &Record<'_>);

    /// Pushes buffered output to its destination. Called on [`flush`] and before an assertion
    /// failure breaks into the debugger.
    fn flush(&self) {}
}

/// A destination for already formatted lines, such as the worker of an
/// [`AsyncSink`](crate::AsyncSink). Same rules as [`Sink`].
pub trait LineSink: Sync {
    /// Writes one line, `'\n'` included, formatted from a record at `level`.
    fn write_line(&self, level: Level, line: &str);

    fn flush(&self) {}
}

// The registry.
//
// Each slot holds a `&'static dyn Sink` as its two pointer words (data and vtable, in whatever
// order the compiler lays them out), guarded by a per-slot seqlock: the writer, under
// `REGISTER_LOCK`, makes the slot's count odd, stores both words, then makes it even. A reader
// loads the count, skips the slot if it is odd (a change in progress counts as "not registered
// yet" or "already removed"), loads both words, and retries if the count changed. So a reader
// only ever rebuilds a reference from words stored together, never a torn pair, and the sink it
// gets is `'static`, so calling it after it was unregistered is still sound. Both null means
// empty: a reference is never null.

/// Capacity of the sink registry.
pub const MAX_SINKS: usize = 8;

/// [`register_sink`] failed: [`MAX_SINKS`] sinks are already registered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegistryFull;

impl fmt::Display for RegistryFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("log sink registry is full")
    }
}

impl core::error::Error for RegistryFull {}

type Words = [*mut (); 2];

const _: () = assert!(size_of::<&dyn Sink>() == size_of::<Words>());

struct Slot {
    seq: AtomicU32,
    words: [AtomicPtr<()>; 2],
}

impl Slot {
    /// Under `REGISTER_LOCK`.
    fn store(&self, sink: Option<&'static dyn Sink>) {
        let words: Words = match sink {
            // SAFETY: a fat reference is two pointer-sized words (checked above); reading them as
            // pointers keeps their provenance for `load` to transmute back.
            Some(sink) => unsafe { core::mem::transmute::<&'static dyn Sink, Words>(sink) },
            None => [ptr::null_mut(); 2],
        };
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.words[0].store(words[0], Ordering::Relaxed);
        self.words[1].store(words[1], Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// Lock-free. `None` if empty or being changed.
    fn load(&self) -> Option<&'static dyn Sink> {
        loop {
            let seq = self.seq.load(Ordering::Acquire);
            if seq & 1 != 0 {
                return None;
            }
            let words: Words = [
                self.words[0].load(Ordering::Relaxed),
                self.words[1].load(Ordering::Relaxed),
            ];
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) != seq {
                spin_loop();
                continue;
            }
            if words == [ptr::null_mut(); 2] {
                return None;
            }
            // SAFETY: the seqlock shows both words come from one `store` of a
            // `&'static dyn Sink`.
            return Some(unsafe { core::mem::transmute::<Words, &'static dyn Sink>(words) });
        }
    }

    /// Under `REGISTER_LOCK`, so the words are stable.
    fn is_empty(&self) -> bool {
        self.words[0].load(Ordering::Relaxed).is_null()
            && self.words[1].load(Ordering::Relaxed).is_null()
    }
}

// SAFETY: the words are only stored as the parts of a `&'static dyn Sink`, which is `Send` and
// `Sync` (`Sink: Sync`).
unsafe impl Sync for Slot {}

static SINKS: [Slot; MAX_SINKS] = [const {
    Slot {
        seq: AtomicU32::new(0),
        words: [const { AtomicPtr::new(ptr::null_mut()) }; 2],
    }
}; MAX_SINKS];
/// Registered sinks; 0 sends records to [`DebugOutput`].
static SINK_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGISTER_LOCK: AtomicBool = AtomicBool::new(false);

fn with_registry<R>(f: impl FnOnce() -> R) -> R {
    // Registration is rare and short, so a spin lock is enough.
    while REGISTER_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        spin_loop();
    }
    let result = f();
    REGISTER_LOCK.store(false, Ordering::Release);
    result
}

/// Whether `a` and `b` are the same sink. Distinct zero-sized sinks may share an address, so for
/// those the vtable must match too.
fn same_sink(a: &dyn Sink, b: &dyn Sink) -> bool {
    ptr::addr_eq(a, b) && (size_of_val(a) != 0 || ptr::eq(a, b))
}

/// Adds `sink` to the registry. Records go to the sinks in slot order; a freed slot is reused.
///
/// While no sink is registered, records go to [`DebugOutput`]. Registering any sink replaces that
/// fallback, so register `&DebugOutput` too to keep it.
pub fn register_sink(sink: &'static dyn Sink) -> Result<(), RegistryFull> {
    with_registry(|| {
        let slot = SINKS
            .iter()
            .find(|slot| slot.is_empty())
            .ok_or(RegistryFull)?;
        slot.store(Some(sink));
        SINK_COUNT.fetch_add(1, Ordering::Release);
        Ok(())
    })
}

/// Removes `sink` (one registration of it) from the registry. Returns whether it was registered.
///
/// Records being dispatched on other threads at that moment may still reach it; records
/// dispatched after this returns do not. Flush it afterwards if it buffers.
pub fn unregister_sink(sink: &'static dyn Sink) -> bool {
    with_registry(|| {
        let Some(slot) = SINKS
            .iter()
            .find(|slot| slot.load().is_some_and(|s| same_sink(s, sink)))
        else {
            return false;
        };
        slot.store(None);
        SINK_COUNT.fetch_sub(1, Ordering::Release);
        true
    })
}

fn sinks() -> impl Iterator<Item = &'static dyn Sink> {
    SINKS.iter().filter_map(Slot::load)
}

/// Sends `record` to every sink, regardless of the level filters.
pub fn dispatch(record: &Record<'_>) {
    if SINK_COUNT.load(Ordering::Acquire) == 0 {
        DebugOutput.log(record);
        return;
    }
    for sink in sinks() {
        sink.log(record);
    }
}

/// Flushes every sink.
pub fn flush() {
    if SINK_COUNT.load(Ordering::Acquire) == 0 {
        Sink::flush(&DebugOutput);
        return;
    }
    for sink in sinks() {
        sink.flush();
    }
}

/// The default sink: one `marlin_pal::debug::output` call per record (stderr, plus the debugger
/// when attached), formatted as [`Record`] displays.
///
/// Lines longer than [`DebugOutput::LINE`] bytes are truncated and end in `...`.
#[derive(Clone, Copy, Default, Debug)]
pub struct DebugOutput;

impl DebugOutput {
    pub const LINE: usize = LINE_LEN;
}

impl Sink for DebugOutput {
    fn log(&self, record: &Record<'_>) {
        debug::output(format_line(record).as_str());
    }
}

impl LineSink for DebugOutput {
    fn write_line(&self, _level: Level, line: &str) {
        debug::output(line);
    }
}

/// [`DebugOutput`] with the level optionally colored by ANSI escapes.
///
/// Colors are only written when `color` is set. The escapes go to the debugger too when one is
/// attached. A Windows console shows them only with virtual terminal processing enabled, which
/// [`ConsoleSink::auto`] turns on (and which it checks stderr supports).
#[derive(Clone, Copy, Default, Debug)]
pub struct ConsoleSink {
    pub color: bool,
}

impl ConsoleSink {
    pub const fn new(color: bool) -> Self {
        Self { color }
    }

    /// Colored when stderr is a terminal that shows ANSI colors, enabling virtual terminal
    /// processing on a Windows console first.
    pub fn auto() -> Self {
        Self {
            color: debug::enable_terminal_colors(),
        }
    }

    const fn color_of(level: Level) -> &'static str {
        match level {
            Level::Error => "\x1b[31m",
            Level::Warn => "\x1b[33m",
            Level::Info => "\x1b[32m",
            Level::Debug => "\x1b[36m",
            Level::Trace => "\x1b[90m",
        }
    }
}

impl Sink for ConsoleSink {
    fn log(&self, record: &Record<'_>) {
        self.write_line(record.level(), format_line(record).as_str());
    }
}

impl LineSink for ConsoleSink {
    /// Colors the first occurrence of the level's name, which in a line formatted from a
    /// [`Record`] is the level field.
    fn write_line(&self, level: Level, line: &str) {
        const RESET: &str = "\x1b[0m";
        let name = level.as_str();
        let Some(at) = line.find(name).filter(|_| self.color) else {
            debug::output(line);
            return;
        };
        let mut out = FixedBuf::<{ LINE_LEN + 16 }>::new();
        let (head, tail) = line.split_at(at);
        let tail = &tail[name.len()..];
        let _ = write!(out, "{head}{}{name}{RESET}{tail}", Self::color_of(level));
        debug::output(out.as_str());
    }
}
