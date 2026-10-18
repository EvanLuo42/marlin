//! The resident meshes: each a cooked mesh's LOD pages on the GPU ([`crate::geometry`]), drawn
//! by the visibility passes through the GPU Scene's mesh table. Also the view and lighting
//! descriptions the passes share.

use alloc::vec::Vec;

use marlin_containers::{Handle, SlotMap};
use marlin_math::Vec3;
use marlin_rhi::{
    Buffer, BufferBarrier, BufferDesc, BufferState, BufferUsage, BufferViewDesc, BufferViewKind,
    Device, Error, ErrorKind, IndexFormat, MemoryLocation, Result, ShaderView,
};

use crate::asset::mesh::MeshView;
use crate::geometry::{self, LodDraw};
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

struct Mesh {
    view: ShaderView,
    buffer: Buffer,
    lods: Vec<LodDraw>,
    resident: ResidentMesh,
}

/// One LOD of a resident mesh as an indexed draw sees it: its geometry buffer's index list.
#[derive(Clone, Copy, Debug)]
pub struct LodIndices<'a> {
    pub buffer: &'a Buffer,
    /// Byte offset of the index list in `buffer`.
    pub offset: u64,
    pub count: u32,
    pub format: IndexFormat,
    /// The geometry buffer's view (`Marlin.Engine.Scene` decodes vertices through it).
    pub geometry: u32,
}

/// The resident meshes (see the module docs).
#[derive(Default)]
pub struct Meshes {
    meshes: SlotMap<Mesh>,
}

impl Meshes {
    pub fn new() -> Self {
        Self::default()
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

    /// LOD `lod` of `mesh` as an indexed draw: for tools and reference rasterizers (the
    /// renderer draws meshlets). `None` if the mesh was released or has no such LOD.
    pub fn lod_indices(&self, mesh: MeshHandle, lod: u32) -> Option<LodIndices<'_>> {
        let m = self.meshes.get(mesh.0)?;
        let d = m.lods.get(lod as usize)?;
        Some(LodIndices {
            buffer: &m.buffer,
            offset: d.indices,
            count: d.index_count,
            format: d.format,
            geometry: m.view.index(),
        })
    }
}
