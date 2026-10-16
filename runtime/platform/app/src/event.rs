use core::mem::MaybeUninit;
use core::{fmt, slice};

use crate::Size;

/// Identifies a [`Window`](crate::Window) in [`Event`]s. Unique for the life of its [`App`](crate::App).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct WindowId(pub(crate) u32);

impl WindowId {
    /// An id for synthetic events (tests, replays). Real windows get theirs from
    /// [`Window::id`](crate::Window::id).
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// An application or window event, gathered by [`App::pump`](crate::App::pump).
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// The user asked to close the window (close button, Alt+F4). The window stays open until it
    /// is dropped.
    CloseRequested(WindowId),
    /// The client area changed size, in physical pixels. Never zero: minimizing reports
    /// [`Event::Minimized`] instead. Consecutive resizes of one window coalesce.
    Resized {
        window: WindowId,
        size: Size,
    },
    /// The window moved to a monitor with a different DPI (96 = 100% scale). A [`Event::Resized`]
    /// with the rescaled size follows.
    DpiChanged {
        window: WindowId,
        dpi: u32,
    },
    /// The window gained or lost keyboard focus.
    Focused {
        window: WindowId,
        focused: bool,
    },
    /// The window was minimized; skip rendering to it until [`Event::Restored`].
    Minimized(WindowId),
    Restored(WindowId),
    /// The system is about to sleep (desktop) or the title is being suspended (console). Stop
    /// audio and GPU work and save what must survive.
    Suspended,
    /// The system or title woke up after [`Event::Suspended`].
    Resumed,
    /// Something asked the whole application to quit (`WM_QUIT` on Windows).
    Quit,
}

impl Event {
    /// The window the event is about, or `None` for application-wide events.
    pub fn window(&self) -> Option<WindowId> {
        match *self {
            Self::CloseRequested(window)
            | Self::Resized { window, .. }
            | Self::DpiChanged { window, .. }
            | Self::Focused { window, .. }
            | Self::Minimized(window)
            | Self::Restored(window) => Some(window),
            Self::Suspended | Self::Resumed | Self::Quit => None,
        }
    }
}

/// Default [`EventQueue`] capacity: far more window events than a frame produces, since input
/// does not go through this queue.
pub const EVENT_CAPACITY: usize = 128;

/// A fixed-capacity event buffer: filled during a pump, read and cleared once per frame. Never
/// allocates.
///
/// [`push`](Self::push) coalesces a resize (or DPI change) with the newest queued one for the
/// same window, so a live resize produces one event. When full, new events are dropped and
/// counted in [`dropped`](Self::dropped).
pub struct EventQueue<const N: usize = EVENT_CAPACITY> {
    events: [MaybeUninit<Event>; N],
    len: usize,
    dropped: usize,
}

impl<const N: usize> EventQueue<N> {
    pub const fn new() -> Self {
        Self {
            events: [const { MaybeUninit::uninit() }; N],
            len: 0,
            dropped: 0,
        }
    }

    /// Appends `event`, or merges it into the last queued one. Returns `false` if the queue was
    /// full and the event was dropped.
    pub fn push(&mut self, event: Event) -> bool {
        if let Some(last) = self.as_mut_slice().last_mut() {
            let merge = match (*last, event) {
                (Event::Resized { window: a, .. }, Event::Resized { window: b, .. })
                | (Event::DpiChanged { window: a, .. }, Event::DpiChanged { window: b, .. }) => {
                    a == b
                }
                _ => false,
            };
            if merge {
                *last = event;
                return true;
            }
        }
        if self.len == N {
            self.dropped += 1;
            return false;
        }
        self.events[self.len].write(event);
        self.len += 1;
        true
    }

    #[inline]
    pub fn as_slice(&self) -> &[Event] {
        // SAFETY: the first `len` elements are initialized, and `Event` is `Copy` (no drop).
        unsafe { slice::from_raw_parts(self.events.as_ptr().cast(), self.len) }
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [Event] {
        // SAFETY: as in `as_slice`; `&mut self` makes the borrow unique.
        unsafe { slice::from_raw_parts_mut(self.events.as_mut_ptr().cast(), self.len) }
    }

    #[inline]
    pub fn iter(&self) -> slice::Iter<'_, Event> {
        self.as_slice().iter()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Events dropped because the queue was full, since the last [`clear`](Self::clear).
    #[inline]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Removes every event and resets the dropped count.
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }

    /// Moves every event of `other` to the end of `self` (coalescing as [`push`](Self::push)
    /// does) and clears `other`. Its dropped count carries over.
    pub fn append<const M: usize>(&mut self, other: &mut EventQueue<M>) {
        for &event in other.as_slice() {
            self.push(event);
        }
        self.dropped += other.dropped;
        other.clear();
    }
}

impl<const N: usize> Default for EventQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, const N: usize> IntoIterator for &'a EventQueue<N> {
    type Item = &'a Event;
    type IntoIter = slice::Iter<'a, Event>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<const N: usize> fmt::Debug for EventQueue<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}
