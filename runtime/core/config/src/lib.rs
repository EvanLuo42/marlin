//! Console variables (cvars), console commands, layered TOML config and command-line overrides.
//!
//! No IO: config text comes in as `&str` and archives go out through `fmt::Write`; reading and
//! writing the files is up to the caller, and so is finding the scripts `exec` runs
//! ([`set_exec_loader`]). The `toml` crate (no_std) only does TOML syntax; this crate owns the
//! cvars, their rules and change tracking, the registry, and the mapping between TOML
//! documents (tables and dotted keys spell cvar names) and cvars.
//!
//! A cvar is a typed static ([`CvarBool`], [`CvarI64`], [`CvarF64`], [`CvarString`],
//! [`CvarEnum`], [`CvarVec2`], [`CvarVec3`], [`CvarVec4`], [`CvarColor`]) with a name, default,
//! help text, [`Flags`] and, for numbers, a range. Reading a scalar is one relaxed atomic load,
//! cheap enough for every frame; wider vectors take a seqlock read. Each cvar derefs to
//! [`Cvar`], the type-erased view the registry, console and config files work with. A
//! [`Command`] is a named function the console runs.
//!
//! ```ignore
//! marlin_config::declare! {
//!     pub static R_VSYNC: CvarBool = CvarBool::new("r.vsync", true, "Wait for vertical blank.").archive();
//!     pub static R_MAX_FPS: CvarI64 = CvarI64::new("r.maxFps", 0, "Frame cap; 0 = none.").range(0, 1000);
//! }
//!
//! for e in marlin_config::load_layer(Source::Engine, &engine_toml) { log(e) }
//! for e in marlin_config::load_platform(&platforms_toml, "windows") { log(e) }
//! for e in marlin_config::load_scalability(&scalability_toml) { log(e) }
//! for e in marlin_config::load_layer(Source::Project, &project_toml) { log(e) }
//! for e in marlin_config::load_device_profile(&profiles_toml, "desktop-high") { log(e) }
//! for e in marlin_config::load_layer(Source::User, &user_toml) { log(e) }
//! for e in marlin_config::apply_command_line(args) { log(e) }
//! marlin_config::finish_startup();
//! if R_VSYNC.get() { ... }
//! marlin_config::execute("r.maxFps 144; toggle r.vsync", &mut console_output);
//! user_toml = marlin_config::archive_string();
//! ```
//!
//! # Registration
//!
//! Cvars and commands declared with [`declare!`] register themselves, from any crate, before
//! the registry is first used (a linker section lists them; see [`register_declared`]).
//! [`register`], [`register_all`] and [`register_command`] are the explicit path, for statics
//! declared without the macro.
//!
//! # Layers
//!
//! Each cvar keeps one value per config layer ([`Source`]), and the highest layer holding one
//! decides: engine < platform < scalability < project < device profile < user < command line <
//! console (and code) < server. [`Cvar::set_by`] says which layer that is. [`load_layer`]
//! replaces a layer with a TOML document (so loading again reloads it), [`clear_layer`] removes
//! one, and the cvars fall back to the layers below. [`write_layer`] writes any layer;
//! [`write_archive`] the user's settings. Scalability groups ([`load_scalability`]) and device
//! profiles ([`load_device_profile`]) fill their own layers.
//!
//! # Who may set what
//!
//! Code (the typed `set` methods, [`Source::Code`]) and the server ([`Source::Server`]) may set
//! anything; code clamps to the range instead of failing. String sets ([`Cvar::set_str`], the
//! console and the command line) and config files check the type, the range and the flags:
//!
//! - [`Flags::READ_ONLY`]: only from code and the command line.
//! - [`Flags::CHEAT`]: from outside code only while [`cheats_enabled`]. Commands too.
//! - [`Flags::DEV_ONLY`]: from outside code only when [`DEV_ENABLED`] (not in shipping builds).
//!   Commands too; such entries are also left out of lists and completion there.
//! - [`Flags::REPLICATED`]: while [`set_replicated_lock`] is on (connected to a server), only
//!   from code and the server. [`on_replicated_change`] is the hook for a net layer.
//! - [`Flags::REQUIRES_RESTART`]: before [`finish_startup`] a set takes effect at once; after it,
//!   the value is kept as pending ([`Cvar::pending_string`]) and archived, and `get` keeps
//!   returning the value the process started with.
//! - [`Flags::ARCHIVE`]: a value code or the console sets is saved by [`write_archive`].
//!
//! # Reacting to changes
//!
//! Every effective change bumps the cvar's [`Cvar::generation`] and the global [`generation`], so
//! a system can compare one integer per frame. [`on_change`] / [`on_any_change`] callbacks run
//! synchronously on the setting thread, after the value is stored.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod console;
mod cvar;
mod declared;
mod error;
mod file;
mod registry;
mod scalability;

pub use console::{
    Command, CommandError, Handler, Statement, SyntaxError, execute, parse, set_exec_loader,
    write_word,
};
pub use cvar::{
    Cvar, CvarBool, CvarColor, CvarEnum, CvarF64, CvarI64, CvarString, CvarVec2, CvarVec3,
    CvarVec4, DEV_ENABLED, Flags, Kind, Source, cheats_enabled, finish_startup, generation,
    replicated_locked, set_cheats_enabled, set_replicated_lock, startup_finished,
};
#[doc(hidden)]
pub use declared::{Declare, Slot};
pub use error::{ApplyError, Bounds, Cause, Error, Expected, Location};
pub use file::{
    apply_command_line, apply_config, archive_string, clear_layer, load_device_profile, load_layer,
    load_platform, load_section, write_archive, write_layer,
};
pub use registry::{
    CallbackId, Entry, commands, complete, cvars, find, find_command, lookup, on_any_change,
    on_change, on_replicated_change, register, register_all, register_command, register_declared,
    remove_callback, set, unregister, unregister_command,
};
pub use scalability::{
    add_scalability_level, clear_scalability, load_scalability, scalability_groups,
    scalability_overrides,
};
