//! The CPU reference of the GPU pipeline (0012 §14): binning into tile lists and shading, with
//! the same formulas ([`crate::shape`]) and the same order. The GPU tests compare against it.
//!
//! Tile lists: a command goes into every tile its bounds touch (lines only where their reach
//! does, §5.3); an opaque command whose interior holds a whole tile empties that tile's list
//! first. Whether a line reaches a tile depends on a square root and a division, which may round
//! differently on the GPU, so entries within [`BORDERLINE`] of the threshold are marked
//! optional: the GPU may or may not list them, and either way their ink in the tile is zero.

use alloc::vec;
use alloc::vec::Vec;

use marlin_font::GlyphRecord;

use crate::command::{Chunk, Kind, TILE};
use crate::shape::{Bounds, Scene, Textures, bounds, line_touches_tile, over, shade};

/// Pixels of slack around a line's reach where the GPU's rounding may decide differently.
pub const BORDERLINE: f32 = 1.0 / 64.0;

/// One tile's commands in order.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct RefTile {
    pub commands: Vec<u32>,
    /// Per command: the GPU may leave it out.
    pub optional: Vec<bool>,
}

impl RefTile {
    /// Whether `gpu` is this list, give or take optional entries.
    pub fn matches(&self, gpu: &[u32]) -> bool {
        let mut i = 0;
        for (&c, &optional) in self.commands.iter().zip(&self.optional) {
            if gpu.get(i) == Some(&c) {
                i += 1;
            } else if !optional {
                return false;
            }
        }
        i == gpu.len()
    }
}

/// Tiles across and down a `width` × `height` surface.
pub fn tile_counts(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(TILE), height.div_ceil(TILE))
}

/// Every command's bounds.
pub fn all_bounds(chunk: &Chunk, glyphs: &[GlyphRecord], width: u32, height: u32) -> Vec<Bounds> {
    chunk
        .commands
        .iter()
        .map(|c| bounds(c, &Scene::new(chunk, glyphs), width, height))
        .collect()
}

/// The tile lists of `chunk`, row by row.
pub fn bin(chunk: &Chunk, glyphs: &[GlyphRecord], width: u32, height: u32) -> Vec<RefTile> {
    let (tx, ty) = tile_counts(width, height);
    let mut tiles = vec![RefTile::default(); (tx * ty) as usize];
    let t = TILE as i32;
    let scene = Scene::new(chunk, glyphs);
    for (i, c) in chunk.commands.iter().enumerate() {
        let b = bounds(c, &scene, width, height);
        if b.is_empty() {
            continue;
        }
        let line = Kind::from_word(c[0]) == Some(Kind::Line);
        for y in b.bbox[1] / t..=(b.bbox[3] - 1) / t {
            for x in b.bbox[0] / t..=(b.bbox[2] - 1) / t {
                let mut optional = false;
                if line {
                    let m =
                        line_touches_tile(c, &scene, (x * t) as f32 + 8.0, (y * t) as f32 + 8.0);
                    if m > BORDERLINE {
                        continue;
                    }
                    optional = m >= -BORDERLINE;
                }
                let tile = &mut tiles[(y as u32 * tx + x as u32) as usize];
                let [ix0, iy0, ix1, iy1] = b.interior;
                if x * t >= ix0 && y * t >= iy0 && x * t + t <= ix1 && y * t + t <= iy1 {
                    tile.commands.clear();
                    tile.optional.clear();
                }
                tile.commands.push(i as u32);
                tile.optional.push(optional);
            }
        }
    }
    tiles
}

/// The image the GPU draws: `clear` (premultiplied, sRGB-encoded) under every tile's commands,
/// as RGBA8 rows.
pub fn rasterize(
    chunk: &Chunk,
    glyphs: &[GlyphRecord],
    textures: &dyn Textures,
    width: u32,
    height: u32,
    clear: [f32; 4],
) -> Vec<u8> {
    let tiles = bin(chunk, glyphs, width, height);
    let (tx, _) = tile_counts(width, height);
    let scene = Scene::new(chunk, glyphs);
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let tile = &tiles[((y / TILE) * tx + x / TILE) as usize];
            let mut color = clear;
            for &i in &tile.commands {
                let c = &chunk.commands[i as usize];
                let src = shade(c, &scene, textures, x as f32 + 0.5, y as f32 + 0.5);
                color = over(src, color);
            }
            let at = ((y * width + x) * 4) as usize;
            for k in 0..4 {
                out[at + k] = (color[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
    }
    out
}
