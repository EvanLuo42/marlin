//! The resident meshes and the streaming of their cluster DAG pages (0009 §3.2, §6.1): each
//! mesh's resident part in a buffer of its own, its pages streamed through the page pool
//! ([`crate::geometry`]). Also the view and lighting descriptions the passes share.
//!
//! # Streaming
//!
//! A mesh's root pages (the DAG's coarsest groups) are uploaded with it and stay; every other
//! page streams. Each frame:
//!
//! 1. The visibility passes' traversal requests every page it would draw from but finds
//!    missing (its page table entry is [`NOT_RESIDENT`](crate::geometry::NOT_RESIDENT)), with
//!    the projected error as priority, and marks every slot it reads as used. The cut draws a
//!    cluster whose finer replacement is not resident instead, so a missing page coarsens the
//!    picture, never tears it.
//! 2. Once that frame is done, [`Meshes::apply_feedback`] reads both: used slots refresh their
//!    page's last use; the requests, highest priority first, each with its sibling pages and
//!    every page they depend on (every page of their groups' parent groups), take free slots
//!    or evict the least recently used pages (each with the resident pages that depend on
//!    it), up to [`Meshes::max_page_uploads`] a frame. Pages whose bytes are not in memory
//!    (streamed blobs) are read first ([`MeshData::request`]); their request loads in a later
//!    frame, once every page of it has arrived.
//! 3. The next frame's [`Meshes::add_passes`] copies the new pages into their slots and writes
//!    the page table, before the visibility passes read either.
//!
//! A page is resident only with every page it depends on (every page of its groups' parent
//! groups), so a whole group's coarser clusters are whole too. A group split across pages is
//! drawn only once all of its pages are; its sibling pages load with a request for one.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_containers::{Handle, SlotMap};
use marlin_math::{Mat4, Transform, Vec3};
use marlin_render_graph::{
    Access, BufferDesc as GraphBufferDesc, BufferHandle, PassContext, RenderGraph,
};
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    ComputePipeline, ComputePipelineDesc, Device, Error, ErrorKind, MemoryLocation, Result,
    ShaderModule, ShaderView,
};
use marlin_serial::cooked::Pod;
use marlin_shader::asset::ShaderLibrary;
use marlin_shader::bind::{BindError, EntryDesc, bind};

use crate::asset::cluster::PAGE_SIZE;
use crate::asset::mesh::MeshView;
use crate::geometry::{self, DagCounts, NOT_RESIDENT};
use crate::raytracing::{RtDesc, RtGeometry, RtUploads};
use crate::scene::{Bounds, ResidentMesh};

/// A view's camera: put it on an entity with a `Transform`. The camera looks
/// down its local −Z with +Y up.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
    /// Full vertical field of view, radians.
    pub fov_y: f32,
    /// Near plane distance; the far plane is at infinity (reverse-Z).
    pub near: f32,
    /// This frame's subpixel offset of the projection, in pixels of the view it draws (x right,
    /// y down, within ±0.5): the upscaler's jitter (0010 §7.1, [`crate::upscale`]). Zero without
    /// one.
    pub jitter: [f32; 2],
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov_y: core::f32::consts::FRAC_PI_3,
            near: 0.1,
            jitter: [0.0; 2],
        }
    }
}

impl Camera {
    /// The camera without its jitter: for motion vectors, which leave the jitter out.
    pub fn unjittered(self) -> Self {
        Self {
            jitter: [0.0; 2],
            ..self
        }
    }
}

/// `camera`'s view-projection for a `width` × `height` view, jittered by its
/// [`Camera::jitter`]: what the view is drawn with, and what turns its pixels back into world
/// positions. Every pass of a view builds its matrix here, so they agree.
pub fn view_projection((eye, camera): (Transform, Camera), width: u32, height: u32) -> Mat4 {
    let (width, height) = (width.max(1) as f32, height.max(1) as f32);
    let projection = Mat4::perspective_infinite_rh(camera.fov_y, width / height, camera.near);
    // A clip-space offset of 2·jitter/size in NDC (y up), scaled by w.
    let jitter = Mat4::from_translation(Vec3::new(
        2.0 * camera.jitter[0] / width,
        -2.0 * camera.jitter[1] / height,
        0.0,
    ));
    jitter * projection * eye.to_matrix().inverse_affine()
}

/// A mesh uploaded with [`Meshes::upload_mesh`]. Generational: once the mesh is released the
/// handle draws nothing, even after a later upload reuses its slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MeshHandle(Handle<Mesh>);

/// Lighting shared by the whole scene, in physical units (0010 §5.1, §9).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Environment {
    /// Direction the sun's light travels (normalized when used).
    pub light_direction: Vec3,
    /// The sun's linear color: [`sun_illuminance`](Self::sun_illuminance) scales it.
    pub light_color: [f32; 3],
    /// The sun's illuminance on a surface facing it, in lux.
    pub sun_illuminance: f32,
    /// Uniform sky light, as the luminance (nits) of a sky that lights every direction alike.
    pub ambient: [f32; 3],
    /// What is seen where nothing is: linear RGB luminance in nits, and alpha.
    pub clear_color: [f32; 4],
    /// The light's angular diameter, in radians: the sun's penumbra (0010 §5.3).
    pub sun_angular_diameter: f32,
}

