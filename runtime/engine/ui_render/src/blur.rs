//! Backdrop blurs (0012 §5.6): a blur reads what is drawn under it, so the frame is drawn in
//! layers. Each layer starts from the last one's pixels; between them, every blur of the next
//! layer gets its own blurred image of the region it reads.
//!
//! - **Layers**: a blur starts a new layer when what it reads (its box grown by its reach)
//!   touches something drawn since the current layer started; otherwise it joins the current
//!   one, so side-by-side panels share a layer. The first blur always starts one. Past
//!   [`MAX_LAYERS`], blurs join the last layer (and read what was drawn before it).
//! - **The blurred image** of a region: averaged down by a power of two until σ is at most
//!   [`MAX_SIGMA`] texels, then a separable Gaussian, horizontal then vertical, each step stored
//!   as RGBA8 like the GPU's textures. The blur samples it bilinearly.
//!
//! This module plans the layers (the renderer and the reference share it) and is the CPU
//! reference of the blur passes (`Marlin.Engine.Ui.Blur`).

use alloc::vec;
use alloc::vec::Vec;

use marlin_font::GlyphRecord;
use marlin_math::scalar::{ceil, exp, floor};

use crate::command::{Chunk, Kind};
use crate::shape::{Scene, bounds};

/// The most layers a frame is drawn in.
pub const MAX_LAYERS: usize = 16;
/// The most a blurred image is scaled down.
pub const MAX_SCALE: u32 = 16;
/// The largest σ, in blurred texels, a blur filters at.
pub const MAX_SIGMA: f32 = 4.0;

/// One backdrop blur's image.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct BlurJob {
    /// The blur command.
    pub command: u32,
    /// The layer it is drawn in (from 1).
    pub layer: u32,
    /// The screen pixel of the image's texel 0 (multiples of `scale`).
    pub origin: [u32; 2],
    /// The image's size in texels (0: the blur draws nowhere).
    pub size: [u32; 2],
    /// Screen pixels per texel, on each side.
    pub scale: u32,
    /// σ in texels.
    pub sigma: f32,
}

/// A frame's layers and blurs.
#[derive(Clone, PartialEq, Debug)]
pub struct Plan {
    /// Every layer's first command; the first is 0.
    pub starts: Vec<u32>,
    /// Every blur, in command order (its index is the blur's, payload word 9).
    pub jobs: Vec<BlurJob>,
}

impl Plan {
    /// Layer `l`'s commands.
    pub fn range(&self, l: usize, commands: u32) -> (u32, u32) {
        (
            self.starts[l],
            self.starts.get(l + 1).copied().unwrap_or(commands),
        )
    }

    /// The layer command `i` is in.
    pub fn layer_of(&self, i: u32) -> usize {
        self.starts.partition_point(|&s| s <= i) - 1
    }
}

