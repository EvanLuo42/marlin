//! The backend contract. Each backend exposes `App` and `Window` types implementing these traits;
//! `lib.rs` asserts it at compile time.

use core::time::Duration;

use alloc::string::String;
use alloc::vec::Vec;

use crate::{
    CursorGrab, CursorIcon, Display, DisplayId, EventQueue, Lifecycle, Point, Result, Size,
    Surface, WindowDesc, WindowId, WindowMode,
};

pub(crate) trait App: Sized {
    type Window;

    fn new() -> Result<Self>;
    fn create_window(&self, desc: &WindowDesc<'_>) -> Result<Self::Window>;
    /// Waits up to `timeout` for a message (`Some(ZERO)`: not at all; `None`: forever), dispatches
    /// every queued message, then appends the gathered events to `out`.
    fn pump(&mut self, timeout: Option<Duration>, out: &mut EventQueue);
    fn lifecycle(&self) -> Lifecycle;
    /// Appends every connected display to `out`, the primary one first.
    fn displays(&self, out: &mut Vec<Display>);
    fn clipboard_text(&self) -> Result<Option<String>>;
    fn set_clipboard_text(&self, text: &str) -> Result<()>;
}

pub(crate) trait Window {
    fn id(&self) -> WindowId;
    fn surface(&self) -> Surface;
    fn size(&self) -> Size;
    fn set_size(&self, size: Size);
    fn position(&self) -> Point;
    fn set_position(&self, position: Point);
    fn set_min_size(&self, size: Option<Size>);
    fn set_max_size(&self, size: Option<Size>);
    fn dpi(&self) -> u32;
    fn display(&self) -> Result<Display>;
    fn set_display(&self, display: DisplayId) -> Result<()>;
    fn mode(&self) -> WindowMode;
    fn set_mode(&self, mode: WindowMode) -> Result<()>;
    fn set_title(&self, title: &str);
    fn set_visible(&self, visible: bool);
    fn is_visible(&self) -> bool;
    fn is_minimized(&self) -> bool;
    fn is_focused(&self) -> bool;
    fn set_cursor_visible(&self, visible: bool);
    fn set_cursor_icon(&self, icon: CursorIcon);
    fn set_cursor_grab(&self, grab: CursorGrab);
    fn cursor_grab(&self) -> CursorGrab;
    fn set_cursor_position(&self, position: Point) -> Result<()>;
    fn take_dropped_files(&self) -> Vec<String>;
}