/// The sun's angular diameter seen from the earth, in degrees.
pub const SUN_ANGULAR_DIAMETER_DEGREES: f32 = 0.53;

/// Direct sunlight at noon, in lux.
pub const SUN_ILLUMINANCE: f32 = 100_000.0;

impl Default for Environment {
    /// Noon sun; the sky and the ambient light are R1's relative values scaled with it
    /// (×100 000/π), so the picture after auto-exposure is R1's.
    fn default() -> Self {
        Self {
            light_direction: Vec3::new(-0.4, -1.0, -0.6),
            light_color: [1.0, 0.96, 0.9],
            sun_illuminance: SUN_ILLUMINANCE,
            ambient: [2546.5, 2864.8, 3819.7],
            clear_color: [636.6, 636.6, 954.9, 1.0],
            sun_angular_diameter: SUN_ANGULAR_DIAMETER_DEGREES.to_radians(),
        }
    }
}

/// A cooked mesh the renderer streams pages from. It stays alive while the mesh is uploaded.
pub trait MeshData: Send + Sync {
    /// The checked payload.
    fn view(&self) -> MeshView<'_>;
    /// Bulk blob `index` (as `ClusterPage::bulk` counts them) if its bytes are in memory: loaded
    /// with the mesh, or every blob for data that has them all.
    fn bulk(&self, index: usize) -> Option<&[u8]>;

    /// Starts reading bulk blob `index`, which [`bulk`](Self::bulk) does not have, from where
    /// it is stored; a no-op while a read of it is on its way.
    fn request(&self, _index: usize) {}

    /// A blob [`request`](Self::request) read since, handed over (the data keeps no copy):
    /// `Some(Err(()))` if it could not be read, `None` while it is on its way or was not
    /// requested.
    fn take(&self, _index: usize) -> Option<core::result::Result<Vec<u8>, ()>> {
        None
    }
}

/// How big the page pool is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PagePoolDesc {
    /// Slots of 128 KiB pages.
    pub slots: u32,
    /// Page table entries: the pages of every uploaded mesh, resident or not.
    pub table_entries: u32,
    /// The ray tracing pools ([`crate::raytracing`]).
    pub rt: RtDesc,
}

impl PagePoolDesc {
    /// What the engine creates: 512 MiB of slots, the streaming pool a desktop AAA renderer
    /// starts from, and a page table for 256 Ki pages (32 GiB of geometry).
    pub const DEFAULT: Self = Self {
        slots: 4096,
        table_entries: 1 << 18,
        rt: RtDesc::DEFAULT,
    };
}

impl Default for PagePoolDesc {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The streaming passes' shaders (`Marlin.Engine.Streaming`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StreamingShaders<'a> {
    pub upload: ShaderModule<'a>,
    pub clear: ShaderModule<'a>,
}

impl<'a> StreamingShaders<'a> {
    /// The entries, in field order.
    pub const ENTRIES: [EntryDesc; 2] = [
        EntryDesc::compute("Marlin.Engine.Streaming.PageUpload").push::<UploadArgs>(),
        EntryDesc::compute("Marlin.Engine.Streaming.ClearWords").push::<ClearArgs>(),
    ];

