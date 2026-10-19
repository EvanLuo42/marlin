//! The CPU reference of the GPU pipeline (0012 §14): binning into tile lists and shading, with
//! the same formulas ([`crate::shape`]) and the same order. The GPU tests compare against it.
//!
//! Tile lists: a command goes into every tile its bounds touch (lines only where their reach
//! does, §5.3); an opaque command whose interior holds a whole tile empties that tile's list
//! first, of its own layer's commands only (backdrop blurs, [`crate::blur`]): a blur in a later
//! layer may read the tile's earlier ones. Whether a line reaches a tile depends on a square root
//! and a division, which may round differently on the GPU, so entries within [`BORDERLINE`] of
//! the threshold are marked optional: the GPU may or may not list them, and either way their ink
//! in the tile is zero.
//!
//! Frames with backdrop blurs are drawn layer by layer, each layer's pixels stored as RGBA8 like
//! the GPU's, and every blur's image made from the pixels under its layer ([`crate::blur`]).

use alloc::vec;
use alloc::vec::Vec;

use marlin_font::GlyphRecord;

use crate::blur::{self, Plan, Rgba8};
use crate::command::{Chunk, Kind, TILE};
use crate::shape::{Bounds, Scene, Textures, bounds, composite, line_touches_tile};

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
    let plan = blur::plan(chunk, glyphs, width, height);
    let (tx, ty) = tile_counts(width, height);
    let mut tiles = vec![RefTile::default(); (tx * ty) as usize];
    let t = TILE as i32;
    let scene = Scene::new(chunk, glyphs);
    for (i, c) in chunk.commands.iter().enumerate() {
        let b = bounds(c, &scene, width, height);
        if b.is_empty() {
            continue;
        }
        let layer_start = plan.starts[plan.layer_of(i as u32)];
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
                    let keep = tile.commands.partition_point(|&c| c < layer_start);
                    tile.commands.truncate(keep);
                    tile.optional.truncate(keep);
                }
                tile.commands.push(i as u32);
                tile.optional.push(optional);
            }
        }
    }
    tiles
}

/// The textures a layer draws with: the caller's, and the blurred images of its blurs.
struct Layered<'a> {
    textures: &'a dyn Textures,
    plan: &'a Plan,
    images: &'a [Option<Rgba8>],
}

impl Textures for Layered<'_> {
    fn glyph(&self, page: u32, x: u32, y: u32) -> f32 {
        self.textures.glyph(page, x, y)
    }

    fn sample(&self, view: u32, sampler: u32, uv: [f32; 2], lod: f32) -> [f32; 4] {
        self.textures.sample(view, sampler, uv, lod)
    }

    fn backdrop(&self, job: u32, x: f32, y: f32) -> [f32; 4] {
        match self.images.get(job as usize) {
            Some(Some(image)) => {
                let [tx, ty] = blur::texel_of(&self.plan.jobs[job as usize], x, y);
                image.bilinear(tx, ty)
            }
            _ => [0.0; 4],
        }
    }
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
    let plan = blur::plan(chunk, glyphs, width, height);
    let tiles = bin(chunk, glyphs, width, height);
    let (tx, _) = tile_counts(width, height);
    let scene = Scene::new(chunk, glyphs);
    let commands = chunk.commands.len() as u32;
    let mut out = Rgba8 {
        width,
        height,
        texels: vec![0u8; (width * height * 4) as usize],
    };
    for l in 0..plan.starts.len() {
        let (first, end) = plan.range(l, commands);
        let images: Vec<Option<Rgba8>> = plan
            .jobs
            .iter()
            .map(|j| (j.layer as usize == l && j.size[0] > 0).then(|| blur::blur(&out, j)))
            .collect();
        let layered = Layered {
            textures,
            plan: &plan,
            images: &images,
        };
        let below = out.clone();
        for y in 0..height {
            for x in 0..width {
                let tile = &tiles[((y / TILE) * tx + x / TILE) as usize];
                let mut color = if l == 0 {
                    clear
                } else {
                    below.load(x as i32, y as i32)
                };
                for &i in tile.commands.iter().filter(|&&i| first <= i && i < end) {
                    let c = &chunk.commands[i as usize];
                    color = composite(c, &scene, &layered, x as f32 + 0.5, y as f32 + 0.5, color);
                }
                let at = ((y * width + x) * 4) as usize;
                out.texels[at..at + 4].copy_from_slice(&blur::quantize(color));
            }
        }
    }
    out.texels
}
