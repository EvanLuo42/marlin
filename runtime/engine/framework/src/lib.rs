//! The host (docs/design/0019-engine-framework-and-world.md §12): startup, the platform and the
//! GPU, the engine's modules and the frame loop. It defines no components: those are the
//! modules'.
//!
//! - [`boot`]: `boot.toml` cvars → mounted content (packages, the cook service) and the entry
//!   level. The modules register before it reads the configuration, so their cvars exist.
//! - [`Engine`]: the simulation side: the modules ([`marlin_world::Modules`]), the asset server
//!   and the engine resources. Worlds are made by it and run a frame at a time through it.
//! - [`run`]: the game's window loop: a game world with the entry level, simulated (frame N+1)
//!   on a job while its `marlin_renderer::WorldRenderer` draws frame N.
//! - [`DevToolsModule`] (not in shipping builds): the stats overlay ([`hud`]) and the frame rate
//!   in the title, drawn by the host when the build has the module.
//! - [`input`]: routing: the host's own commands first, then a viewport's state decides what
//!   its world's players get and what the cursor does.
//! - [`scalability`]: the quality presets content holds, applied at boot.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod boot;
mod dev;
mod engine;
#[cfg(not(marlin_build = "shipping"))]
pub mod hud;
pub mod input;
mod run;
pub mod scalability;

pub use boot::{Boot, BootError, ContentSettings, boot, mount, register_cvars};
#[cfg(not(marlin_build = "shipping"))]
pub use dev::{DevToolsModule, R_STATS};
pub use engine::{Engine, EngineError};
pub use run::{RunError, run};
