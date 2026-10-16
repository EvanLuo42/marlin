//! Console variables (cvars), TOML config files and command-line overrides.
//!
//! No IO: config text comes in as `&str` and archives go out through `fmt::Write`; reading and
//! writing the files is up to the caller. The `toml` crate (no_std) only does TOML syntax; this
//! crate owns the cvars, their rules and change tracking, the registry, and the mapping between
//! TOML documents (tables and dotted keys spell cvar names) and cvars.
//!
//! A cvar is a typed static ([`CvarBool`], [`CvarI64`], [`CvarF64`], [`CvarString`],
//! [`CvarEnum`]) with a name, default, help text, [`Flags`] and, for numbers, a range. Reading a
//! scalar is one relaxed atomic load, cheap enough for every frame. Each cvar derefs to [`Cvar`],
//! the type-erased view the registry, console and config files work with.
//!
//! ```ignore
//! pub static R_VSYNC: CvarBool = CvarBool::new("r.vsync", true, "Wait for vertical blank.").archive();
//! pub static R_MAX_FPS: CvarI64 = CvarI64::new("r.maxFps", 0, "Frame cap; 0 = none.").range(0, 1000);
//!
//! marlin_config::register_all(&[&R_VSYNC, &R_MAX_FPS])?;
//! for e in marlin_config::apply_config(&text, Source::ConfigFile) { log(e) }
//! for e in marlin_config::apply_command_line(args) { log(e) }
//! marlin_config::finish_startup();
//! if R_VSYNC.get() { ... }
//! ```
//!
//! # Who may set what
//!
//! Code (the typed `set` methods, [`Source::Code`]) may set anything; it clamps to the range
//! instead of failing. String sets ([`Cvar::set_str`], the console and the command line) and
//! config files check the type, the range and the flags:
//!
//! - [`Flags::READ_ONLY`]: only from code and the command line.
//! - [`Flags::CHEAT`]: from outside code only while [`cheats_enabled`].
//! - [`Flags::REQUIRES_RESTART`]: before [`finish_startup`] a set takes effect at once; after it,
//!   the value is kept as pending ([`Cvar::pending_string`]) and archived, and `get` keeps
//!   returning the value the process started with.
//! - [`Flags::ARCHIVE`]: written by [`write_archive`] when it differs from the default.
//!
//! # Reacting to changes
//!
//! Every effective change bumps the cvar's [`Cvar::generation`] and the global [`generation`], so
//! a system can compare one integer per frame. [`on_change`] / [`on_any_change`] callbacks run
//! synchronously on the setting thread, after the value is stored.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod cvar;
mod error;
mod file;
mod registry;

pub use cvar::{
    Cvar, CvarBool, CvarEnum, CvarF64, CvarI64, CvarString, Flags, Kind, Source, cheats_enabled,
    finish_startup, generation, set_cheats_enabled, startup_finished,
};
pub use error::{ApplyError, Bounds, Cause, Error, Expected, Location};
pub use file::{apply_command_line, apply_config, archive_string, write_archive};
pub use registry::{
    CallbackId, complete, cvars, find, on_any_change, on_change, register, register_all,
    remove_callback, set, unregister,
};