    /// The shaders from `modules`, in [`ENTRIES`](Self::ENTRIES) order.
    pub fn from_modules([upload, clear]: [ShaderModule<'a>; 2]) -> Self {
        Self { upload, clear }
    }

    /// The streaming passes' shaders from `library`, checked against what they set.
    pub fn bind(library: &'a ShaderLibrary) -> core::result::Result<Self, BindError> {
        bind(library, &Self::ENTRIES).map(Self::from_modules)
    }
}

/// Most slots a pool may have: byte offsets into it stay 32-bit.
pub const MAX_POOL_SLOTS: u32 = 32 << 10;

#[derive(Clone, Copy, Debug)]
struct PageState {
    /// Its slot, or [`NOT_RESIDENT`].
    slot: u32,
    /// The frame that last read it (or uploaded it).
    last_used: u64,
    /// A root page: never evicted.
    pinned: bool,
    /// Failed its check or its read: never loaded.
    failed: bool,
    /// Its bytes are being read ([`MeshData::request`]).
    loading: bool,
}

struct Mesh {
    data: Arc<dyn MeshData>,
    _dag: Buffer,
    _dag_view: ShaderView,
    /// Its first page table entry.
    table: u32,
    pages: Vec<PageState>,
    /// Each page's bulk blob.
    bulk: Vec<u32>,
    /// Each page's dependencies (every page of its groups' parent groups), the pages that
    /// depend on it, and its siblings (the other pages of its groups).
    dependencies: Vec<Vec<u32>>,
    dependents: Vec<Vec<u32>>,
    siblings: Vec<Vec<u32>>,
    counts: DagCounts,
    resident: ResidentMesh,
    /// Streamed pages read but not in a slot yet (waiting for the rest of their request), with
    /// the frame they arrived.
    arrived: BTreeMap<u32, (Vec<u8>, u64)>,
}

/// A write the next frame's upload pass makes into the pool.
#[derive(Clone, Debug)]
enum Write {
    /// A page's bytes into its slot: bytes streamed in, or `None` to take them from the mesh's
    /// data.
    Page {
        mesh: MeshHandle,
        page: u32,
        slot: u32,
        bytes: Option<Vec<u8>>,
    },
    /// A page table entry.
    Entry {
        mesh: MeshHandle,
        entry: u32,
        value: u32,
    },
}

impl Write {
    fn mesh(&self) -> MeshHandle {
        match self {
            Self::Page { mesh, .. } | Self::Entry { mesh, .. } => *mesh,
        }
    }
}

/// The writes of one frame's upload pass: ops (destination byte, first data word, words) and
/// their data.
pub struct Uploads {
    ops: Vec<[u32; 4]>,
    data: Vec<u32>,
    frame: u64,
    rt: RtUploads,
}

impl Uploads {
    /// Pages and table entries written.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// What [`Meshes::add_passes`] makes: the pool after this frame's uploads, the used-slot marks
/// the visibility passes set, and the frame's number for its feedback.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PagePool<'g> {
    pub pages: BufferHandle<'g>,
    /// A u32 per slot, cleared: nonzero once a pass read the slot.
    pub used: BufferHandle<'g>,
    pub frame: u64,
    /// The RT LOD geometry, the BLASes and the BLAS table (a mesh slot's BLAS address), after
    /// the frame's uploads and builds ([`crate::raytracing`]).
    pub rt_geometry: BufferHandle<'g>,
    pub blas: BufferHandle<'g>,
    pub blas_table: BufferHandle<'g>,
}

/// The visibility passes' feedback of one frame, read back.
#[derive(Clone, Copy, Debug)]
pub struct Feedback<'a> {
    /// [`PagePool::frame`] of the frame.
    pub frame: u64,
    /// The used-slot marks ([`PagePool::used`]).
    pub used: &'a [u8],
    /// Page requests: (byte offset of the page's table entry in the pool, priority as f32 bits).
    pub requests: &'a [u8],
    /// Requests made (may pass what `requests` holds).
    pub request_count: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct UploadArgs {
    ops: u32,
    data: u32,
    pool: u32,
    count: u32,
}

#[repr(C)]
#[derive(Pod, Clone, Copy)]
struct ClearArgs {
    words: u32,
    count: u32,
}

/// The resident meshes and the page pool their pages stream through (see the module docs).
pub struct Meshes {
    pool: Buffer,
    pool_view: ShaderView,
    upload: ComputePipeline,
    clear: ComputePipeline,
    slots: u32,
    /// Free slots, the lowest last.
    free: Vec<u32>,
    /// The page in each slot.
    owner: Vec<Option<(MeshHandle, u32)>>,
    /// Free runs of page table entries: start → length.
    table_free: BTreeMap<u32, u32>,
    /// Owned runs: start → (length, mesh).
    tables: BTreeMap<u32, (u32, MeshHandle)>,
    meshes: SlotMap<Mesh>,
    writes: Vec<Write>,
    /// The meshes whose resident pages the last [`take_uploads`](Self::take_uploads) changed.
    changed: Vec<MeshHandle>,
    /// The frame the next [`add_passes`](Self::add_passes) declares.
    frame: u64,
    /// The newest frame whose feedback was applied.
    seen: u64,
    /// Pages the last feedback asked for that are still missing.
    wanted: usize,
    /// Pages being read.
    loading: Vec<(MeshHandle, u32)>,
    /// Most pages loaded a frame (each up to 128 KiB of upload).
    pub max_page_uploads: u32,
    /// RT LOD geometry and BLASes.
    rt: RtGeometry,
}

impl Meshes {
    /// A page pool of `desc`'s size (at most [`MAX_POOL_SLOTS`] slots) and its passes' shaders.
    pub fn new(
        device: &Device,
        desc: &PagePoolDesc,
        shaders: &StreamingShaders<'_>,
    ) -> Result<Self> {
        let slots = desc.slots.clamp(1, MAX_POOL_SLOTS);
        let entries = desc.table_entries.max(1);
        let pool = device.create_buffer(&BufferDesc {
            size: geometry::table_offset(slots) + u64::from(entries) * 4,
            usage: BufferUsage::SHADER_READ | BufferUsage::SHADER_WRITE | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })?;
        let pool_view =
            device.create_buffer_view(&pool, &BufferViewDesc::whole(BufferViewKind::Raw, false))?;
        let compute = |shader| {
            device.create_compute_pipeline(&ComputePipelineDesc {
                shader,
                cache: None,
            })
        };
        Ok(Self {
            pool,
            pool_view,
            upload: compute(shaders.upload)?,
            clear: compute(shaders.clear)?,
            slots,
            free: (0..slots).rev().collect(),
            owner: alloc::vec![None; slots as usize],
            table_free: BTreeMap::from([(0, entries)]),
            tables: BTreeMap::new(),
            meshes: SlotMap::new(),
            writes: Vec::new(),
            changed: Vec::new(),
            frame: 1,
            seen: 0,
            wanted: 0,
            loading: Vec::new(),
            max_page_uploads: 64,
            rt: RtGeometry::new(device, &desc.rt)?,
        })
    }

    /// Byte offset of page table entry `entry` in the pool.
    fn entry_offset(&self, entry: u32) -> u32 {
        (geometry::table_offset(self.slots) + u64::from(entry) * 4) as u32
    }

    /// Takes a run of `len` page table entries.
    fn alloc_table(&mut self, len: u32) -> Option<u32> {
        let (&start, &free) = self.table_free.iter().find(|(_, l)| **l >= len)?;
        self.table_free.remove(&start);
        if free > len {
            self.table_free.insert(start + len, free - len);
        }
        Some(start)
    }

