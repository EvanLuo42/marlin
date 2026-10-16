use core::ffi::c_void;
use core::ptr::NonNull;

use crate::backend::imp;
use crate::contract::Window as _;
use crate::{Result, WindowId};

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
    /// A borderless window covering its whole monitor. With a flip-model swapchain this gets the
    /// same latency as exclusive fullscreen without mode switches.
    BorderlessFullscreen,
}

/// Window creation parameters.
#[derive(Clone, Copy, Debug)]
pub struct WindowDesc<'a> {
    pub title: &'a str,
    /// Client area size in logical pixels (96 DPI), scaled by the monitor's DPI.
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub mode: WindowMode,
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
            mode: WindowMode::Windowed,
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

    /// Dots per inch of the window's monitor; 96 is 100% scale.
    #[inline]
    pub fn dpi(&self) -> u32 {
        self.inner.dpi()
    }

    /// `dpi / 96`: logical-to-physical pixel ratio.
    #[inline]
    pub fn scale_factor(&self) -> f32 {
        self.inner.dpi() as f32 / 96.0
    }

    #[inline]
    pub fn mode(&self) -> WindowMode {
        self.inner.mode()
    }

    /// Switches between windowed and borderless fullscreen. Leaving fullscreen restores the
    /// previous windowed position and size.
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
    pub fn is_minimized(&self) -> bool {
        self.inner.is_minimized()
    }

    #[inline]
    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }
}
