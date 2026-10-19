//! Pairing patterns for ReSTIR's paired spatial reuse (0010 §2.5, PTE §3): tiles of pixels
//! paired two by two, each pixel holding the offset to its partner, so that when A reuses from
//! B, B reuses from A and the two shifts between them are computed once.
//!
//! The cooker generates them (`marlin-cook-render`); the engine's is
//! `content/engine/lighting/PairingPattern.masset`, found by name. One pattern per spatial
//! neighbor, of different sizes so their repeats do not line up.

use alloc::vec;
use alloc::vec::Vec;

use marlin_reflect::Reflect;
use marlin_serial::cooked::{CookedView, LayoutError, Pod, RelSlice};

/// What the cooker generates: patterns of the side lengths `sizes`, the partners' offsets about
/// normally distributed, at a mean distance of a disk of radius `radius`'s (`2r/3`).
///
/// ```text
/// PairingPattern
/// {
///     !Uuid = "…"
///     !Name = "Marlin.Engine.Lighting.PairingPattern"
///     Radius = 30.0
///     Sizes = [254, 230, 210]
/// }
/// ```
#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(name = "marlin_render::PairingPattern", default)]
pub struct PairingPattern {
    /// Pixels.
    pub radius: f32,
    /// Each pattern's side, even and at most [`MAX_PATTERN_SIZE`]; at most [`MAX_PATTERNS`].
    pub sizes: Vec<u32>,
    /// Seeds the generation.
    pub seed: u64,
}

impl Default for PairingPattern {
    /// 0010 §2.5: three patterns, 254², 230² and 210², radius 30 (σ ≈ 16).
    fn default() -> Self {
        Self {
            radius: 30.0,
            sizes: vec![254, 230, 210],
            seed: 0x5EED_9A1E,
        }
    }
}

/// The most patterns (spatial neighbors) a cooked pattern set holds.
pub const MAX_PATTERNS: usize = 4;
/// The largest side: offsets are at most half of it, and fit 8 bits.
pub const MAX_PATTERN_SIZE: u32 = 254;

/// The cooked form: the patterns' sides, then their offsets, pattern by pattern, row-major. An
/// offset is `(dx + 128) | (dy + 128) << 8`, the partner of `(x, y)` at `(x + dx, y + dy)`
/// wrapped around the pattern; the partner's offset is `(-dx, -dy)`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
pub struct CookedPairingPattern {
    pub sizes: RelSlice<u32>,
    pub offsets: RelSlice<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PairingError {
    Layout(LayoutError),
    /// No patterns, too many, a side odd or too large, or the offsets not as many as the
    /// pixels.
    Malformed,
}

impl From<LayoutError> for PairingError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A cooked pattern set, checked.
#[derive(Clone, Copy, Debug)]
pub struct PairingPatternView<'a> {
    pub sizes: &'a [u32],
    pub offsets: &'a [u16],
}

impl<'a> PairingPatternView<'a> {
    pub fn new(payload: &'a [u8]) -> Result<Self, PairingError> {
        let view = CookedView::new(payload);
        let root: &CookedPairingPattern = view.root()?;
        let sizes = view.slice(&root.sizes)?;
        let offsets = view.slice(&root.offsets)?;
        let pixels: u64 = sizes.iter().map(|&s| u64::from(s) * u64::from(s)).sum();
        if sizes.is_empty()
            || sizes.len() > MAX_PATTERNS
            || sizes
                .iter()
                .any(|&s| !(2..=MAX_PATTERN_SIZE).contains(&s) || !s.is_multiple_of(2))
            || pixels != offsets.len() as u64
        {
            return Err(PairingError::Malformed);
        }
        Ok(Self { sizes, offsets })
    }

    /// The offsets of pattern `i`, row-major.
    pub fn pattern(&self, i: usize) -> &'a [u16] {
        let start: usize = self.sizes[..i].iter().map(|&s| (s * s) as usize).sum();
        let s = self.sizes[i] as usize;
        &self.offsets[start..start + s * s]
    }
}

/// An offset's `(dx, dy)`.
pub fn unpack_offset(packed: u16) -> (i32, i32) {
    (i32::from(packed & 0xFF) - 128, i32::from(packed >> 8) - 128)
}

/// `(dx, dy)` packed; each in -128..=127.
pub fn pack_offset(dx: i32, dy: i32) -> u16 {
    debug_assert!((-128..128).contains(&dx) && (-128..128).contains(&dy));
    ((dx + 128) as u16) | (((dy + 128) as u16) << 8)
}

/// Loaded in place (`marlin_asset::InPlace<PairingPattern>`), checked once.
impl marlin_asset::CookedAsset for PairingPattern {
    type View<'a> = PairingPatternView<'a>;
    type Error = PairingError;

    fn view<'a>(payload: &'a [u8], _: &[u64]) -> Result<PairingPatternView<'a>, PairingError> {
        PairingPatternView::new(payload)
    }
}