fn overlaps(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The layers and blurs of `chunk` on a `width` × `height` surface.
pub fn plan(chunk: &Chunk, glyphs: &[GlyphRecord], width: u32, height: u32) -> Plan {
    let mut plan = Plan {
        starts: vec![0],
        jobs: Vec::new(),
    };
    let Some(first) = chunk
        .commands
        .iter()
        .position(|c| Kind::from_word(c[0]) == Some(Kind::Blur))
    else {
        return plan;
    };
    let scene = Scene::new(chunk, glyphs);
    // What was drawn since the current layer started, from the first blur on.
    let mut since: Vec<[i32; 4]> = Vec::new();
    for (i, c) in chunk.commands.iter().enumerate().skip(first) {
        let b = bounds(c, &scene, width, height);
        if Kind::from_word(c[0]) == Some(Kind::Blur) {
            let mut job = BlurJob {
                command: i as u32,
                scale: 1,
                ..BlurJob::default()
            };
            if !b.is_empty() {
                let radius = f32::from_bits(chunk.payload[c[3] as usize + 8]).max(0.0);
                let sigma = radius * 0.5;
                let reach = ceil(sigma * 3.0) as i32;
                let region = [
                    (b.bbox[0] - reach).max(0),
                    (b.bbox[1] - reach).max(0),
                    (b.bbox[2] + reach).min(width as i32),
                    (b.bbox[3] + reach).min(height as i32),
                ];
                let current = plan.starts.len() == 1 || since.iter().any(|&s| overlaps(s, region));
                if current && plan.starts.len() < MAX_LAYERS {
                    plan.starts.push(i as u32);
                    since.clear();
                }
                let mut scale = 1;
                while sigma / scale as f32 > MAX_SIGMA && scale < MAX_SCALE {
                    scale *= 2;
                }
                let s = scale as i32;
                let origin = [region[0] / s * s, region[1] / s * s];
                job.origin = origin.map(|v| v as u32);
                job.size = [
                    ((region[2] - origin[0]) as u32).div_ceil(scale),
                    ((region[3] - origin[1]) as u32).div_ceil(scale),
                ];
                job.scale = scale;
                job.sigma = sigma / scale as f32;
            }
            job.layer = plan.starts.len() as u32 - 1;
            plan.jobs.push(job);
        }
        if !b.is_empty() {
            since.push(b.bbox);
        }
    }
    plan
}

/// An RGBA8 image, rows packed.
#[derive(Clone, PartialEq, Debug)]
pub struct Rgba8 {
    pub width: u32,
    pub height: u32,
    pub texels: Vec<u8>,
}

impl Rgba8 {
    /// Texel `(x, y)`, clamped to the image, as 0–1 floats.
    pub fn load(&self, x: i32, y: i32) -> [f32; 4] {
        let x = x.clamp(0, self.width as i32 - 1) as u32;
        let y = y.clamp(0, self.height as i32 - 1) as u32;
        let at = ((y * self.width + x) * 4) as usize;
        core::array::from_fn(|k| self.texels[at + k] as f32 / 255.0)
    }

    /// Bilinear at texel coordinates `(tx, ty)` (texel centers at halves), clamped.
    pub fn bilinear(&self, tx: f32, ty: f32) -> [f32; 4] {
        let (x, y) = (tx - 0.5, ty - 0.5);
        let (x0, y0) = (floor(x), floor(y));
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let mut out = [0.0; 4];
        for (t, w) in [
            (self.load(x0, y0), (1.0 - fx) * (1.0 - fy)),
            (self.load(x0 + 1, y0), fx * (1.0 - fy)),
            (self.load(x0, y0 + 1), (1.0 - fx) * fy),
            (self.load(x0 + 1, y0 + 1), fx * fy),
        ] {
            for k in 0..4 {
                out[k] += t[k] * w;
            }
        }
        out
    }
}

/// A float color stored as RGBA8, as the GPU's targets round it.
pub fn quantize(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

fn image(width: u32, height: u32, f: impl Fn(i32, i32) -> [f32; 4]) -> Rgba8 {
    let mut texels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            texels.extend_from_slice(&quantize(f(x, y)));
        }
    }
    Rgba8 {
        width,
        height,
        texels,
    }
}

/// One Gaussian pass of σ `sigma` along `(dx, dy)`.
fn gaussian(src: &Rgba8, sigma: f32, dx: i32, dy: i32) -> Rgba8 {
    let n = ceil(sigma * 3.0) as i32;
    image(src.width, src.height, |x, y| {
        let mut acc = [0.0f32; 4];
        let mut total = 0.0;
        for t in -n..=n {
            let w = if sigma > 0.0 {
                exp(-((t * t) as f32) / (2.0 * sigma * sigma))
            } else {
                1.0
            };
            let s = src.load(x + t * dx, y + t * dy);
            for k in 0..4 {
                acc[k] += s[k] * w;
            }
            total += w;
        }
        acc.map(|v| v / total)
    })
}

/// `job`'s blurred image of `src` (the surface after the layers below it).
pub fn blur(src: &Rgba8, job: &BlurJob) -> Rgba8 {
    let s = job.scale as i32;
    let (ox, oy) = (job.origin[0] as i32, job.origin[1] as i32);
    let down = image(job.size[0], job.size[1], |x, y| {
        let mut acc = [0.0f32; 4];
        for j in 0..s {
            for i in 0..s {
                let t = src.load(ox + x * s + i, oy + y * s + j);
                for k in 0..4 {
                    acc[k] += t[k];
                }
            }
        }
        acc.map(|v| v / (s * s) as f32)
    });
    let h = gaussian(&down, job.sigma, 1, 0);
    gaussian(&h, job.sigma, 0, 1)
}

/// The texel coordinates of screen point `(x, y)` in `job`'s image.
pub fn texel_of(job: &BlurJob, x: f32, y: f32) -> [f32; 2] {
    let s = job.scale as f32;
    [
        (x - job.origin[0] as f32) / s,
        (y - job.origin[1] as f32) / s,
    ]
}
