use core::marker::PhantomData;
use core::time::Duration;

use alloc::boxed::Box;

use crate::backend::imp;
use crate::contract::App as _;
use crate::{Event, EventQueue, Result, Window, WindowDesc};

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
    #[inline]
    pub fn pump(&mut self) -> &[Event] {
        self.pump_timeout(Some(Duration::ZERO))
    }

    /// Like [`pump`](Self::pump), but first sleeps until a message arrives or `timeout` passes
    /// (`None` waits forever). For idling while minimized or suspended.
    pub fn pump_timeout(&mut self, timeout: Option<Duration>) -> &[Event] {
        self.frame.clear();
        self.inner.pump(timeout, &mut self.frame);
        self.dropped += self.frame.dropped() as u64;
        self.frame.as_slice()
    }

    /// Events lost to a full queue since the `App` was created. Should stay 0.
    #[inline]
    pub fn dropped_events(&self) -> u64 {
        self.dropped
    }
}
