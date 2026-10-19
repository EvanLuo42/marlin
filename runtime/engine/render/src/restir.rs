//! ReSTIR DI (0010 §2, R3c): local lights over the material buffer's proxy lobe, the reservoir
//! owning `Dnee` (0010 §2.9). The passes are `Marlin.Engine.Lighting`'s (`Lighting/Restir.mshader`,
//! whose file comment describes them): initial sampling from the presampled light tiles
//! ([`crate::lights`]) with a visibility test of the sample kept, temporal reuse of last frame's
//! reservoirs (a history resource per view, the confidence capped by a duplication map), paired
//! spatial reuse between pixels two by two (pairing patterns from the cooker,
//! [`crate::asset::pairing`]), then shading with one shadow ray against the TLAS
//! ([`crate::raytracing`]), into a noisy signal for the denoiser ([`crate::denoise`]) with its
//! hints: the reservoirs' fresh share and a correlation radius from the duplication map.
//!
//! The reservoirs are a checkerboard of half the pixels by default (0010 §3, L2; `checkerboard`):
//! every reservoir pass runs on the cells, and the shading resamples a pixel without one from its
//! four neighbors.
//!
//! Not yet: specular (the final shading, 0010 §2.6).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_math::Transform;
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureDesc as GraphTextureDesc,
    TextureHandle,
};
use marlin_rhi::{
    Buffer, ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule, ShaderView,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::asset::PairingPatternView;
use crate::denoise::{HINTS_FORMAT, NoisySignal, SIGNAL_FORMAT};
use crate::lights::{LIGHT_TILE_SIZE, LIGHT_TILES, LightTileOutput};
use crate::meshes::{Camera, copy_to_new_buffer};
use crate::raytracing::RtScene;

/// ReSTIR DI's shaders (`Marlin.Engine.Lighting`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RestirShaders<'a> {
    pub initial: ShaderModule<'a>,
    pub initial_visibility: ShaderModule<'a>,
    pub temporal: ShaderModule<'a>,
    pub spatial_shift: ShaderModule<'a>,
    pub spatial: ShaderModule<'a>,
    pub spatial_biased: ShaderModule<'a>,
    pub shade: ShaderModule<'a>,
    pub duplication: ShaderModule<'a>,
}

impl<'a> RestirShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 8] = [
        EntryDesc::compute("Marlin.Engine.Lighting.RestirInitial").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirInitialVisibility").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirTemporal").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirSpatialShift").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirSpatial").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirSpatialBiased").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirShade").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirDuplication").push::<RestirArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules(
        [
            initial,
            initial_visibility,
            temporal,
            spatial_shift,
            spatial,
            spatial_biased,
            shade,
            duplication,
        ]: [ShaderModule<'a>; 8],
    ) -> Self {
        Self {
            initial,
            initial_visibility,
            temporal,
            spatial_shift,
            spatial,
            spatial_biased,
            shade,
            duplication,
        }
    }

    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// `Marlin.Engine.Lighting.RestirConstants`.
#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct Constants {
    inverse_view_projection: [f32; 16],
    eye: [f32; 4],
    width: u32,
    height: u32,
    candidates: u32,
    seed: u32,
    tile_count: u32,
    tile_size: u32,
    initial_visibility: u32,
    has_history: u32,
    max_history: f32,
    duplication: u32,
    duplication_power: f32,
    changed_words: u32,
    neighbors: u32,
    spatial_visibility: u32,
    pairing_offset: [u32; 2],
    pairing_flags: u32,
    duplication_ready: u32,
    checkerboard: u32,
    parity: u32,
    previous_parity: u32,
    grid_width: u32,
    spare: [u32; 2],
}

/// `Marlin.Engine.Lighting.RestirArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct RestirArgs {
    constants: u32,
    material: u32,
    depth: u32,
    velocity: u32,
    lights: u32,
    tiles: u32,
    input: u32,
    output: u32,
    previous: u32,
    duplication_map: u32,
    changed_lights: u32,
    tlas: u32,
    pairing: u32,
    shifts_out: u32,
    shifts: u32,
    signal: u32,
    hints: u32,
}

