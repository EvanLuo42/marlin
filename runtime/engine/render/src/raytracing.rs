//! The ray tracing scene (0007 §9, 0009 §3.1; R3a): a BLAS per mesh from its RT LOD, and a TLAS
//! over the GPU Scene's instances, rebuilt every frame. Ray queries in any pass trace it
//! ([`RtScene`]).
//!
//! # BLAS
//!
//! [`Meshes`](crate::Meshes) owns them. Each uploaded mesh's RT LOD blob (streamed like its
//! pages) goes whole into the RT geometry pool once it is in memory; then
//! [`Meshes::prepare_rt`](crate::Meshes::prepare_rt) sizes its BLAS, places it in the BLAS pool
//! and queues its build, as many triangles a frame as [`RtDesc::triangles_per_frame`] allows.
//! The frame's upload pass writes the geometry and the mesh's row of the BLAS table (its BLAS's
//! address, by the mesh's slot, `MeshEntry.Slot`); `BuildBlas` builds the queued ones. Both pools
//! are one buffer each, imported every frame: the BLAS pool is the graph's "AS group" (0007
//! §9.4), its accesses covering every BLAS in it. A mesh without a BLAS yet (or whose geometry
//! did not fit) has a zero row: its instances are inactive in the TLAS.
//!
//! # TLAS
//!
//! [`Tlas`] owns its storage, grown to a power of two of the instance rows (never shrunk). A
//! frame: `WriteTlasInstances` writes record i of instance row i (so a hit's `InstanceIndex` is
//! the row), inactive (BLAS address 0) for rows that are removed, hidden or without a BLAS;
//! `BuildTlas` builds it from them. Ray query passes declare [`RtScene::read`]: the TLAS and,
//! since a query reads the BLASes it instances, the BLAS pool.
//!
//! Not yet: BLAS compaction (0007 §9.7), dynamic BLAS refits (§9.8), streaming the RT LOD by
//! distance instead of with the mesh.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_render_graph::{
    Access, BufferDesc as GraphBufferDesc, BufferHandle, FRAMES_IN_FLIGHT, FrameContext,
    PassBuilder, PassContext, RenderGraph, TextureHandle,
};
use marlin_rhi::{
    ACCELERATION_STRUCTURE_ALIGNMENT, AccelerationStructure, AccelerationStructureGeometry,
    AccelerationStructureInputs, AccelerationStructureKind, AccelerationStructureTriangles, Buffer,
    BufferBarrier, BufferDesc, BufferState, BufferUsage, BuildAccelerationStructure, BuildFlags,
    ComputePipeline, ComputePipelineDesc, Device, GeometryFlags, IndexFormat, MemoryLocation,
    Result, ShaderModule, VertexFormat,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::asset::cluster::RtLod;
use crate::meshes::MeshData;
use crate::scene::GpuScene;

/// How big the ray tracing pools are, and how fast BLASes are built.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RtDesc {
    /// Bytes of RT LOD geometry (positions, attributes, indices).
    pub geometry_bytes: u64,
    /// Bytes of BLASes.
    pub blas_bytes: u64,
    /// Rows of the BLAS table: mesh slots.
    pub mesh_slots: u32,
    /// Most triangles whose BLASes a frame builds (a mesh over it builds alone).
    pub triangles_per_frame: u32,
}

impl RtDesc {
    /// The desktop default: 256 MiB of each pool (RT LODs are a quarter of the source
    /// triangles), 64 Ki mesh slots, a million triangles of BLAS builds a frame.
    pub const DEFAULT: Self = Self {
        geometry_bytes: 256 << 20,
        blas_bytes: 256 << 20,
        mesh_slots: 1 << 16,
        triangles_per_frame: 1 << 20,
    };
}

impl Default for RtDesc {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// First-fit byte ranges of a pool, merged when freed.
struct Ranges {
    /// Start → length.
    free: BTreeMap<u64, u64>,
}

impl Ranges {
    fn new(size: u64) -> Self {
        Self {
            free: BTreeMap::from([(0, size)]),
        }
    }

    /// `size` bytes at an `align`ed start.
    fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        let (start, len, at) = self.free.iter().find_map(|(&start, &len)| {
            let at = start.next_multiple_of(align);
            (at + size <= start + len).then_some((start, len, at))
        })?;
        self.free.remove(&start);
        if at > start {
            self.free.insert(start, at - start);
        }
        if at + size < start + len {
            self.free.insert(at + size, start + len - at - size);
        }
        Some(at)
    }

