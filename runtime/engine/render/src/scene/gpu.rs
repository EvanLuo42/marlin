//! The GPU Scene (0009 §2): the scene's tables as persistent buffers of the render graph, and
//! the one scatter pass a frame that copies the changed rows into them from upload memory.
//!
//! Rows, in 32-bit words (`Marlin.Engine.Scene`'s structs):
//!
//! | table | words | content |
//! |---|---|---|
//! | instance data | 8 | mesh row, flags, material row (`u32::MAX`: none), spare, color (float4) |
//! | instance transforms | 24 | current, then previous: 3 rows of a 3×4 matrix each |
//! | instance bounds | 4 | world bounding sphere: center, radius |
//! | meshes | 20 | page pool view, DAG buffer view, flags, strides; position offset (3), resident (0/1); position scale (3), UV sets; local bounding sphere; the mesh's slot (its BLAS table row), 3 spare (`Marlin.Engine.Scene.MeshEntry`) |
//! | lights | 16 | kind, range, cone (2); intensity (candela · color) + source radius; position + spare; direction + spare |
//! | materials | 4 | permutation (`u32::MAX`: removed), row in its parameter table, 2 spare |
//! | parameters, one table per permutation | the permutation's block | the material struct (0011 §7.5) |
//!
//! The buffers are graph persistent resources keyed by the scene: they keep their contents
//! across frames and grow by powers of two. A new or grown buffer comes back fresh, and the
//! scatter rewrites every live row of it. The transform shift (last frame's current becomes the
//! previous) happens on the GPU: a row that moved writes only its new current transform, and a
//! row that stopped moving writes nothing.
//!
//! Parameter blocks live in one table per permutation because a shader reads a material's block
//! as its struct type, `ReadOnlyBuffer<M>[row]`: rows of one table share one size.

use alloc::vec::Vec;

use marlin_render_graph::{Access, BufferDesc, BufferHandle, PassContext, RenderGraph};
use marlin_rhi::{ComputePipeline, ComputePipelineDesc, Device, Result, ShaderModule};
use marlin_serial::cooked::Pod;
use marlin_shader::bind::EntryDesc;

use super::{RenderScene, dirty};

/// Words per row of the fixed tables, in table order.
const STRIDES: [u32; 6] = [8, 24, 4, 20, 16, 4];
const DATA: usize = 0;
const TRANSFORMS: usize = 1;
const BOUNDS: usize = 2;
const MESHES: usize = 3;
const LIGHTS: usize = 4;
const MATERIALS: usize = 5;
const NAMES: [&str; 6] = [
    "GpuScene.InstanceData",
    "GpuScene.InstanceTransforms",
    "GpuScene.InstanceBounds",
    "GpuScene.Meshes",
    "GpuScene.Lights",
    "GpuScene.Materials",
];

/// What a scatter op does with its row.
const WRITE: u32 = 0;
const SHIFT: u32 = 1;
const SHIFT_WRITE: u32 = 2;

/// A row that names no material.
pub const NO_MATERIAL: u32 = u32::MAX;

/// The scatter shader (`Marlin.Engine.Scene.Scatter`) as a pipeline.
pub struct ScatterShader {
    pipeline: ComputePipeline,
}

