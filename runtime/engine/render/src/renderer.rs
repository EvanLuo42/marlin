//! The forward renderer, as passes of the frame's render graph.

use alloc::vec::Vec;

use marlin_containers::{Handle, SlotMap};
use marlin_math::{Mat4, Transform, Vec3};
use marlin_render_graph::{
    Access, PassContext, RenderGraph, TextureDesc as GraphTextureDesc, TextureHandle,
};
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    ClearValue, ColorTargetState, DepthState, Device, Error, ErrorKind, Format, GraphicsPipeline,
    GraphicsPipelineDesc, LoadOp, MemoryLocation, MultisampleState, PrimitiveTopology, RasterState,
    Result, ShaderModule, ShaderView, StoreOp,
};

use crate::asset::mesh::MeshView;
use crate::geometry::{self, LodDraw};
use crate::scene::{Bounds, GpuScene, RenderScene, ResidentMesh};

/// The view a [`Renderer`] draws from: put it on an entity with a [`Transform`]. The camera looks
/// down its local −Z with +Y up.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
    /// Full vertical field of view, radians.
    pub fov_y: f32,
    /// Near plane distance; the far plane is at infinity (reverse-Z).
    pub near: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov_y: core::f32::consts::FRAC_PI_3,
            near: 0.1,
        }
    }
}

/// A mesh uploaded with [`Renderer::upload_mesh`]. Generational: once the mesh is released the
/// handle draws nothing, even after a later upload reuses its slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MeshHandle(Handle<Mesh>);

/// Lighting shared by the whole scene.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Environment {
    /// Direction the light travels (normalized when used).
    pub light_direction: Vec3,
    /// Linear radiance.
    pub light_color: [f32; 3],
    pub ambient: [f32; 3],
    /// Linear RGBA.
    pub clear_color: [f32; 4],
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            light_direction: Vec3::new(-0.4, -1.0, -0.6),
            light_color: [1.0, 0.96, 0.9],
            ambient: [0.08, 0.09, 0.12],
            clear_color: [0.02, 0.02, 0.03, 1.0],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RendererDesc<'a> {
    /// Format of the targets [`Renderer::add_passes`] draws into.
    pub color_format: Format,
    /// The forward pass's shaders, which the engine gets from the cooker like any asset. Without
    /// them the renderer only clears.
    pub shaders: Option<ForwardShaders<'a>>,
}

/// The forward pass: vertices decoded from the mesh's resident LOD page, per-instance transforms
/// from the GPU Scene, one directional light plus ambient. Its push constants are the views of
/// the frame constants, the instance list, the GPU Scene's tables and the mesh's geometry, the
/// LOD and the draw's first instance (`docs/design/0002-shader-interface.md`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ForwardShaders<'a> {
    pub vertex: ShaderModule<'a>,
    pub pixel: ShaderModule<'a>,
}

/// Bytes of push constants the forward pass sets: eight 32-bit words (0002). Its shaders'
/// `[Push]` struct must be this size.
pub const FORWARD_PUSH_BYTES: u32 = 32;

/// What [`Renderer::add_passes`] declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ForwardOutput<'g> {
    pub color: TextureHandle<'g>,
    /// The pass's depth (`D32Float`, reverse-Z): a transient the caller may read.
    pub depth: TextureHandle<'g>,
    pub stats: RenderStats,
}

/// What a [`Renderer::add_passes`] call draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RenderStats {
    pub instances: u32,
    pub draws: u32,
    /// Triangles drawn, at the LODs picked.
    pub triangles: u64,
}

const DEPTH_FORMAT: Format = Format::D32Float;

struct Mesh {
    view: ShaderView,
    buffer: Buffer,
    lods: Vec<LodDraw>,
    resident: ResidentMesh,
}

/// One instanced draw of the forward pass.
struct Draw<'g> {
    buffer: &'g Buffer,
    lod: u32,
    draw: LodDraw,
    geometry: u32,
    first: u32,
    count: u32,
}

/// Draws a [`RenderScene`]: see the crate docs.
pub struct Renderer {
    /// `None` without shaders: frames are only cleared.
    pipeline: Option<GraphicsPipeline>,
    meshes: SlotMap<Mesh>,
    /// The screen-space error, in pixels, a LOD may have: each instance draws the coarsest LOD
    /// whose error, projected at the instance's distance, stays within it.
    pub lod_error_pixels: f32,
}

impl Renderer {
    pub fn new(device: &Device, desc: &RendererDesc) -> Result<Self> {
        let pipeline = desc
            .shaders
            .map(|s| {
                device.create_graphics_pipeline(&GraphicsPipelineDesc {
                    vertex: s.vertex,
                    pixel: Some(s.pixel),
                    topology: PrimitiveTopology::TriangleList,
                    raster: RasterState::default(),
                    depth: Some(DepthState::reverse_z(DEPTH_FORMAT)),
                    color_targets: &[ColorTargetState::new(desc.color_format)],
                    multisample: MultisampleState::NONE,
                    primitive_restart: None,
                    cache: None,
                })
            })
            .transpose()?;
        Ok(Self {
            pipeline,
            meshes: SlotMap::new(),
            lod_error_pixels: 1.0,
        })
    }

