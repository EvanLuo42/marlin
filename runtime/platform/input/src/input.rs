use marlin_app::{App, Rect, Window};

use crate::backend::imp;
use crate::contract::Input as _;
use crate::{InputState, Key, MAX_GAMEPADS, Result};

/// Buffer size that fits any [`Input::key_label`].
pub const KEY_LABEL_CAPACITY: usize = 64;

/// The input system of one [`App`]. Main thread only.
pub struct Input {
    pub(crate) inner: imp::Input,
    state: InputState,
}

impl Input {
    /// Starts gathering keyboard, mouse, touch and pen input from every window of `app` and
    /// polling gamepads.
    pub fn new(app: &App) -> Result<Self> {
        Ok(Self {
            inner: imp::Input::new(app)?,
            state: InputState::new(),
        })
    }

    /// Latches everything since the previous call into [`state`](Self::state). Call once per
    /// frame, after [`App::pump`](marlin_app::App::pump).
    pub fn update(&mut self) {
        self.inner.update(&mut self.state);
    }

    /// The snapshot taken by the last [`update`](Self::update).
    #[inline]
    pub fn state(&self) -> &InputState {
        &self.state
    }

    /// Sets the rumble motor speeds, in [0, 1]: `low` is the heavy left motor, `high` the light
    /// right one. Fails with `NotFound` if the gamepad is not connected. Panics if
    /// `gamepad >= MAX_GAMEPADS`.
    pub fn set_rumble(&mut self, gamepad: usize, low: f32, high: f32) -> Result<()> {
        assert!(gamepad < MAX_GAMEPADS);
        self.inner
            .set_rumble(gamepad, low.clamp(0.0, 1.0), high.clamp(0.0, 1.0))
    }

    /// The key's label on the user's current keyboard layout (for prompts like "Press Z"), in
    /// `buf`. `None` if the OS has no name for it; fall back to [`Key::name`].
    pub fn key_label<'b>(
        &self,
        key: Key,
        buf: &'b mut [u8; KEY_LABEL_CAPACITY],
    ) -> Option<&'b str> {
        self.inner.key_label(key, buf)
    }

    /// Turns IME text input on for `window`, with the IME's candidate list placed next to `area`
    /// (the focused text field, in client pixels), or off with `None`.
    ///
    /// Turn it off during gameplay: otherwise a Chinese, Japanese or Korean IME composes from
    /// movement keys. Typed text arrives in [`InputState::text`] either way; while composing, the
    /// unfinished text is in [`InputState::composition`].
    pub fn set_text_input(&mut self, window: &Window, area: Option<Rect>) -> Result<()> {
        self.inner.set_text_input(window, area)
    }
}
