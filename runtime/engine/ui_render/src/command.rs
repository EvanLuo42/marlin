//! The command format the GPU reads (0012 §3.1), shared by the painter, the CPU reference and
//! `content/engine/shaders/Ui/Ui.mshader`.
//!
//! Every primitive is one **command**, four words:
//!
//! | word | bits | meaning |
//! |---|---|---|
//! | 0 | 0–5 | kind ([`Kind`]) |
//! | 0 | 6–15 | flags ([`OPAQUE`], image flags) |
//! | 0 | 16–31 | group ([`GROUP_WORDS`]; 0: the screen) |
//! | 1 | 0–15 | clip ([`CLIP_WORDS`]; 0: none) |
//! | 2 | | paint index |
//! | 3 | | payload offset, in words |
//!
//! A **paint** is four words: kind ([`PAINT_SOLID`], [`PAINT_IMAGE`] or [`PAINT_GRADIENT`] with
//! its gradient index in the second word and a [`GRADIENT_WORDS`] record), the color
//! ([`Color::packed`], sRGB, straight alpha; an image's tint), and for images the texture's view
//! index and the sampler index ([`DEFAULT_SAMPLER`] until the renderer puts its own there). The **payload** is 32-bit words, per kind (coordinates are physical
//! pixels, y down, as `f32` bits):
//!
//! | kind | words |
//! |---|---|
//! | `Rect` | x0 y0 x1 y1, radii top-left top-right bottom-right bottom-left, stroke width, stroke color |
//! | `Circle` | cx cy radius, stroke width, stroke color |
//! | `Line` | ax ay bx by, width, cap, dash, gap, phase (dash 0: solid) |
//! | `Triangle` | x0 y0 x1 y1 x2 y2, stroke width, stroke color |
//! | `Glyph` | pen x, pen y (whole pixels, `i32`), glyph id |
//! | `Image` | x0 y0 x1 y1, u0 v0 u1 v1, texture width \| height << 16, radii ×4 |
//! | `Effect` | 0 shadow / 1 glow, 0, the members' screen box x0 y0 x1 y1 (`f32`), and the radius of the box used instead of the members' shape (`f32`, negative: their shape) |
//! | `Path` | fill rule (0 non-zero, 1 even-odd), the payload's length, band origin y (`f32`), bands, segments, screen box x0 y0 x1 y1 (`f32`); per 16-pixel band from the origin, the offset (from the payload's start) and length of its segment list; the lists; the segments as x0 y0 x1 y1 (`f32`, screen pixels) |

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
/// Paint kinds.
pub const PAINT_SOLID: u32 = 0;
pub const PAINT_IMAGE: u32 = 1;
/// A gradient: the paint's second word is its index among the frame's gradients.
pub const PAINT_GRADIENT: u32 = 2;
/// An external image: the paint's third word is its `ExternalImageId` until the passes put the
/// texture's view there.
pub const PAINT_EXTERNAL: u32 = 3;

