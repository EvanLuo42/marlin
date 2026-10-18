//! The command format the GPU reads (0012 §3.1), shared by the painter, the CPU reference and
//! `content/engine/shaders/Ui/Ui.mshader`.
//!
//! Every primitive is one **command**, four words:
//!
//! | word | bits | meaning |
//! |---|---|---|
//! | 0 | 0–5 | kind ([`Kind`]) |
//! | 0 | 6–15 | flags ([`OPAQUE`], image flags) |
//! | 0 | 16–31 | group (U2; 0 now) |
//! | 1 | 0–15 | clip (U2; 0 now) |
//! | 2 | | paint index |
//! | 3 | | payload offset, in words |
//!
//! A **paint** is four words: kind (0, solid), the color ([`Color::packed`], sRGB, straight
//! alpha), two spare. The **payload** is 32-bit words, per kind (coordinates are physical
//! pixels, y down, as `f32` bits):
//!
//! | kind | words |
//! |---|---|
//! | `Rect` | x0 y0 x1 y1, radii top-left top-right bottom-right bottom-left, stroke width, stroke color |
//! | `Circle` | cx cy radius, stroke width, stroke color |
//! | `Line` | ax ay bx by, width, cap, dash, gap, phase (dash 0: solid) |
//! | `Triangle` | x0 y0 x1 y1 x2 y2, stroke width, stroke color |
//! | `Glyph` | pen x, pen y (whole pixels, `i32`), glyph id |
//! | `Image` | x0 y0 x1 y1, u0 v0 u1 v1, view, sampler, width \| height << 16, radii ×4 |

use alloc::vec::Vec;

use crate::style::Color;

/// Pixels on a tile's side: one workgroup shades a tile (0012 §5.4).
pub const TILE: u32 = 16;
/// Tiles on a bin's side: 256 × 256 pixel bins (§5.2).
pub const BIN_TILES: u32 = 16;
pub const BIN: u32 = TILE * BIN_TILES;
/// Commands a bin mask covers: one per bit of 8 words (§5.2).
pub const CHUNK_COMMANDS: u32 = 256;
/// Words of a tile list block: 31 command indices, then the next block's index.
pub const BLOCK_WORDS: u32 = 32;
pub const BLOCK_COMMANDS: u32 = BLOCK_WORDS - 1;
/// No block.
pub const NO_BLOCK: u32 = u32::MAX;
/// The renderer's own sampler, in an image command's sampler word.
pub const DEFAULT_SAMPLER: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Kind {
    Rect = 1,
    Circle = 2,
    Line = 3,
    Triangle = 4,
    Glyph = 5,
    Image = 6,
}

impl Kind {
    pub const fn from_word(w: u32) -> Option<Self> {
        Some(match w & 63 {
            1 => Self::Rect,
            2 => Self::Circle,
            3 => Self::Line,
            4 => Self::Triangle,
            5 => Self::Glyph,
            6 => Self::Image,
            _ => return None,
        })
    }

    /// Payload words.
    pub const fn payload_words(self) -> u32 {
        match self {
            Self::Rect => 10,
            Self::Circle => 5,
            Self::Line => 9,
            Self::Triangle => 8,
            Self::Glyph => 3,
            Self::Image => 15,
        }
    }
}

/// The command covers its interior fully and opaquely: tiles inside the interior drop what
/// came before (§5.3).
pub const OPAQUE: u32 = 1 << 6;
/// Image: the texels are sRGB-encoded and premultiplied (else linear and straight).
pub const IMAGE_SRGB_PREMULTIPLIED: u32 = 1 << 7;

pub type CommandWords = [u32; 4];
pub type PaintWords = [u32; 4];

/// A run of commands with its payload and paints, indices local to it.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Chunk {
    pub commands: Vec<CommandWords>,
    pub payload: Vec<u32>,
    pub paints: Vec<PaintWords>,
}

impl Chunk {
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn clear(&mut self) {
        self.commands.clear();
        self.payload.clear();
        self.paints.clear();
    }

    /// The paint index of a solid `color`; consecutive commands of one color share it.
    pub(crate) fn solid(&mut self, color: Color) -> u32 {
        let words = [0, color.packed(), 0, 0];
        if self.paints.last() == Some(&words) {
            return self.paints.len() as u32 - 1;
        }
        self.paints.push(words);
        self.paints.len() as u32 - 1
    }

    pub(crate) fn push(&mut self, kind: Kind, flags: u32, paint: u32, payload: &[u32]) {
        debug_assert_eq!(payload.len() as u32, kind.payload_words());
        self.commands
            .push([kind as u32 | flags, 0, paint, self.payload.len() as u32]);
        self.payload.extend_from_slice(payload);
    }

    /// Appends `other`, its indices rebased.
    pub(crate) fn append(&mut self, other: &Chunk) {
        let (payload, paints) = (self.payload.len() as u32, self.paints.len() as u32);
        self.commands.extend(
            other
                .commands
                .iter()
                .map(|c| [c[0], c[1], c[2] + paints, c[3] + payload]),
        );
        self.payload.extend_from_slice(&other.payload);
        self.paints.extend_from_slice(&other.paints);
    }
}

pub(crate) fn f(v: f32) -> u32 {
    v.to_bits()
}