    fn release(&mut self, start: u64, size: u64) {
        let (mut start, mut size) = (start, size);
        if let Some((&before, &len)) = self.free.range(..start).next_back()
            && before + len == start
        {
            self.free.remove(&before);
            start = before;
            size += len;
        }
        if let Some(len) = self.free.remove(&(start + size)) {
            size += len;
        }
        self.free.insert(start, size);
    }

    fn free_bytes(&self) -> u64 {
        self.free.values().sum()
    }
}

enum RtState {
    /// Its blob is being read.
    Loading,
    /// Its blob is in memory (read, or loaded with the mesh), waiting for room or budget.
    Ready(Option<Vec<u8>>),
    /// Built (or queued to build this frame).
    Built {
        geometry: (u64, u64),
        blas: (u64, u64),
        accel: AccelerationStructure,
    },
    /// Its blob failed to read or its check.
    Failed,
}

struct RtMesh {
    data: Arc<dyn MeshData>,
    lod: RtLod,
    state: RtState,
}

/// A BLAS build queued for the frame.
#[derive(Clone, Copy)]
pub(crate) struct BlasBuild {
    slot: u32,
    positions: u64,
    vertex_count: u32,
    indices: u64,
    triangle_count: u32,
    scratch: u64,
}

/// The frame's RT writes and builds ([`Meshes::take_uploads`](crate::Meshes::take_uploads)).
#[derive(Default)]
pub(crate) struct RtUploads {
    pub(crate) geometry_ops: Vec<[u32; 4]>,
    pub(crate) geometry_data: Vec<u32>,
    pub(crate) table_ops: Vec<[u32; 4]>,
    pub(crate) table_data: Vec<u32>,
    pub(crate) mesh_table_ops: Vec<[u32; 4]>,
    pub(crate) mesh_table_data: Vec<u32>,
    pub(crate) builds: Vec<BlasBuild>,
    /// Scratch bytes the builds need, each at an aligned offset.
    pub(crate) scratch: u64,
}

/// The meshes' RT geometry and BLASes (see the module docs).
pub(crate) struct RtGeometry {
    pub(crate) geometry: Buffer,
    pub(crate) blas: Buffer,
    pub(crate) table: Buffer,
    /// Per mesh slot: where its RT LOD lies in `geometry` (`RtMeshRow`, [`RT_MESH_WORDS`] words),
    /// for ray hits' attributes.
    pub(crate) mesh_table: Buffer,
    geometry_free: Ranges,
    blas_free: Ranges,
    slots: u32,
    meshes: BTreeMap<u32, RtMesh>,
    /// Slot, byte offset, bytes.
    geometry_writes: Vec<(u32, u64, Vec<u8>)>,
    /// Slot → BLAS address (0: none).
    table_writes: BTreeMap<u32, u64>,
    /// Slot → its `RtMeshRow`.
    mesh_writes: BTreeMap<u32, [u32; RT_MESH_WORDS]>,
    builds: Vec<BlasBuild>,
    pub(crate) triangles_per_frame: u32,
}

impl RtGeometry {
    pub(crate) fn new(device: &Device, desc: &RtDesc) -> Result<Self> {
        let slots = desc.mesh_slots.max(1);
        let geometry = device.create_buffer(&BufferDesc {
            size: desc.geometry_bytes.max(256),
            usage: BufferUsage::SHADER_READ
                | BufferUsage::SHADER_WRITE
                | BufferUsage::ACCELERATION_STRUCTURE_INPUT,
            memory: MemoryLocation::GpuOnly,
        })?;
        geometry.set_name("RtGeometry");
        let blas = device.create_buffer(&BufferDesc {
            size: desc.blas_bytes.max(ACCELERATION_STRUCTURE_ALIGNMENT),
            usage: BufferUsage::ACCELERATION_STRUCTURE_STORAGE,
            memory: MemoryLocation::GpuOnly,
        })?;
        blas.set_name("BlasPool");
        let table = zeroed_buffer(
            device,
            u64::from(slots) * 8,
            BufferUsage::SHADER_READ | BufferUsage::SHADER_WRITE,
        )?;
        table.set_name("BlasTable");
        let mesh_table = zeroed_buffer(
            device,
            u64::from(slots) * 4 * RT_MESH_WORDS as u64,
            BufferUsage::SHADER_READ | BufferUsage::SHADER_WRITE,
        )?;
        mesh_table.set_name("RtMeshTable");
        Ok(Self {
            geometry_free: Ranges::new(geometry.desc().size),
            blas_free: Ranges::new(blas.desc().size),
            geometry,
            blas,
            table,
            mesh_table,
            slots,
            meshes: BTreeMap::new(),
            geometry_writes: Vec::new(),
            table_writes: BTreeMap::new(),
            mesh_writes: BTreeMap::new(),
            builds: Vec::new(),
            triangles_per_frame: desc.triangles_per_frame,
        })
    }

