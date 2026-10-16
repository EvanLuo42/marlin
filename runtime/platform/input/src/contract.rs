//! The backend contract: each backend exposes an `Input` type implementing [`Input`].

use marlin_app::App;

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
}