    fn free_table(&mut self, start: u32, len: u32) {
        let (mut start, mut len) = (start, len);
        if let Some((&before, &l)) = self.table_free.range(..start).next_back()
            && before + l == start
        {
            self.table_free.remove(&before);
            start = before;
            len += l;
        }
        if let Some(l) = self.table_free.remove(&(start + len)) {
            len += l;
        }
        self.table_free.insert(start, len);
    }

    /// Uploads a cooked mesh: its resident part into a buffer of its own (blocking until the GPU
    /// has copied it) and its root pages into the pool (with the next frame's uploads); its other
    /// pages stream. A malformed root page is `InvalidInput`; a mesh whose root pages find no
    /// slots, or whose pages find no page table entries, is `OutOfMemory`.
    pub fn upload_mesh(&mut self, device: &Device, data: Arc<dyn MeshData>) -> Result<MeshHandle> {
        let invalid = || Error::from_kind(ErrorKind::InvalidInput);
        let out_of_memory = || Error::from_kind(ErrorKind::OutOfMemory);
        let view = data.view();
        let dag = view.cluster_dag;
        let bulk: Vec<u32> = dag.pages.iter().map(|p| p.bulk).collect();
        let dependencies: Vec<Vec<u32>> = (0..dag.pages.len())
            .map(|p| dag.page_dependencies(p).to_vec())
            .collect();
        let siblings: Vec<Vec<u32>> = (0..dag.pages.len())
            .map(|p| dag.page_siblings(p).to_vec())
            .collect();
        // Pinned: the root pages and every page of a root group, so the coarsest groups are
        // always whole, with what they depend on.
        let mut pinned: Vec<u32> = (0..dag.header.root_page_count).collect();
        pinned.extend(
            dag.groups
                .iter()
                .filter(|g| g.error == f32::INFINITY)
                .flat_map(|g| &dag.parts[g.first_part as usize..][..g.part_count as usize])
                .map(|p| p.page),
        );
        pinned.sort_unstable();
        pinned.dedup();
        let pinned = dependency_closure(&dependencies, pinned);
        for &page in &pinned {
            let bytes = data
                .bulk(dag.pages[page as usize].bulk as usize)
                .ok_or_else(invalid)?;
            if !geometry::check_page(&dag, page as usize, bytes) {
                return Err(invalid());
            }
        }
        let page_count = bulk.len() as u32;
        let table = self.alloc_table(page_count).ok_or_else(out_of_memory)?;
        let mut slots = Vec::with_capacity(pinned.len());
        for _ in &pinned {
            match self.take_slot(&[]) {
                Some(s) => slots.push(s),
                None => {
                    self.free.extend(slots.iter().rev());
                    self.free_table(table, page_count);
                    return Err(out_of_memory());
                }
            }
        }
        let dag_bytes = geometry::dag_buffer(&dag, self.entry_offset(table));
        let (dag_buffer, dag_view) = match copy_to_new_buffer(device, &dag_bytes) {
            Ok(r) => r,
            Err(e) => {
                self.free.extend(slots.iter().rev());
                self.free_table(table, page_count);
                return Err(e);
            }
        };
        let mut dependents = alloc::vec![Vec::new(); bulk.len()];
        for (p, deps) in dependencies.iter().enumerate() {
            for &d in deps {
                dependents[d as usize].push(p as u32);
            }
        }
        let h = view.header;
        let s = h.sphere;
        let resident = ResidentMesh {
            // Set below, once the slot is known.
            handle: MeshHandle(Handle::default()),
            pages: self.pool_view.index(),
            dag: dag_view.index(),
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
            slot: 0,
        };
        let counts = geometry::counts(&dag);
        let mut pages: Vec<PageState> = (0..bulk.len())
            .map(|_| PageState {
                slot: NOT_RESIDENT,
                last_used: self.frame,
                pinned: false,
                failed: false,
                loading: false,
            })
            .collect();
        for (&page, &slot) in pinned.iter().zip(&slots) {
            pages[page as usize].slot = slot;
            pages[page as usize].pinned = true;
        }
        let handle = MeshHandle(self.meshes.insert(Mesh {
            data,
            _dag: dag_buffer,
            _dag_view: dag_view,
            table,
            bulk,
            dependencies,
            dependents,
            siblings,
            counts,
            resident,
            pages,
            arrived: BTreeMap::new(),
        }));
        let Some(mesh) = self.meshes.get_mut(handle.0) else {
            unreachable!("just inserted");
        };
        mesh.resident.handle = handle;
        mesh.resident.slot = handle.0.index();
        let rt_data = mesh.data.clone();
        self.rt.add(handle.0.index(), rt_data);
        self.tables.insert(table, (page_count, handle));
        for (&page, &slot) in pinned.iter().zip(&slots) {
            self.owner[slot as usize] = Some((handle, page));
            self.writes.push(Write::Page {
                mesh: handle,
                page,
                slot,
                bytes: None,
            });
        }
        for (page, state) in mesh.pages.iter().enumerate() {
            self.writes.push(Write::Entry {
                mesh: handle,
                entry: table + page as u32,
                value: state.slot,
            });
        }
        Ok(handle)
    }