    /// Registers the mesh in `slot` and starts reading its RT LOD if it is not in memory.
    pub(crate) fn add(&mut self, slot: u32, data: Arc<dyn MeshData>) {
        let lod = data.view().cluster_dag.header.rt;
        if lod.triangle_count == 0 || lod.vertex_count == 0 || slot >= self.slots {
            return;
        }
        let state = match data.bulk(lod.bulk as usize) {
            Some(_) => RtState::Ready(None),
            None => {
                data.request(lod.bulk as usize);
                RtState::Loading
            }
        };
        self.meshes.insert(slot, RtMesh { data, lod, state });
    }

    /// Frees the mesh in `slot`: its geometry, its BLAS, its table row.
    ///
    /// # Safety
    /// The GPU has finished every frame that traced it.
    pub(crate) unsafe fn remove(&mut self, slot: u32) {
        let Some(mesh) = self.meshes.remove(&slot) else {
            return;
        };
        if let RtState::Built { geometry, blas, .. } = mesh.state {
            self.geometry_free.release(geometry.0, geometry.1);
            self.blas_free.release(blas.0, blas.1);
            self.table_writes.insert(slot, 0);
        }
        self.geometry_writes.retain(|w| w.0 != slot);
        self.builds.retain(|b| b.slot != slot);
    }

    /// Takes the RT LODs read since, then places and queues as many BLASes as the budget allows.
    pub(crate) fn prepare(&mut self, device: &Device) -> Result<()> {
        let mut budget = self.triangles_per_frame;
        let mut first = true;
        let slots: Vec<u32> = self.meshes.keys().copied().collect();
        for slot in slots {
            let Some(mesh) = self.meshes.get_mut(&slot) else {
                continue;
            };
            let bulk = mesh.lod.bulk as usize;
            if matches!(mesh.state, RtState::Loading) {
                mesh.state = match mesh.data.take(bulk) {
                    None => continue,
                    Some(Ok(bytes)) => RtState::Ready(Some(bytes)),
                    Some(Err(())) => RtState::Failed,
                };
            }
            if !matches!(mesh.state, RtState::Ready(_)) {
                continue;
            }
            let triangles = mesh.lod.triangle_count;
            // A mesh over the budget still builds, alone.
            if triangles > budget && !first {
                continue;
            }
            match self.place(device, slot)? {
                Placed::Yes => {
                    budget = budget.saturating_sub(triangles);
                    first = false;
                }
                Placed::NoRoom => {}
            }
        }
        Ok(())
    }

