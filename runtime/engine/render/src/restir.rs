//! ReSTIR DI (0010 §2, R3c): local lights over the material buffer's proxy lobe, the reservoir
//! owning `Dnee` (0010 §2.9). The passes are `Marlin.Engine.Lighting`'s (`Lighting/Restir.mshader`,
//! whose file comment describes them): initial sampling from the presampled light tiles
//! ([`crate::lights`]), then shading with one shadow ray against the TLAS ([`crate::raytracing`]),
//! added to the HDR scene.
//!
//! Not yet: temporal and spatial reuse (R3c2, R3c3), reduced-resolution reservoirs (0010 §3),
//! specular (the final shading, 0010 §2.6).

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
    pub shade: ShaderModule<'a>,
}

impl<'a> RestirShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 2] = [
        EntryDesc::compute("Marlin.Engine.Lighting.RestirInitial").push::<RestirArgs>(),
        EntryDesc::compute("Marlin.Engine.Lighting.RestirShade").push::<RestirArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([initial, shade]: [ShaderModule<'a>; 2]) -> Self {
        Self { initial, shade }
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
    spare: [u32; 2],
}

/// `Marlin.Engine.Lighting.RestirArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct RestirArgs {
    constants: u32,
    material: u32,
    depth: u32,
    lights: u32,
    tiles: u32,
    reservoirs: u32,
    tlas: u32,
    scene: u32,
}

/// Bytes of a pixel's reservoir (`DiReservoir`).
pub const RESERVOIR_BYTES: u64 = 32;

/// What [`RestirDi::add_passes`] reads of the frame.
#[derive(Clone, Copy)]
pub struct RestirInputs<'g> {
    /// The material buffer (`marlin_material`'s `MATERIAL_FORMAT`).
    pub material: TextureHandle<'g>,
    /// The camera's depth (`R32Float`, reverse-Z).
    pub depth: TextureHandle<'g>,
    /// The GPU Scene's light table.
    pub lights: BufferHandle<'g>,
    pub rt: RtScene<'g>,
    pub tiles: LightTileOutput<'g>,
}

/// ReSTIR DI's pipelines and settings (see the module docs).
pub struct RestirDi {
    initial: ComputePipeline,
    shade: ComputePipeline,
    /// Lights each pixel draws from its tile (0010 §3: 4, 8, 16 and 32 from L0 to L3).
    pub candidates: u32,
}

impl RestirDi {
    /// The desktop default draws 16 candidates (L2).
    pub fn new(device: &Device, shaders: &RestirShaders<'_>) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            initial: compute(shaders.initial)?,
            shade: compute(shaders.shade)?,
            candidates: 16,
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
        let constants = Constants {
            inverse_view_projection: view_projection.inverse().to_cols_array(),
            eye: [p.x, p.y, p.z, 0.0],
            width,
            height,
            candidates: self.candidates.max(1),
            seed,
            tile_count: LIGHT_TILES,
            tile_size: LIGHT_TILE_SIZE,
            spare: [0; 2],
        };
        let reservoirs = g.create_buffer(
            "DiReservoirs",
            BufferDesc {
                size: u64::from(width) * u64::from(height) * RESERVOIR_BYTES,
                stride: Some(RESERVOIR_BYTES as u32),
            },
        );
        let groups = (width.div_ceil(8), height.div_ceil(8));
        let RestirInputs {
            material,
            depth,
            lights,
            rt,
            tiles,
        } = inputs;
        let reservoirs = g.add_compute("RestirInitial", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let tiles = b.read_buffer(tiles.tiles, Access::ComputeRead);
            let reservoirs = b.write_buffer(reservoirs, Access::ComputeWrite);
            (reservoirs, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.initial);
                let args = RestirArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    lights: ctx.srv(lights),
                    tiles: ctx.srv(tiles),
                    reservoirs: ctx.uav(reservoirs),
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        });
        g.add_compute("RestirShade", |b| {
            let material = b.read_texture(material, Access::ComputeRead);
            let depth = b.read_texture(depth, Access::ComputeRead);
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let reservoirs = b.read_buffer(reservoirs, Access::ComputeRead);
            rt.read(b);
            let scene = b.write_texture(scene, Access::ComputeWrite);
            (scene, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.shade);
                let args = RestirArgs {
                    constants: ctx.upload(&constants),
                    material: ctx.srv(material),
                    depth: ctx.srv(depth),
                    lights: ctx.srv(lights),
                    reservoirs: ctx.srv(reservoirs),
                    tlas: rt.index,
                    scene: ctx.uav(scene),
                    ..RestirArgs::default()
                };
                ctx.push(&args);
                ctx.dispatch(groups.0, groups.1, 1);
            })
        })
    }
}
