//! A [`RenderScene`] on the shared hardware device: its GPU Scene, its visibility passes and a
//! reference rasterizer (`Render.ReferenceVs`/`ReferencePs`: each drawable instance's LOD 0 as an
//! indexed draw with a depth buffer, unlit) drawn into one small frame and read back. Tests
//! change the scene the way extract does, with updates on reserved handles. Meshes are cooked
//! by the real cooker ([`cook`]).

pub mod cook;

use marlin_containers::HashMap;
use marlin_gpu_test_support::{Shader, hardware_device, shader};
use marlin_math::{Mat4, Transform, Vec3};
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, MaterialDesc, MaterialId, MaterialUpdate, MeshId,
    MeshUpdate,
};
use marlin_render::{
    Camera, Environment, GpuScene, MeshHandle, Meshes, RenderScene, ScatterShader, SceneUpdates,
    Visibility, VisibilityShaders,
};
use marlin_render_graph::{
    Access, FrameContext, FrameContextDesc, PassContext, Readback, RenderGraph, TextureDesc,
};
use marlin_rhi::{
    ClearValue, ColorTargetState, DepthState, Device, Format, GraphicsPipeline,
    GraphicsPipelineDesc, LoadOp, MultisampleState, PrimitiveTopology, RasterState, StoreOp,
};

use cook::Cooked;

pub const SIZE: u32 = 64;

const DEPTH_FORMAT: Format = Format::D32Float;

pub struct Harness {
    pub device: &'static Device,
    pub meshes: Meshes,
    pub scene: RenderScene,
    scatter: ScatterShader,
    /// The visibility passes, LOD 0 everywhere unless a test sets its error budget.
    pub vis: Visibility,
    reference: GraphicsPipeline,
    /// The unit cube, in the scene and on the GPU.
    pub cube: MeshId,
    /// What instances draw with: a material row of permutation 0 (nothing here reads it).
    pub material: MaterialId,
    /// Applied with the next frame.
    pub updates: SceneUpdates,
    pub camera: Option<(Transform, Camera)>,
    frame: FrameContext,
}

impl Harness {
    /// The passes, a scene lit by `environment` and the unit cube uploaded into it.
    pub fn new(environment: Environment) -> Self {
        let device = hardware_device();
        let mut meshes = Meshes::new();
        let scatter = ScatterShader::new(device, shader("Marlin.Engine.Scene.Scatter")).unwrap();
        let v = |name: &str| shader(&format!("Marlin.Engine.Visibility.{name}"));
        let mut vis = Visibility::new(
            device,
            &VisibilityShaders {
                reset: v("Reset"),
                instance_cull: v("InstanceCull"),
                instance_args: v("InstanceArgs"),
                cluster_cull: v("ClusterCull"),
                finish_args: v("FinishArgs"),
                mesh: v("VisMesh"),
                pixel: v("VisPs"),
                clear: v("VisClear"),
                depth: v("VisDepth"),
            },
        )
        .unwrap();
        vis.lod_error_pixels = 0.0;
        let own = |entry: &'static str| Shader::new(entry).in_crate(env!("CARGO_MANIFEST_DIR"));
        let reference = device
            .create_graphics_pipeline(&GraphicsPipelineDesc {
                vertex: own("Render.ReferenceVs").module(),
                pixel: Some(own("Render.ReferencePs").module()),
                topology: PrimitiveTopology::TriangleList,
                raster: RasterState::default(),
                depth: Some(DepthState::reverse_z(DEPTH_FORMAT)),
                color_targets: &[ColorTargetState::new(Format::Rgba8Unorm)],
                multisample: MultisampleState::NONE,
                primitive_restart: None,
                cache: None,
            })
            .unwrap();
        let frame = FrameContext::new(device, &FrameContextDesc::default()).unwrap();
        let scene = RenderScene::new();
        let mut updates = SceneUpdates {
            environment: Some(environment),
            ..SceneUpdates::default()
        };
        let (cube, _) = upload(device, &mut meshes, &scene, &mut updates, &cook::cube());
        let material = scene.ids().materials.reserve();
        updates
            .materials
            .push(material, MaterialUpdate::Add(MaterialDesc::default()));
        Self {
            device,
            meshes,
            scene,
            scatter,
            vis,
            reference,
            cube,
            material,
            updates,
            camera: None,
            frame,
        }
    }

