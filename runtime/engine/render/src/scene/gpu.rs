//! The GPU Scene (0009 §2): the scene's tables as persistent buffers of the render graph, and
//! the one scatter pass a frame that copies the changed rows into them from upload memory.
//!
//! Rows, in 32-bit words (`Marlin.Engine.Scene`'s structs):
//!
//! | table | words | content |
//! |---|---|---|
//! | instance data | 8 | mesh row, flags, 2 spare, color (float4) |
//! | instance transforms | 24 | current, then previous: 3 rows of a 3×4 matrix each |
//! | instance bounds | 4 | world bounding sphere: center, radius |
//! | meshes | 16 | geometry view, LOD count, flags, strides; position offset (3), resident (0/1); position scale (3), UV sets; local bounding sphere (`Marlin.Engine.Scene.MeshEntry`) |
//! | lights | 16 | kind, range, cone (2); radiance + spare; position + spare; direction + spare |
//!
//! The buffers are graph persistent resources keyed by the scene: they keep their contents
//! across frames and grow by powers of two. A new or grown buffer comes back fresh, and the
//! scatter rewrites every live row of it. The transform shift (last frame's current becomes the
//! previous) happens on the GPU: a row that moved writes only its new current transform, and a
//! row that stopped moving writes nothing.

use alloc::vec::Vec;

use marlin_render_graph::{Access, BufferDesc, BufferHandle, PassContext, RenderGraph};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule};
use marlin_serial::cooked::Pod;

use super::{RenderScene, dirty};

/// Words per row of each table, in table order.
const STRIDES: [u32; 5] = [8, 24, 4, 16, 16];
const DATA: u32 = 0;
const TRANSFORMS: u32 = 1;
const BOUNDS: u32 = 2;
const MESHES: u32 = 3;
const LIGHTS: u32 = 4;

/// What a scatter op does with its row.
const WRITE: u32 = 0;
const SHIFT: u32 = 1;
const SHIFT_WRITE: u32 = 2;

/// The scatter shader (`Marlin.Engine.Scene.Scatter`) as a pipeline.
pub struct ScatterShader {
    pipeline: ComputePipeline,
}

impl ScatterShader {
    pub fn new(device: &Device, shader: ShaderModule<'_>) -> Result<Self> {
        Ok(Self {
            pipeline: device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })?,
        })
    }
}

/// The GPU Scene's tables this frame, up to date once the scatter pass ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GpuScene<'g> {
    pub instance_data: BufferHandle<'g>,
    pub instance_transforms: BufferHandle<'g>,
    pub instance_bounds: BufferHandle<'g>,
    pub meshes: BufferHandle<'g>,
    pub lights: BufferHandle<'g>,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct ScatterArgs {
    ops: u32,
    data: u32,
    tables: [u32; 5],
    count: u32,
}

/// Rows to `capacity` for `rows`: a power of two, at least 64.
fn capacity(rows: usize) -> u64 {
    (rows.max(64) as u64).next_power_of_two()
}

/// The rows of a 3×4 matrix (row-major, the last row of the affine 4×4 dropped).
fn rows_of(t: &marlin_math::Transform) -> [f32; 12] {
    let a = t.to_matrix().to_cols_array();
    let mut out = [0.0; 12];
    for r in 0..3 {
        for c in 0..4 {
            out[r * 4 + c] = a[c * 4 + r];
        }
    }
    out
}

struct Ops {
    ops: Vec<[u32; 4]>,
    data: Vec<u32>,
}

impl Ops {
    fn push(&mut self, table: u32, kind: u32, row: u32, words: &[u32]) {
        self.ops.push([
            table | (kind << 8),
            row,
            self.data.len() as u32,
            words.len() as u32,
        ]);
        self.data.extend_from_slice(words);
    }
}

fn floats(f: &[f32]) -> impl Iterator<Item = u32> + '_ {
    f.iter().map(|x| x.to_bits())
}