    /// A free slot, evicting for it if there is none; never evicts one of `keep`.
    fn take_slot(&mut self, keep: &[(MeshHandle, u32)]) -> Option<u32> {
        if let Some(s) = self.free.pop() {
            return Some(s);
        }
        self.evict_one(keep);
        self.free.pop()
    }

    /// Evicts the least recently used page that may go, with every resident page that depends
    /// on it: not pinned, not read in the newest frame seen, not one of `keep`.
    fn evict_one(&mut self, keep: &[(MeshHandle, u32)]) -> bool {
        let seen = self.seen;
        let mut candidates: Vec<(u64, MeshHandle, u32)> = self
            .owner
            .iter()
            .flatten()
            .filter_map(|&(m, p)| {
                let state = self.meshes.get(m.0)?.pages[p as usize];
                (!state.pinned && state.last_used < seen).then_some((state.last_used, m, p))
            })
            .collect();
        candidates.sort_unstable_by_key(|c| c.0);
        for (_, m, p) in candidates {
            let Some(mesh) = self.meshes.get(m.0) else {
                continue;
            };
            // The page and every resident page that depends on it, transitively.
            let mut closure = alloc::vec![p];
            let mut i = 0;
            while i < closure.len() {
                for &d in &mesh.dependents[closure[i] as usize] {
                    if mesh.pages[d as usize].slot != NOT_RESIDENT && !closure.contains(&d) {
                        closure.push(d);
                    }
                }
                i += 1;
            }
            let blocked = closure.iter().any(|&q| {
                let s = mesh.pages[q as usize];
                s.pinned || s.last_used >= seen || keep.contains(&(m, q))
            });
            if blocked {
                continue;
            }
            let Some(mesh) = self.meshes.get_mut(m.0) else {
                continue;
            };
            let table = mesh.table;
            for &q in &closure {
                let slot = core::mem::replace(&mut mesh.pages[q as usize].slot, NOT_RESIDENT);
                self.owner[slot as usize] = None;
                self.free.push(slot);
                self.writes.push(Write::Entry {
                    mesh: m,
                    entry: table + q,
                    value: NOT_RESIDENT,
                });
            }
            return true;
        }
        false
    }

    /// Applies one frame's feedback (see the module docs): takes the pages read since, refreshes
    /// used pages and loads requested ones, evicting for them, up to
    /// [`max_page_uploads`](Self::max_page_uploads). A requested page whose bytes are not in
    /// memory is read first ([`MeshData::request`]); its request loads once all of its pages
    /// have arrived.
    pub fn apply_feedback(&mut self, feedback: &Feedback<'_>) {
        self.seen = self.seen.max(feedback.frame);
        self.take_arrivals();
        for (slot, w) in feedback.used.as_chunks::<4>().0.iter().enumerate() {
            if u32::from_le_bytes(*w) == 0 {
                continue;
            }
            if let Some(Some((m, p))) = self.owner.get(slot)
                && let Some(mesh) = self.meshes.get_mut(m.0)
            {
                let state = &mut mesh.pages[*p as usize];
                state.last_used = state.last_used.max(feedback.frame);
            }
        }

        // Requested pages, deduplicated, highest priority first.
        let table = geometry::table_offset(self.slots);
        let count = (feedback.request_count as usize).min(feedback.requests.len() / 8);
        let mut wanted: BTreeMap<u32, f32> = BTreeMap::new();
        for r in feedback.requests[..count * 8].as_chunks::<8>().0 {
            let at = u64::from(u32::from_le_bytes([r[0], r[1], r[2], r[3]]));
            let priority = f32::from_le_bytes([r[4], r[5], r[6], r[7]]);
            if at < table || !(at - table).is_multiple_of(4) {
                continue;
            }
            let entry = ((at - table) / 4) as u32;
            let p = wanted.entry(entry).or_insert(priority);
            *p = p.max(priority);
        }
        let mut order: Vec<(f32, MeshHandle, u32)> = wanted
            .into_iter()
            .filter_map(|(entry, priority)| {
                let (&start, &(len, mesh)) = self.tables.range(..=entry).next_back()?;
                (entry < start + len).then_some((priority, mesh, entry - start))
            })
            .collect();
        order.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));

        let mut loaded: Vec<(MeshHandle, u32)> = Vec::new();
        let mut missing = 0;
        for (_, m, page) in order {
            let Some(mesh) = self.meshes.get(m.0) else {
                continue;
            };
            // The page with its siblings (its groups whole) and what they depend on,
            // transitively, that is not resident yet.
            let mut start = alloc::vec![page];
            start.extend(&mesh.siblings[page as usize]);
            let mut closure = dependency_closure(&mesh.dependencies, start);
            closure.retain(|&q| mesh.pages[q as usize].slot == NOT_RESIDENT);
            if closure.is_empty() {
                continue;
            }
            // A page that failed its read or check never loads: coarser clusters stand in.
            if closure.iter().any(|&q| mesh.pages[q as usize].failed) {
                continue;
            }
            // Bytes first: read what is not in memory; the request loads once it has arrived.
            if !self.read_missing(m, &closure) {
                missing += 1;
                continue;
            }
            // A closure larger than the budget still loads, alone in its frame.
            if (!loaded.is_empty() && loaded.len() + closure.len() > self.max_page_uploads as usize)
                || !self.load(m, &closure, &mut loaded)
            {
                missing += 1;
            }
        }
        self.wanted = missing;
        self.prune_arrivals();
    }

