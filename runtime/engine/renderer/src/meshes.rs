//! Loaded meshes: the cooked mesh in place with its root pages, shared with the render side
//! through an `Arc`; its other pages stream from disk when the renderer wants them.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_asset::{BlobInfo, BlobRequest, BlobStreamer, InPlace, LoadContext};
use marlin_render::MeshData;
use marlin_render::asset::Mesh;
use marlin_render::asset::mesh::MeshView;
use marlin_sync::Mutex;

/// A loaded `Mesh`: its cooked payload and resident pages, checked but for the pages' contents
/// (the renderer checks those when it uploads them), and its streamed pages, read through the
/// engine's [`BlobStreamer`] when the renderer asks for them.
#[derive(Clone)]
pub struct LoadedMesh(pub Arc<StreamedMesh>);

impl core::fmt::Debug for LoadedMesh {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LoadedMesh")
            .field("payload", &self.0.cooked.payload().len())
            .field("pages", &self.0.blobs.len())
            .finish()
    }
}

/// A mesh whose streamed pages are read on request (see [`MeshData`]).
pub struct StreamedMesh {
    /// The payload and the blobs loaded with it.
    pub cooked: InPlace<Mesh>,
    unit: u64,
    blobs: Vec<BlobInfo>,
    streamer: Arc<BlobStreamer>,
    /// The read on its way of each blob.
    reads: Mutex<Vec<Option<BlobRequest>>>,
}

impl StreamedMesh {
    pub fn view(&self) -> MeshView<'_> {
        let sizes: Vec<u64> = self.blobs.iter().map(|b| b.size).collect();
        MeshView::new(self.cooked.payload(), &sizes).expect("checked at load")
    }
}

impl MeshData for StreamedMesh {
    fn view(&self) -> MeshView<'_> {
        StreamedMesh::view(self)
    }

    fn bulk(&self, index: usize) -> Option<&[u8]> {
        self.blobs
            .get(index)
            .is_some_and(|b| b.present)
            .then(|| self.cooked.bulk(index))
    }

    fn request(&self, index: usize) {
        let Some(blob) = self.blobs.get(index) else {
            return;
        };
        let mut reads = self.reads.lock();
        if reads[index].is_none() {
            reads[index] = Some(self.streamer.request(self.unit, blob.index));
        }
    }

    fn take(&self, index: usize) -> Option<Result<Vec<u8>, ()>> {
        let mut reads = self.reads.lock();
        let result = reads.get(index)?.as_ref()?.take()?;
        reads[index] = None;
        Some(
            result
                .map(|bytes| bytes.as_slice().to_vec())
                .map_err(|e| marlin_diag::warn!("a streamed mesh page did not load: {e}")),
        )
    }
}

/// Loads a `Mesh` unit: checked, kept in place, its streamed pages read through `streamer`.
pub fn load_mesh(
    ctx: &mut LoadContext<'_>,
    streamer: &Arc<BlobStreamer>,
) -> Result<LoadedMesh, alloc::string::String> {
    // The blobs' sizes as cooked: streamed ones are not in the unit.
    let sizes: Vec<u64> = ctx.blobs.iter().map(|b| b.size).collect();
    MeshView::new(ctx.payload, &sizes).map_err(|e| alloc::format!("{e:?}"))?;
    Ok(LoadedMesh(Arc::new(StreamedMesh {
        cooked: InPlace::new(ctx),
        unit: ctx.unit_id,
        blobs: ctx.blobs.to_vec(),
        streamer: streamer.clone(),
        reads: Mutex::new((0..ctx.blobs.len()).map(|_| None).collect()),
    })))
}
