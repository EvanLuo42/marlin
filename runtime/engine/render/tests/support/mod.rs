//! A [`RenderScene`] on the shared hardware device: its GPU Scene, its visibility passes and a
//! reference rasterizer (`Render.ReferenceVs`/`ReferencePs`: the LOD cut computed on the CPU,
//! every cluster of it drawn with a vertex shader and a depth buffer, unlit) drawn into one
//! small frame and read back. Tests change the scene the way extract does, with updates on
//! reserved handles. Meshes are cooked by the real cooker ([`cook`]).

pub mod cook;

use std::sync::Arc;

use marlin_containers::HashMap;
use marlin_gpu_test_support::{Shader, hardware_device, shader};
use marlin_math::{Mat4, Transform, Vec3};
use marlin_render::asset::cluster::ClusterRecord;
use marlin_render::scene::{
    InstanceDesc, InstanceId, InstanceUpdate, MaterialDesc, MaterialId, MaterialUpdate, MeshId,
    MeshUpdate,
};
use marlin_render::shadows::{Vsm, VsmSettings, VsmShaders};
use marlin_render::visibility::{ViewDesc, counters};
use marlin_render::{
    Camera, Environment, Feedback, GpuScene, MeshData, MeshHandle, Meshes, PagePool, PagePoolDesc,
    RenderScene, ScatterShader, SceneUpdates, StreamingShaders, Visibility, VisibilityShaders,
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

/// A small clipmap for the harness's small scenes: 4 levels of 1024² texels, 8 × 8 pages a
/// level, level 0 4 units across; a 64-page pool.
pub const SHADOW_SETTINGS: VsmSettings = VsmSettings {
    levels: 4,
    virtual_resolution: 1024,
    page_size: 128,
    first_texel: 4.0 / 1024.0,
    depth_range: 200.0,
    pool_pages: [8, 8],
    normal_offset: 0.0,
    depth_bias: 1.5,
};

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
    /// The sun's virtual shadow maps, at [`SHADOW_SETTINGS`].
    pub vsm: Vsm,
    /// The unit cube, in the scene and on the GPU.
    pub cube: MeshId,
    /// What instances draw with: a material row of permutation 0 (nothing here reads it).
    pub material: MaterialId,
    /// Applied with the next frame.
    pub updates: SceneUpdates,
    pub camera: Option<(Transform, Camera)>,
    /// The last visibility frame's camera: its HZB is the next frame's first pass's.
    pub previous: Option<(Transform, Camera)>,
    frame: FrameContext,
}

impl Harness {
    /// The passes, a scene lit by `environment` and the unit cube uploaded into it.
    pub fn new(environment: Environment) -> Self {
        Self::with_pool(environment, POOL_PAGES)
    }

