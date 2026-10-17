//! Gamepad motion sensors: DualSense, DualShock 4 and Switch Pro on Windows.

use crate::contract::ext::Gyro as Contract;
use crate::{GamepadState, Input, MAX_GAMEPADS, Motion, Result};

/// Proof that this input system reads gamepad motion sensors. Obtain one with [`Gyro::get`].
#[derive(Clone, Copy, Debug)]
pub struct Gyro(());

impl Gyro {
    #[inline]
    pub fn get(input: &Input) -> Option<Self> {
        input.inner.motion_available().then_some(Self(()))
    }

    /// The pad's latest motion reading; `None` if it has no sensors or they are off.
    #[inline]
    pub fn motion(self, gamepad: &GamepadState) -> Option<Motion> {
        gamepad.motion()
    }

    /// Switches a pad's sensors on (the default) or off; off saves power and bandwidth on some
    /// controllers. Fails with `NotFound` for an empty slot and `Unsupported` for a pad without
    /// sensors. Panics if `gamepad >= MAX_GAMEPADS`.
    pub fn set_enabled(self, input: &mut Input, gamepad: usize, enabled: bool) -> Result<()> {
        assert!(gamepad < MAX_GAMEPADS);
        input.inner.set_motion_enabled(gamepad, enabled)
    }
}
