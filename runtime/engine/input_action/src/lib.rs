//! Input actions (docs/design/0019-engine-framework-and-world.md §9): gameplay reads actions,
//! never keys.
//!
//! - Assets ([`asset`]): [`InputAction`]s and [`MappingContext`]s binding [`InputSource`]s to
//!   them, shaped by [`Modifier`]s and fired by [`Trigger`]s. Cooked as authored.
//! - [`InputFrame`]: one frame of raw input for one user, built from the platform's
//!   `InputState` for the devices it reads; routing layers consume what they use from it.
//! - [`InputUser`]: a stack of [`CompiledContext`]s by priority, evaluated each frame into
//!   [`ActionState`]s. The host has one for its own commands; each world player has one.
//! - [`InputModule`]: a world's [`Players`] (the host routes input to them) evaluated in
//!   `FrameStart`, the contexts they ask for loaded, and the game's [`InputSettings`] (input
//!   mode, cursor) for the host to follow.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod asset;
mod eval;
mod module;
mod source;

pub use asset::{
    ActionValueType, InputAction, Mapping, MappingContext, Modifier, SwizzleOrder, Trigger,
};
pub use eval::{ActionState, CompiledContext, InputUser};
pub use module::{
    CursorMode, InputMode, InputModule, InputSettings, Player, Players, evaluate_players,
    load_context,
};
pub use source::{Devices, InputFrame, InputSource, KeyCode, Mouse, Pad};