    /// [`new`](Self::new) with a page pool of `slots` slots.
    pub fn with_pool(environment: Environment, slots: u32) -> Self {
        let device = hardware_device();
        let s = |name: &str| shader(&format!("Marlin.Engine.Streaming.{name}"));
        let mut meshes = Meshes::new(
            device,
            &PagePoolDesc {
                slots,
                table_entries: 1 << 14,
            },
            &StreamingShaders {
                upload: s("PageUpload"),
                clear: s("ClearWords"),
            },
        )
        .unwrap();
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
        let vsm = Vsm::new(
            device,
            &VsmShaders {
                mark: shader("Marlin.Engine.Shadows.VsmMark"),
                allocate: shader("Marlin.Engine.Shadows.VsmAllocate"),
                clear_args: shader("Marlin.Engine.Shadows.VsmClearArgs"),
                clear: shader("Marlin.Engine.Shadows.VsmClear"),
                mask: shader("Marlin.Engine.Shadows.VsmMask"),
            },
            SHADOW_SETTINGS,
        )
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
            Arc::new(cook::cube()),
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
            vsm,
            cube,
            material,
            updates,
            camera: None,
            previous: None,
            frame,
        }
    }

    /// Uploads `mesh` and adds it to the scene, resident.
    pub fn upload(&mut self, mesh: &Cooked) -> (MeshId, MeshHandle) {
        self.upload_from(mesh, Arc::new(mesh.clone()))
    }

    /// Uploads `mesh` with its pages coming from `data`.
    pub fn upload_from(&mut self, mesh: &Cooked, data: Arc<dyn MeshData>) -> (MeshId, MeshHandle) {
        upload(
            self.device,
            &mut self.meshes,
            &mut self.cooked,
            &self.scene,
            &mut self.updates,
            mesh,
            data,
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

    /// The LOD cut of the scene as it is (after the last frame's updates, with the pages resident
    /// now) on the CPU, by `Marlin.Engine.Visibility`'s rule: every drawable instance's clusters
    /// of whole resident groups whose error projects over the budget and whose own LOD error
    /// does not or whose source group is not resident, as visible-cluster entries (row,
    /// `slot << 12 | cluster`), before any culling.
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
            let group_resident = |g: u32| {
                let g = &dag.groups[g as usize];
                dag.parts[g.first_part as usize..][..g.part_count as usize]
                    .iter()
                    .all(|p| self.meshes.page_slot(mesh, p.page as usize).is_some())
            };
            for (page, p) in dag.pages.iter().enumerate() {
                let Some(slot) = self.meshes.page_slot(mesh, page) else {
                    continue;
                };
                let pv = dag.page(page, &cooked.pages[p.bulk as usize]).unwrap();
                for i in 0..pv.header.cluster_count as usize {
                    let c = pv.cluster(i);
                    let g = &dag.groups[c.group as usize];
                    if group_resident(c.group)
                        && project(g.sphere, g.error) > budget
                        && (project(c.lod_sphere, c.lod_error) <= budget
                            || !group_resident(c.source_group))
                    {
                        out.push([row, slot << 12 | i as u32]);
                    }
                }
            }
        }
        out
    }

    /// Applies the pending updates, declares the page pool's uploads and the GPU Scene, and reads
    /// back whatever `read` asks for of them.
    pub fn frame_with<R>(
        &mut self,
        read: impl for<'g> FnOnce(&mut RenderGraph<'g>, GpuScene<'g>, PagePool<'g>) -> R,
    ) -> R {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let uploads = self.meshes.take_uploads();
        let (meshes, scene, scatter) = (&self.meshes, &mut self.scene, &self.scatter);
        let out = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                read(g, gpu, pool)
            })
            .unwrap();
        assert_eq!(self.device.validation_errors(), 0);
        out
    }

    /// Applies the pending updates and draws one frame with both the reference rasterizer and
    /// the visibility passes, reading back what the visibility tests compare, then hands the
    /// frame's streaming feedback to the meshes.
    pub fn visibility_frame(&mut self) -> VisFrame {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        // The frame's uploads make the pages resident that the cut sees.
        let uploads = self.meshes.take_uploads();
        let cut = self.cut();
        let (meshes, scene, scatter, vis, reference) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.reference,
        );
        let camera = self.camera.expect("a camera");
        let view = ViewDesc {
            id: 0,
            camera,
            previous: self.previous,
            width: SIZE,
            height: SIZE,
        };
        self.previous = Some(camera);
        let list = cut.clone();
        let (tickets, capacity, levels, frame, hzb_tickets, two_pass) = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let (color, depth) = reference_pass(g, reference, list, gpu, pool, camera);
                let out = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                (
                    [
                        g.readback_texture(color),
                        g.readback_texture(depth),
                        g.readback_texture(out.visbuffer),
                        g.readback_texture(out.depth),
                        g.readback(out.counters),
                        g.readback(out.clusters),
                        g.readback(out.requests),
                        g.readback(out.used),
                    ],
                    out.capacity,
                    out.levels,
                    pool.frame,
                    out.hzb.map(|h| {
                        let (_, mips) = marlin_render::visibility::hzb_size(SIZE, SIZE);
                        (0..mips)
                            .map(|m| g.readback_texture(h.mip(m)))
                            .collect::<Vec<_>>()
                    }),
                    out.two_pass,
                )
            })
            .unwrap();
        let [
            color,
            depth,
            visbuffer,
            vis_depth,
            counter_words,
            clusters,
            requests,
            used,
        ] = tickets.map(|t| self.frame.wait_readback(t).unwrap());
        let hzb: Vec<Vec<f32>> = hzb_tickets
            .unwrap_or_default()
            .into_iter()
            .map(|t| {
                words(&self.frame.wait_readback(t).unwrap())
                    .into_iter()
                    .map(f32::from_bits)
                    .collect()
            })
            .collect();
        assert_eq!(self.device.validation_errors(), 0);
        let floats = |b: &[u8]| words(b).into_iter().map(f32::from_bits).collect::<Vec<_>>();
        let clusters = words(&clusters);
        let c = words(&counter_words);
        self.meshes.apply_feedback(&Feedback {
            frame,
            used: &used,
            requests: &requests,
            request_count: c[counters::REQUESTS],
        });
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
                c[counters::CLUSTERS_FOUND] + c[counters::SW_CLUSTERS_FOUND],
                c[counters::CLUSTERS_DRAWN] + c[counters::SW_CLUSTERS_DRAWN],
            ],
            hw_drawn: c[counters::CLUSTERS_DRAWN],
            sw_drawn: c[counters::SW_CLUSTERS_DRAWN],
            pass_first_sw_cluster: c[counters::PASS_FIRST_SW_CLUSTER],
            nodes: c[counters::NODES],
            parts: c[counters::PARTS],
            requests: c[counters::REQUESTS],
            occluded_instances: c[counters::OCCLUDED_INSTANCES],
            occluded_clusters: c[counters::OCCLUDED_CLUSTERS],
            pass_first_cluster: c[counters::PASS_FIRST_CLUSTER],
            two_pass,
            hzb,
            clusters: clusters.as_chunks::<2>().0.to_vec(),
            capacity,
            levels,
            cut,
        }
    }

    /// Applies the pending updates and draws one frame through the visibility passes and the
    /// sun's shadow maps, reading back the shadow mask, the depth and the pages.
    pub fn shadow_frame(&mut self) -> ShadowFrame {
        let updates = core::mem::take(&mut self.updates);
        self.scene.apply(&updates);
        let uploads = self.meshes.take_uploads();
        let camera = self.camera.expect("a camera");
        let view = ViewDesc {
            id: 0,
            camera,
            previous: self.previous,
            width: SIZE,
            height: SIZE,
        };
        self.previous = Some(camera);
        let (meshes, scene, scatter, vis, vsm) = (
            &self.meshes,
            &mut self.scene,
            &self.scatter,
            &self.vis,
            &self.vsm,
        );
        let tickets = self
            .frame
            .execute(self.device, |g, _| {
                let pool = meshes.add_passes(g, uploads);
                let gpu = scene.add_passes(g, scatter);
                let out = vis.add_passes(g, meshes, scene, gpu, pool, &view);
                let env = *scene.environment();
                let s = vsm.add_passes(
                    g, vis, meshes, scene, gpu, out.pages, out.used, out.depth, None, &env, camera,
                    SIZE, SIZE,
                );
                [
                    g.readback_texture(s.mask),
                    g.readback_texture(out.depth),
                    g.readback(s.page_table),
                    g.readback(s.counters),
                ]
            })
            .unwrap();
        let [mask, depth, page_table, counters] =
            tickets.map(|t| self.frame.wait_readback(t).unwrap());
        assert_eq!(self.device.validation_errors(), 0);
        ShadowFrame {
            mask: mask
                .as_chunks::<2>()
                .0
                .iter()
                .map(|h| marlin_render::asset::mesh::f16_to_f32(u16::from_le_bytes(*h)))
                .collect(),
            depth: words(&depth).into_iter().map(f32::from_bits).collect(),
            page_table: words(&page_table),
            marked: words(&counters)[0],
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
    pool: PagePool<'g>,
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
        // The clusters are read from the pool through the mesh table's view.
        b.read_buffer(pool.pages, Access::VertexRead);
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
    data: Arc<dyn MeshData>,
) -> (MeshId, MeshHandle) {
    let handle = meshes.upload_mesh(device, data).unwrap();
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
    /// Visible instances, visible clusters found, clusters drawn (hardware and software raster).
    pub counters: [u32; 3],
    /// Clusters drawn by the mesh shader and by the software raster, and the first software one
    /// of the last pass.
    pub hw_drawn: u32,
    pub sw_drawn: u32,
    pub pass_first_sw_cluster: u32,
    /// Hierarchy nodes and group parts the traversal appended, and the page requests it made.
    pub nodes: u32,
    pub parts: u32,
    pub requests: u32,
    /// What pass 1 found occluded (pass 2 tested it again), the first cluster pass 2 drew, and
    /// whether there was a pass 2.
    pub occluded_instances: u32,
    pub occluded_clusters: u32,
    pub pass_first_cluster: u32,
    pub two_pass: bool,
    /// The final HZB's mips, row-major (empty without occlusion culling).
    pub hzb: Vec<Vec<f32>>,
    /// Instance row, `slot << 12 | cluster`, for each of the buffer's entries: hardware raster
    /// from 0, software raster from `capacity`.
    pub clusters: Vec<[u32; 2]>,
    pub capacity: u32,
    /// Levels of hierarchy nodes the passes traversed.
    pub levels: u32,
    /// The CPU cut the reference drew.
    pub cut: Vec<[u32; 2]>,
}

impl VisFrame {
    /// The clusters drawn, hardware and software raster: (visibility buffer cluster id, entry).
    pub fn drawn(&self) -> Vec<(u32, [u32; 2])> {
        let hw = (0..self.hw_drawn).map(|i| (i, self.clusters[i as usize]));
        let sw = (0..self.sw_drawn).map(|i| {
            let id = self.capacity + i;
            (id, self.clusters[id as usize])
        });
        hw.chain(sw).collect()
    }

    /// The clusters the last pass drew (pass 2 when there was one).
    pub fn last_pass(&self) -> Vec<[u32; 2]> {
        let hw = self.pass_first_cluster..self.hw_drawn;
        let sw = self.capacity + self.pass_first_sw_cluster..self.capacity + self.sw_drawn;
        hw.chain(sw).map(|i| self.clusters[i as usize]).collect()
    }
}

/// One frame of [`Harness::shadow_frame`]: images row-major, `SIZE` × `SIZE`.
pub struct ShadowFrame {
    /// The sun's visibility: 1 lit, 0 in shadow.
    pub mask: Vec<f32>,
    pub depth: Vec<f32>,
    /// A word per virtual page of every level: its physical page or `u32::MAX`.
    pub page_table: Vec<u32>,
    /// Pages marked (taken from the pool, or not if it ran out).
    pub marked: u32,
}