    fn place(&mut self, device: &Device, slot: u32) -> Result<Placed> {
        let Some(mesh) = self.meshes.get_mut(&slot) else {
            return Ok(Placed::NoRoom);
        };
        let lod = mesh.lod;
        let bytes: Vec<u8> = match &mesh.state {
            RtState::Ready(Some(b)) => b.clone(),
            RtState::Ready(None) => match mesh.data.bulk(lod.bulk as usize) {
                Some(b) => b.to_vec(),
                None => {
                    mesh.state = RtState::Failed;
                    return Ok(Placed::NoRoom);
                }
            },
            _ => return Ok(Placed::NoRoom),
        };
        if mesh.data.view().cluster_dag.check_rt(&bytes).is_err()
            || lod.positions.size < lod.vertex_count * 12
            || lod.indices.size < lod.triangle_count * 12
        {
            mesh.state = RtState::Failed;
            return Ok(Placed::NoRoom);
        }
        let size = (bytes.len() as u64).next_multiple_of(16);
        let Some(at) = self.geometry_free.alloc(size, 256) else {
            return Ok(Placed::NoRoom);
        };
        let positions = at + u64::from(lod.positions.offset);
        let indices = at + u64::from(lod.indices.offset);
        let triangles = AccelerationStructureTriangles {
            vertices: &self.geometry,
            vertex_offset: positions,
            vertex_stride: 12,
            vertex_format: VertexFormat::Float3,
            vertex_count: lod.vertex_count,
            indices: Some((&self.geometry, indices, IndexFormat::U32)),
            triangle_count: lod.triangle_count,
            transform: None,
            flags: GeometryFlags::OPAQUE,
        };
        let geometries = [AccelerationStructureGeometry::Triangles(triangles)];
        let sizes = device.acceleration_structure_sizes(
            &AccelerationStructureInputs::BottomLevel(&geometries),
            BLAS_FLAGS,
        )?;
        let blas_size = sizes
            .size
            .next_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT);
        let Some(blas_at) = self
            .blas_free
            .alloc(blas_size, ACCELERATION_STRUCTURE_ALIGNMENT)
        else {
            self.geometry_free.release(at, size);
            return Ok(Placed::NoRoom);
        };
        let accel = device.create_acceleration_structure_in(
            &self.blas,
            blas_at,
            AccelerationStructureKind::BottomLevel,
            blas_size,
        )?;
        self.table_writes.insert(slot, accel.instance_address());
        let header = mesh.data.view().header;
        self.mesh_writes.insert(
            slot,
            [
                at as u32,
                lod.positions.offset,
                lod.attributes.offset,
                lod.indices.offset,
                lod.attributes.size / lod.vertex_count.max(1),
                header.uv_sets,
                lod.vertex_count,
                lod.triangle_count,
            ],
        );
        self.geometry_writes.push((slot, at, bytes));
        self.builds.push(BlasBuild {
            slot,
            positions,
            vertex_count: lod.vertex_count,
            indices,
            triangle_count: lod.triangle_count,
            scratch: sizes
                .build_scratch_size
                .next_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT),
        });
        mesh.state = RtState::Built {
            geometry: (at, size),
            blas: (blas_at, blas_size),
            accel,
        };
        Ok(Placed::Yes)
    }

    /// The frame's writes and builds.
    pub(crate) fn take(&mut self) -> RtUploads {
        let mut out = RtUploads::default();
        for (_, at, bytes) in core::mem::take(&mut self.geometry_writes) {
            let first = out.geometry_data.len() as u32;
            out.geometry_data.extend(bytes.chunks(4).map(|c| {
                u32::from_le_bytes(core::array::from_fn(|i| c.get(i).copied().unwrap_or(0)))
            }));
            out.geometry_ops
                .push([at as u32, first, out.geometry_data.len() as u32 - first, 0]);
        }
        for (slot, address) in core::mem::take(&mut self.table_writes) {
            out.table_ops
                .push([slot * 8, out.table_data.len() as u32, 2, 0]);
            out.table_data
                .extend([address as u32, (address >> 32) as u32]);
        }
        for (slot, row) in core::mem::take(&mut self.mesh_writes) {
            out.mesh_table_ops.push([
                slot * 4 * RT_MESH_WORDS as u32,
                out.mesh_table_data.len() as u32,
                RT_MESH_WORDS as u32,
                0,
            ]);
            out.mesh_table_data.extend(row);
        }
        out.builds = core::mem::take(&mut self.builds);
        out.scratch = out.builds.iter().map(|b| b.scratch).sum();
        out
    }

    /// The BLAS of the mesh in `slot`, if built.
    pub(crate) fn accel(&self, slot: u32) -> Option<&AccelerationStructure> {
        match &self.meshes.get(&slot)?.state {
            RtState::Built { accel, .. } => Some(accel),
            _ => None,
        }
    }

    pub(crate) fn built(&self) -> usize {
        self.meshes
            .values()
            .filter(|m| matches!(m.state, RtState::Built { .. }))
            .count()
    }

    pub(crate) fn free_blas_bytes(&self) -> u64 {
        self.blas_free.free_bytes()
    }

    /// Declares the BLAS builds of `rt` into the pool `blas`, reading `geometry`.
    pub(crate) fn add_builds<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        geometry: BufferHandle<'g>,
        blas: BufferHandle<'g>,
        builds: Vec<BlasBuild>,
        scratch: u64,
    ) -> BufferHandle<'g> {
        if builds.is_empty() {
            return blas;
        }
        // Rounded up so a frame's builds change the plan little.
        let scratch = g.create_buffer(
            "BlasScratch",
            GraphBufferDesc::new(scratch.next_power_of_two().max(1 << 16)),
        );
        g.add_compute("BuildBlas", |b| {
            let geometry = b.read_buffer(geometry, Access::AccelInput);
            let scratch = b.write_buffer(scratch, Access::AccelScratch);
            let blas = b.write_buffer(blas, Access::AccelWrite);
            (blas, move |ctx: &mut PassContext<'_>| {
                let vertices = ctx.accel_buffer(geometry, Access::AccelInput);
                let scratch = ctx.accel_buffer(scratch, Access::AccelScratch);
                let mut offset = 0;
                for build in &builds {
                    let Some(dst) = self.accel(build.slot) else {
                        continue;
                    };
                    let geometries = [AccelerationStructureGeometry::Triangles(
                        AccelerationStructureTriangles {
                            vertices,
                            vertex_offset: build.positions,
                            vertex_stride: 12,
                            vertex_format: VertexFormat::Float3,
                            vertex_count: build.vertex_count,
                            indices: Some((vertices, build.indices, IndexFormat::U32)),
                            triangle_count: build.triangle_count,
                            transform: None,
                            flags: GeometryFlags::OPAQUE,
                        },
                    )];
                    ctx.build_acceleration_structure(&BuildAccelerationStructure {
                        dst,
                        inputs: AccelerationStructureInputs::BottomLevel(&geometries),
                        flags: BLAS_FLAGS,
                        update: None,
                        scratch,
                        scratch_offset: offset,
                    });
                    offset += build.scratch;
                }
            })
        })
    }
}

