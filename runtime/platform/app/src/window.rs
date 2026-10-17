use core::ffi::c_void;
use core::fmt;
use core::ptr::NonNull;

use alloc::string::String;
use alloc::vec::Vec;

use crate::backend::imp;
use crate::contract::Window as _;
use crate::{Display, DisplayId, Point, Result, WindowId};

/// A size in physical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    #[inline]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum WindowMode {
    #[default]
    Windowed,
    /// A borderless window covering its whole display. With a flip-model swapchain this gets the
    /// same latency as exclusive fullscreen without mode switches.
    BorderlessFullscreen,
}

/// Window creation parameters.
#[derive(Clone, Copy, Debug)]
pub struct WindowDesc<'a> {
    pub title: &'a str,
    /// Client area size in logical pixels (96 DPI), scaled by the display's DPI.
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    /// Title bar and border. Undecorated windows are for splash screens and custom title bars.
    pub decorated: bool,
    pub mode: WindowMode,
    /// The display to open on (centered in its work area, or covering it in fullscreen). `None`:
    /// the OS chooses for windowed windows, the primary display for fullscreen ones.
    pub display: Option<DisplayId>,
    /// Makes this an owned window (tool palette, dialog): it stays above `owner`, is minimized
    /// and hidden with it, and has no taskbar button.
    pub owner: Option<&'a Window>,
    /// Accept files dragged from the OS shell; see [`Event::FilesDropped`](crate::Event).
    pub accept_files: bool,
    /// Show the window immediately. A renderer may prefer to show it after its first frame.
    pub visible: bool,
}

impl Default for WindowDesc<'_> {
    fn default() -> Self {
        Self {
            title: "Marlin",
            width: 1280,
            height: 720,
            resizable: true,
            decorated: true,
            mode: WindowMode::Windowed,
            display: None,
            owner: None,
            accept_files: false,
            visible: true,
        }
    }
}

/// The native handles a GPU API needs to present into a window.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Win32 {
        hwnd: NonNull<c_void>,
        hinstance: NonNull<c_void>,
    },
}

/// The pointer shape over a window's client area.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum CursorIcon {
    #[default]
    Arrow,
    /// I-beam, over editable text.
    Text,
    /// Pointing hand, over links and buttons.
    Hand,
    Crosshair,
    /// Busy, but still accepting input.
    Progress,
    /// Busy; input is ignored.
    Wait,
    NotAllowed,
    /// Four-way arrow, for moving things.
    Move,
    ResizeHorizontal,
    ResizeVertical,
    /// Top-left to bottom-right.
    ResizeDiagonalDown,
    /// Bottom-left to top-right.
    ResizeDiagonalUp,
}

/// Restricts the mouse cursor while the window has focus. Released while the window is
/// unfocused or minimized, and restored when it comes back.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum CursorGrab {
    #[default]
    None,
    /// The cursor stays inside the client area (strategy games, windowed fullscreen on several
    /// displays).
    Confine,
    /// The cursor is hidden and held in place; read motion from raw mouse deltas
    /// (`marlin_input`'s `MouseState::delta`). For first-person cameras.
    Lock,
}

/// An OS window. Main thread only; closes when dropped.
pub struct Window {
    pub(crate) inner: imp::Window,
}

impl Window {
    #[inline]
    pub fn id(&self) -> WindowId {
        self.inner.id()
    }

    /// Native handles for creating a swapchain. Valid until the window is dropped.
    #[inline]
    pub fn surface(&self) -> Surface {
        self.inner.surface()
    }

    /// Client area size in physical pixels. Keeps the last non-zero size while minimized.
    #[inline]
    pub fn size(&self) -> Size {
        self.inner.size()
    }

    /// Resizes the client area, in physical pixels, within the minimum and maximum sizes.
    /// Ignored in fullscreen.
    #[inline]
    pub fn set_size(&self, size: Size) {
        self.inner.set_size(size);
    }