    /// Takes the pages read since the last call into their meshes' arrivals.
    fn take_arrivals(&mut self) {
        let frame = self.frame;
        let loading = core::mem::take(&mut self.loading);
        for (m, p) in loading {
            let Some(mesh) = self.meshes.get_mut(m.0) else {
                continue;
            };
            match mesh.data.take(mesh.bulk[p as usize] as usize) {
                None => self.loading.push((m, p)),
                Some(Ok(bytes)) => {
                    mesh.pages[p as usize].loading = false;
                    mesh.arrived.insert(p, (bytes, frame));
                }
                Some(Err(())) => {
                    let state = &mut mesh.pages[p as usize];
                    state.loading = false;
                    state.failed = true;
                }
            }
        }
    }

    /// Drops arrived pages nothing has loaded for a while (the camera moved on before the rest
    /// of their request arrived): they are read again if wanted again.
    fn prune_arrivals(&mut self) {
        /// Frames an arrived page waits for the rest of its request.
        const KEEP_FRAMES: u64 = 120;
        let frame = self.frame;
        for (_, mesh) in self.meshes.iter_mut() {
            mesh.arrived
                .retain(|_, (_, at)| frame.saturating_sub(*at) <= KEEP_FRAMES);
        }
    }

    /// Whether every page of `pages` of mesh `m` has its bytes in memory; reads the ones that
    /// do not.
    fn read_missing(&mut self, m: MeshHandle, pages: &[u32]) -> bool {
        let Some(mesh) = self.meshes.get_mut(m.0) else {
            return false;
        };
        let mut all = true;
        for &q in pages {
            let b = mesh.bulk[q as usize] as usize;
            if mesh.arrived.contains_key(&q) || mesh.data.bulk(b).is_some() {
                continue;
            }
            all = false;
            let state = &mut mesh.pages[q as usize];
            if !state.loading {
                state.loading = true;
                mesh.data.request(b);
                self.loading.push((m, q));
            }
        }
        all
    }

    /// Loads `pages` of mesh `m` together, or none of them: their checks, then slots (evicting
    /// for them, but none of `loaded` or `pages`). Their bytes are in memory
    /// ([`read_missing`](Self::read_missing)).
    fn load(&mut self, m: MeshHandle, pages: &[u32], loaded: &mut Vec<(MeshHandle, u32)>) -> bool {
        let Some(data) = self.meshes.get(m.0).map(|mesh| mesh.data.clone()) else {
            return false;
        };
        let bad: Vec<u32> = {
            let view = data.view();
            let Some(mesh) = self.meshes.get(m.0) else {
                return false;
            };
            pages
                .iter()
                .copied()
                .filter(|&q| {
                    let bytes = match mesh.arrived.get(&q) {
                        Some((b, _)) => Some(b.as_slice()),
                        None => data.bulk(mesh.bulk[q as usize] as usize),
                    };
                    !bytes.is_some_and(|b| geometry::check_page(&view.cluster_dag, q as usize, b))
                })
                .collect()
        };
        if !bad.is_empty() {
            if let Some(mesh) = self.meshes.get_mut(m.0) {
                for q in bad {
                    mesh.pages[q as usize].failed = true;
                    mesh.arrived.remove(&q);
                }
            }
            return false;
        }
        let mut keep: Vec<(MeshHandle, u32)> = loaded.clone();
        keep.extend(pages.iter().map(|&q| (m, q)));
        let mut slots = Vec::with_capacity(pages.len());
        for _ in pages {
            match self.take_slot(&keep) {
                Some(s) => slots.push(s),
                None => {
                    self.free.extend(slots.iter().rev());
                    return false;
                }
            }
        }
        let frame = self.frame;
        let Some(mesh) = self.meshes.get_mut(m.0) else {
            self.free.extend(slots.iter().rev());
            return false;
        };
        let table = mesh.table;
        for (&q, &slot) in pages.iter().zip(&slots) {
            let state = &mut mesh.pages[q as usize];
            state.slot = slot;
            state.last_used = frame;
            self.owner[slot as usize] = Some((m, q));
            self.writes.push(Write::Page {
                mesh: m,
                page: q,
                slot,
                bytes: mesh.arrived.remove(&q).map(|(b, _)| b),
            });
            self.writes.push(Write::Entry {
                mesh: m,
                entry: table + q,
                value: slot,
            });
            loaded.push((m, q));
        }
        true
    }

    /// Loads every page of `mesh` with the next frame's uploads, evicting for them if it must
    /// (a level's loading screen, a cinematic's close-up, tools): all of them, or none if they
    /// do not fit or one fails its check. Pages not in memory are read first: until they have
    /// all arrived this loads nothing and returns `false`; call it again (after
    /// [`poll_reads`](Self::poll_reads)). Not bounded by
    /// [`max_page_uploads`](Self::max_page_uploads).
    pub fn prefetch(&mut self, mesh: MeshHandle) -> bool {
        let Some(m) = self.meshes.get(mesh.0) else {
            return false;
        };
        let missing: Vec<u32> = (0..m.pages.len() as u32)
            .filter(|&p| m.pages[p as usize].slot == NOT_RESIDENT)
            .collect();
        missing.is_empty()
            || (self.read_missing(mesh, &missing) && self.load(mesh, &missing, &mut Vec::new()))
    }