    /// Uploads `mesh` and adds it to the scene, resident.
    pub fn upload(&mut self, mesh: &Cooked) -> (MeshId, MeshHandle) {
        upload(
            self.device,
            &mut self.meshes,
            &self.scene,
            &mut self.updates,
            mesh,
        )
    }

    /// Adds an instance of `mesh`.
    pub fn add(&mut self, mesh: MeshId, transform: Transform, color: [f32; 4]) -> InstanceId {
        let id = self.scene.ids().instances.reserve();
        self.updates.instances.push(
            id,
            InstanceUpdate::Add(InstanceDesc {
                mesh,
                material: self.material,
                transform,
                color,
                flags: 0,
            }),
        );
        id
    }

    pub fn set(&mut self, id: InstanceId, update: InstanceUpdate) {
        self.updates.instances.push(id, update);
    }

    /// A camera at (0, 0, `z`) looking down -Z.
    pub fn camera_at(&mut self, z: f32) {
        self.camera = Some((
            Transform::from_translation(Vec3::new(0.0, 0.0, z)),
            Camera::default(),
        ));
    }

    /// Applies the pending updates, declares the GPU Scene and reads back whatever `read` asks
    /// for of it.
    pub fn frame_with<R>(
        &mut self,
        read: impl for<'g> FnOnce(&mut RenderGraph<'g>, GpuScene<'g>) -> R,
    ) -> R {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let (scene, scatter) = (&mut self.scene, &self.scatter);
        let out = self
            .frame
            .execute(self.device, |g, _| {
                let gpu = scene.add_passes(g, scatter);
                read(g, gpu)
            })
            .unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        out
    }

    /// Applies the pending updates and draws one frame with both the reference rasterizer and
    /// the visibility passes, reading back what the visibility tests compare.
    pub fn visibility_frame(&mut self) -> VisFrame {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let (meshes, scene, scatter, vis, reference) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.reference,
        );
        let camera = self.camera.expect("a camera");
        let (tickets, capacity) = self
            .frame
            .execute(self.device, |g, _| {
                let gpu = scene.add_passes(g, scatter);
                let (color, depth) = reference_pass(g, reference, meshes, scene, gpu, camera);
                let out = vis.add_passes(g, meshes, scene, gpu, camera, SIZE, SIZE);
                (
                    [
                        g.readback_texture(color),
                        g.readback_texture(depth),
                        g.readback_texture(out.visbuffer),
                        g.readback_texture(out.depth),
                        g.readback(out.counters),
                        g.readback(out.clusters),
                    ],
                    out.capacity,
                )
            })
            .unwrap();
        let [color, depth, visbuffer, vis_depth, counters, clusters] =
            tickets.map(|t| self.frame.wait_readback(t).unwrap());
        assert_eq!(self.device.validation_errors(), 0);
        let floats = |b: &[u8]| words(b).into_iter().map(f32::from_bits).collect::<Vec<_>>();
        let clusters = words(&clusters);
        VisFrame {
            color: words(&color),
            depth: floats(&depth),
            visbuffer: visbuffer
                .as_chunks::<8>()
                .0
                .iter()
                .map(|b| u64::from_le_bytes(*b))
                .collect(),
            vis_depth: floats(&vis_depth),
            counters: words(&counters)[..3].try_into().unwrap(),
            clusters: clusters.as_chunks::<2>().0.to_vec(),
            capacity,
        }
    }

    /// Waits for the readback `ticket` of the last frame.
    pub fn wait_readback(&mut self, ticket: Readback) -> Vec<u8> {
        self.frame.wait_readback(ticket).unwrap()
    }

    /// Waits for every frame, so meshes can be released.
    #[allow(dead_code, reason = "not every test releases meshes")]
    pub fn wait_idle(&mut self) {
        self.frame.wait_idle().unwrap();
    }
}

/// `Render.ReferenceArgs` but its padding: eight 32-bit words.
type ReferenceArgs = [u32; 8];

