//! Input action assets (0019 §9.1): [`InputAction`]s and the [`MappingContext`]s that bind
//! sources to them, with [`Modifier`]s shaping the value and [`Trigger`]s deciding when the
//! action fires.
//!
//! ```text
//! InputAction { !Uuid = "…" !Name = "Marlin.Engine.Input.Move" Value = Axis2D }
//!
//! MappingContext
//! {
//!     !Uuid = "…"
//!     !Name = "Marlin.Engine.Input.Spectator"
//!     Mappings = [
//!         { Action = <:Marlin.Engine.Input.Move> Source = Key("W") Modifiers = [Swizzle(Yxz)] },
//!         { Action = <:Marlin.Engine.Input.Move> Source = LeftStick },
//!         { Action = <:Marlin.Engine.Input.Look> Source = MouseMotion
//!           Modifiers = [Scale { Factor = (0.003, -0.003, 0.0) }]
//!           Triggers = [Chord(<:Marlin.Engine.Input.LookEnable>)] },
//!     ]
//! }
//! ```

use alloc::vec::Vec;

use marlin_asset::Link;
use marlin_math::Vec3;
use marlin_reflect::Reflect;

use crate::source::InputSource;

/// What an action's value is: a button (x is 0 or 1) or an axis of one to three components.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::ActionValueType", default)]
pub enum ActionValueType {
    #[default]
    Bool,
    Axis1D,
    Axis2D,
    Axis3D,
}

/// Something the player can do: jump, move, look. Gameplay reads actions, never keys.
#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::InputAction", default)]
pub struct InputAction {
    pub value: ActionValueType,
}

/// Component orders for [`Modifier::Swizzle`]: `Yxz` puts the source's x into y (a key on the
/// forward axis).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::SwizzleOrder", default)]
pub enum SwizzleOrder {
    #[default]
    Yxz,
    Zyx,
    Xzy,
    Yzx,
    Zxy,
}

/// Shapes a mapping's value, in order, before its triggers see it.
#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::Modifier")]
pub enum Modifier {
    /// Every component negated.
    #[default]
    Negate,
    /// Components reordered.
    Swizzle(SwizzleOrder),
    /// Components multiplied.
    Scale { factor: Vec3 },
    /// Multiplied by the frame's time step: a stick turning at a rate, not by a distance.
    ScaleByDeltaTime,
    /// The length below `lower` reads 0, above `upper` 1, rescaled in between (radial).
    DeadZone { lower: f32, upper: f32 },
}

/// When a mapping fires. A mapping without triggers fires while its source is actuated
/// (non-zero).
#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::Trigger")]
pub enum Trigger {
    /// While actuated.
    #[default]
    Down,
    /// The frame the source becomes actuated.
    Pressed,
    /// The frame it stops.
    Released,
    /// While actuated, once held `seconds`.
    Hold { seconds: f32 },
    /// When released within `seconds` of being actuated.
    Tap { seconds: f32 },
    /// Only while another action fires (a modifier key: Alt + Enter).
    Chord(Link<InputAction>),
}

/// A source bound to an action.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::Mapping", default)]
pub struct Mapping {
    pub action: Link<InputAction>,
    pub source: InputSource,
    pub modifiers: Vec<Modifier>,
    pub triggers: Vec<Trigger>,
}

/// A set of mappings switched on and off together (on foot, driving, a menu), stacked by
/// priority per player: what a higher context fires with, lower ones do not see.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
#[reflect(name = "marlin_input_action::MappingContext", default)]
pub struct MappingContext {
    pub mappings: Vec<Mapping>,
}
