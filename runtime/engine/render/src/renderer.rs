//! The forward renderer.

use alloc::vec::Vec;

use marlin_ecs::World;
use marlin_math::{Mat4, Transform, Vec3};
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    ClearValue, ColorAttachment, ColorTargetState, CommandList, DepthAttachment, DepthState,
    DepthStencilView, DepthStencilViewDesc, Device, Error, ErrorKind, Format, GraphicsPipeline,
    GraphicsPipelineDesc, IndexFormat, LoadOp, MemoryLocation, MultisampleState, PrimitiveTopology,
    RasterState,
    Rect, RenderPassDesc, RenderTargetView, Result, ShaderView, StoreOp, Texture, TextureBarrier,
    TextureDesc, TextureState, TextureUsage, Viewport,
};

use crate::mesh::{MeshData, Vertex};
use crate::shaders;

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

/// A mesh uploaded with [`Renderer::upload_mesh`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MeshHandle(u32);

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
pub struct RendererDesc {
    /// Format of the targets [`Renderer::render`] draws into.
    pub color_format: Format,
    /// Frames in flight: [`Renderer::render`]'s `frame` is below this.
    pub frames_in_flight: usize,
    /// Initial target size; see [`Renderer::resize`].
    pub width: u32,
    pub height: u32,
}

/// What a [`Renderer::render`] call drew.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RenderStats {
    pub instances: u32,
    pub draws: u32,
}

/// `Frame` in the shader: view-projection, light direction, light color, ambient.
const FRAME_SIZE: u64 = 256;
/// `Instance` in the shader: model matrix and color.
const INSTANCE_WORDS: usize = 20;
const INSTANCE_SIZE: u64 = INSTANCE_WORDS as u64 * 4;
const DEPTH_FORMAT: Format = Format::D32Float;

struct Mesh {
    vertex_view: ShaderView,
    _vertices: Buffer,
    indices: Buffer,
    index_count: u32,
}

/// Per-frame-in-flight upload buffers: written by the CPU while that frame records.
struct FrameData {
    frame_view: ShaderView,
    frame: Buffer,
    instances_view: ShaderView,
    instances: Buffer,
    capacity: u32,
}

struct DepthTarget {
    view: DepthStencilView,
    texture: Texture,
    /// The texture has left its initial layout (it stays in depth-write afterwards).
    initialized: bool,
}

/// Draws the meshes of a [`World`]: see the crate docs.
pub struct Renderer {
    pipeline: GraphicsPipeline,
    /// By handle; `None` once released.
    meshes: Vec<Option<Mesh>>,
    frames: Vec<Option<FrameData>>,
    depth: DepthTarget,
    size: (u32, u32),
    /// Scratch: (mesh, instance words), sorted by mesh each frame.
    instances: Vec<(u32, [f32; INSTANCE_WORDS])>,
    pub environment: Environment,
}

fn create_depth(device: &Device, width: u32, height: u32) -> Result<DepthTarget> {
    let texture = device.create_texture(&TextureDesc {
        clear_value: Some(ClearValue::Depth(0.0)),
        ..TextureDesc::new_2d(width, height, DEPTH_FORMAT, TextureUsage::DEPTH_STENCIL)
    })?;
    let view = device.create_depth_stencil_view(&texture, &DepthStencilViewDesc::default())?;
    Ok(DepthTarget {
        view,
        texture,
        initialized: false,
    })
}

/// The bytes of `words`.
fn bytes(words: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding or invalid bit patterns as bytes; the length is in bounds.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast(), size_of_val(words)) }
}

