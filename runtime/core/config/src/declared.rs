//! Registration without lists: a distributed slice in a linker section.
//!
//! [`declare!`](crate::declare) emits, next to each cvar or command, a [`Slot`]: a pointer-sized
//! static holding a function that returns the [`Entry`]. Every slot goes into the same linker
//! section, from whichever crate declares it, and the linker concatenates them; [`entries`]
//! finds where the section starts and ends and reads it as a `[Slot]`. A slot is
//! `Option<fn() -> Entry>`, so all-zero bytes are `None`: padding the linker inserts and the
//! COFF markers below are skipped. Slots are pointer-aligned and pointer-sized, so padding
//! (which comes in multiples of the alignment) never splits one.
//!
//! How each object format finds the bounds:
//!
//! - **COFF** (Windows, MSVC and lld-link): a section named `name$suffix` is merged into `name`,
//!   its pieces ordered by suffix. The slots go into `.mcfg$m`, between a marker static in
//!   `.mcfg$a` and one in `.mcfg$z`; the span runs from the first marker to the second. This
//!   is how the C runtime finds its initializers (`.CRT$XCA` .. `.CRT$XCZ`), and it is also why
//!   zeroed slots must be allowed: incremental linking pads the pieces.
//! - **ELF** (Linux, Android, the BSDs and the consoles built on them): for a section whose
//!   name is a C identifier, the linker defines `__start_<name>` and `__stop_<name>` (with
//!   protected visibility, so each shared object sees its own). The slots go into
//!   `marlin_config`. `#[used]` marks their sections `SHF_GNU_RETAIN`, so `--gc-sections`
//!   keeps them even under `-z start-stop-gc`; a sentinel slot, referenced from [`entries`],
//!   makes sure the section exists, so the symbols are always defined.
//! - **Mach-O** (macOS, iOS): ld64 resolves `section$start$<segment>$<section>` and
//!   `section$end$...` to the bounds of a section. The slots go into `__DATA,__marlin_config`,
//!   marked `no_dead_strip` so `-dead_strip` keeps them; the symbol names start with `\x01` so
//!   LLVM does not add the usual `_` prefix. The sentinel is there too.
//!
//! What a linker drops is never seen: a slot exists only if the object file declaring it is
//! linked. Objects of the binary's own crate always are; one in a library is linked when
//! anything in it is used, which is normal for code that reads its own cvars. Each executable
//! and shared library has its own section and registers its own declarations, through its own
//! copy of this crate.
//!
//! These are object-format `cfg`s (`windows`, `target_vendor = "apple"`), not a platform
//! backend: the same three cases cover every target.

use core::hint::black_box;
use core::{mem, slice};

use crate::registry::Entry;

/// One element of the section: returns the declared entry. `None` is padding or a marker.
#[doc(hidden)]
pub type Slot = Option<fn() -> Entry>;

/// What [`declare!`](crate::declare) can declare: the typed cvars and [`Command`].
///
/// [`Command`]: crate::Command
#[doc(hidden)]
pub trait Declare {
    fn entry(&'static self) -> Entry;
}

/// Every declared entry in this binary, in link order.
pub(crate) fn entries() -> impl Iterator<Item = Entry> {
    slots().iter().filter_map(|slot| slot.map(|entry| entry()))
}

#[cfg(windows)]
fn slots() -> &'static [Slot] {
    #[used]
    // SAFETY: a pointer-sized `None` in its own piece of the slot section: a slot like the others.
    #[unsafe(link_section = ".mcfg$a")]
    static START: Slot = None;
    #[used]
    // SAFETY: as `START`.
    #[unsafe(link_section = ".mcfg$z")]
    static STOP: Slot = None;
    span(&raw const START, &raw const STOP)
}

