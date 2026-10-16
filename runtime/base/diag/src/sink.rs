//! Log records, the sink registry and the default debug-output sink.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use marlin_pal::debug;

use crate::{FixedBuf, Level};

/// One log message, handed to every sink by reference. Borrowed and formatted lazily: sinks
/// that drop a record never pay for formatting it.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    level: Level,
    target: &'a str,
    file: &'a str,
    line: u32,
    thread_id: u64,
    args: fmt::Arguments<'a>,
}

impl<'a> Record<'a> {
    pub fn new(
        level: Level,
        target: &'a str,
        file: &'a str,
        line: u32,
        thread_id: u64,
        args: fmt::Arguments<'a>,
    ) -> Self {
        Self {
            level,
            target,
            file,
            line,
            thread_id,
            args,
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

    /// The message. Format it with `write!`; `to_string` would allocate.
    pub fn args(&self) -> fmt::Arguments<'a> {
        self.args
    }
}

/// A log destination. Called concurrently from any thread, so it must synchronize its own state.
///
/// Must not log itself: that would recurse into every sink.
pub trait Sink: Sync {
    fn log(&self, record: &Record<'_>);

    /// Pushes buffered output to its destination. Called on [`flush`] and before an assertion
    /// failure breaks into the debugger.
    fn flush(&self) {}
}

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

struct Slot(UnsafeCell<MaybeUninit<&'static dyn Sink>>);

// SAFETY: a slot is written once, under `REGISTER_LOCK`, before `SINK_COUNT` is raised past it
// with Release; readers only touch slots below a count they loaded with Acquire.
unsafe impl Sync for Slot {}

static SINKS: [Slot; MAX_SINKS] =
    [const { Slot(UnsafeCell::new(MaybeUninit::uninit())) }; MAX_SINKS];
static SINK_COUNT: AtomicUsize = AtomicUsize::new(0);
static REGISTER_LOCK: AtomicBool = AtomicBool::new(false);

/// Adds `sink` to the registry. Intended for startup; sinks cannot be removed.
///
/// While no sink is registered, records go to [`DebugOutput`]. Registering any sink replaces that
/// fallback, so register `&DebugOutput` too to keep it.
pub fn register_sink(sink: &'static dyn Sink) -> Result<(), RegistryFull> {
    // Registration is rare and short, so a spin lock is enough.
    while REGISTER_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let count = SINK_COUNT.load(Ordering::Relaxed);
    let result = if count < MAX_SINKS {
        // SAFETY: we hold the lock, and slot `count` is not yet visible to readers.
        unsafe { (*SINKS[count].0.get()).write(sink) };
        SINK_COUNT.store(count + 1, Ordering::Release);
        Ok(())
    } else {
        Err(RegistryFull)
    };
    REGISTER_LOCK.store(false, Ordering::Release);
    result
}

fn sinks() -> impl Iterator<Item = &'static dyn Sink> {
    let count = SINK_COUNT.load(Ordering::Acquire);
    SINKS[..count].iter().map(|slot| {
        // SAFETY: slots below an Acquire-loaded count are initialized and never written again.
        unsafe { (*slot.0.get()).assume_init() }
    })
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
        DebugOutput.flush();
        return;
    }
    for sink in sinks() {
        sink.flush();
    }
}

/// The default sink: one `marlin_pal::debug::output` call per record (stderr, plus the debugger
/// when attached), as `LEVEL [thread] target: message`.
///
/// Lines longer than [`DebugOutput::LINE`] bytes are truncated and end in `...`.
#[derive(Clone, Copy, Default, Debug)]
pub struct DebugOutput;

impl DebugOutput {
    pub const LINE: usize = 1024;
}

impl Sink for DebugOutput {
    fn log(&self, record: &Record<'_>) {
        let mut line = FixedBuf::<{ Self::LINE }>::new();
        let _ = write!(
            line,
            "{:<5} [{}] {}: {}",
            record.level(),
            record.thread_id(),
            record.target(),
            record.args()
        );
        line.end_line();
        debug::output(line.as_str());
    }
}
