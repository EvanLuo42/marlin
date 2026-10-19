use core::marker::PhantomData;
use core::time::Duration;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::backend::imp;
use crate::contract::App as _;
use crate::{Display, Event, EventQueue, Lifecycle, Result, Window, WindowDesc};

/// The application: owns the main thread's message pump and creates windows.
///
/// Not `Send`: OS windows deliver their messages to the thread that created them, so the `App`,
/// its windows and the pump all stay on one thread (the main thread on platforms that require
/// it). Several `App`s may exist, typically one per test thread.
pub struct App {
    pub(crate) inner: imp::App,
    frame: Box<EventQueue>,
    dropped: u64,
    _not_send: PhantomData<*const ()>,
}

impl App {
    /// Initializes the windowing system for this thread (on Windows: per-monitor DPI awareness
    /// and the window class).
    pub fn new() -> Result<Self> {
        Ok(Self {
            inner: imp::App::new()?,
            frame: Box::new(EventQueue::new()),
            dropped: 0,
            _not_send: PhantomData,
        })
    }

    pub fn create_window(&self, desc: &WindowDesc<'_>) -> Result<Window> {
        Ok(Window {
            inner: self.inner.create_window(desc)?,
        })
    }

    /// Processes every pending OS message without blocking and returns the events gathered since
    /// the previous pump. Call once per frame.
    ///
    /// On Windows, dragging a window's border or title bar runs a modal loop inside the OS, and
    /// this call returns only when the drag ends; the resulting resizes coalesce into one event.
    /// Use [`pump_with`](Self::pump_with) to keep rendering meanwhile.
    #[inline]
    pub fn pump(&mut self) -> &[Event] {
        self.pump_timeout(Some(Duration::ZERO))
    }

    /// Like [`pump`](Self::pump), but first sleeps until a message arrives or `timeout` passes
    /// (`None` waits forever). For idling while minimized or suspended.
    pub fn pump_timeout(&mut self, timeout: Option<Duration>) -> &[Event] {
        self.frame.clear();
        self.inner.pump(timeout, &mut self.frame, None);
        self.dropped += self.frame.dropped() as u64;
        self.frame.as_slice()
    }

    /// Like [`pump_timeout`](Self::pump_timeout), but `tick` keeps the application running while
    /// the OS holds the pump in a modal loop (on Windows, dragging or resizing a window): it is
    /// called every few milliseconds with the events gathered since the last call, and those events
    /// are then not part of the returned slice. Run a frame in it.
    pub fn pump_with(
        &mut self,
        timeout: Option<Duration>,
        mut tick: impl FnMut(&[Event]),
    ) -> &[Event] {
        self.frame.clear();
        self.inner.pump(timeout, &mut self.frame, Some(&mut tick));
        self.dropped += self.frame.dropped() as u64;
        self.frame.as_slice()
    }

    /// Events lost to a full queue since the `App` was created. Should stay 0.
    #[inline]
    pub fn dropped_events(&self) -> u64 {
        self.dropped
    }

    /// Where the application is in its lifecycle, as of the last pump.
    #[inline]
    pub fn lifecycle(&self) -> Lifecycle {
        self.inner.lifecycle()
    }

    /// Every connected display, the primary one first. Enumerate again after
    /// [`Event::DisplaysChanged`].
    pub fn displays(&self) -> Vec<Display> {
        let mut out = Vec::new();
        self.inner.displays(&mut out);
        out
    }

    /// The display holding the desktop's origin, where the taskbar and new windows go by default.
    pub fn primary_display(&self) -> Option<Display> {
        self.displays().into_iter().find(|d| d.primary)
    }

    /// The text on the system clipboard, or `None` if it holds no text.
    #[inline]
    pub fn clipboard_text(&self) -> Result<Option<String>> {
        self.inner.clipboard_text()
    }

    /// Replaces the system clipboard's contents with `text`.
    #[inline]
    pub fn set_clipboard_text(&self, text: &str) -> Result<()> {
        self.inner.set_clipboard_text(text)
    }
}