/// The pairing patterns as `RestirArgs::Pairing` reads them: the sides (4 words, 0 past the
/// last), where each pattern's offsets start (4 words, in offsets), then the offsets, two to a
/// word.
fn pairing_words(pairing: PairingPatternView<'_>) -> Vec<u8> {
    let mut header = [0u32; 8];
    let mut start = 0;
    for (i, &size) in pairing.sizes.iter().enumerate() {
        header[i] = size;
        header[4 + i] = start;
        start += size * size;
    }
    let mut bytes: Vec<u8> = header.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.extend(pairing.offsets.iter().flat_map(|o| o.to_le_bytes()));
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

/// This frame's placement of the pairing patterns (`PairingOffset`, `PairingFlags`): a
/// translation and a flip or transpose, from `seed`, so no pixel keeps its partners (Falcor's
/// `PairedNeighborTransform`).
fn pairing_placement(seed: u32) -> ([u32; 2], u32) {
    // SplitMix32-style mixing: the frame seeds are consecutive.
    let mut h = seed.wrapping_add(0x9E37_79B9);
    h = (h ^ (h >> 16)).wrapping_mul(0x21F0_AAAD);
    h = (h ^ (h >> 15)).wrapping_mul(0x735A_2D97);
    h ^= h >> 15;
    ([h & 0x7FFF, (h >> 15) & 0x3FFF], h >> 29)
}

/// Bytes of a pixel's reservoir (`DiReservoir`).
pub const RESERVOIR_BYTES: u64 = 32;

/// What [`RestirDi::add_passes`] reads of the frame.
#[derive(Clone, Copy)]
pub struct RestirInputs<'g> {
    /// Keys the view's history (its reservoirs last frame).
    pub view: u64,
    /// The material buffer (`marlin_material`'s `MATERIAL_FORMAT`).
    pub material: TextureHandle<'g>,
    /// The camera's depth (`R32Float`, reverse-Z).
    pub depth: TextureHandle<'g>,
    /// The material passes' motion vectors (`marlin_material`'s `VELOCITY_FORMAT`).
    pub velocity: TextureHandle<'g>,
    /// The GPU Scene's light table.
    pub lights: BufferHandle<'g>,
    pub rt: RtScene<'g>,
    pub tiles: LightTileOutput<'g>,
    /// The light rows added, changed or removed this frame (`RenderScene::light_changes`).
    pub changed_lights: &'g [u32],
}

/// ReSTIR DI's pipelines and settings (see the module docs).
pub struct RestirDi {
    initial_pass: ComputePipeline,
    initial_visibility_pass: ComputePipeline,
    temporal_pass: ComputePipeline,
    shade_pass: ComputePipeline,
    duplication_pass: ComputePipeline,
    spatial_shift_pass: ComputePipeline,
    spatial_pass: ComputePipeline,
    spatial_biased_pass: ComputePipeline,
    /// The pairing patterns, as `Pairing` reads them; how many there are.
    _pairing: Buffer,
    pairing_view: ShaderView,
    patterns: u32,
    /// Lights each pixel draws from its tile (0010 §3: 4, 8, 16 and 32 from L0 to L3).
    pub candidates: u32,
    /// Test the visibility of the initial sample kept, so reuse spreads only visible lights.
    pub initial_visibility: bool,
    /// Reuse last frame's reservoirs (0010 §2.4).
    pub temporal: bool,
    /// The most confidence a reservoir takes from last frame (0010 §3: 10, 15, 20, 20).
    pub max_history: f32,
    /// Lower that cap where a sample has spread (PTE §5): `lerp(max_history, 1, d^power)`, d the
    /// pixel's duplication map; the power 0.1.
    pub duplication: Option<f32>,
    /// Paired spatial neighbors (0010 §3: 1, 1, 2, 3 from L0 to L3), at most one per pairing
    /// pattern; 0 turns spatial reuse off.
    pub spatial_neighbors: u32,
    /// A shadow ray for each sample shifted to a neighbor (0010 §2.6: L2 and up); without, the
    /// final shading's ray alone (biased, cheaper).
    pub spatial_visibility: bool,
    /// The biased spatial reuse (Bitterli et al. 2020): confidence-weighted (1/M) instead of
    /// pairwise MIS, no shift pass and no shadow rays for the partners' samples. Cheaper; brighter
    /// or darker than right at shadow edges.
    pub biased_spatial: bool,
    /// Reservoirs for half the pixels, a checkerboard whose parity alternates a frame (0010 §3,
    /// L2); the shading resamples the others from their neighbors (§2.6). Off: a reservoir a
    /// pixel (L3).
    pub checkerboard: bool,
    /// Last frame's checkerboard parity (2 before the first frame), for the temporal pass.
    last_parity: AtomicU32,
}

