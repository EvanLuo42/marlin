//! The clustered light grid (0010 §5.4, R4e2): which local lights reach each cell of a grid
//! aligned with a view's frustum, for the paths that light without reservoirs (translucent
//! surfaces, 0009 §8; later fog, particles, hair). `Lighting/LightGrid.mshader` builds it
//! (`BuildLightGrid`, a thread a cell against every light: thousands of lights stay
//! milliseconds below the frame, as the cells are few) and reads it (`GridLights`).

use marlin_math::Transform;
use marlin_render_graph::{
    Access, BufferDesc, BufferHandle, PassBuilder, PassContext, RenderGraph,
};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::bind::EntryDesc;

use crate::meshes::Camera;
use crate::scene::RenderScene;

/// `Marlin.Engine.Lighting.LightGridConstants`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
pub struct LightGridConstants {
    view: [f32; 16],
    grid: [u32; 3],
    cell_lights: u32,
    near: f32,
    far: f32,
    tan_y: f32,
    tan_x: f32,
    light_rows: u32,
    spare: [u32; 3],
}

/// `Marlin.Engine.Lighting.LightGridSources`: the grid in a reading pass's arguments.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LightGridSources {
    pub lights: u32,
    pub counts: u32,
    pub indices: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct LightGridArgs {
    constants: u32,
    lights: u32,
    counts: u32,
    indices: u32,
}

/// The frame's grid, for the passes that read it.
#[derive(Clone, Copy)]
pub struct LightGridOutput<'g> {
    pub lights: BufferHandle<'g>,
    pub counts: BufferHandle<'g>,
    pub indices: BufferHandle<'g>,
    /// What a reading pass's uniform holds (`LightGridConstants`).
    pub constants: LightGridConstants,
}

/// A pass's views of the grid ([`LightGridOutput::read`]).
#[derive(Clone, Copy)]
pub struct LightGridViews<'g> {
    lights: BufferHandle<'g>,
    counts: BufferHandle<'g>,
    indices: BufferHandle<'g>,
}

impl<'g> LightGridOutput<'g> {
    /// Declares a pass's reads of it, in pixel shaders.
    pub fn read(&self, b: &mut PassBuilder<'_, 'g>) -> LightGridViews<'g> {
        LightGridViews {
            lights: b.read_buffer(self.lights, Access::PixelRead),
            counts: b.read_buffer(self.counts, Access::PixelRead),
            indices: b.read_buffer(self.indices, Access::PixelRead),
        }
    }
}

impl LightGridViews<'_> {
    pub fn bind(&self, ctx: &PassContext<'_>) -> LightGridSources {
        LightGridSources {
            lights: ctx.srv(self.lights),
            counts: ctx.srv(self.counts),
            indices: ctx.srv(self.indices),
        }
    }
}

/// The grid's pipeline and shape.
pub struct LightGrid {
    build: ComputePipeline,
    /// Tiles across and down the screen, and depth slices.
    pub cells: [u32; 3],
    /// Lights a cell lists at most; more are dropped (the nearest are not chosen).
    pub cell_lights: u32,
    /// The slices' depth range in meters, exponential from `near` to `far`; beyond `far` the
    /// last slice.
    pub near: f32,
    pub far: f32,
}

impl LightGrid {
    pub const ENTRY: EntryDesc =
        EntryDesc::compute("Marlin.Engine.Lighting.BuildLightGrid").push::<LightGridArgs>();

    /// 16 × 9 tiles by 24 slices from 0.1 to 500 m, 64 lights a cell.
    pub fn new(device: &Device, shader: ShaderModule<'_>) -> Result<Self> {
        Ok(Self {
            build: device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })?,
            cells: [16, 9, 24],
            cell_lights: 64,
            near: 0.1,
            far: 500.0,
        })
    }

    /// Declares the grid of `camera`'s view (`width` × `height`) over `scene`'s lights, the GPU
    /// Scene's light table `lights`.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        scene: &RenderScene,
        lights: BufferHandle<'g>,
        (eye, camera): (Transform, Camera),
        width: u32,
        height: u32,
    ) -> LightGridOutput<'g> {
        let [x, y, z] = self.cells.map(|c| c.max(1));
        let tan_y = marlin_math::scalar::tan(camera.fov_y * 0.5);
        let constants = LightGridConstants {
            view: eye.to_matrix().inverse().to_cols_array(),
            grid: [x, y, z],
            cell_lights: self.cell_lights.max(1),
            near: self.near.max(1e-3),
            far: self.far.max(self.near * 2.0),
            tan_y,
            tan_x: tan_y * width as f32 / height.max(1) as f32,
            light_rows: scene.light_rows(),
            spare: [0; 3],
        };
        let cells = u64::from(x * y * z);
        let counts = g.create_buffer("LightGridCounts", BufferDesc::structured::<u32>(cells));
        let indices = g.create_buffer(
            "LightGridIndices",
            BufferDesc::structured::<u32>(cells * u64::from(constants.cell_lights)),
        );
        let (counts, indices) = g.add_compute("BuildLightGrid", |b| {
            let lights = b.read_buffer(lights, Access::ComputeRead);
            let counts = b.write_buffer(counts, Access::ComputeWrite);
            let indices = b.write_buffer(indices, Access::ComputeWrite);
            ((counts, indices), move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.build);
                let constants = ctx.upload(&constants);
                ctx.push(&LightGridArgs {
                    constants,
                    lights: ctx.srv(lights),
                    counts: ctx.uav(counts),
                    indices: ctx.uav(indices),
                });
                ctx.dispatch((cells as u32).div_ceil(64), 1, 1);
            })
        });
        LightGridOutput {
            lights,
            counts,
            indices,
            constants,
        }
    }
}