impl Renderer {
    pub fn new(device: &Device, desc: &RendererDesc) -> Result<Self> {
        let (vertex, pixel) = shaders::forward().ok_or(Error::from_kind(ErrorKind::Unsupported))?;
        let pipeline = device.create_graphics_pipeline(&GraphicsPipelineDesc {
            vertex,
            pixel: Some(pixel),
            topology: PrimitiveTopology::TriangleList,
            raster: RasterState::default(),
            depth: Some(DepthState::reverse_z(DEPTH_FORMAT)),
            color_targets: &[ColorTargetState::new(desc.color_format)],
            multisample: MultisampleState::NONE,
            primitive_restart: None,
            cache: None,
        })?;
        Ok(Self {
            pipeline,
            meshes: Vec::new(),
            frames: (0..desc.frames_in_flight.max(1)).map(|_| None).collect(),
            depth: create_depth(device, desc.width, desc.height)?,
            size: (desc.width, desc.height),
            instances: Vec::new(),
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
        self.meshes.push(Some(Mesh {
            vertex_view,
            _vertices: vertices,
            indices,
            index_count: data.indices.len() as u32,
        }));
        Ok(MeshHandle(self.meshes.len() as u32 - 1))
    }

    /// Frees a mesh's GPU buffers. Its handle draws nothing from now on.
    ///
    /// # Safety
    /// The GPU has finished every frame that drew the mesh (after `frames_in_flight` more frames,
    /// or a wait for idle).
    pub unsafe fn release_mesh(&mut self, mesh: MeshHandle) {
        if let Some(slot) = self.meshes.get_mut(mesh.0 as usize) {
            *slot = None;
        }
    }

    /// Meshes uploaded and not released.
    pub fn mesh_count(&self) -> usize {
        self.meshes.iter().filter(|m| m.is_some()).count()
    }

    /// The target size [`render`](Self::render) draws at.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Changes the target size, recreating the depth buffer.
    ///
    /// # Safety
    /// The GPU has finished all work this renderer recorded (resizing a swapchain waits for
    /// that).
    pub unsafe fn resize(&mut self, device: &Device, width: u32, height: u32) -> Result<()> {
        if (width, height) != self.size {
            self.depth = create_depth(device, width, height)?;
            self.size = (width, height);
        }
        Ok(())
    }

    /// Records the scene into `list`: one render pass that clears `target` (already in
    /// [`TextureState::RENDER_TARGET`], of the size given at creation or by
    /// [`resize`](Self::resize)) and draws every [`MeshRenderer`] seen by the first [`Camera`].
    /// Without a camera the pass only clears.
    ///
    /// # Safety
    /// `frame` (below [`RendererDesc::frames_in_flight`]) names a slot whose previously recorded
    /// work the GPU has finished, as [`marlin_rhi::FramesInFlight::begin`] guarantees for its
    /// [`index`](marlin_rhi::FramesInFlight::index). The list is submitted before the slot is
    /// reused, and the renderer outlives its GPU work.
    pub unsafe fn render(
        &mut self,
        device: &Device,
        list: &mut CommandList,
        frame: usize,
        target: &RenderTargetView,
        world: &World,
    ) -> Result<RenderStats> {
        let scene = Scene::extract(world);
        // SAFETY: forwarded from the caller.
        unsafe { self.render_scene(device, list, frame, target, &scene) }
    }

    /// Records `scene` like [`render`](Self::render) records a world. Instances of released meshes
    /// are skipped.
    ///
    /// # Safety
    /// As [`render`](Self::render).
    pub unsafe fn render_scene(
        &mut self,
        device: &Device,
        list: &mut CommandList,
        frame: usize,
        target: &RenderTargetView,
        scene: &Scene,
    ) -> Result<RenderStats> {
        let camera = scene.camera.as_ref().map(|(t, c)| (t, c));
        self.instances.clear();
        if camera.is_some() {
            for instance in &scene.instances {
                let mut words = [0.0; INSTANCE_WORDS];
                words[..16].copy_from_slice(&instance.transform.to_matrix().to_cols_array());
                words[16..].copy_from_slice(&instance.color);
                self.instances.push((instance.mesh.0, words));
            }
            self.instances.sort_unstable_by_key(|(mesh, _)| *mesh);
        }

        if !self.depth.initialized {
            list.texture_barriers(&[TextureBarrier::new(
                &self.depth.texture,
                TextureState::UNDEFINED,
                TextureState::DEPTH_WRITE,
            )]);
            self.depth.initialized = true;
        }
        list.begin_render_pass(&RenderPassDesc {
            color: &[ColorAttachment {
                view: target,
                load: LoadOp::Clear(self.environment.clear_color),
                store: StoreOp::Store,
            }],
            depth: Some(DepthAttachment {
                view: &self.depth.view,
                load: LoadOp::Clear(0.0),
                store: StoreOp::DontCare,
                stencil_load: LoadOp::DontCare,
                stencil_store: StoreOp::DontCare,
            }),
        });
        let mut stats = RenderStats::default();
        if let Some((camera_transform, camera)) = camera
            && !self.instances.is_empty()
        {
            // SAFETY: forwarded from the caller.
            let (frame_index, instances_index) =
                unsafe { self.write_frame(device, frame, camera_transform, camera)? };
            let (width, height) = self.size;
            list.set_graphics_pipeline(&self.pipeline);
            list.set_viewport(&Viewport::full(width, height));
            list.set_scissor(&Rect {
                x: 0,
                y: 0,
                width,
                height,
            });
            let mut start = 0;
            while start < self.instances.len() {
                let mesh_index = self.instances[start].0;
                let count = self.instances[start..]
                    .iter()
                    .take_while(|(m, _)| *m == mesh_index)
                    .count();
                let Some(mesh) = self.meshes[mesh_index as usize].as_ref() else {
                    start += count;
                    continue;
                };
                list.set_index_buffer(&mesh.indices, 0, IndexFormat::U32);
                list.push_constants(
                    0,
                    &[
                        frame_index,
                        instances_index,
                        mesh.vertex_view.index(),
                        start as u32,
                    ],
                );
                list.draw_indexed(mesh.index_count, count as u32, 0, 0, 0);
                stats.draws += 1;
                start += count;
            }
            stats.instances = self.instances.len() as u32;
        }
        list.end_render_pass();
        Ok(stats)
    }

    /// Writes the frame constants and instances into slot `frame`'s upload buffers, growing
    /// them as needed. Returns the frame constants' and instances' view indices.
    ///
    /// # Safety
    /// As [`render`](Self::render).
    unsafe fn write_frame(
        &mut self,
        device: &Device,
        frame: usize,
        camera_transform: &Transform,
        camera: &Camera,
    ) -> Result<(u32, u32)> {
        let needed = self.instances.len() as u32;
        let slot = &mut self.frames[frame];
        if slot.as_ref().is_none_or(|d| d.capacity < needed) {
            // The slot's GPU work is done (caller), so its old buffers may go.
            *slot = Some(create_frame_data(
                device,
                needed.next_power_of_two().max(64),
            )?);
        }
        let data = slot.as_ref().expect("created above");

        let (width, height) = self.size;
        let aspect = width as f32 / height.max(1) as f32;
        let view = camera_transform.to_matrix().inverse_affine();
        let projection = Mat4::perspective_infinite_rh(camera.fov_y, aspect, camera.near);
        let env = &self.environment;
        let light = env.light_direction.normalize_or_zero();
        let mut constants = [0.0f32; 28];
        constants[..16].copy_from_slice(&(projection * view).to_cols_array());
        constants[16..19].copy_from_slice(&[light.x, light.y, light.z]);
        constants[20..23].copy_from_slice(&env.light_color);
        constants[24..27].copy_from_slice(&env.ambient);

        // SAFETY: the GPU is done with this slot's buffers (caller).
        unsafe {
            data.frame.write(0, bytes(&constants));
            for (i, (_, words)) in self.instances.iter().enumerate() {
                data.instances.write(i as u64 * INSTANCE_SIZE, bytes(words));
            }
        }
        Ok((data.frame_view.index(), data.instances_view.index()))
    }
}

fn create_frame_data(device: &Device, capacity: u32) -> Result<FrameData> {
    let frame = device.create_buffer(&BufferDesc {
        size: FRAME_SIZE,
        usage: BufferUsage::UNIFORM,
        memory: MemoryLocation::Upload,
    })?;
    let instances = device.create_buffer(&BufferDesc {
        size: capacity as u64 * INSTANCE_SIZE,
        usage: BufferUsage::SHADER_READ,
        memory: MemoryLocation::Upload,
    })?;
    Ok(FrameData {
        frame_view: device.create_buffer_view(
            &frame,
            &BufferViewDesc::whole(BufferViewKind::Uniform, false),
        )?,
        instances_view: device.create_buffer_view(
            &instances,
            &BufferViewDesc::whole(
                BufferViewKind::Structured {
                    stride: INSTANCE_SIZE as u32,
                },
                false,
            ),
        )?,
        frame,
        instances,
        capacity,
    })
}