impl RestirDi {
    /// The desktop default (L2): 16 candidates, initial visibility, temporal reuse with a cap of
    /// 20 lowered by the duplication map, two paired spatial neighbors with visibility.
    /// `pairing`: the pairing patterns (the engine's `Marlin.Engine.Lighting.PairingPattern`).
    pub fn new(
        device: &Device,
        shaders: &RestirShaders<'_>,
        pairing: PairingPatternView<'_>,
    ) -> Result<Self> {
        let (pairing_buffer, pairing_view) = copy_to_new_buffer(device, &pairing_words(pairing))?;
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            initial_pass: compute(shaders.initial)?,
            initial_visibility_pass: compute(shaders.initial_visibility)?,
            temporal_pass: compute(shaders.temporal)?,
            shade_pass: compute(shaders.shade)?,
            duplication_pass: compute(shaders.duplication)?,
            spatial_shift_pass: compute(shaders.spatial_shift)?,
            spatial_pass: compute(shaders.spatial)?,
            spatial_biased_pass: compute(shaders.spatial_biased)?,
            _pairing: pairing_buffer,
            pairing_view,
            patterns: pairing.sizes.len() as u32,
            candidates: 16,
            initial_visibility: true,
            temporal: true,
            max_history: 20.0,
            duplication: Some(0.1),
            spatial_neighbors: 2,
            spatial_visibility: true,
            biased_spatial: false,
            checkerboard: true,
            last_parity: AtomicU32::new(2),
        })
    }

    /// Declares the local lights of the view `camera` (`width` × `height`), drawn with `seed` (a
    /// new one each frame); returns their light as a noisy signal for the denoiser
    /// ([`crate::denoise`]), its hints from the reservoirs, or to compose as it is.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        inputs: RestirInputs<'g>,
        (eye, camera): (Transform, Camera),
        width: u32,
        height: u32,
        seed: u32,
    ) -> NoisySignal<'g> {
        let view_projection = crate::view_projection((eye, camera), width, height);
        let p = eye.translation;
        let RestirInputs {
            view,
            material,
            depth,
            velocity,
            lights,
            rt,
            tiles,
            changed_lights,
        } = inputs;
        // The changed rows as a bitset.
        let mut changed: Vec<u32> = Vec::new();
        for &row in changed_lights {
            let word = (row / 32) as usize;
            if changed.len() <= word {
                changed.resize(word + 1, 0);
            }
            changed[word] |= 1 << (row % 32);
        }
        let changed_words = changed.len() as u32;
        if changed.is_empty() {
            changed.push(0);
        }
        // The reservoirs: a cell a pixel, or half the pixels in a checkerboard of alternating parity.
        let grid_width = if self.checkerboard {
            width.div_ceil(2)
        } else {
            width
        };
        let parity = 0;
        let previous_parity = match self.last_parity.swap(parity, Ordering::Relaxed) {
            p @ (0 | 1) if self.checkerboard => p,
            _ => parity ^ u32::from(self.checkerboard),
        };
        let cells = u64::from(grid_width) * u64::from(height);
        let reservoir_desc = BufferDesc {
            size: cells * RESERVOIR_BYTES,
            stride: Some(RESERVOIR_BYTES as u32),
        };
        let history = g.history("DiReservoirs", view, reservoir_desc);
        let previous = history.prev.filter(|_| self.temporal);
        let duplication = self.duplication.filter(|_| self.temporal);
        let neighbors = self.spatial_neighbors.min(self.patterns);
        let (pairing_offset, pairing_flags) = pairing_placement(seed);
        let constants = Constants {
            inverse_view_projection: view_projection.inverse().to_cols_array(),
            eye: [p.x, p.y, p.z, 0.0],
            width,
            height,
            candidates: self.candidates.max(1),
            seed,
            tile_count: LIGHT_TILES,
            tile_size: LIGHT_TILE_SIZE,
            initial_visibility: u32::from(self.initial_visibility),
            has_history: u32::from(previous.is_some()),
            max_history: self.max_history.max(1.0),
            duplication: u32::from(duplication.is_some()),
            duplication_power: duplication.unwrap_or(1.0),
            changed_words,
            neighbors,
            spatial_visibility: u32::from(self.spatial_visibility),
            pairing_offset,
            pairing_flags,
            duplication_ready: 0,
            checkerboard: u32::from(self.checkerboard),
            parity,
            previous_parity,
            grid_width,
            spare: [0; 2],
        };
        // The reservoir passes run on the cells; the shading on the pixels.
        let groups = (grid_width.div_ceil(8), height.div_ceil(8));
        let pixel_groups = (width.div_ceil(8), height.div_ceil(8));

        let initial = g.create_buffer("DiInitial", reservoir_desc);
        let initial = g.add_compute("RestirInitial", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let tiles = b.read_buffer(tiles.tiles, Access::ComputeRead);
            let output = b.write_buffer(initial, Access::ComputeWrite);
            (output, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.initial_pass);
                let args = RestirArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    lights: ctx.srv(lights),
                    tiles: ctx.srv(tiles),
                    output: ctx.uav(output),
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });
        // The kept sample's shadow ray, a pass of its own (see `RestirInitialVisibility`).
        let initial = if self.initial_visibility {
            g.add_compute("RestirInitialVisibility", |b| {
                let material = b.read_texture(material, Access::ComputeRead);
                let depth = b.read_texture(depth, Access::ComputeRead);
                let lights = b.read_buffer(lights, Access::ComputeRead);
                rt.read(b);
                let output = b.write_buffer(initial, Access::ComputeWrite);
                (output, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(&self.initial_visibility_pass);
                    let args = RestirArgs {
                        constants: ctx.upload(&constants),
                        material: ctx.srv(material),
                        depth: ctx.srv(depth),
                        lights: ctx.srv(lights),
                        output: ctx.uav(output),
                        tlas: rt.index,
                        ..RestirArgs::default()
                    };
                    ctx.push(&args);
                    ctx.dispatch(groups.0, groups.1, 1);
                })
            })
        } else {
            initial
        };

        // Last frame's duplication map, this frame's written at the end.
        let map = g.persistent("DiDuplication", view, BufferDesc::structured::<f32>(cells));
        let mut map_handle = map.handle;
        if map.fresh {
            map_handle = g.add_compute("DiDuplicationClear", |b| {
                let m = b.write_buffer(map_handle, Access::Clear);
                (m, move |ctx: &mut PassContext<'_>| ctx.fill_buffer(m, 0))
            });
        }
        // Into the history, unless spatial reuse follows: then its output is.
        let temporal_out = if neighbors > 0 {
            g.create_buffer("DiTemporal", reservoir_desc)
        } else {
            history.cur
        };
        let reservoirs = g.add_compute("RestirTemporal", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let velocity = b.read_texture(velocity, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let input = b.read_buffer(initial, Access::ComputeRead);
            let previous = previous.map(|h| b.read_buffer(h, Access::ComputeRead));
            let dup = b.read_buffer(map_handle, Access::ComputeRead);
            let output = b.write_buffer(temporal_out, Access::ComputeWrite);
            (output, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.temporal_pass);
                let args = RestirArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    velocity: ctx.srv(velocity),
                    lights: ctx.srv(lights),
                    input: ctx.srv(input),
                    // Without history the pass copies: its previous reservoirs go unread.
                    previous: previous.map_or_else(|| ctx.srv(input), |h| ctx.srv(h)),
                    duplication_map: ctx.srv(dup),
                    changed_lights: ctx.upload_slice(&changed),
                    output: ctx.uav(output),
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });

        // Paired spatial reuse (0010 §2.5): both shifts of every pair, then the resampling.
        let reservoirs = if neighbors > 0 && self.biased_spatial {
            // One pass: the partners' samples weighed here, by confidence (`RestirSpatialBiased`).
            let pairing = self.pairing_view.index();
            g.add_compute("RestirSpatial", |b| {
                let material = b.read_texture(material, Access::ComputeRead);
                let depth = b.read_texture(depth, Access::ComputeRead);
                let lights = b.read_buffer(lights, Access::ComputeRead);
                let input = b.read_buffer(reservoirs, Access::ComputeRead);
                let output = b.write_buffer(history.cur, Access::ComputeWrite);
                (output, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(&self.spatial_biased_pass);
                    let args = RestirArgs {
                        constants: ctx.upload(&constants),
                        material: ctx.srv(material),
                        depth: ctx.srv(depth),
                        lights: ctx.srv(lights),
                        input: ctx.srv(input),
                        pairing,
                        output: ctx.uav(output),
                        ..RestirArgs::default()
                    };
                    ctx.push(&args);
                    ctx.dispatch(groups.0, groups.1, 1);
                })
            })
        } else if neighbors > 0 {
            let shifts = g.create_buffer(
                "DiShifts",
                BufferDesc::structured::<f32>(cells * u64::from(neighbors)),
            );
            let pairing = self.pairing_view.index();
            let shifts = g.add_compute("RestirSpatialShift", |b| {
                let material = b.read_texture(material, Access::ComputeRead);
                let depth = b.read_texture(depth, Access::ComputeRead);
                let lights = b.read_buffer(lights, Access::ComputeRead);
                let input = b.read_buffer(reservoirs, Access::ComputeRead);
                rt.read(b);
                let shifts = b.write_buffer(shifts, Access::ComputeWrite);
                (shifts, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(&self.spatial_shift_pass);
                    let args = RestirArgs {
                        constants: ctx.upload(&constants),
                        material: ctx.srv(material),
                        depth: ctx.srv(depth),
                        lights: ctx.srv(lights),
                        input: ctx.srv(input),
                        tlas: rt.index,
                        pairing,
                        shifts_out: ctx.uav(shifts),
                        ..RestirArgs::default()
                    };
                    ctx.push(&args);
                    ctx.dispatch(groups.0, groups.1, 1);
                })
            });
            g.add_compute("RestirSpatial", |b| {
                let input = b.read_buffer(reservoirs, Access::ComputeRead);
                let shifts = b.read_buffer(shifts, Access::ComputeRead);
                let output = b.write_buffer(history.cur, Access::ComputeWrite);
                (output, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(&self.spatial_pass);
                    let args = RestirArgs {
                        constants: ctx.upload(&constants),
                        input: ctx.srv(input),
                        pairing,
                        shifts: ctx.srv(shifts),
                        output: ctx.uav(output),
                        ..RestirArgs::default()
                    };
                    ctx.push(&args);
                    ctx.dispatch(groups.0, groups.1, 1);
                })
            })
        } else {
            reservoirs
        };

        // This frame's duplication map, before the shading reads it for the hints.
        let map_ready = self.temporal || neighbors > 0;
        let map_handle = if map_ready {
            g.add_compute("RestirDuplication", |b| {
                let input = b.read_buffer(reservoirs, Access::ComputeRead);
                let map = b.write_buffer(map_handle, Access::ComputeWrite);
                (map, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(&self.duplication_pass);
                    let args = RestirArgs {
                        constants: ctx.upload(&constants),
                        input: ctx.srv(input),
                        duplication_map: ctx.uav(map),
                        ..RestirArgs::default()
                    };
                    ctx.push(&args);
                    ctx.dispatch(grid_width.div_ceil(16), height.div_ceil(16), 1);
                })
            })
        } else {
            map_handle
        };
        let shade = Constants {
            duplication_ready: u32::from(map_ready),
            ..constants
        };

        let light = g.create_texture(
            "DiSignal",
            GraphTextureDesc::new_2d(width, height, SIGNAL_FORMAT),
        );
        let hints = g.create_texture(
            "DiHints",
            GraphTextureDesc::new_2d(width, height, HINTS_FORMAT),
        );
        let (light, hints) = g.add_compute("RestirShade", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let input = b.read_buffer(reservoirs, Access::ComputeRead);
            let map = b.read_buffer(map_handle, Access::ComputeRead);
            rt.read(b);
            let light = b.write_texture(light, Access::ComputeWrite);
            let hints = b.write_texture(hints, Access::ComputeWrite);
            ((light, hints), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.shade_pass);
                let args = RestirArgs {
                    constants: ctx.upload(&shade),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    lights: ctx.srv(lights),
                    input: ctx.srv(input),
                    duplication_map: ctx.srv(map),
                    tlas: rt.index,
                    signal: ctx.uav(light),
                    hints: ctx.uav(hints),
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(pixel_groups.0, pixel_groups.1, 1);
            })
        });
        NoisySignal { light, hints }
    }
}
