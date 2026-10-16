//! The backend contract. Each backend exposes `App` and `Window` types implementing these traits;
//! `lib.rs` asserts it at compile time.

use core::time::Duration;

use crate::{EventQueue, Result, Size, Surface, WindowDesc, WindowId, WindowMode};

pub(crate) trait App: Sized {
    type Window;

    fn new() -> Result<Self>;
    fn create_window(&self, desc: &WindowDesc<'_>) -> Result<Self::Window>;
    /// Waits up to `timeout` for a message (`Some(ZERO)`: not at all; `None`: forever), dispatches
    /// every queued message, then appends the gathered events to `out`.
    fn pump(&mut self, timeout: Option<Duration>, out: &mut EventQueue);
}

pub(crate) trait Window {
    fn id(&self) -> WindowId;
    fn surface(&self) -> Surface;
    fn size(&self) -> Size;
    fn dpi(&self) -> u32;
    fn mode(&self) -> WindowMode;
    fn set_mode(&self, mode: WindowMode) -> Result<()>;
    fn set_title(&self, title: &str);
    fn set_visible(&self, visible: bool);
    fn is_minimized(&self) -> bool;
    fn is_focused(&self) -> bool;
}
