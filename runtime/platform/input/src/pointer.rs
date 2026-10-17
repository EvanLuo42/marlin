use marlin_app::WindowId;

/// Touches tracked at once; further fingers are ignored until one lifts.
pub const MAX_TOUCHES: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    /// The OS took the touch over (an edge swipe, palm rejection): undo what it started.
    Cancelled,
}

/// One finger on a touch screen or precision touchpad in direct mode.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Touch {
    /// Stable while the finger stays down; reused afterwards.
    pub id: u32,
    pub window: WindowId,
    /// In physical pixels of the window's client area.
    pub position: [f32; 2],
    /// Touched down this frame.
    pub pressed: bool,
    /// Lifted (or cancelled) this frame; gone from the next frame.
    pub released: bool,
    pub cancelled: bool,
}

/// What a pen reports with each sample. Backends produce these.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PenSample {
    pub window: WindowId,
    /// In physical pixels of the window's client area.
    pub position: [f32; 2],
    /// [0, 1]; 0 while hovering.
    pub pressure: f32,
    /// Degrees from vertical toward +x and +y, in [-90, 90].
    pub tilt: [f32; 2],
    /// Clockwise rotation about the pen's axis in degrees, [0, 360).
    pub twist: f32,
    /// The tip (or eraser) touches the surface.
    pub contact: bool,
    /// The barrel button is held.
    pub barrel: bool,
    /// The eraser end is in use.
    pub eraser: bool,
}

/// The pen, as of this frame.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct PenState {
    pub(crate) sample: Option<PenSample>,
    pub(crate) pressed: bool,
    pub(crate) released: bool,
}

impl PenState {
    /// The last sample while the pen is in range (hovering or touching), else `None`.
    #[inline]
    pub fn sample(&self) -> Option<&PenSample> {
        self.sample.as_ref()
    }

    #[inline]
    pub fn in_range(&self) -> bool {
        self.sample.is_some()
    }

    /// Touching the surface now.
    #[inline]
    pub fn down(&self) -> bool {
        self.sample.is_some_and(|s| s.contact)
    }

    /// Touched down at least once this frame.
    #[inline]
    pub fn pressed(&self) -> bool {
        self.pressed
    }

    /// Lifted at least once this frame.
    #[inline]
    pub fn released(&self) -> bool {
        self.released
    }
}
