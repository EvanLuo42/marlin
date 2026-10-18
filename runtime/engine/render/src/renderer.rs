//! The forward renderer, as passes of the frame's render graph.

use alloc::vec::Vec;

use marlin_containers::{Handle, SlotMap};
use marlin_ecs::World;
use marlin_math::{Mat4, Transform, Vec3};
use marlin_render_graph::{
    PassContext, RenderGraph, TextureDesc as GraphTextureDesc, TextureHandle,
};
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    ClearValue, ColorTargetState, DepthState, Device, Error, ErrorKind, Format, GraphicsPipeline,
    GraphicsPipelineDesc, IndexFormat, LoadOp, MemoryLocation, MultisampleState, PrimitiveTopology,
    RasterState, Result, ShaderModule, ShaderView, StoreOp,
};

use crate::mesh::{MeshData, Vertex};

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

/// Draws a mesh at the entity's [`Transform`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MeshRenderer {
    pub mesh: MeshHandle,
    /// Linear RGBA.
    pub color: [f32; 4],
}

/// One mesh drawn at a transform.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SceneInstance {
    pub mesh: MeshHandle,
    pub transform: Transform,
    /// Linear RGBA.
    pub color: [f32; 4],
}

/// What a frame draws, taken out of the simulation so the two can run apart: the camera and
/// every instance. Immutable once built.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Scene {
    pub camera: Option<(Transform, Camera)>,
    pub instances: Vec<SceneInstance>,
}

impl Scene {
    /// The first [`Camera`] and every [`MeshRenderer`] of `world`, with their [`Transform`]s.
    pub fn extract(world: &World) -> Self {
        Self {
            camera: world
                .query_ref::<(&Transform, &Camera)>()
                .next()
                .map(|(t, c)| (*t, *c)),
            instances: world
                .query_ref::<(&Transform, &MeshRenderer)>()
                .map(|(t, r)| SceneInstance {
                    mesh: r.mesh,
                    transform: *t,
                    color: r.color,
                })
                .collect(),
        }
    }
}

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

/// The forward pass: vertex pulling and per-instance transforms, one directional light plus
/// ambient. Its push constants are the frame constants', instances' and vertices' view indices
/// and the draw's first instance (`docs/design/0002-shader-interface.md`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ForwardShaders<'a> {
    pub vertex: ShaderModule<'a>,
    pub pixel: ShaderModule<'a>,
}

/// Bytes of push constants the forward pass sets: four 32-bit words (0002). Its shaders' `[Push]`
/// struct must be this size.
pub const FORWARD_PUSH_BYTES: u32 = 16;

/// What a [`Renderer::add_passes`] call draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RenderStats {
    pub instances: u32,
    pub draws: u32,
}

/// `Instance` in the shader: model matrix and color.
const INSTANCE_WORDS: usize = 20;
const DEPTH_FORMAT: Format = Format::D32Float;

struct Mesh {
    vertex_view: ShaderView,
    _vertices: Buffer,
    indices: Buffer,
    index_count: u32,
}

/// One instanced draw of the forward pass.
struct Draw<'g> {
    indices: &'g Buffer,
    index_count: u32,
    vertices: u32,
    first: u32,
    count: u32,
}