impl RenderScene {
    /// Declares this frame's GPU Scene: the five tables (persistent resources keyed by the
    /// scene) and, if any row changed since the last call, the scatter pass that writes them.
    /// Call it once a frame, before the passes that read the tables. Needs a graph with a state
    /// table.
    pub fn add_passes<'g>(
        &mut self,
        g: &mut RenderGraph<'g>,
        scatter: &'g ScatterShader,
    ) -> GpuScene<'g> {
        let scope = self.id;
        let table = |g: &mut RenderGraph<'g>, name: &str, rows: usize, stride: u32| {
            g.persistent(
                name,
                scope,
                BufferDesc::new(capacity(rows) * u64::from(stride) * 4),
            )
        };
        let data = table(
            g,
            "GpuScene.InstanceData",
            self.instance_rows.len(),
            STRIDES[0],
        );
        let transforms = table(
            g,
            "GpuScene.InstanceTransforms",
            self.instance_rows.len(),
            STRIDES[1],
        );
        let bounds = table(
            g,
            "GpuScene.InstanceBounds",
            self.instance_rows.len(),
            STRIDES[2],
        );
        let meshes = table(g, "GpuScene.Meshes", self.mesh_rows.len(), STRIDES[3]);
        let lights = table(g, "GpuScene.Lights", self.light_rows.len(), STRIDES[4]);

        let mut ops = Ops {
            ops: Vec::new(),
            data: Vec::new(),
        };
        // Instances: every live row when a table is new, else the changed ones.
        let fresh_instances = data.fresh || transforms.fresh || bounds.fresh;
        let mut instance_rows = self.instances_dirty.take();
        if fresh_instances {
            instance_rows = (0..self.instance_rows.len() as u32)
                .filter(|&r| self.instance_rows.alive[r as usize])
                .map(|r| (r, dirty::DATA | dirty::BOTH | dirty::BOUNDS))
                .collect();
        }
        for (row, bits) in instance_rows {
            let i = row as usize;
            if !self.instance_rows.alive[i] {
                continue;
            }
            if bits & dirty::DATA != 0 {
                let mut words = [0u32; 8];
                words[0] = self.instance_mesh[i].index;
                words[1] = self.instance_flags[i];
                for (w, c) in words[4..].iter_mut().zip(floats(&self.instance_color[i])) {
                    *w = c;
                }
                ops.push(DATA, WRITE, row, &words);
            }
            if bits & dirty::BOTH != 0 {
                let mut words: Vec<u32> = floats(&rows_of(&self.instance_transform[i])).collect();
                words.extend(floats(&rows_of(&self.instance_previous[i])));
                ops.push(TRANSFORMS, WRITE, row, &words);
            } else if bits & dirty::MOVED != 0 {
                let words: Vec<u32> = floats(&rows_of(&self.instance_transform[i])).collect();
                ops.push(TRANSFORMS, SHIFT_WRITE, row, &words);
            } else if bits & dirty::STOPPED != 0 {
                ops.push(TRANSFORMS, SHIFT, row, &[]);
            }
            if bits & dirty::BOUNDS != 0 {
                let sphere = self.world_sphere(i);
                let words: Vec<u32> = floats(&sphere).collect();
                ops.push(BOUNDS, WRITE, row, &words);
            }
        }
        let mut mesh_rows: Vec<u32> = self
            .meshes_dirty
            .take()
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        if meshes.fresh {
            mesh_rows = (0..self.mesh_rows.len() as u32).collect();
        }
        for row in mesh_rows {
            let i = row as usize;
            if !self.mesh_rows.alive[i] {
                continue;
            }
            // Not resident: all zero (`Resident` = 0).
            let mut words = [0u32; 16];
            if let Some(m) = self.mesh_resident[i] {
                let (o, s, b) = (m.position_offset, m.position_scale, m.bounds);
                words = [
                    m.geometry,
                    m.lods,
                    m.flags,
                    m.strides,
                    o[0].to_bits(),
                    o[1].to_bits(),
                    o[2].to_bits(),
                    1,
                    s[0].to_bits(),
                    s[1].to_bits(),
                    s[2].to_bits(),
                    m.uv_sets,
                    b.center[0].to_bits(),
                    b.center[1].to_bits(),
                    b.center[2].to_bits(),
                    b.radius.to_bits(),
                ];
            }
            ops.push(MESHES, WRITE, row, &words);
        }
        let mut light_rows: Vec<u32> = self
            .lights_dirty
            .take()
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        if lights.fresh {
            light_rows = (0..self.light_rows.len() as u32).collect();
        }
        for row in light_rows {
            let i = row as usize;
            if !self.light_rows.alive[i] {
                continue;
            }
            let l = self.light_desc[i];
            let kind = match l.kind {
                super::LightKind::Directional => 0,
                super::LightKind::Point => 1,
                super::LightKind::Spot => 2,
            };
            let mut words = [0u32; 16];
            words[0] = kind;
            words[1] = l.range.to_bits();
            words[2] = l.cone[0].to_bits();
            words[3] = l.cone[1].to_bits();
            for (k, v) in [l.radiance, l.position, l.direction].iter().enumerate() {
                for (j, f) in v.iter().enumerate() {
                    words[4 + 4 * k + j] = f.to_bits();
                }
            }
            ops.push(LIGHTS, WRITE, row, &words);
        }
        let handles = [
            data.handle,
            transforms.handle,
            bounds.handle,
            meshes.handle,
            lights.handle,
        ];
        if ops.ops.is_empty() {
            return GpuScene {
                instance_data: handles[0],
                instance_transforms: handles[1],
                instance_bounds: handles[2],
                meshes: handles[3],
                lights: handles[4],
            };
        }
        let pipeline = &scatter.pipeline;
        let written = g.add_compute("GpuSceneScatter", |b| {
            let written = handles.map(|h| b.write_buffer(h, Access::ComputeWrite));
            (written, move |ctx: &mut PassContext<'_>| {
                let count = ops.ops.len() as u32;
                let op_view = ctx.upload_slice(&ops.ops);
                let data_view = if ops.data.is_empty() {
                    op_view
                } else {
                    ctx.upload_slice(&ops.data)
                };
                ctx.set_pipeline(pipeline);
                ctx.push(&ScatterArgs {
                    ops: op_view,
                    data: data_view,
                    tables: written.map(|h| ctx.uav(h)),
                    count,
                });
                ctx.dispatch(count.div_ceil(64), 1, 1);
            })
        });
        GpuScene {
            instance_data: written[0],
            instance_transforms: written[1],
            instance_bounds: written[2],
            meshes: written[3],
            lights: written[4],
        }
    }
}