enum Placed {
    Yes,
    NoRoom,
}

/// Static BLASes: built once, traced often.
/// Words of an `RtMeshRow` (`Scene/RayTracing.mshader`): a mesh slot's RT LOD in the geometry pool.
pub const RT_MESH_WORDS: usize = 8;

const BLAS_FLAGS: BuildFlags = BuildFlags::PREFER_FAST_TRACE;

/// A new GPU buffer of `size` zero bytes, cleared before this returns.
fn zeroed_buffer(device: &Device, size: u64, usage: BufferUsage) -> Result<Buffer> {
    let buffer = device.create_buffer(&BufferDesc {
        size,
        usage: usage | BufferUsage::SHADER_WRITE,
        memory: MemoryLocation::GpuOnly,
    })?;
    let mut pool = device.create_command_pool()?;
    let mut list = device.create_command_list(&pool)?;
    let fence = device.create_fence(0)?;
    list.begin(&mut pool)?;
    list.fill_buffer(&buffer, 0, size, 0);
    list.buffer_barriers(&[BufferBarrier {
        buffer: &buffer,
        before: BufferState::CLEAR_STORAGE,
        after: BufferState::SHADER_READ,
    }]);
    list.end()?;
    // SAFETY: everything the list uses lives until the wait below returns.
    unsafe { device.queue().submit(&[&list], Some((&fence, 1)))? };
    fence.wait(1, None)?;
    Ok(buffer)
}

/// The ray tracing passes' shaders (`Marlin.Engine.Scene`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RtShaders<'a> {
    pub tlas_instances: ShaderModule<'a>,
    pub trace_rays: ShaderModule<'a>,
}

impl<'a> RtShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 2] = [
        EntryDesc::compute("Marlin.Engine.Scene.WriteTlasInstances").push::<TlasArgs>(),
        EntryDesc::compute("Marlin.Engine.Scene.TraceRays").push::<TraceArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([tlas_instances, trace_rays]: [ShaderModule<'a>; 2]) -> Self {
        Self {
            tlas_instances,
            trace_rays,
        }
    }

    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// `Marlin.Engine.Scene.TlasArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct TlasArgs {
    instances: u32,
    transforms: u32,
    meshes: u32,
    blas_table: u32,
    records: u32,
    rows: u32,
    capacity: u32,
    translucent: u32,
    materials: u32,
}

/// `Marlin.Engine.Scene.TraceArgs`.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default)]
struct TraceArgs {
    tlas: u32,
    rays: u32,
    hits: u32,
    count: u32,
    sources: HitSources,
}

/// `Marlin.Ray`: from `origin` along `direction`, hitting between `t_min` and `t_max`.
#[repr(C)]
#[derive(Pod, Clone, Copy, PartialEq, Debug, Default)]
pub struct Ray {
    pub origin: [f32; 3],
    pub t_min: f32,
    pub direction: [f32; 3],
    pub t_max: f32,
}

/// A ray's closest hit, as [`Tlas::add_trace`] reads it back.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RayHit {
    /// The GPU Scene instance row.
    pub instance: u32,
    pub primitive: u32,
    pub t: f32,
    pub front_face: bool,
    /// What it hit (`SurfaceOfHit`): the world position, the shading and geometric normals (on
    /// the ray's side), UV0.
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub geometric_normal: [f32; 3],
    pub uv: [f32; 2],
}