/// Draws the meshes of a [`World`]: see the crate docs.
pub struct Renderer {
    /// `None` without shaders: frames are only cleared.
    pipeline: Option<GraphicsPipeline>,
    meshes: SlotMap<Mesh>,
    pub environment: Environment,
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
            environment: Environment::default(),
        })
    }

    /// Uploads a mesh, blocking until the GPU has copied it.
    pub fn upload_mesh(&mut self, device: &Device, data: &MeshData<'_>) -> Result<MeshHandle> {
        if data.vertices.is_empty() || data.indices.is_empty() {
            return Err(Error::from_kind(ErrorKind::InvalidInput));
        }
        // SAFETY: Vertex is plain f32 data without padding.
        let vertex_bytes = unsafe {
            core::slice::from_raw_parts(
                data.vertices.as_ptr().cast::<u8>(),
                size_of_val(data.vertices),
            )
        };
        let index_bytes: Vec<u8> = data.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let (vertex_size, index_size) = (vertex_bytes.len() as u64, index_bytes.len() as u64);

        let staging = device.create_buffer(&BufferDesc {
            size: vertex_size + index_size,
            usage: BufferUsage::COPY_SRC,
            memory: MemoryLocation::Upload,
        })?;
        // SAFETY: the GPU has no work using the new buffer.
        unsafe {
            staging.write(0, vertex_bytes);
            staging.write(vertex_size, &index_bytes);
        }
        let vertices = device.create_buffer(&BufferDesc {
            size: vertex_size,
            usage: BufferUsage::SHADER_READ | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })?;
        let indices = device.create_buffer(&BufferDesc {
            size: index_size,
            usage: BufferUsage::INDEX | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })?;

        let mut pool = device.create_command_pool()?;
        let mut list = device.create_command_list(&pool)?;
        let fence = device.create_fence(0)?;
        list.begin(&mut pool)?;
        list.copy_buffer(&staging, 0, &vertices, 0, vertex_size);
        list.copy_buffer(&staging, vertex_size, &indices, 0, index_size);
        list.buffer_barriers(&[
            BufferBarrier {
                buffer: &vertices,
                before: BufferState::COPY_DST,
                after: BufferState::SHADER_READ,
            },
            BufferBarrier {
                buffer: &indices,
                before: BufferState::COPY_DST,
                after: BufferState::INDEX,
            },
        ]);
        list.end()?;
        // SAFETY: everything the list uses lives until the wait below returns.
        unsafe { device.queue().submit(&[&list], Some((&fence, 1)))? };
        fence.wait(1, None)?;

        let vertex_view = device.create_buffer_view(
            &vertices,
            &BufferViewDesc::whole(
                BufferViewKind::Structured {
                    stride: size_of::<Vertex>() as u32,
                },
                false,
            ),
        )?;
        Ok(MeshHandle(self.meshes.insert(Mesh {
            vertex_view,
            _vertices: vertices,
            indices,
            index_count: data.indices.len() as u32,
        })))
    }

    /// Frees a mesh's GPU buffers. Its handle draws nothing from now on.
    ///
    /// # Safety
    /// The GPU has finished every frame that drew the mesh (after `frames_in_flight` more frames,
    /// or a wait for idle).
    pub unsafe fn release_mesh(&mut self, mesh: MeshHandle) {
        self.meshes.remove(mesh.0);
    }

    /// Meshes uploaded and not released.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Declares the forward pass into `g` (0007 §11.2): one raster pass that clears `target` and
    /// a transient depth buffer and draws every instance of `scene` seen from its camera, one
    /// instanced draw per mesh. Without a camera, or shaders, it only clears. The frame and
    /// instance constants go through the frame's upload memory. Returns the target's new handle
    /// and what the pass draws.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        target: TextureHandle<'g>,
        scene: &Scene,
    ) -> (TextureHandle<'g>, RenderStats) {
        let size = g.texture_desc(target);
        let (width, height) = (size.width, size.height);
        let mut instances: Vec<(Handle<Mesh>, [f32; INSTANCE_WORDS])> = Vec::new();
        let camera = scene.camera.filter(|_| self.pipeline.is_some());
        if camera.is_some() {
            for instance in &scene.instances {
                let mut words = [0.0; INSTANCE_WORDS];
                words[..16].copy_from_slice(&instance.transform.to_matrix().to_cols_array());
                words[16..].copy_from_slice(&instance.color);
                instances.push((instance.mesh.0, words));
            }
            instances.sort_unstable_by_key(|(mesh, _)| *mesh);
        }
        let mut draws: Vec<Draw<'g>> = Vec::new();
        let mut start = 0;
        while start < instances.len() {
            let handle = instances[start].0;
            let count = instances[start..]
                .iter()
                .take_while(|(m, _)| *m == handle)
                .count();
            if let Some(mesh) = self.meshes.get(handle) {
                draws.push(Draw {
                    indices: &mesh.indices,
                    index_count: mesh.index_count,
                    vertices: mesh.vertex_view.index(),
                    first: start as u32,
                    count: count as u32,
                });
            }
            start += count;
        }
        let stats = if draws.is_empty() {
            RenderStats::default()
        } else {
            RenderStats {
                instances: instances.len() as u32,
                draws: draws.len() as u32,
            }
        };
        let constants = camera.map(|(transform, camera)| {
            let aspect = width as f32 / height.max(1) as f32;
            let view = transform.to_matrix().inverse_affine();
            let projection = Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near);
            let env = &self.environment;
            let light = env.light_direction.normalize_or_zero();
            let mut constants = [0.0f32; 28];
            constants[..16].copy_from_slice(&(projection * view).to_cols_array());
            constants[16..19].copy_from_slice(&[light.x, light.y, light.z]);
            constants[20..23].copy_from_slice(&env.light_color);
            constants[24..27].copy_from_slice(&env.ambient);
            constants
        });
        let words: Vec<[f32; INSTANCE_WORDS]> = instances.into_iter().map(|(_, w)| w).collect();
        let depth = g.create_texture(
            "Depth",
            GraphTextureDesc::new_2d(width, height, DEPTH_FORMAT)
                .with_clear(ClearValue::Depth(0.0)),
        );
        let clear = self.environment.clear_color;
        let pipeline = self.pipeline.as_ref();
        let target = g.add_raster("Forward", |b| {
            let target = b.color(target, LoadOp::Clear(clear), StoreOp::Store);
            b.depth(depth, LoadOp::Clear(0.0), StoreOp::DontCare);
            (target, move |ctx: &mut PassContext<'_>| {
                let (Some(pipeline), Some(constants)) = (pipeline, constants) else {
                    return;
                };
                if draws.is_empty() {
                    return;
                }
                let frame = ctx.upload(&constants);
                let instances = ctx.upload_slice(&words);
                ctx.set_pipeline(pipeline);
                for draw in &draws {
                    ctx.resident_index_buffer(draw.indices, 0, IndexFormat::U32);
                    ctx.push_words(&[frame, instances, draw.vertices, draw.first]);
                    ctx.draw_indexed(draw.index_count, draw.count, 0, 0, 0);
                }
            })
        });
        (target, stats)
    }
}