impl ScatterShader {
    /// The scatter entry.
    pub const ENTRY: EntryDesc =
        EntryDesc::compute("Marlin.Engine.Scene.Scatter").push::<ScatterArgs>();

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
    pub materials: BufferHandle<'g>,
    /// Each permutation's parameter table, by permutation.
    pub material_params: &'g [BufferHandle<'g>],
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct ScatterArgs {
    ops: u32,
    data: u32,
    table: u32,
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

/// One table's ops: `[first word of the row, kind, first data word, words]`.
#[derive(Default)]
struct Ops {
    stride: u32,
    ops: Vec<[u32; 4]>,
    data: Vec<u32>,
}

impl Ops {
    fn new(stride: u32) -> Self {
        Self {
            stride,
            ..Self::default()
        }
    }

    fn push(&mut self, kind: u32, row: u32, words: &[u32]) {
        self.ops.push([
            row * self.stride,
            kind,
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
    /// Declares this frame's GPU Scene: its tables (persistent resources keyed by the scene)
    /// and, if any row changed since the last call, the scatter pass that writes them. Call it
    /// once a frame, before the passes that read the tables. Needs a graph with a state table.
    pub fn add_passes<'g>(
        &mut self,
        g: &mut RenderGraph<'g>,
        scatter: &'g ScatterShader,
    ) -> GpuScene<'g> {
        let scope = self.id;
        let rows = [
            self.instance_rows.len(),
            self.instance_rows.len(),
            self.instance_rows.len(),
            self.mesh_rows.len(),
            self.light_rows.len(),
            self.material_rows.len(),
        ];
        let fixed: [_; 6] = core::array::from_fn(|t| {
            g.persistent(
                NAMES[t],
                scope,
                BufferDesc::new(capacity(rows[t]) * u64::from(STRIDES[t]) * 4),
            )
        });
        // Parameter tables: one per permutation, rows of the permutation's block size.
        let strides: Vec<u32> = (0..self.permutation_slots.len())
            .map(|p| self.param_words(p as u32))
            .collect();
        let params: Vec<_> = self
            .permutation_slots
            .iter()
            .enumerate()
            .map(|(p, slots)| {
                let bytes = capacity(slots.next as usize) * u64::from(strides[p].max(1)) * 4;
                // The scene's id in the high bits: one scope per (scene, permutation).
                g.persistent(
                    "GpuScene.MaterialParams",
                    scope << 20 | p as u64,
                    BufferDesc::new(bytes),
                )
            })
            .collect();

        let mut tables: Vec<Ops> = STRIDES.iter().map(|&s| Ops::new(s)).collect();
        tables.extend(strides.iter().map(|&s| Ops::new(s)));
        self.instance_ops(&mut tables, fixed.map(|t| t.fresh));
        self.mesh_ops(&mut tables[MESHES], fixed[MESHES].fresh);
        self.light_ops(&mut tables[LIGHTS], fixed[LIGHTS].fresh);
        let fresh_params: Vec<bool> = params.iter().map(|t| t.fresh).collect();
        self.material_ops(&mut tables, fixed[MATERIALS].fresh, &fresh_params);

        let mut handles: Vec<BufferHandle<'g>> = fixed.iter().map(|t| t.handle).collect();
        handles.extend(params.iter().map(|t| t.handle));
        if tables.iter().any(|t| !t.ops.is_empty()) {
            let pipeline = &scatter.pipeline;
            let declared = handles.clone();
            handles = g.add_compute("GpuSceneScatter", |b| {
                let written: Vec<BufferHandle<'g>> = declared
                    .iter()
                    .map(|&h| b.write_buffer(h, Access::ComputeWrite))
                    .collect();
                let views = written.clone();
                (written, move |ctx: &mut PassContext<'_>| {
                    ctx.set_pipeline(pipeline);
                    for (t, table) in tables.iter().enumerate() {
                        if table.ops.is_empty() {
                            continue;
                        }
                        let count = table.ops.len() as u32;
                        let ops = ctx.upload_slice(&table.ops);
                        let data = if table.data.is_empty() {
                            ops
                        } else {
                            ctx.upload_slice(&table.data)
                        };
                        ctx.push(&ScatterArgs {
                            ops,
                            data,
                            table: ctx.uav(views[t]),
                            count,
                        });
                        ctx.dispatch(count.div_ceil(64), 1, 1);
                    }
                })
            });
        }
        GpuScene {
            instance_data: handles[DATA],
            instance_transforms: handles[TRANSFORMS],
            instance_bounds: handles[BOUNDS],
            meshes: handles[MESHES],
            lights: handles[LIGHTS],
            materials: handles[MATERIALS],
            material_params: g.arena().alloc_slice_copy(&handles[MATERIALS + 1..]),
        }
    }

    /// Words of `permutation`'s parameter blocks (its live materials share one size).
    fn param_words(&self, permutation: u32) -> u32 {
        (0..self.material_rows.len())
            .filter(|&r| {
                self.material_rows.alive[r] && self.material_desc[r].permutation == permutation
            })
            .map(|r| self.material_desc[r].params.len() as u32)
            .max()
            .unwrap_or(0)
    }

    fn instance_ops(&mut self, tables: &mut [Ops], fresh: [bool; 6]) {
        let mut instance_rows = self.instances_dirty.take();
        if fresh[DATA] || fresh[TRANSFORMS] || fresh[BOUNDS] {
            // Every row: dead ones are written hidden.
            instance_rows = (0..self.instance_rows.len() as u32)
                .map(|r| (r, dirty::DATA | dirty::BOTH | dirty::BOUNDS))
                .collect();
        }
        for (row, bits) in instance_rows {
            let i = row as usize;
            if !self.instance_rows.alive[i] {
                // A removed row: hidden, so the visibility passes skip it.
                if bits & dirty::DATA != 0 {
                    let hidden = [0, super::flags::HIDDEN, NO_MATERIAL, 0, 0, 0, 0, 0];
                    tables[DATA].push(WRITE, row, &hidden);
                }
                continue;
            }
            if bits & dirty::DATA != 0 {
                let material = self.instance_material[i];
                let mut words = [0u32; 8];
                words[0] = self.instance_mesh[i].index;
                words[1] = self.instance_flags[i];
                words[2] = if self.material_rows.live(material) {
                    material.index
                } else {
                    NO_MATERIAL
                };
                for (w, c) in words[4..].iter_mut().zip(floats(&self.instance_color[i])) {
                    *w = c;
                }
                tables[DATA].push(WRITE, row, &words);
            }
            if bits & dirty::BOTH != 0 {
                let mut words: Vec<u32> = floats(&rows_of(&self.instance_transform[i])).collect();
                words.extend(floats(&rows_of(&self.instance_previous[i])));
                tables[TRANSFORMS].push(WRITE, row, &words);
            } else if bits & dirty::MOVED != 0 {
                let words: Vec<u32> = floats(&rows_of(&self.instance_transform[i])).collect();
                tables[TRANSFORMS].push(SHIFT_WRITE, row, &words);
            } else if bits & dirty::STOPPED != 0 {
                tables[TRANSFORMS].push(SHIFT, row, &[]);
            }
            if bits & dirty::BOUNDS != 0 {
                let sphere = self.world_sphere(i);
                let words: Vec<u32> = floats(&sphere).collect();
                tables[BOUNDS].push(WRITE, row, &words);
            }
        }
    }

    fn mesh_ops(&mut self, ops: &mut Ops, fresh: bool) {
        let mut rows: Vec<u32> = self
            .meshes_dirty
            .take()
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        if fresh {
            rows = (0..self.mesh_rows.len() as u32).collect();
        }
        for row in rows {
            let i = row as usize;
            if !self.mesh_rows.alive[i] {
                continue;
            }
            // Not resident: all zero (`Resident` = 0).
            let mut words = [0u32; 20];
            if let Some(m) = self.mesh_resident[i] {
                let (o, s, b) = (m.position_offset, m.position_scale, m.bounds);
                words = [
                    m.pages,
                    m.dag,
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
                    m.slot,
                    0,
                    0,
                    0,
                ];
            }
            ops.push(WRITE, row, &words);
        }
    }

    fn light_ops(&mut self, ops: &mut Ops, fresh: bool) {
        let mut rows: Vec<u32> = self
            .lights_dirty
            .take()
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        if fresh {
            rows = (0..self.light_rows.len() as u32).collect();
        }
        for row in rows {
            let i = row as usize;
            if !self.light_rows.alive[i] {
                // Removed: no light (kind 0 lights nothing local), so a reservoir still naming it
                // gives nothing.
                ops.push(WRITE, row, &[0u32; 16]);
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
            words[7] = l.radius.to_bits();
            ops.push(WRITE, row, &words);
        }
    }

    /// The material table's rows and the parameter blocks, into `tables[MATERIALS]` and
    /// `tables[MATERIALS + 1 + permutation]`.
    fn material_ops(&mut self, tables: &mut [Ops], fresh: bool, fresh_params: &[bool]) {
        let mut rows: Vec<u32> = self
            .materials_dirty
            .take()
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        if fresh || fresh_params.iter().any(|&f| f) {
            rows = (0..self.material_rows.len() as u32).collect();
        }
        for row in rows {
            let i = row as usize;
            if !self.material_rows.alive[i] {
                tables[MATERIALS].push(WRITE, row, &[NO_MATERIAL, 0, 0, 0]);
                continue;
            }
            let desc = &self.material_desc[i];
            let slot = self.material_slot[i];
            tables[MATERIALS].push(WRITE, row, &[desc.permutation, slot, 0, 0]);
            let params = &mut tables[MATERIALS + 1 + desc.permutation as usize];
            if !desc.params.is_empty() {
                params.push(WRITE, slot, &desc.params);
            }
        }
    }
}
