//! High-definition rumble: two tones per actuator (Switch Pro on Windows).

use crate::contract::ext::Haptics as Contract;
use crate::{HapticTone, Input, MAX_GAMEPADS, Result};

/// Proof that this input system plays HD rumble. Obtain one with [`Haptics::get`].
#[derive(Clone, Copy, Debug)]
pub struct Haptics(());

impl Haptics {
    #[inline]
    pub fn get(input: &Input) -> Option<Self> {
        input.inner.haptics_available().then_some(Self(()))
    }

    /// Plays `left` and `right` on the pad's two actuators until replaced; overrides
    /// [`Input::set_rumble`] until [`stop`](Self::stop). Fails with `NotFound` for an empty slot
    /// and `Unsupported` for a pad without HD rumble. Panics if `gamepad >= MAX_GAMEPADS`.
    pub fn play(
        self,
        input: &mut Input,
        gamepad: usize,
        left: HapticTone,
        right: HapticTone,
    ) -> Result<()> {
        assert!(gamepad < MAX_GAMEPADS);
        input.inner.set_haptics(gamepad, Some([left, right]))
    }

    /// Hands the actuators back to plain rumble.
    pub fn stop(self, input: &mut Input, gamepad: usize) -> Result<()> {
        assert!(gamepad < MAX_GAMEPADS);
        input.inner.set_haptics(gamepad, None)
    }
}