impl RayHit {
    /// The hits of [`Tlas::add_trace`]'s read back buffer (64 bytes a ray): `None` for misses.
    pub fn read(bytes: &[u8]) -> Vec<Option<Self>> {
        bytes
            .as_chunks::<64>()
            .0
            .iter()
            .map(|c| {
                let w = |i: usize| u32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]);
                let f = |i: usize| f32::from_bits(w(i));
                (w(0) != u32::MAX).then(|| Self {
                    instance: w(0),
                    primitive: w(4),
                    t: f(8),
                    front_face: w(12) != 0,
                    position: [f(16), f(20), f(24)],
                    normal: [f(32), f(36), f(40)],
                    geometric_normal: [f(48), f(52), f(56)],
                    uv: [f(28), f(44)],
                })
            })
            .collect()
    }
}

/// The frame's ray tracing scene: what ray query passes read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RtScene<'g> {
    /// The TLAS's storage, after its build.
    pub tlas: BufferHandle<'g>,
    /// The BLAS pool, after the frame's builds.
    pub blas: BufferHandle<'g>,
    /// What a hit's attributes come from ([`read_hits`](Self::read_hits)): the RT LOD geometry
    /// pool, where each mesh slot's lies in it (`RtMeshRow`), and the GPU Scene's instance,
    /// mesh and transform tables (a hit's `InstanceIndex` is its instance row).
    pub geometry: BufferHandle<'g>,
    pub mesh_table: BufferHandle<'g>,
    pub instances: BufferHandle<'g>,
    pub meshes: BufferHandle<'g>,
    pub transforms: BufferHandle<'g>,
    /// The TLAS's bindless index: a shader's `AccelerationStructure`.
    pub index: u32,
}

/// A pass's views of a hit's attribute sources, as declared by [`RtScene::read_hits`].
#[derive(Clone, Copy)]
pub struct HitViews<'g> {
    geometry: BufferHandle<'g>,
    mesh_table: BufferHandle<'g>,
    instances: BufferHandle<'g>,
    meshes: BufferHandle<'g>,
    transforms: BufferHandle<'g>,
}

/// `Marlin.Engine.Scene.HitSources`: a hit's attribute sources in a shader's arguments.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct HitSources {
    pub geometry: u32,
    pub mesh_table: u32,
    pub instances: u32,
    pub meshes: u32,
    pub transforms: u32,
}

impl<'g> HitViews<'g> {
    /// The views' bindless indices, for a pass's arguments.
    pub fn bind(&self, ctx: &PassContext<'_>) -> HitSources {
        HitSources {
            geometry: ctx.srv(self.geometry),
            mesh_table: ctx.srv(self.mesh_table),
            instances: ctx.srv(self.instances),
            meshes: ctx.srv(self.meshes),
            transforms: ctx.srv(self.transforms),
        }
    }
}

impl<'g> RtScene<'g> {
    /// Declares a ray query pass's reads: the TLAS and the BLASes it instances.
    pub fn read(&self, b: &mut PassBuilder<'_, 'g>) {
        b.read_buffer(self.tlas, Access::AccelRead);
        b.read_buffer(self.blas, Access::AccelRead);
    }

    /// Declares a pass's reads of a hit's attribute sources, besides [`read`](Self::read).
    pub fn read_hits(&self, b: &mut PassBuilder<'_, 'g>) -> HitViews<'g> {
        HitViews {
            geometry: b.read_buffer(self.geometry, Access::ComputeRead),
            mesh_table: b.read_buffer(self.mesh_table, Access::ComputeRead),
            instances: b.read_buffer(self.instances, Access::ComputeRead),
            meshes: b.read_buffer(self.meshes, Access::ComputeRead),
            transforms: b.read_buffer(self.transforms, Access::ComputeRead),
        }
    }
}

/// The TLAS's storage: grown, never shrunk.
struct TlasStorage {
    buffer: Buffer,
    accel: AccelerationStructure,
    capacity: u32,
    scratch: u64,
}

/// The TLAS over the GPU Scene's instances (see the module docs).
pub struct Tlas {
    write_instances: ComputePipeline,
    trace: ComputePipeline,
    /// For size queries: any input buffer does.
    probe: Buffer,
    storage: Option<TlasStorage>,
    /// Storage outgrown, with the frame it was replaced in.
    retired: Vec<(u64, TlasStorage)>,
    frame: u64,
}

/// The fewest instance records the TLAS holds.
const MIN_TLAS_CAPACITY: u32 = 64;