    /// Takes the pages read since the last call (what [`apply_feedback`](Self::apply_feedback)
    /// does first), for [`prefetch`](Self::prefetch) without feedback.
    pub fn poll_reads(&mut self) {
        self.take_arrivals();
    }

    /// The meshes whose resident pages (and so their LOD cuts) the last
    /// [`take_uploads`](Self::take_uploads) changed: shadows cached from them are stale.
    pub fn changed_meshes(&self) -> &[MeshHandle] {
        &self.changed
    }

    /// Whether streaming has caught up: no writes pending, no page being read, and the last
    /// feedback asked for no page it could not load.
    pub fn streaming_idle(&self) -> bool {
        self.writes.is_empty() && self.loading.is_empty() && self.wanted == 0
    }

    /// Takes the writes for this frame's upload pass (call once a frame, before
    /// [`add_passes`](Self::add_passes)). The last write to a slot or table entry wins.
    pub fn take_uploads(&mut self) -> Uploads {
        let frame = self.frame;
        self.frame += 1;
        let mut pages: BTreeMap<u32, (MeshHandle, u32, Option<Vec<u8>>)> = BTreeMap::new();
        let mut entries: BTreeMap<u32, u32> = BTreeMap::new();
        self.changed.clear();
        for w in core::mem::take(&mut self.writes) {
            match w {
                Write::Page {
                    mesh,
                    page,
                    slot,
                    bytes,
                } => {
                    pages.insert(slot, (mesh, page, bytes));
                }
                Write::Entry { mesh, entry, value } => {
                    entries.insert(entry, value);
                    if !self.changed.contains(&mesh) {
                        self.changed.push(mesh);
                    }
                }
            }
        }
        let mut ops = Vec::new();
        let mut data: Vec<u32> = Vec::new();
        for (slot, (m, page, streamed)) in pages {
            let Some(mesh) = self.meshes.get(m.0) else {
                continue;
            };
            let bytes = match &streamed {
                Some(b) => b.as_slice(),
                None => match mesh.data.bulk(mesh.bulk[page as usize] as usize) {
                    Some(b) => b,
                    None => continue,
                },
            };
            let first = data.len() as u32;
            data.extend(bytes.chunks(4).map(|c| {
                u32::from_le_bytes(core::array::from_fn(|i| c.get(i).copied().unwrap_or(0)))
            }));
            ops.push([
                (u64::from(slot) * PAGE_SIZE as u64) as u32,
                first,
                data.len() as u32 - first,
                0,
            ]);
        }
        for (entry, value) in entries {
            ops.push([self.entry_offset(entry), data.len() as u32, 1, 0]);
            data.push(value);
        }
        Uploads {
            ops,
            data,
            frame,
            rt: self.rt.take(),
        }
    }