/// The reference rasterizer's pass: color (RGBA8, cleared to black) and depth (`D32Float`,
/// reverse-Z), each drawable instance's LOD 0, one instanced draw per mesh.
fn reference_pass<'g>(
    g: &mut RenderGraph<'g>,
    pipeline: &'g GraphicsPipeline,
    meshes: &'g Meshes,
    scene: &RenderScene,
    gpu: GpuScene<'g>,
    (eye, camera): (Transform, Camera),
) -> (
    marlin_render_graph::TextureHandle<'g>,
    marlin_render_graph::TextureHandle<'g>,
) {
    let mut by_mesh: HashMap<MeshHandle, Vec<u32>> = HashMap::default();
    for (row, mesh, _) in scene.drawable() {
        by_mesh.entry(mesh).or_default().push(row);
    }
    let mut list = Vec::new();
    let mut draws = Vec::new();
    for (mesh, rows) in by_mesh {
        let Some(lod) = meshes.lod_indices(mesh, 0) else {
            continue;
        };
        draws.push((lod, list.len() as u32, rows.len() as u32));
        list.extend(rows);
    }
    let view_projection = Mat4::perspective_infinite_rh(camera.fov_y, 1.0, camera.near)
        * eye.to_matrix().inverse_affine();
    let frame = view_projection.to_cols_array();
    let color = g.create_texture(
        "Reference",
        TextureDesc::new_2d(SIZE, SIZE, Format::Rgba8Unorm)
            .with_clear(ClearValue::Color([0.0, 0.0, 0.0, 1.0])),
    );
    let depth = g.create_texture(
        "ReferenceDepth",
        TextureDesc::new_2d(SIZE, SIZE, DEPTH_FORMAT).with_clear(ClearValue::Depth(0.0)),
    );
    g.add_raster("Reference", |b| {
        let color = b.color(color, LoadOp::Clear([0.0, 0.0, 0.0, 1.0]), StoreOp::Store);
        let depth = b.depth(depth, LoadOp::Clear(0.0), StoreOp::Store);
        let transforms = b.read_buffer(gpu.instance_transforms, Access::VertexRead);
        let instances = b.read_buffer(gpu.instance_data, Access::VertexRead);
        let mesh_table = b.read_buffer(gpu.meshes, Access::VertexRead);
        ((color, depth), move |ctx: &mut PassContext<'_>| {
            if draws.is_empty() {
                return;
            }
            let frame = ctx.upload(&frame);
            let list = ctx.upload_slice(&list);
            let tables = [transforms, instances, mesh_table].map(|h| ctx.srv(h));
            ctx.set_pipeline(pipeline);
            for (lod, first, count) in &draws {
                ctx.resident_index_buffer(lod.buffer, lod.offset, lod.format);
                let args: ReferenceArgs = [
                    frame,
                    list,
                    tables[0],
                    tables[1],
                    tables[2],
                    lod.geometry,
                    0,
                    *first,
                ];
                ctx.push_words(&args);
                ctx.draw_indexed(lod.count, *count, 0, 0, 0);
            }
        })
    })
}

/// Little-endian 32-bit words of `bytes`.
pub fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&p| u32::from_le_bytes(p))
        .collect()
}

fn upload(
    device: &Device,
    meshes: &mut Meshes,
    scene: &RenderScene,
    updates: &mut SceneUpdates,
    mesh: &Cooked,
) -> (MeshId, MeshHandle) {
    let handle = meshes
        .upload_mesh(device, &mesh.view(), &mesh.pages())
        .unwrap();
    let id = scene.ids().meshes.reserve();
    updates.meshes.push(id, MeshUpdate::Add);
    let resident = meshes.resident_mesh(handle).unwrap();
    updates.meshes.push(id, MeshUpdate::Resident(resident));
    (id, handle)
}

/// One frame of [`Harness::visibility_frame`]: every image row-major, `SIZE` × `SIZE`.
pub struct VisFrame {
    /// The reference rasterizer's color (RGBA8, R lowest: each pixel its instance's color) and
    /// depth.
    pub color: Vec<u32>,
    pub depth: Vec<f32>,
    pub visbuffer: Vec<u64>,
    pub vis_depth: Vec<f32>,
    /// Visible instances, visible clusters found, clusters drawn.
    pub counters: [u32; 3],
    /// Instance row, meshlet | LOD << 24, for each of the buffer's entries.
    pub clusters: Vec<[u32; 2]>,
    pub capacity: u32,
}