#[cfg(target_vendor = "apple")]
fn slots() -> &'static [Slot] {
    unsafe extern "C" {
        #[link_name = "\x01section$start$__DATA$__marlin_config"]
        static START: u8;
        #[link_name = "\x01section$end$__DATA$__marlin_config"]
        static STOP: u8;
    }
    #[used]
    // SAFETY: a slot in the slot section, like the ones `declare!` emits.
    #[unsafe(link_section = "__DATA,__marlin_config,regular,no_dead_strip")]
    static SENTINEL: Slot = Some(sentinel);
    black_box(&raw const SENTINEL);
    span(&raw const START, &raw const STOP)
}

#[cfg(not(any(windows, target_vendor = "apple")))]
fn slots() -> &'static [Slot] {
    unsafe extern "C" {
        #[link_name = "__start_marlin_config"]
        static START: u8;
        #[link_name = "__stop_marlin_config"]
        static STOP: u8;
    }
    #[used]
    // SAFETY: a slot in the slot section, like the ones `declare!` emits.
    #[unsafe(link_section = "marlin_config")]
    static SENTINEL: Slot = Some(sentinel);
    black_box(&raw const SENTINEL);
    span(&raw const START, &raw const STOP)
}

/// What the sentinel slot holds: a built-in command (declared as well, so listed twice, which
/// registration ignores). Not `None`, so that the sentinel needs a relocation like every other
/// slot: LLVM derives a section's flags from its globals, and one object holding a read-only
/// and a writable (relocated, under PIC) `marlin_config` would be a conflict.
#[cfg(not(windows))]
fn sentinel() -> Entry {
    Entry::Command(&crate::console::HELP)
}

/// The slots from `start` up to `stop`, two addresses in the slot section. The linker symbols
/// are declared as bytes: only their addresses mean anything.
fn span<T>(start: *const T, stop: *const T) -> &'static [Slot] {
    // The bounds go through `black_box`: as far as the optimizer knows they are unrelated
    // statics, and it must not reason about the bytes between them.
    let (start, stop) = (
        black_box(start.cast::<Slot>()),
        black_box(stop.cast::<Slot>()),
    );
    let len = stop.addr().saturating_sub(start.addr()) / mem::size_of::<Slot>();
    debug_assert!(start.is_aligned(), "misaligned slot section");
    // SAFETY: the linker placed `len` slots back to back from `start` (see the module docs):
    // each is a static `declare!` emitted, the sentinel (all valid `Option<fn() -> Entry>`), a
    // marker (`None`) or zeroed padding (`None` too). `start` is aligned for `Slot`, all pieces
    // having its alignment, and the section is never written.
    unsafe { slice::from_raw_parts(start, len) }
}

/// Declares cvars and console commands that register themselves: each static is defined as
/// written, and also listed in a linker section that the registry reads before its first use
/// (see [`register_declared`](crate::register_declared)). Works in any crate, on statics at
/// module level or inside functions.
///
/// ```ignore
/// marlin_config::declare! {
///     /// Wait for vertical blank.
///     pub static R_VSYNC: CvarBool = CvarBool::new("r.vsync", true, "Wait for vertical blank.");
///     static QUIT: Command = Command::new("quit", "Quits.", |_, _| Ok(()));
/// }
/// ```
#[macro_export]
macro_rules! declare {
    ($($(#[$attr:meta])* $vis:vis static $name:ident: $ty:ty = $init:expr;)+) => {$(
        $(#[$attr])*
        $vis static $name: $ty = $init;

        const _: () = {
            #[used]
            // SAFETY: a `Slot` (pointer-sized, `None` or a function) in the slot section; see
            // marlin-config's `declared` module.
            #[cfg_attr(windows, unsafe(link_section = ".mcfg$m"))]
            #[cfg_attr(
                target_vendor = "apple",
                unsafe(link_section = "__DATA,__marlin_config,regular,no_dead_strip")
            )]
            #[cfg_attr(
                not(any(windows, target_vendor = "apple")),
                unsafe(link_section = "marlin_config")
            )]
            static SLOT: $crate::Slot = {
                fn entry() -> $crate::Entry {
                    $crate::Declare::entry(&$name)
                }
                Some(entry)
            };
        };
    )+};
}
