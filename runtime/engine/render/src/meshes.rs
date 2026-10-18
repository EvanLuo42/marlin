//! The resident meshes: each a cooked mesh's cluster DAG on the GPU ([`crate::geometry`]): its
//! pages in the page pool, its resident part in a buffer of its own, drawn by the visibility
//! passes through the GPU Scene's mesh table. Also the view and lighting descriptions the
//! passes share.

use alloc::vec::Vec;

use marlin_containers::{Handle, SlotMap};
use marlin_math::Vec3;
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    Device, Error, ErrorKind, MemoryLocation, Result, ShaderView,
};

use crate::asset::cluster::{ClusterDagView, PAGE_SIZE};
use crate::asset::mesh::MeshView;
use crate::geometry::{self, Built, DagCounts};
use crate::scene::{Bounds, ResidentMesh};

/// A view's camera: put it on an entity with a `Transform`. The camera looks
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

/// A mesh uploaded with [`Meshes::upload_mesh`]. Generational: once the mesh is released the
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

/// Page pool slots the engine creates: 512 MiB of 128 KiB pages, the streaming pool a desktop
/// AAA renderer starts from. Every page stays resident until streaming (R2b) evicts.
pub const DEFAULT_POOL_PAGES: u32 = 4096;

struct Mesh {
    _dag: Buffer,
    _dag_view: ShaderView,
    /// The pool slot of each page, in DAG order.
    slots: Vec<u32>,
    counts: DagCounts,
    resident: ResidentMesh,
}

/// The resident meshes and the page pool their pages are in (see the module docs).
pub struct Meshes {
    pool: Buffer,
    pool_view: ShaderView,
    /// Free slots, the lowest last.
    free: Vec<u32>,
    meshes: SlotMap<Mesh>,
}

impl Meshes {
    /// A page pool of `pool_pages` slots ([`DEFAULT_POOL_PAGES`] for the engine).
    pub fn new(device: &Device, pool_pages: u32) -> Result<Self> {
        let slots = pool_pages.max(1);
        let pool = device.create_buffer(&BufferDesc {
            size: geometry::pool_size(slots),
            usage: BufferUsage::SHADER_READ | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })?;
        let pool_view =
            device.create_buffer_view(&pool, &BufferViewDesc::whole(BufferViewKind::Raw, false))?;
        Ok(Self {
            pool,
            pool_view,
            free: (0..slots).rev().collect(),
            meshes: SlotMap::new(),
        })
    }

