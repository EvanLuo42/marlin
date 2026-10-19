//! ReSTIR DI (0010 §2, R3c): local lights over the material buffer's proxy lobe, the reservoir
//! owning `Dnee` (0010 §2.9). The passes are `Marlin.Engine.Lighting`'s (`Lighting/Restir.mshader`,
//! whose file comment describes them): initial sampling from the presampled light tiles
//! ([`crate::lights`]) with a visibility test of the sample kept, temporal reuse of last frame's
//! reservoirs (a history resource per view, the confidence capped by a duplication map), then
//! shading with one shadow ray against the TLAS ([`crate::raytracing`]), added to the HDR scene.
//!
//! Not yet: spatial reuse (R3c3), reduced-resolution reservoirs (0010 §3), specular (the final
//! shading, 0010 §2.6).

use alloc::vec::Vec;

use marlin_math::{Mat4, Transform};
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassContext, RenderGraph, TextureHandle,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::lights::{LIGHT_TILE_SIZE, LIGHT_TILES, LightTileOutput};
use crate::meshes::Camera;
use crate::raytracing::RtScene;

/// ReSTIR DI's shaders (`Marlin.Engine.Lighting`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RestirShaders<'a> {
    pub initial: ShaderModule<'a>,
    pub temporal: ShaderModule<'a>,
    pub shade: ShaderModule<'a>,
    pub duplication: ShaderModule<'a>,
}

impl<'a> RestirShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 4] = [
        EntryDesc::compute("Marlin.Engine.Lighting.RestirInitial").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirTemporal").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirShade").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirDuplication").push::<RestirArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([initial, temporal, shade, duplication]: [ShaderModule<'a>; 4]) -> Self {
        Self {
            initial,
            temporal,
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
    scene: u32,
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
    temporal_pass: ComputePipeline,
    shade_pass: ComputePipeline,
    duplication_pass: ComputePipeline,
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
}

impl RestirDi {
    /// The desktop default (L2): 16 candidates, initial visibility, temporal reuse with a cap of
    /// 20 lowered by the duplication map.
    pub fn new(device: &Device, shaders: &RestirShaders<'_>) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            initial_pass: compute(shaders.initial)?,
            temporal_pass: compute(shaders.temporal)?,
            shade_pass: compute(shaders.shade)?,
            duplication_pass: compute(shaders.duplication)?,
            candidates: 16,
            initial_visibility: true,
            temporal: true,
            max_history: 20.0,
            duplication: Some(0.1),
        })
    }

    /// Declares the local lights of the view `camera` (`width` × `height`) into `scene` (the HDR
    /// scene the direct lighting wrote), drawn with `seed` (a new one each frame); returns
    /// `scene` after.
    #[allow(clippy::too_many_arguments, reason = "one view's inputs")]
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: TextureHandle<'g>,
        inputs: RestirInputs<'g>,
        (eye, camera): (Transform, Camera),
        width: u32,
        height: u32,
        seed: u32,
    ) -> TextureHandle<'g> {
        let aspect = width as f32 / height.max(1) as f32;
        let view_projection = Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near)
            * eye.to_matrix().inverse_affine();
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
        let pixels = u64::from(width) * u64::from(height);
        let reservoir_desc = BufferDesc {
            size: pixels * RESERVOIR_BYTES,
            stride: Some(RESERVOIR_BYTES as u32),
        };
        let history = g.history("DiReservoirs", view, reservoir_desc);
        let previous = history.prev.filter(|_| self.temporal);
        let duplication = self.duplication.filter(|_| self.temporal);
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
        };
        let groups = (width.div_ceil(8), height.div_ceil(8));

        let initial = g.create_buffer("DiInitial", reservoir_desc);
        let initial = g.add_compute("RestirInitial", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let tiles = b.read_buffer(tiles.tiles, Access::ComputeRead);
            rt.read(b);
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
                    tlas: rt.index,
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });

        // Last frame's duplication map, this frame's written at the end.
        let map = g.persistent("DiDuplication", view, BufferDesc::structured::<f32>(pixels));
        let mut map_handle = map.handle;
        if map.fresh {
            map_handle = g.add_compute("DiDuplicationClear", |b| {
                let m = b.write_buffer(map_handle, Access::Clear);
                (m, move |ctx: &mut PassContext<'_>| ctx.fill_buffer(m, 0))
            });
        }
        let reservoirs = g.add_compute("RestirTemporal", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let velocity = b.read_texture(velocity, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let input = b.read_buffer(initial, Access::ComputeRead);
            let previous = previous.map(|h| b.read_buffer(h, Access::ComputeRead));
            let dup = b.read_buffer(map_handle, Access::ComputeRead);
            let output = b.write_buffer(history.cur, Access::ComputeWrite);
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

        let scene = g.add_compute("RestirShade", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let input = b.read_buffer(reservoirs, Access::ComputeRead);
            rt.read(b);
            let scene = b.write_texture(scene, Access::ComputeWrite);
            (scene, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.shade_pass);
                let args = RestirArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    lights: ctx.srv(lights),
                    input: ctx.srv(input),
                    tlas: rt.index,
                    scene: ctx.uav(scene),
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });

        if duplication.is_some() {
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
                    ctx.dispatch(width.div_ceil(16), height.div_ceil(16), 1);
                })
            });
        }
        scene
    }
}