    /// Uploads a cooked mesh's LOD pages as they are (the [`geometry`](crate::geometry) layout),
    /// blocking until the GPU has copied them. `pages` are the mesh's bulk blobs; each LOD page
    /// is checked first. Malformed pages are `InvalidInput`.
    pub fn upload_mesh(
        &mut self,
        device: &Device,
        mesh: &MeshView<'_>,
        pages: &[&[u8]],
    ) -> Result<MeshHandle> {
        let (bytes, lods) =
            geometry::build(mesh, pages).map_err(|_| Error::from_kind(ErrorKind::InvalidInput))?;
        let size = bytes.len() as u64;
        let staging = device.create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::COPY_SRC,
            memory: MemoryLocation::Upload,
        })?;
        // SAFETY: the GPU has no work using the new buffer.
        unsafe { staging.write(0, &bytes) };
        let buffer = device.create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::SHADER_READ | BufferUsage::INDEX | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })?;

        let mut pool = device.create_command_pool()?;
        let mut list = device.create_command_list(&pool)?;
        let fence = device.create_fence(0)?;
        list.begin(&mut pool)?;
        list.copy_buffer(&staging, 0, &buffer, 0, size);
        list.buffer_barriers(&[BufferBarrier {
            buffer: &buffer,
            before: BufferState::COPY_DST,
            after: BufferState {
                stage: BufferState::SHADER_READ
                    .stage
                    .union(BufferState::INDEX.stage),
                access: BufferState::SHADER_READ
                    .access
                    .union(BufferState::INDEX.access),
            },
        }]);
        list.end()?;
        // SAFETY: everything the list uses lives until the wait below returns.
        unsafe { device.queue().submit(&[&list], Some((&fence, 1)))? };
        fence.wait(1, None)?;

        let view = device
            .create_buffer_view(&buffer, &BufferViewDesc::whole(BufferViewKind::Raw, false))?;
        let h = mesh.header;
        let s = h.sphere;
        let resident = ResidentMesh {
            // Set below, once the slot is known.
            handle: MeshHandle(Handle::default()),
            geometry: view.index(),
            lods: lods.len() as u32,
            flags: h.flags,
            strides: h.position_stride | h.attribute_stride << 8 | h.skin_stride << 16,
            uv_sets: h.uv_sets,
            position_offset: h.position_offset,
            position_scale: h.position_scale,
            bounds: Bounds {
                center: [s[0], s[1], s[2]],
                radius: s[3],
                min: h.bounds_min,
                max: h.bounds_max,
            },
        };
        let handle = MeshHandle(self.meshes.insert(Mesh {
            view,
            buffer,
            lods,
            resident,
        }));
        if let Some(m) = self.meshes.get_mut(handle.0) {
            m.resident.handle = handle;
        }
        Ok(handle)
    }

    /// Frees a mesh's GPU buffers. Its handle draws nothing from now on.
    ///
    /// # Safety
    /// The GPU has finished every frame that drew the mesh (after `frames_in_flight` more frames,
    /// or a wait for idle).
    pub unsafe fn release_mesh(&mut self, mesh: MeshHandle) {
        self.meshes.remove(mesh.0);
    }

    /// The most meshlets any LOD of `mesh` has (0 if it was released): what one instance may
    /// add to the visible clusters.
    pub(crate) fn meshlet_capacity(&self, mesh: MeshHandle) -> u32 {
        self.meshes
            .get(mesh.0)
            .map_or(0, |m| m.lods.iter().map(|l| l.meshlets).max().unwrap_or(0))
    }

    /// Meshes uploaded and not released.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// What the GPU Scene's mesh table needs of `mesh` (for `MeshUpdate::Resident`). `None` if
    /// the mesh was released.
    pub fn resident_mesh(&self, mesh: MeshHandle) -> Option<ResidentMesh> {
        self.meshes.get(mesh.0).map(|m| m.resident)
    }

    /// The LOD an instance of `mesh` draws: the coarsest whose error, scaled by the instance's
    /// largest scale and projected at the distance of its bounding sphere's near side, is at
    /// most [`lod_error_pixels`](Self::lod_error_pixels). `pixels_per_unit` is the projection's
    /// pixels per world unit at distance 1.
    fn pick_lod(&self, mesh: &Mesh, transform: &Transform, eye: Vec3, pixels_per_unit: f32) -> u32 {
        let b = mesh.resident.bounds;
        let center = transform.transform_point(Vec3::new(b.center[0], b.center[1], b.center[2]));
        let s = transform.scale.abs();
        let scale = s.x.max(s.y).max(s.z);
        let distance = ((center - eye).length() - b.radius * scale).max(1e-4);
        let limit = self.lod_error_pixels * distance / (pixels_per_unit * scale);
        let mut lod = 0;
        for (i, l) in mesh.lods.iter().enumerate() {
            if l.error <= limit {
                lod = i;
            }
        }
        lod as u32
    }

    /// Declares the forward pass into `g` (0007 §11.2): one raster pass that clears `target` and
    /// a transient depth buffer and draws every drawable instance of `scene` seen from `camera`,
    /// one instanced draw per mesh and LOD. Transforms and colors come from the GPU Scene `gpu`
    /// (after its scatter), vertices from the meshes' resident pages; the pass uploads only the
    /// frame constants and the instance rows grouped by draw. Without a camera, or shaders, it
    /// only clears. Returns the target's and the depth buffer's new handles and what the pass
    /// draws.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
        scene: &RenderScene,
        gpu: GpuScene<'g>,
        camera: Option<(Transform, Camera)>,
    ) -> ForwardOutput<'g> {
        let size = g.texture_desc(target);
        let (width, height) = (size.width, size.height);
        let camera = camera.filter(|_| self.pipeline.is_some());
        // (mesh, LOD, row) for every drawable instance, grouped into draws.
        let mut rows: Vec<(Handle<Mesh>, u32, u32)> = Vec::new();
        if let Some((eye, camera)) = camera {
            let pixels_per_unit =
                height as f32 / (2.0 * marlin_math::scalar::tan(camera.fov_y * 0.5));
            for (row, mesh, transform) in scene.drawable() {
                if let Some(m) = self.meshes.get(mesh.0) {
                    let lod = self.pick_lod(m, &transform, eye.translation, pixels_per_unit);
                    rows.push((mesh.0, lod, row));
                }
            }
            rows.sort_unstable();
        }
        let mut draws: Vec<Draw<'g>> = Vec::new();
        let mut list: Vec<u32> = Vec::with_capacity(rows.len());
        let mut triangles = 0u64;
        let mut start = 0;
        while start < rows.len() {
            let (handle, lod, _) = rows[start];
            let count = rows[start..]
                .iter()
                .take_while(|(m, l, _)| (*m, *l) == (handle, lod))
                .count();
            if let Some(mesh) = self.meshes.get(handle) {
                let draw = mesh.lods[lod as usize];
                triangles += u64::from(draw.index_count / 3) * count as u64;
                draws.push(Draw {
                    buffer: &mesh.buffer,
                    lod,
                    draw,
                    geometry: mesh.view.index(),
                    first: list.len() as u32,
                    count: count as u32,
                });
                list.extend(rows[start..start + count].iter().map(|(_, _, r)| *r));
            }
            start += count;
        }
        let stats = RenderStats {
            instances: list.len() as u32,
            draws: draws.len() as u32,
            triangles,
        };
        let env = *scene.environment();
        let constants = camera.map(|(transform, camera)| {
            let aspect = width as f32 / height.max(1) as f32;
            let view = transform.to_matrix().inverse_affine();
            let projection = Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near);
            let light = env.light_direction.normalize_or_zero();
            let mut constants = [0.0f32; 28];
            constants[..16].copy_from_slice(&(projection * view).to_cols_array());
            constants[16..19].copy_from_slice(&[light.x, light.y, light.z]);
            constants[20..23].copy_from_slice(&env.light_color);
            constants[24..27].copy_from_slice(&env.ambient);
            constants
        });
        let depth = g.create_texture(
            "Depth",
            GraphTextureDesc::new_2d(width, height, DEPTH_FORMAT)
                .with_clear(ClearValue::Depth(0.0)),
        );
        let pipeline = self.pipeline.as_ref();
        let (target, depth) = g.add_raster("Forward", |b| {
            let target = b.color(target, LoadOp::Clear(env.clear_color), StoreOp::Store);
            let depth = b.depth(depth, LoadOp::Clear(0.0), StoreOp::Store);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::VertexRead);
            let instances = b.read_buffer(gpu.instance_data, Access::VertexRead);
            let meshes = b.read_buffer(gpu.meshes, Access::VertexRead);
            ((target, depth), move |ctx: &mut PassContext<'_>| {
                let (Some(pipeline), Some(constants)) = (pipeline, constants) else {
                    return;
                };
                if draws.is_empty() {
                    return;
                }
                let frame = ctx.upload(&constants);
                let list = ctx.upload_slice(&list);
                let tables = [transforms, instances, meshes].map(|h| ctx.srv(h));
                ctx.set_pipeline(pipeline);
                for d in &draws {
                    ctx.resident_index_buffer(d.buffer, d.draw.indices, d.draw.format);
                    ctx.push_words(&[
                        frame, list, tables[0], tables[1], tables[2], d.geometry, d.lod, d.first,
                    ]);
                    ctx.draw_indexed(d.draw.index_count, d.count, 0, 0, 0);
                }
            })
        });
        ForwardOutput {
            color: target,
            depth,
            stats,
        }
    }
}
