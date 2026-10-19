//! Presampled light tiles (0010 §2.8; R3b): each frame, [`LIGHT_TILES`] tiles of
//! [`LIGHT_TILE_SIZE`] lights drawn from the scene's point and spot lights in proportion to
//! their power (luminous flux), with each draw's probability. ReSTIR's initial sampling picks a
//! tile per 8×8 screen tile and its candidates from it: its cost does not grow with the lights,
//! and neighboring pixels read the same few.
//!
//! The draws come from an alias table (Vose) the CPU builds every frame from the scene's lights:
//! O(lights), microseconds for thousands. A GPU-built power pyramid can replace it when scenes
//! reach hundreds of thousands of lights. The sun is not in the tiles (VSM, 0010 §5.3).

use alloc::vec::Vec;

use marlin_render_graph::{
    Access, BufferDesc as GraphBufferDesc, BufferHandle, PassContext, RenderGraph,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::bind::EntryDesc;

use crate::scene::{LightDesc, LightKind, RenderScene};

/// Tiles a frame draws.
pub const LIGHT_TILES: u32 = 128;
/// Lights a tile holds.
pub const LIGHT_TILE_SIZE: u32 = 1024;

/// `Marlin.Engine.Lighting.AliasEntry`.
#[repr(C)]
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
pub struct AliasEntry {
    /// Keep this candidate below it (a uniform draw in [0, 1)), else take `alias`.
    pub threshold: f32,
    pub alias: u32,
    /// The light's row in the light table, and its probability of being drawn.
    pub row: u32,
    pub pdf: f32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct PresampleArgs {
    table: u32,
    tiles: u32,
    candidates: u32,
    entries: u32,
    seed: u32,
}

/// A light's power, the weight it is drawn by: luminous flux in lumens (4π times its candela),
/// by the luminance of its color. 0 for directional lights.
pub fn light_power(light: &LightDesc) -> f32 {
    if light.kind == LightKind::Directional {
        return 0.0;
    }
    let [r, g, b] = light.radiance;
    let candela = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (4.0 * core::f32::consts::PI * candela).max(0.0)
}

/// The alias table of `lights` (row, weight), weights above 0: one entry per light, each with
/// its probability `weight / Σ weights`. Empty if no weight is above 0.
pub fn alias_table(lights: &[(u32, f32)]) -> Vec<AliasEntry> {
    let lights: Vec<(u32, f32)> = lights
        .iter()
        .copied()
        .filter(|&(_, w)| w > 0.0 && w.is_finite())
        .collect();
    let n = lights.len();
    let total: f64 = lights.iter().map(|&(_, w)| f64::from(w)).sum();
    if n == 0 || total <= 0.0 {
        return Vec::new();
    }
    let mut out: Vec<AliasEntry> = lights
        .iter()
        .enumerate()
        .map(|(i, &(row, w))| AliasEntry {
            threshold: 1.0,
            alias: i as u32,
            row,
            pdf: (f64::from(w) / total) as f32,
        })
        .collect();
    // Each candidate's share of a uniform pick, scaled so 1 is average.
    let mut scaled: Vec<f64> = lights
        .iter()
        .map(|&(_, w)| f64::from(w) * n as f64 / total)
        .collect();
    let (mut small, mut large): (Vec<usize>, Vec<usize>) = (0..n).partition(|&i| scaled[i] < 1.0);
    while let (Some(&s), Some(&l)) = (small.last(), large.last()) {
        small.pop();
        out[s].threshold = scaled[s] as f32;
        out[s].alias = l as u32;
        scaled[l] -= 1.0 - scaled[s];
        if scaled[l] < 1.0 {
            large.pop();
            small.push(l);
        }
    }
    // What rounding leaves: full candidates.
    for i in small.into_iter().chain(large) {
        out[i].threshold = 1.0;
    }
    out
}

/// What [`LightTiles::add_passes`] draws: [`LIGHT_TILES`] × [`LIGHT_TILE_SIZE`] entries of
/// (light row, probability as f32 bits), tile after tile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LightTileOutput<'g> {
    pub tiles: BufferHandle<'g>,
    /// Lights the tiles were drawn from.
    pub lights: u32,
}

/// The light tile pass (see the module docs).
pub struct LightTiles {
    presample: ComputePipeline,
}

impl LightTiles {
    /// The presampling entry.
    pub const ENTRY: EntryDesc =
        EntryDesc::compute("Marlin.Engine.Lighting.PresampleLights").push::<PresampleArgs>();

    pub fn new(device: &Device, presample: ShaderModule<'_>) -> Result<Self> {
        Ok(Self {
            presample: device.create_compute_pipeline(&ComputePipelineDesc {
                shader: presample,
                cache: None,
            })?,
        })
    }

    /// Declares the frame's tiles, drawn from `scene`'s lights with `seed` (a new one each
    /// frame); `None` if the scene has no point or spot light with power.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: &RenderScene,
        seed: u32,
    ) -> Option<LightTileOutput<'g>> {
        let weights: Vec<(u32, f32)> = scene
            .lights()
            .map(|(row, l)| (row, light_power(&l)))
            .collect();
        let table = alias_table(&weights);
        if table.is_empty() {
            return None;
        }
        let candidates = table.len() as u32;
        let entries = LIGHT_TILES * LIGHT_TILE_SIZE;
        let tiles = g.create_buffer(
            "LightTiles",
            GraphBufferDesc::structured::<[u32; 2]>(u64::from(entries)),
        );
        let pipeline = &self.presample;
        let tiles = g.add_compute("PresampleLights", |b| {
            let tiles = b.write_buffer(tiles, Access::ComputeWrite);
            (tiles, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                let table = ctx.upload_slice(&table);
                ctx.push(&PresampleArgs {
                    table,
                    tiles: ctx.uav(tiles),
                    candidates,
                    entries,
                    seed,
                });
                ctx.dispatch(entries.div_ceil(64), 1, 1);
            })
        });
        Some(LightTileOutput {
            tiles,
            lights: candidates,
        })
    }
}