/// Words of a gradient record: a header, then [`GRADIENT_TEXELS`] premultiplied RGBA8 texels.
///
/// | words | meaning |
/// |---|---|
/// | 0 | shape: 0 linear, 1 radial, 2 angle |
/// | 1–4 | on the screen (`f32`): linear from x y to x y; radial center x y, radius; angle center x y, start angle |
/// | 5 | opacity (`f32`) |
/// | 6, 7 | spare |
/// | 8.. | the ramp from 0 to 1, sRGB-encoded, premultiplied |
pub const GRADIENT_WORDS: usize = 8 + GRADIENT_TEXELS;
pub const GRADIENT_TEXELS: usize = 256;
pub type GradientWords = [u32; GRADIENT_WORDS];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Kind {
    Rect = 1,
    Circle = 2,
    Line = 3,
    Triangle = 4,
    Glyph = 5,
    Image = 6,
    /// A group's shadow or glow, before its members (§4.3).
    Effect = 7,
    /// A filled path (§4.2).
    Path = 8,
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
            7 => Self::Effect,
            8 => Self::Path,
            _ => return None,
        })
    }

    /// Payload words: a path's are its word 1.
    pub fn payload_len(self, payload: &[u32]) -> usize {
        match self {
            Self::Path => payload[1] as usize,
            k => k.payload_words() as usize,
        }
    }

    /// Payload words, for every kind but paths (0).
    pub const fn payload_words(self) -> u32 {
        match self {
            Self::Rect => 10,
            Self::Circle => 5,
            Self::Line => 9,
            Self::Triangle => 8,
            Self::Glyph => 3,
            Self::Image => 13,
            Self::Effect => 7,
            Self::Path => 0,
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

/// Words of a group record (§2.2, §3.1):
///
/// | words | meaning |
/// |---|---|
/// | 0–5 | the transform from the group's space to the screen (physical pixels), [`Affine2`] order |
/// | 6–11 | its inverse |
/// | 12 | opacity (`f32`) |
/// | 13 | group units per screen pixel (`f32`): the inverse's scale, exact for similarities |
/// | 14 | flags: [`GROUP_NONUNIFORM`], [`GROUP_AXIS_ALIGNED`] |
/// | 15, 16 | shadow and glow: the first member command and the member count |
/// | 17–20 | shadow offset x, y and blur radius (screen pixels, `f32`), shadow color |
/// | 21, 22 | glow radius (screen pixels, `f32`), glow color |
/// | 23 | the most group units one screen pixel spans (`f32`, the inverse's largest singular value) |
///
/// Group 0 is the screen itself: the identity, opacity 1. Its commands' coordinates are physical
/// pixels; every other group's commands are in its own space.
///
/// [`Affine2`]: crate::style::Affine2
pub const GROUP_WORDS: usize = 24;
pub type GroupWords = [u32; GROUP_WORDS];
/// The transform is not a similarity: coverage measures its distance gradient per pixel.
pub const GROUP_NONUNIFORM: u32 = 1;
/// No rotation or skew: opaque interiors map to screen rectangles.
pub const GROUP_AXIS_ALIGNED: u32 = 2;

/// Words of a clip record (§3.1): a rounded rectangle in a group's space and its parent clip.
///
/// | words | meaning |
/// |---|---|
/// | 0–3 | x0 y0 x1 y1 in the group's space (`f32`) |
/// | 4–7 | corner radii, as a rectangle's (`f32`) |
/// | 8 | its group |
/// | 9 | the parent clip (0: none) |
/// | 10–13 | the chain's screen bounding box (`f32`), its own and every parent's intersected |
/// | 14–17 | a screen rectangle the chain fully covers (`f32`; empty when unknown) |
///
/// Clip 0 is no clip.
pub const CLIP_WORDS: usize = 20;
pub type ClipWords = [u32; CLIP_WORDS];

/// The identity group record.
pub fn identity_group() -> GroupWords {
    let mut g = [0u32; GROUP_WORDS];
    for (i, v) in [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0].into_iter().enumerate() {
        g[i] = v.to_bits();
        g[6 + i] = v.to_bits();
    }
    g[12] = 1.0f32.to_bits();
    g[13] = 1.0f32.to_bits();
    g[14] = GROUP_AXIS_ALIGNED;
    g[23] = 1.0f32.to_bits();
    g
}

/// The empty clip record: no clip, covering everything.
pub fn no_clip() -> ClipWords {
    let mut c = [0u32; CLIP_WORDS];
    for i in [10, 11, 14, 15] {
        c[i] = (-1.0e30f32).to_bits();
    }
    for i in [12, 13, 16, 17] {
        c[i] = 1.0e30f32.to_bits();
    }
    c
}

/// The group index of a command.
pub const fn group_of(c: &CommandWords) -> u32 {
    c[0] >> 16
}

/// The clip index of a command.
pub const fn clip_of(c: &CommandWords) -> u32 {
    c[1] & 0xFFFF
}

/// A run of commands with its payload, paints, groups and clips, indices local to it. Group 0
/// and clip 0 are the sentinels ([`identity_group`], [`no_clip`]), always there.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub commands: Vec<CommandWords>,
    pub payload: Vec<u32>,
    pub paints: Vec<PaintWords>,
    pub gradients: Vec<GradientWords>,
    pub groups: Vec<GroupWords>,
    pub clips: Vec<ClipWords>,
}

impl Default for Chunk {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            payload: Vec::new(),
            paints: Vec::new(),
            gradients: Vec::new(),
            groups: alloc::vec![identity_group()],
            clips: alloc::vec![no_clip()],
        }
    }
}

impl Chunk {
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// The paint index of a solid `color`; consecutive commands of one color share it.
    pub(crate) fn solid(&mut self, color: Color) -> u32 {
        let words = [PAINT_SOLID, color.packed(), 0, 0];
        if self.paints.last() == Some(&words) {
            return self.paints.len() as u32 - 1;
        }
        self.paints.push(words);
        self.paints.len() as u32 - 1
    }

    /// A new image paint.
    pub(crate) fn image(&mut self, kind: u32, tint: Color, view: u32, sampler: u32) -> u32 {
        self.paints.push([kind, tint.packed(), view, sampler]);
        self.paints.len() as u32 - 1
    }

    pub(crate) fn push(
        &mut self,
        kind: Kind,
        flags: u32,
        group: u32,
        clip: u32,
        paint: u32,
        payload: &[u32],
    ) {
        debug_assert_eq!(payload.len(), kind.payload_len(payload));
        self.commands.push([
            kind as u32 | flags | group << 16,
            clip,
            paint,
            self.payload.len() as u32,
        ]);
        self.payload.extend_from_slice(payload);
    }

    /// Appends `other`, its indices rebased; its commands without a clip take `clip` (an index
    /// of `self`), and so do its root clips.
    pub(crate) fn append_clipped(&mut self, other: &Chunk, clip: u32) {
        let (payload, paints) = (self.payload.len() as u32, self.paints.len() as u32);
        let gradients = self.gradients.len() as u32;
        let commands = self.commands.len() as u32;
        // Other's group or clip `i` (from 1) lands at `base + i - 1`.
        let gb = self.groups.len() as u32 - 1;
        let cb = self.clips.len() as u32 - 1;
        let group = |g: u32| if g == 0 { 0 } else { g + gb };
        let rebase_clip = |c: u32| if c == 0 { clip } else { c + cb };
        self.groups.extend(other.groups[1..].iter().map(|g| {
            let mut g = *g;
            if g[16] != 0 {
                g[15] += commands;
            }
            g
        }));
        self.clips.extend(other.clips[1..].iter().map(|c| {
            let mut c = *c;
            c[8] = group(c[8]);
            c[9] = rebase_clip(c[9]);
            c
        }));
        self.commands.extend(other.commands.iter().map(|c| {
            [
                (c[0] & 0xFFFF) | group(c[0] >> 16) << 16,
                rebase_clip(c[1] & 0xFFFF),
                c[2] + paints,
                c[3] + payload,
            ]
        }));
        self.payload.extend_from_slice(&other.payload);
        self.paints.extend(other.paints.iter().map(|p| {
            let mut p = *p;
            if p[0] == PAINT_GRADIENT {
                p[1] += gradients;
            }
            p
        }));
        self.gradients.extend_from_slice(&other.gradients);
    }
}

pub(crate) fn f(v: f32) -> u32 {
    v.to_bits()
}