    /// Uploads a cooked mesh's cluster DAG, blocking until the GPU has copied it: every page
    /// into a free slot of the pool, the resident part into a buffer of its own. `pages` are
    /// the mesh's bulk blobs; each DAG page is checked first. A mesh cooked without the DAG or
    /// with a malformed page is `InvalidInput`; one whose pages do not fit in the pool's free
    /// slots is `OutOfMemory`.
    pub fn upload_mesh(
        &mut self,
        device: &Device,
        mesh: &MeshView<'_>,
        pages: &[&[u8]],
    ) -> Result<MeshHandle> {
        let (dag, built) =
            geometry::build(mesh, pages).map_err(|_| Error::from_kind(ErrorKind::InvalidInput))?;
        if built.pages.len() > self.free.len() {
            return Err(Error::from_kind(ErrorKind::OutOfMemory));
        }
        let at = self.free.len() - built.pages.len();
        let slots: Vec<u32> = self.free.drain(at..).rev().collect();
        let (dag_buffer, dag_view) = match self.copy(device, &dag, &built, &slots) {
            Ok(r) => r,
            Err(e) => {
                self.free.extend(slots.iter().rev());
                return Err(e);
            }
        };
        let h = mesh.header;
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
        };
        let handle = MeshHandle(self.meshes.insert(Mesh {
            _dag: dag_buffer,
            _dag_view: dag_view,
            slots,
            counts: built.counts,
            resident,
        }));
        if let Some(m) = self.meshes.get_mut(handle.0) {
            m.resident.handle = handle;
        }
        Ok(handle)
    }

    /// Copies the pages into `slots` of the pool and the DAG buffer into a new buffer.
    fn copy(
        &self,
        device: &Device,
        dag: &ClusterDagView<'_>,
        built: &Built<'_>,
        slots: &[u32],
    ) -> Result<(Buffer, ShaderView)> {
        let dag_bytes = geometry::dag_buffer(dag, slots);
        let pages_size: usize = built
            .pages
            .iter()
            .map(|p| p.len().next_multiple_of(16))
            .sum();
        let staging = device.create_buffer(&BufferDesc {
            size: (pages_size + dag_bytes.len()) as u64,
            usage: BufferUsage::COPY_SRC,
            memory: MemoryLocation::Upload,
        })?;
        let dag_buffer = device.create_buffer(&BufferDesc {
            size: dag_bytes.len() as u64,
            usage: BufferUsage::SHADER_READ | BufferUsage::COPY_DST,
            memory: MemoryLocation::GpuOnly,
        })?;
        let mut copies = Vec::with_capacity(built.pages.len());
        let mut at = 0u64;
        for (page, &slot) in built.pages.iter().zip(slots) {
            // SAFETY: the GPU has no work using the new buffer.
            unsafe { staging.write(at, page) };
            copies.push((at, u64::from(slot) * PAGE_SIZE as u64, page.len() as u64));
            at += page.len().next_multiple_of(16) as u64;
        }
        // SAFETY: as above.
        unsafe { staging.write(at, &dag_bytes) };

        let mut pool = device.create_command_pool()?;
        let mut list = device.create_command_list(&pool)?;
        let fence = device.create_fence(0)?;
        list.begin(&mut pool)?;
        // Frames in flight read other slots of the pool; the barrier orders the copies after
        // them all the same.
        list.buffer_barriers(&[BufferBarrier {
            buffer: &self.pool,
            before: BufferState::SHADER_READ,
            after: BufferState::COPY_DST,
        }]);
        for &(src, dst, size) in &copies {
            list.copy_buffer(&staging, src, &self.pool, dst, size);
        }
        list.copy_buffer(&staging, at, &dag_buffer, 0, dag_bytes.len() as u64);
        list.buffer_barriers(&[
            BufferBarrier {
                buffer: &self.pool,
                before: BufferState::COPY_DST,
                after: BufferState::SHADER_READ,
            },
            BufferBarrier {
                buffer: &dag_buffer,
                before: BufferState::COPY_DST,
                after: BufferState::SHADER_READ,
            },
        ]);
        list.end()?;
        // SAFETY: everything the list uses lives until the wait below returns.
        unsafe { device.queue().submit(&[&list], Some((&fence, 1)))? };
        fence.wait(1, None)?;
        let view = device.create_buffer_view(
            &dag_buffer,
            &BufferViewDesc::whole(BufferViewKind::Raw, false),
        )?;
        Ok((dag_buffer, view))
    }

    /// Frees a mesh's DAG buffer and its pool slots. Its handle draws nothing from now on.
    ///
    /// # Safety
    /// The GPU has finished every frame that drew the mesh (after `frames_in_flight` more frames,
    /// or a wait for idle).
    pub unsafe fn release_mesh(&mut self, mesh: MeshHandle) {
        if let Some(m) = self.meshes.remove(mesh.0) {
            self.free.extend(m.slots.iter().rev());
        }
    }

    /// What one instance of `mesh` may add to the visibility passes' buffers (zero if it was
    /// released).
    pub(crate) fn dag_counts(&self, mesh: MeshHandle) -> DagCounts {
        self.meshes
            .get(mesh.0)
            .map_or(DagCounts::default(), |m| m.counts)
    }

    /// Meshes uploaded and not released.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Free slots of the page pool.
    pub fn free_pages(&self) -> usize {
        self.free.len()
    }

    /// What the GPU Scene's mesh table needs of `mesh` (for `MeshUpdate::Resident`). `None` if
    /// the mesh was released.
    pub fn resident_mesh(&self, mesh: MeshHandle) -> Option<ResidentMesh> {
        self.meshes.get(mesh.0).map(|m| m.resident)
    }

    /// The pool slot holding page `page` (in DAG order) of `mesh`, for tools and reference
    /// rasterizers that address clusters as the visibility passes do. `None` if the mesh was
    /// released or has no such page.
    pub fn page_slot(&self, mesh: MeshHandle, page: usize) -> Option<u32> {
        self.meshes.get(mesh.0)?.slots.get(page).copied()
    }
}