    /// Declares the frame's upload pass (`uploads`, from [`take_uploads`](Self::take_uploads))
    /// into the imported pool, and the cleared used-slot marks.
    pub fn add_passes<'g>(&'g self, g: &mut RenderGraph<'g>, uploads: Uploads) -> PagePool<'g> {
        let Uploads {
            ops,
            data,
            frame,
            rt,
        } = uploads;
        let pages = g.import_buffer("PagePool", &self.pool);
        let pages = self.add_upload(g, "PageUpload", pages, ops, data);
        let rt_geometry = g.import_buffer("RtGeometry", &self.rt.geometry);
        let rt_geometry = self.add_upload(
            g,
            "RtGeometryUpload",
            rt_geometry,
            rt.geometry_ops,
            rt.geometry_data,
        );
        let blas_table = g.import_buffer("BlasTable", &self.rt.table);
        let blas_table = self.add_upload(
            g,
            "BlasTableUpload",
            blas_table,
            rt.table_ops,
            rt.table_data,
        );
        let blas = g.import_buffer("BlasPool", &self.rt.blas);
        let blas = self
            .rt
            .add_builds(g, rt_geometry, blas, rt.builds, rt.scratch);
        let slots = self.slots;
        let used = g.create_buffer(
            "PageUsed",
            GraphBufferDesc::structured::<u32>(u64::from(slots)),
        );
        let used = g.add_compute("PageUsedClear", |b| {
            let used = b.write_buffer(used, Access::ComputeWrite);
            (used, move |ctx: &mut PassContext<'_>| {
                ctx.set_pipeline(&self.clear);
                ctx.push(&ClearArgs {
                    words: ctx.uav(used),
                    count: slots,
                });
                ctx.dispatch(slots.div_ceil(64), 1, 1);
            })
        });
        PagePool {
            pages,
            used,
            frame,
            rt_geometry,
            blas,
            blas_table,
        }
    }

    /// Declares a `PageUpload` pass writing `ops` (destination byte, first data word, words) of
    /// `data` into `target`, if there are any.
    fn add_upload<'g>(
        &'g self,
        g: &mut RenderGraph<'g>,
        name: &str,
        target: BufferHandle<'g>,
        ops: Vec<[u32; 4]>,
        data: Vec<u32>,
    ) -> BufferHandle<'g> {
        if ops.is_empty() {
            return target;
        }
        g.add_compute(name, |b| {
            let target = b.write_buffer(target, Access::ComputeWrite);
            (target, move |ctx: &mut PassContext<'_>| {
                let count = ops.len() as u32;
                let ops = ctx.upload_slice(&ops);
                let data = ctx.upload_slice(&data);
                ctx.set_pipeline(&self.upload);
                ctx.push(&UploadArgs {
                    ops,
                    data,
                    pool: ctx.uav(target),
                    count,
                });
                ctx.dispatch(count.min(65535), count.div_ceil(65535), 1);
            })
        })
    }

    /// Frees a mesh's DAG buffer, its slots and its page table entries. Its handle draws nothing
    /// from now on.
    ///
    /// # Safety
    /// The GPU has finished every frame that drew the mesh (after `frames_in_flight` more frames,
    /// or a wait for idle).
    pub unsafe fn release_mesh(&mut self, mesh: MeshHandle) {
        let Some(m) = self.meshes.remove(mesh.0) else {
            return;
        };
        for state in &m.pages {
            if state.slot != NOT_RESIDENT {
                self.owner[state.slot as usize] = None;
                self.free.push(state.slot);
            }
        }
        self.writes.retain(|w| w.mesh() != mesh);
        self.loading.retain(|&(m, _)| m != mesh);
        self.tables.remove(&m.table);
        self.free_table(m.table, m.pages.len() as u32);
        // SAFETY: forwarded from the caller.
        unsafe { self.rt.remove(mesh.0.index()) };
    }

    /// What one instance of `mesh` may add to the visibility passes' buffers (zero if it was
    /// released).
    pub(crate) fn dag_counts(&self, mesh: MeshHandle) -> DagCounts {
        self.meshes
            .get(mesh.0)
            .map_or(DagCounts::default(), |m| m.counts)
    }

    /// Takes the RT LODs read since and places and queues the BLASes this frame's budget
    /// allows (call once a frame, before [`take_uploads`](Self::take_uploads)).
    pub fn prepare_rt(&mut self, device: &Device) -> Result<()> {
        self.rt.prepare(device)
    }

    /// Meshes with a BLAS (built, or building this frame).
    pub fn blas_count(&self) -> usize {
        self.rt.built()
    }

    /// Free bytes of the BLAS pool.
    pub fn free_blas_bytes(&self) -> u64 {
        self.rt.free_blas_bytes()
    }

    /// Most triangles whose BLASes a frame builds.
    pub fn set_blas_budget(&mut self, triangles: u32) {
        self.rt.triangles_per_frame = triangles;
    }

    /// Meshes uploaded and not released.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Free slots of the page pool.
    pub fn free_pages(&self) -> usize {
        self.free.len()
    }

    /// Resident pages of `mesh` (0 if it was released).
    pub fn resident_pages(&self, mesh: MeshHandle) -> usize {
        self.meshes.get(mesh.0).map_or(0, |m| {
            m.pages.iter().filter(|p| p.slot != NOT_RESIDENT).count()
        })
    }

    /// What the GPU Scene's mesh table needs of `mesh` (for `MeshUpdate::Resident`). `None` if
    /// the mesh was released.
    pub fn resident_mesh(&self, mesh: MeshHandle) -> Option<ResidentMesh> {
        self.meshes.get(mesh.0).map(|m| m.resident)
    }

    /// The pool slot holding page `page` (in DAG order) of `mesh`, for tools and reference
    /// rasterizers that address clusters as the visibility passes do. `None` if the page is not
    /// resident or the mesh was released.
    pub fn page_slot(&self, mesh: MeshHandle, page: usize) -> Option<u32> {
        let slot = self.meshes.get(mesh.0)?.pages.get(page)?.slot;
        (slot != NOT_RESIDENT).then_some(slot)
    }
}

/// A new GPU buffer holding `bytes`, read through a raw view, copied before this returns.
pub(crate) fn copy_to_new_buffer(device: &Device, bytes: &[u8]) -> Result<(Buffer, ShaderView)> {
    let size = bytes.len() as u64;
    let staging = device.create_buffer(&BufferDesc {
        size,
        usage: BufferUsage::COPY_SRC,
        memory: MemoryLocation::Upload,
    })?;
    // SAFETY: the GPU has no work using the new buffer.
    unsafe { staging.write(0, bytes) };
    let buffer = device.create_buffer(&BufferDesc {
        size,
        usage: BufferUsage::SHADER_READ | BufferUsage::COPY_DST,
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
        after: BufferState::SHADER_READ,
    }]);
    list.end()?;
    // SAFETY: everything the list uses lives until the wait below returns.
    unsafe { device.queue().submit(&[&list], Some((&fence, 1)))? };
    fence.wait(1, None)?;
    let view =
        device.create_buffer_view(&buffer, &BufferViewDesc::whole(BufferViewKind::Raw, false))?;
    Ok((buffer, view))
}

/// `start` and every page it depends on, transitively, sorted.
fn dependency_closure(dependencies: &[Vec<u32>], start: Vec<u32>) -> Vec<u32> {
    let mut seen = alloc::vec![false; dependencies.len()];
    let mut stack = start;
    let mut out = Vec::new();
    while let Some(p) = stack.pop() {
        if core::mem::replace(&mut seen[p as usize], true) {
            continue;
        }
        out.push(p);
        stack.extend(&dependencies[p as usize]);
    }
    out.sort_unstable();
    out
}
