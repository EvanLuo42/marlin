//! The host (docs/design/0019-engine-framework-and-world.md §12): startup, the platform and the
//! GPU, the engine's modules and the frame loop. It defines no components: those are the
//! modules'.
//!
//! - [`boot`]: `boot.toml` cvars → mounted content (packages, the cook service) and the entry
//!   level. The modules register before it reads the configuration, so their cvars exist.
//! - [`Engine`]: the simulation side: the modules ([`marlin_world::Modules`]), the asset server
//!   and the engine resources. Worlds are made by it and run a frame at a time through it.
//! - [`host`]: the host loop every application runs in ([`run_app`]): one window and device, the
//!   engine, the host's commands, and an application ([`HostApp`]) whose simulation side runs
//!   on a job (frame N+1) while its render side draws frame N.
//! - [`run`]: the game ([`GameApp`]): a game world with the entry level in the whole window.
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
mod game;
pub mod host;
#[cfg(not(marlin_build = "shipping"))]
pub mod hud;
pub mod input;
pub mod scalability;

pub use boot::{Boot, BootError, ContentSettings, boot, mount, register_cvars};
#[cfg(not(marlin_build = "shipping"))]
pub use dev::{DevToolsModule, R_STATS};
pub use engine::{Engine, EngineError};
pub use game::{GameApp, GameSim, run};
pub use host::{
    HostApp, HostExit, HostOptions, HostSetup, RenderContext, RunError, SimContext, WindowRequest,
    WindowState, WindowTarget, run_app,
};