impl Tlas {
    pub fn new(device: &Device, shaders: &RtShaders<'_>) -> Result<Self> {
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        let probe = device.create_buffer(&BufferDesc {
            size: 64,
            usage: BufferUsage::ACCELERATION_STRUCTURE_INPUT,
            memory: MemoryLocation::GpuOnly,
        })?;
        Ok(Self {
            write_instances: compute(shaders.tlas_instances)?,
            trace: compute(shaders.trace_rays)?,
            probe,
            storage: None,
            retired: Vec::new(),
            frame: 0,
        })
    }

    /// Makes room for `rows` instance rows (call once a frame, before declaring the frame);
    /// outgrown storage is retired, and released by [`release`](Self::release) once no frame
    /// uses it.
    pub fn prepare(&mut self, device: &Device, rows: u32) -> Result<()> {
        self.frame += 1;
        let need = rows.max(MIN_TLAS_CAPACITY).next_power_of_two();
        if self.storage.as_ref().is_some_and(|s| s.capacity >= need) {
            return Ok(());
        }
        let sizes = device.acceleration_structure_sizes(
            &AccelerationStructureInputs::TopLevel {
                instances: &self.probe,
                offset: 0,
                count: need,
            },
            TLAS_FLAGS,
        )?;
        let size = sizes
            .size
            .next_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT);
        let buffer = device.create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::ACCELERATION_STRUCTURE_STORAGE,
            memory: MemoryLocation::GpuOnly,
        })?;
        buffer.set_name("Tlas");
        let accel = device.create_acceleration_structure_in(
            &buffer,
            0,
            AccelerationStructureKind::TopLevel,
            size,
        )?;
        let new = TlasStorage {
            buffer,
            accel,
            capacity: need,
            scratch: sizes
                .build_scratch_size
                .next_multiple_of(ACCELERATION_STRUCTURE_ALIGNMENT),
        };
        if let Some(old) = self.storage.replace(new) {
            self.retired.push((self.frame, old));
        }
        Ok(())
    }

    /// Releases retired storage no frame in flight uses: `frames` forgets it first.
    pub fn release(&mut self, frames: &mut FrameContext) {
        let frame = self.frame;
        self.retired.retain(|(at, storage)| {
            let done = frame > at + FRAMES_IN_FLIGHT as u64;
            if done {
                frames.forget_buffer(&storage.buffer);
            }
            !done
        });
    }

    /// Instance records the TLAS holds (0 before [`prepare`](Self::prepare)).
    pub fn capacity(&self) -> u32 {
        self.storage.as_ref().map_or(0, |s| s.capacity)
    }

    /// Declares the frame's TLAS over `rows` instance rows of `gpu` (`rows` at most what
    /// [`prepare`](Self::prepare) made room for), the BLASes in `pool`.
    pub fn add_passes<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        gpu: GpuScene<'g>,
        pool: &crate::PagePool<'g>,
        rows: u32,
    ) -> RtScene<'g> {
        let storage = self
            .storage
            .as_ref()
            .expect("Tlas::prepare before Tlas::add_passes");
        assert!(rows <= storage.capacity, "prepare made room for fewer rows");
        let capacity = storage.capacity;
        let records = g.create_buffer(
            "TlasInstances",
            GraphBufferDesc::new(u64::from(capacity) * 64),
        );
        let pipeline = &self.write_instances;
        let table = pool.blas_table;
        let records = g.add_compute("WriteTlasInstances", |b| {
            let instances = b.read_buffer(gpu.instance_data, Access::ComputeRead);
            let transforms = b.read_buffer(gpu.instance_transforms, Access::ComputeRead);
            let materials = b.read_buffer(gpu.materials, Access::ComputeRead);
            let translucent = gpu.translucent;
            let meshes = b.read_buffer(gpu.meshes, Access::ComputeRead);
            let table = b.read_buffer(table, Access::ComputeRead);
            let records = b.write_buffer(records, Access::ComputeWrite);
            (records, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                ctx.push(&TlasArgs {
                    instances: ctx.srv(instances),
                    transforms: ctx.srv(transforms),
                    meshes: ctx.srv(meshes),
                    blas_table: ctx.srv(table),
                    records: ctx.uav(records),
                    rows,
                    capacity,
                    translucent,
                    materials: ctx.srv(materials),
                });
                ctx.dispatch(capacity.div_ceil(64), 1, 1);
            })
        });
        let tlas = g.import_buffer("Tlas", &storage.buffer);
        let scratch = g.create_buffer(
            "TlasScratch",
            GraphBufferDesc::new(storage.scratch.max(256)),
        );
        let blas = pool.blas;
        let tlas = g.add_compute("BuildTlas", |b| {
            let records = b.read_buffer(records, Access::AccelInput);
            b.read_buffer(blas, Access::AccelRead);
            let scratch = b.write_buffer(scratch, Access::AccelScratch);
            let tlas = b.write_buffer(tlas, Access::AccelWrite);
            (tlas, move |ctx: &mut PassContext<'_>| {
                let instances = ctx.accel_buffer(records, Access::AccelInput);
                let scratch = ctx.accel_buffer(scratch, Access::AccelScratch);
                ctx.build_acceleration_structure(&BuildAccelerationStructure {
                    dst: &storage.accel,
                    inputs: AccelerationStructureInputs::TopLevel {
                        instances,
                        offset: 0,
                        count: capacity,
                    },
                    flags: TLAS_FLAGS,
                    update: None,
                    scratch,
                    scratch_offset: 0,
                });
            })
        });
        RtScene {
            tlas,
            blas,
            geometry: pool.rt_geometry,
            mesh_table: pool.rt_mesh_table,
            instances: gpu.instance_data,
            meshes: gpu.meshes,
            transforms: gpu.instance_transforms,
            index: storage.accel.index(),
        }
    }

    /// Declares a pass tracing `rays` against `rt` (closest hits; for tools and tests): the
    /// hits, 16 bytes a ray, for [`RayHit::read`].
    pub fn add_trace<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        rt: &RtScene<'g>,
        rays: Vec<Ray>,
    ) -> BufferHandle<'g> {
        let count = rays.len() as u32;
        let hits = g.create_buffer(
            "RayHits",
            GraphBufferDesc::structured::<[u32; 4]>(4 * u64::from(count.max(1))),
        );
        let pipeline = &self.trace;
        let rt = *rt;
        g.add_compute("TraceRays", |b| {
            rt.read(b);
            let sources = rt.read_hits(b);
            let hits = b.write_buffer(hits, Access::ComputeWrite);
            (hits, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(pipeline);
                // Uploads are never empty; a count of 0 reads none of it.
                let rays = match rays.is_empty() {
                    true => ctx.upload_slice(&[Ray::default()]),
                    false => ctx.upload_slice(&rays),
                };
                ctx.push(&TraceArgs {
                    tlas: rt.index,
                    rays,
                    hits: ctx.uav(hits),
                    count,
                    sources: sources.bind(ctx),
                });
                ctx.dispatch(count.div_ceil(64).max(1), 1, 1);
            })
        })
    }
}

