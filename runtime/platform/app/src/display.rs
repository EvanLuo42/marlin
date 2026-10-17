use core::fmt;

/// A position in physical pixels. Screen positions are on the virtual desktop, whose origin is
/// the primary display's top-left corner (other displays may have negative coordinates).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    #[inline]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// A rectangle in physical pixels.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[inline]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether `p` lies inside (right and bottom edges excluded).
    #[inline]
    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x
            && p.y >= self.y
            && ((p.x - self.x) as i64) < self.width as i64
            && ((p.y - self.y) as i64) < self.height as i64
    }

    /// Whether `other` lies entirely inside `self`.
    #[inline]
    pub const fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x as i64 + other.width as i64 <= self.x as i64 + self.width as i64
            && other.y as i64 + other.height as i64 <= self.y as i64 + self.height as i64
    }
}

/// Identifies a display while the display configuration stays the same. Enumerate again after
/// [`Event::DisplaysChanged`](crate::Event::DisplaysChanged): ids of removed displays become
/// stale, and functions taking one fail with `NotFound`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DisplayId(pub(crate) u64);

impl DisplayId {
    /// An id for synthetic displays (tests, replays).
    #[inline]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The native handle (`HMONITOR` on Windows).
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// What a display can do with HDR output.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Hdr {
    #[default]
    Unsupported,
    /// The display supports HDR but the user has it switched off; an HDR10 swapchain would be
    /// tone-mapped back to SDR by the OS.
    Supported,
    /// HDR is on: present HDR10 or scRGB for full range.
    Enabled,
}

/// Longest [`Display::name`] kept, in bytes.
pub const DISPLAY_NAME_CAPACITY: usize = 64;

/// A connected display (monitor), as [`App::displays`](crate::App::displays) found it.
///
/// Luminance, primaries and bit depth come from the RHI's output description, matched by
/// [`device_name`](Self::device_name), because only the GPU API reports them reliably.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Display {
    pub id: DisplayId,
    /// The whole display on the virtual desktop.
    pub bounds: Rect,
    /// [`bounds`](Self::bounds) minus taskbars and docks: where windowed windows should go.
    pub work_area: Rect,
    /// 96 is 100% scale.
    pub dpi: u32,
    /// Current refresh rate in millihertz (59 940 for 59.94 Hz), or 0 if unknown.
    pub refresh_rate_mhz: u32,
    pub primary: bool,
    pub hdr: Hdr,
    /// Brightness the OS gives SDR white while HDR is on, in nits (80 = the sRGB reference).
    /// Composite SDR content (UI) at this level so it matches the desktop.
    pub sdr_white_nits: f32,
    name: InlineStr<DISPLAY_NAME_CAPACITY>,
    device_name: InlineStr<32>,
}

impl Display {
    /// A display with default properties, for tests and replays; backends fill in the rest.
    pub fn new(id: DisplayId, bounds: Rect) -> Self {
        Self {
            id,
            bounds,
            work_area: bounds,
            dpi: 96,
            refresh_rate_mhz: 0,
            primary: false,
            hdr: Hdr::Unsupported,
            sdr_white_nits: 80.0,
            name: InlineStr::new(),
            device_name: InlineStr::new(),
        }
    }

    /// Human-readable monitor name ("DELL U2723QE"); the device name when the monitor has none.
    #[inline]
    pub fn name(&self) -> &str {
        match self.name.as_str() {
            "" => self.device_name(),
            name => name,
        }
    }

    /// The OS name of the display output (`\\.\DISPLAY1` on Windows).
    #[inline]
    pub fn device_name(&self) -> &str {
        self.device_name.as_str()
    }

    #[inline]
    pub fn refresh_rate(&self) -> f32 {
        self.refresh_rate_mhz as f32 / 1000.0
    }

    /// `dpi / 96`.
    #[inline]
    pub fn scale_factor(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Sets [`name`](Self::name), truncated to [`DISPLAY_NAME_CAPACITY`] bytes.
    pub fn set_name(&mut self, name: &str) {
        self.name = InlineStr::from_chars(name.chars());
    }

    pub fn set_device_name(&mut self, name: &str) {
        self.device_name = InlineStr::from_chars(name.chars());
    }

    #[cfg_attr(not(marlin_platform = "windows"), expect(dead_code))]
    pub(crate) fn set_name_chars(&mut self, chars: impl Iterator<Item = char>) {
        self.name = InlineStr::from_chars(chars);
    }

    #[cfg_attr(not(marlin_platform = "windows"), expect(dead_code))]
    pub(crate) fn set_device_name_chars(&mut self, chars: impl Iterator<Item = char>) {
        self.device_name = InlineStr::from_chars(chars);
    }
}

/// UTF-8 in a fixed buffer, truncated at a character boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
struct InlineStr<const N: usize> {
    bytes: [u8; N],
    len: u8,
}

impl<const N: usize> InlineStr<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    fn from_chars(chars: impl Iterator<Item = char>) -> Self {
        let mut s = Self::new();
        let mut len = 0;
        for c in chars {
            if c == '\0' || len + c.len_utf8() > N.min(u8::MAX as usize) {
                break;
            }
            len += c.encode_utf8(&mut s.bytes[len..]).len();
        }
        s.len = len as u8;
        s
    }

    fn as_str(&self) -> &str {
        // SAFETY: only whole chars are encoded into `bytes[..len]`.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len as usize]) }
    }
}

impl<const N: usize> fmt::Debug for InlineStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}
