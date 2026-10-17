//! The backend contract: each backend exposes an `Input` type implementing [`Input`], plus the
//! [`ext`] traits of the extensions its platform manifest declares.

use marlin_app::{App, Rect, Window};

use crate::{InputState, Key, Result};

pub(crate) trait Input: Sized {
    /// Starts observing `app`'s windows. The backend keeps working if `app` is dropped first; it
    /// just stops receiving keyboard and mouse input.
    fn new(app: &App) -> Result<Self>;
    /// Polls devices, then moves everything gathered since the last call into `out` (replacing
    /// its contents) and starts a new frame.
    fn update(&mut self, out: &mut InputState);
    fn set_rumble(&mut self, gamepad: usize, low: f32, high: f32) -> Result<()>;
    fn key_label<'b>(&self, key: Key, buf: &'b mut [u8]) -> Option<&'b str>;
    /// Enables the IME on `window` with its candidate list placed around `area` (client pixels),
    /// or disables it with `None`.
    fn set_text_input(&mut self, window: &Window, area: Option<Rect>) -> Result<()>;
}

pub(crate) mod ext {
    #[cfg(marlin_ext = "gyro")]
    pub(crate) trait Gyro {
        fn motion_available(&self) -> bool;
        fn set_motion_enabled(&mut self, gamepad: usize, enabled: bool) -> crate::Result<()>;
    }

    #[cfg(marlin_ext = "adaptive_triggers")]
    pub(crate) trait AdaptiveTriggers {
        fn adaptive_triggers_available(&self) -> bool;
        fn set_trigger_effect(
            &mut self,
            gamepad: usize,
            trigger: crate::Trigger,
            effect: &crate::TriggerEffect,
        ) -> crate::Result<()>;
    }

    #[cfg(marlin_ext = "haptics")]
    pub(crate) trait Haptics {
        fn haptics_available(&self) -> bool;
        fn set_haptics(
            &mut self,
            gamepad: usize,
            tones: Option<[crate::HapticTone; 2]>,
        ) -> crate::Result<()>;
    }
}