    /// The client area's top-left corner on the virtual desktop.
    #[inline]
    pub fn position(&self) -> Point {
        self.inner.position()
    }

    /// Moves the window so the client area's top-left corner is at `position`. Ignored in
    /// fullscreen.
    #[inline]
    pub fn set_position(&self, position: Point) {
        self.inner.set_position(position);
    }

    /// Limits user and [`set_size`](Self::set_size) resizing, in client-area physical pixels.
    /// `None` removes the limit.
    #[inline]
    pub fn set_min_size(&self, size: Option<Size>) {
        self.inner.set_min_size(size);
    }

    #[inline]
    pub fn set_max_size(&self, size: Option<Size>) {
        self.inner.set_max_size(size);
    }

    /// Dots per inch of the window's display; 96 is 100% scale.
    #[inline]
    pub fn dpi(&self) -> u32 {
        self.inner.dpi()
    }

    /// `dpi / 96`: logical-to-physical pixel ratio.
    #[inline]
    pub fn scale_factor(&self) -> f32 {
        self.inner.dpi() as f32 / 96.0
    }

    /// The display holding most of the window.
    #[inline]
    pub fn display(&self) -> Result<Display> {
        self.inner.display()
    }

    /// Moves the window to another display: centered in its work area when windowed, covering
    /// it in fullscreen. Fails with `NotFound` for a stale id.
    #[inline]
    pub fn set_display(&self, display: DisplayId) -> Result<()> {
        self.inner.set_display(display)
    }

    #[inline]
    pub fn mode(&self) -> WindowMode {
        self.inner.mode()
    }

    /// Switches between windowed and borderless fullscreen on the window's current display.
    /// Leaving fullscreen restores the previous windowed position and size.
    #[inline]
    pub fn set_mode(&self, mode: WindowMode) -> Result<()> {
        self.inner.set_mode(mode)
    }

    /// Flips between [`WindowMode::Windowed`] and [`WindowMode::BorderlessFullscreen`].
    pub fn toggle_fullscreen(&self) -> Result<()> {
        self.set_mode(match self.mode() {
            WindowMode::Windowed => WindowMode::BorderlessFullscreen,
            WindowMode::BorderlessFullscreen => WindowMode::Windowed,
        })
    }

    /// Sets the title bar text. Long titles are truncated.
    #[inline]
    pub fn set_title(&self, title: &str) {
        self.inner.set_title(title);
    }

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        self.inner.set_visible(visible);
    }

    #[inline]
    pub fn is_visible(&self) -> bool {
        self.inner.is_visible()
    }

    #[inline]
    pub fn is_minimized(&self) -> bool {
        self.inner.is_minimized()
    }

    #[inline]
    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// Shows or hides the cursor over the client area.
    #[inline]
    pub fn set_cursor_visible(&self, visible: bool) {
        self.inner.set_cursor_visible(visible);
    }

    #[inline]
    pub fn set_cursor_icon(&self, icon: CursorIcon) {
        self.inner.set_cursor_icon(icon);
    }

    /// Confines or locks the cursor; takes effect whenever the window has focus.
    #[inline]
    pub fn set_cursor_grab(&self, grab: CursorGrab) {
        self.inner.set_cursor_grab(grab);
    }

    #[inline]
    pub fn cursor_grab(&self) -> CursorGrab {
        self.inner.cursor_grab()
    }

    /// Moves the cursor to `position` in client-area pixels.
    #[inline]
    pub fn set_cursor_position(&self, position: Point) -> Result<()> {
        self.inner.set_cursor_position(position)
    }

    /// The paths of files dropped since the last call, in drop order (see
    /// [`Event::FilesDropped`](crate::Event::FilesDropped)).
    #[inline]
    pub fn take_dropped_files(&self) -> Vec<String> {
        self.inner.take_dropped_files()
    }
}

impl fmt::Debug for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Window").field("id", &self.id()).finish()
    }
}
