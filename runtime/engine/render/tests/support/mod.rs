//! A [`RenderScene`] on the shared hardware device: its GPU Scene, its visibility passes and a
//! reference rasterizer (`Render.ReferenceVs`/`ReferencePs`: the LOD cut computed on the CPU,
//! every cluster of it drawn with a vertex shader and a depth buffer, unlit) drawn into one
//! small frame and read back. Tests change the scene the way extract does, with updates on
//! reserved handles. Meshes are cooked by the real cooker ([`cook`]).

pub mod cook;

use marlin_containers::HashMap;
use marlin_gpu_test_support::{Shader, hardware_device, shader};
use marlin_math::{Mat4, Transform, Vec3};
use marlin_render::asset::cluster::ClusterRecord;
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, MaterialDesc, MaterialId, MaterialUpdate, MeshId,
    MeshUpdate,
};
use marlin_render::visibility::counters;
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

/// Page pool slots the harness has.
pub const POOL_PAGES: u32 = 256;

const DEPTH_FORMAT: Format = Format::D32Float;

pub struct Harness {
    pub device: &'static Device,
    pub meshes: Meshes,
    /// The cooked form of every uploaded mesh, for the CPU cut.
    cooked: HashMap<MeshHandle, Cooked>,
    pub scene: RenderScene,
    scatter: ScatterShader,
    /// The visibility passes, with a budget of 0 (the finest clusters) unless a test sets one.
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
        let mut meshes = Meshes::new(device, POOL_PAGES).unwrap();
        let scatter = ScatterShader::new(device, shader("Marlin.Engine.Scene.Scatter")).unwrap();
        let mut vis = Visibility::new(
            device,
            &VisibilityShaders::from_fn(|name| shader(&format!("Marlin.Engine.Visibility.{name}"))),
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
        let mut cooked = HashMap::default();
        let (cube, _) = upload(
            device,
            &mut meshes,
            &mut cooked,
            &scene,
            &mut updates,
            &cook::cube(),
        );
        let material = scene.ids().materials.reserve();
        updates
            .materials
            .push(material, MaterialUpdate::Add(MaterialDesc::default()));
        Self {
            device,
            meshes,
            cooked,
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
            &mut self.cooked,
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

    /// The cluster the passes address as `address` (`slot << 12 | cluster`).
    pub fn cluster(&self, address: u32) -> ClusterRecord {
        let (slot, index) = (address >> 12, (address & 4095) as usize);
        for (&mesh, cooked) in &self.cooked {
            let view = cooked.view();
            let dag = view.cluster_dag;
            for page in 0..dag.pages.len() {
                if self.meshes.page_slot(mesh, page) == Some(slot) {
                    let bytes = &cooked.pages[dag.pages[page].bulk as usize];
                    return dag.page(page, bytes).unwrap().cluster(index);
                }
            }
        }
        panic!("no cluster at {address:x}");
    }

    /// The LOD cut of the scene as it is (after the last frame's updates) on the CPU, by
    /// `Marlin.Engine.Visibility`'s rule: every drawable instance's clusters whose group's error
    /// projects over the budget and whose own LOD error does not, as visible-cluster entries
    /// (row, `slot << 12 | cluster`), before any culling.
    pub fn cut(&self) -> Vec<[u32; 2]> {
        let (eye, camera) = self.camera.expect("a camera");
        let pixels = SIZE as f32 / (2.0 * (camera.fov_y * 0.5).tan());
        let budget = self.vis.lod_error_pixels;
        let mut out = Vec::new();
        for (row, mesh, transform) in self.scene.drawable() {
            let m = transform.to_matrix();
            let rows: [[f32; 4]; 3] =
                core::array::from_fn(|r| core::array::from_fn(|c| m.col(c)[r]));
            let scale = (0..3)
                .map(|c| m.col(c).truncate().length())
                .fold(0.0f32, f32::max);
            let project = |sphere: [f32; 4], error: f32| {
                let p = [sphere[0], sphere[1], sphere[2], 1.0];
                let center: [f32; 3] =
                    core::array::from_fn(|r| (0..4).map(|k| rows[r][k] * p[k]).sum::<f32>());
                let d = Vec3::from(center) - eye.translation;
                let distance = (d.length() - sphere[3] * scale).max(1e-4);
                error * scale * pixels / distance
            };
            let cooked = &self.cooked[&mesh];
            let view = cooked.view();
            let dag = view.cluster_dag;
            for (page, p) in dag.pages.iter().enumerate() {
                let slot = self.meshes.page_slot(mesh, page).unwrap();
                let pv = dag.page(page, &cooked.pages[p.bulk as usize]).unwrap();
                for i in 0..pv.header.cluster_count as usize {
                    let c = pv.cluster(i);
                    let g = &dag.groups[c.group as usize];
                    if project(g.sphere, g.error) > budget
                        && project(c.lod_sphere, c.lod_error) <= budget
                    {
                        out.push([row, slot << 12 | i as u32]);
                    }
                }
            }
        }
        out
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
        let cut = self.cut();
        let (meshes, scene, scatter, vis, reference) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.reference,
        );
        let camera = self.camera.expect("a camera");
        let list = cut.clone();
        let (tickets, capacity, levels) = self
            .frame
            .execute(self.device, |g, _| {
                let gpu = scene.add_passes(g, scatter);
                let (color, depth) = reference_pass(g, reference, list, gpu, camera);
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
                    out.levels,
                )
            })
            .unwrap();
        let [color, depth, visbuffer, vis_depth, counter_words, clusters] =
            tickets.map(|t| self.frame.wait_readback(t).unwrap());
        assert_eq!(self.device.validation_errors(), 0);
        let floats = |b: &[u8]| words(b).into_iter().map(f32::from_bits).collect::<Vec<_>>();
        let clusters = words(&clusters);
        let c = words(&counter_words);
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
            counters: [
                c[counters::INSTANCES],
                c[counters::CLUSTERS_FOUND],
                c[counters::CLUSTERS_DRAWN],
            ],
            nodes: c[counters::NODES],
            parts: c[counters::PARTS],
            clusters: clusters.as_chunks::<2>().0.to_vec(),
            capacity,
            levels,
            cut,
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

/// `Render.ReferenceArgs`: five 32-bit words.
type ReferenceArgs = [u32; 5];

/// Most triangles in a cluster: the reference draws this many per cluster, the rest degenerate.
const CLUSTER_TRIANGLES: u32 = 128;

/// The reference rasterizer's pass: color (RGBA8, cleared to black) and depth (`D32Float`,
/// reverse-Z), every cluster of `list` (row, `slot << 12 | cluster`), one instanced draw.
fn reference_pass<'g>(
    g: &mut RenderGraph<'g>,
    pipeline: &'g GraphicsPipeline,
    list: Vec<[u32; 2]>,
    gpu: GpuScene<'g>,
    (eye, camera): (Transform, Camera),
) -> (
    marlin_render_graph::TextureHandle<'g>,
    marlin_render_graph::TextureHandle<'g>,
) {
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
            if list.is_empty() {
                return;
            }
            let frame = ctx.upload(&frame);
            let clusters = ctx.upload_slice(&list);
            let args: ReferenceArgs = [
                frame,
                clusters,
                ctx.srv(transforms),
                ctx.srv(instances),
                ctx.srv(mesh_table),
            ];
            ctx.set_pipeline(pipeline);
            ctx.push_words(&args);
            ctx.draw(CLUSTER_TRIANGLES * 3, list.len() as u32, 0, 0);
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
    cooked: &mut HashMap<MeshHandle, Cooked>,
    scene: &RenderScene,
    updates: &mut SceneUpdates,
    mesh: &Cooked,
) -> (MeshId, MeshHandle) {
    let handle = meshes
        .upload_mesh(device, &mesh.view(), &mesh.pages())
        .unwrap();
    cooked.insert(handle, mesh.clone());
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
    /// Hierarchy nodes and group parts the traversal appended.
    pub nodes: u32,
    pub parts: u32,
    /// Instance row, `slot << 12 | cluster`, for each of the buffer's entries.
    pub clusters: Vec<[u32; 2]>,
    pub capacity: u32,
    /// Levels of hierarchy nodes the passes traversed.
    pub levels: u32,
    /// The CPU cut the reference drew.
    pub cut: Vec<[u32; 2]>,
}
