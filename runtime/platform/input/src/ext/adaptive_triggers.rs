//! Adaptive trigger force feedback: DualSense on Windows.

use crate::contract::ext::AdaptiveTriggers as Contract;
use crate::{Input, MAX_GAMEPADS, Result, Trigger, TriggerEffect};

/// Proof that this input system drives adaptive triggers. Obtain one with
/// [`AdaptiveTriggers::get`].
#[derive(Clone, Copy, Debug)]
pub struct AdaptiveTriggers(());

impl AdaptiveTriggers {
    #[inline]
    pub fn get(input: &Input) -> Option<Self> {
        input
            .inner
            .adaptive_triggers_available()
            .then_some(Self(()))
    }

    /// Sets the resistance of one trigger until replaced (or the pad reconnects). Fails with
    /// `NotFound` for an empty slot and `Unsupported` for a pad without adaptive triggers
    /// (check [`GamepadState::kind`](crate::GamepadState::kind)). Panics if
    /// `gamepad >= MAX_GAMEPADS`.
    pub fn set_effect(
        self,
        input: &mut Input,
        gamepad: usize,
        trigger: Trigger,
        effect: &TriggerEffect,
    ) -> Result<()> {
        assert!(gamepad < MAX_GAMEPADS);
        input.inner.set_trigger_effect(gamepad, trigger, effect)
    }
}
