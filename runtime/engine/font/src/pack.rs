//! Skyline bottom-left rectangle packing (0012 §6.3), for glyph cache pages.
//!
//! The packed area's top profile is a list of horizontal segments; a rectangle goes where its
//! top edge would be lowest (then leftmost), resting on the highest segment it spans. Packing is
//! only ever reset as a whole: pages are evicted entire, never glyph by glyph.

use alloc::vec;
use alloc::vec::Vec;

/// A placed rectangle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
struct Span {
    x: u32,
    y: u32,
    width: u32,
}

/// A skyline packer over a `width` × `height` area.
#[derive(Clone, Debug)]
pub struct Skyline {
    width: u32,
    height: u32,
    spans: Vec<Span>,
    used: u64,
}

impl Skyline {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            spans: vec![Span { x: 0, y: 0, width }],
            used: 0,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Area handed out, in pixels.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// Empties the area.
    pub fn clear(&mut self) {
        *self = Self::new(self.width, self.height);
    }

    /// The top edge a `width` × `height` rectangle would have if placed at span `i`, or `None` if
    /// it does not fit there.
    fn fit(&self, i: usize, width: u32, height: u32) -> Option<u32> {
        let x = self.spans[i].x;
        if x + width > self.width {
            return None;
        }
        let mut y = 0;
        for s in &self.spans[i..] {
            if s.x >= x + width {
                break;
            }
            y = y.max(s.y);
        }
        (y + height <= self.height).then_some(y)
    }

    /// Places a `width` × `height` rectangle, or returns `None` if the area has no room for it.
    pub fn insert(&mut self, width: u32, height: u32) -> Option<Rect> {
        if width == 0 || height == 0 || width > self.width || height > self.height {
            return None;
        }
        let mut best: Option<(u32, u32, usize)> = None;
        for i in 0..self.spans.len() {
            if let Some(y) = self.fit(i, width, height) {
                let key = (y + height, self.spans[i].x);
                if best.is_none_or(|(b, bx, _)| key < (b, bx)) {
                    best = Some((key.0, key.1, i));
                }
            }
        }
        let (bottom, x, i) = best?;
        let y = bottom - height;
        // The new span replaces what it covers; a span it covers partly keeps its rest.
        let new = Span {
            x,
            y: bottom,
            width,
        };
        let end = x + width;
        let j = i;
        while j < self.spans.len() && self.spans[j].x < end {
            let s = self.spans[j];
            if s.x + s.width <= end {
                self.spans.remove(j);
            } else {
                self.spans[j] = Span {
                    x: end,
                    y: s.y,
                    width: s.x + s.width - end,
                };
                break;
            }
        }
        self.spans.insert(i, new);
        // Neighbours at the same height merge.
        let mut k = 0;
        while k + 1 < self.spans.len() {
            if self.spans[k].y == self.spans[k + 1].y {
                self.spans[k].width += self.spans[k + 1].width;
                self.spans.remove(k + 1);
            } else {
                k += 1;
            }
        }
        self.used += u64::from(width) * u64::from(height);
        Some(Rect {
            x,
            y,
            width,
            height,
        })
    }
}