/// Rebuilt every frame from few instances: a fast trace is worth its build.
const TLAS_FLAGS: BuildFlags = BuildFlags::PREFER_FAST_TRACE;

/// The simplified materials ray hits read (0011 §9.1–9.2), as marlin-material's bakes write
/// them: the atlases and the per-instance factors.
/// `Marlin.Engine.Material.SimpleSources`: the simplified materials in a pass's arguments.
#[repr(C)]
#[derive(Pod, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SimpleSources {
    pub albedo: u32,
    pub specular: u32,
    pub emission: u32,
    pub factors: u32,
}

/// The frame's simplified materials, after its bakes (marlin-material's `add_simple_passes`).
#[derive(Clone, Copy)]
pub struct SimpleOutput<'g> {
    pub albedo: TextureHandle<'g>,
    pub specular: TextureHandle<'g>,
    pub emission: TextureHandle<'g>,
    pub factors: BufferHandle<'g>,
}

/// A pass's views of the simplified materials ([`SimpleOutput::read`]).
#[derive(Clone, Copy)]
pub struct SimpleViews<'g> {
    albedo: TextureHandle<'g>,
    specular: TextureHandle<'g>,
    emission: TextureHandle<'g>,
    factors: BufferHandle<'g>,
}

impl<'g> SimpleOutput<'g> {
    /// Declares a pass's reads of them.
    pub fn read(&self, b: &mut PassBuilder<'_, 'g>) -> SimpleViews<'g> {
        SimpleViews {
            albedo: b.read_texture(self.albedo, Access::ComputeRead),
            specular: b.read_texture(self.specular, Access::ComputeRead),
            emission: b.read_texture(self.emission, Access::ComputeRead),
            factors: b.read_buffer(self.factors, Access::ComputeRead),
        }
    }
}

impl SimpleViews<'_> {
    /// Their bindless indices, for a pass's arguments.
    pub fn bind(&self, ctx: &PassContext<'_>) -> SimpleSources {
        SimpleSources {
            albedo: ctx.srv(self.albedo),
            specular: ctx.srv(self.specular),
            emission: ctx.srv(self.emission),
            factors: ctx.srv(self.factors),
        }
    }
}
