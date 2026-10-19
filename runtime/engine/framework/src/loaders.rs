//! The engine's asset loaders: what each cooked type becomes at runtime.
//!
//! - `Level` and `Prefab` → `CookedLevel` (marlin-serial): instances expanded, by archetype.
//! - `ShaderLibrary` (a cooked `ShaderPackage`): as cooked (marlin-serial); see [`crate::shaders`].
//! - `Mesh` → [`LoadedMesh`]: the cooked mesh in place with its root pages, shared with the
//!   render side through an `Arc`; its other pages stream from disk when the renderer wants them.
//! - `Texture`, `Skeleton`, `AnimationClip`, `Font`, `PairingPattern` → [`InPlace`]: the cooked
//!   layout checked once and kept in the unit's bytes, read through its view without copying.
//!   Texture upload arrives with the renderer that samples textures.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::ops::Range;

use marlin_anim::asset::{AnimationClip, AnimationView, Skeleton, SkeletonView};
use marlin_asset::{BlobInfo, BlobRequest, BlobStreamer, LoadContext, Loaders, UnitBytes};
use marlin_font::{Font, FontView};
use marlin_reflect::type_info;
use marlin_render::MeshData;
use marlin_render::asset::mesh::MeshView;
use marlin_render::asset::texture::TextureView;
use marlin_render::asset::{Mesh, PairingPattern, PairingPatternView, Texture};
use marlin_shader::asset::ShaderLibrary;
use marlin_sync::Mutex;

use marlin_world::CookedLevel;

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

/// A cooked payload used where it was loaded: the unit's bytes stay alive with it.
pub struct InPlace<T> {
    unit: Arc<UnitBytes>,
    payload: Range<usize>,
    bulk: Vec<Range<usize>>,
    _type: PhantomData<fn() -> T>,
}

impl<T> InPlace<T> {
    fn new(ctx: &LoadContext<'_>) -> Self {
        let unit = ctx.unit_bytes();
        let base = unit.as_slice().as_ptr() as usize;
        let range = |s: &[u8]| {
            // A streamed blob the unit was loaded without has no bytes, and none in the unit.
            if s.is_empty() {
                return 0..0;
            }
            let start = s.as_ptr() as usize - base;
            start..start + s.len()
        };
        Self {
            payload: range(ctx.payload),
            bulk: ctx.bulk.iter().map(|(_, b)| range(b)).collect(),
            unit,
            _type: PhantomData,
        }
    }

    pub fn payload(&self) -> &[u8] {
        &self.unit.as_slice()[self.payload.clone()]
    }

    pub fn bulk(&self, index: usize) -> &[u8] {
        &self.unit.as_slice()[self.bulk[index].clone()]
    }

    pub fn bulk_count(&self) -> usize {
        self.bulk.len()
    }

    fn bulk_sizes(&self) -> Vec<u64> {
        self.bulk.iter().map(|r| r.len() as u64).collect()
    }
}

impl InPlace<Texture> {
    pub fn view(&self) -> TextureView<'_> {
        // Checked when loaded.
        TextureView::new(self.payload(), &self.bulk_sizes()).expect("checked at load")
    }
}

impl InPlace<Font> {
    pub fn view(&self) -> FontView<'_> {
        FontView::new(self.payload()).expect("checked at load")
    }
}

impl InPlace<PairingPattern> {
    pub fn view(&self) -> PairingPatternView<'_> {
        PairingPatternView::new(self.payload()).expect("checked at load")
    }
}

impl InPlace<Skeleton> {
    pub fn view(&self) -> SkeletonView<'_> {
        SkeletonView::new(self.payload()).expect("checked at load")
    }
}

impl InPlace<AnimationClip> {
    pub fn view(&self) -> AnimationView<'_> {
        AnimationView::new(self.payload()).expect("checked at load")
    }
}

fn load_mesh(
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

/// Every engine asset type's loader. Meshes read their streamed pages through `streamer`, which
/// whoever owns the IO drives ([`BlobStreamer::update`]).
pub fn engine_loaders(streamer: &Arc<BlobStreamer>) -> Loaders {
    let streamer = streamer.clone();
    let mut l = Loaders::new();
    l.register_serial::<CookedLevel>();
    l.register_serial::<crate::scalability::Scalability>();
    l.register_serial::<crate::scalability::DeviceProfile>();
    l.register_serial::<ShaderLibrary>();
    l.register::<LoadedMesh>(type_info::<Mesh>().id.0, move |ctx| {
        load_mesh(ctx, &streamer)
    });
    l.register::<InPlace<Texture>>(type_info::<Texture>().id.0, |ctx| {
        let sizes: Vec<u64> = ctx.bulk.iter().map(|(_, b)| b.len() as u64).collect();
        TextureView::new(ctx.payload, &sizes).map_err(|e| alloc::format!("{e:?}"))?;
        Ok(InPlace::new(ctx))
    });
    l.register::<InPlace<Font>>(type_info::<Font>().id.0, |ctx| {
        FontView::new(ctx.payload).map_err(|e| alloc::format!("{e:?}"))?;
        Ok(InPlace::new(ctx))
    });
    l.register::<InPlace<PairingPattern>>(type_info::<PairingPattern>().id.0, |ctx| {
        PairingPatternView::new(ctx.payload).map_err(|e| alloc::format!("{e:?}"))?;
        Ok(InPlace::new(ctx))
    });
    l.register::<InPlace<Skeleton>>(type_info::<Skeleton>().id.0, |ctx| {
        SkeletonView::new(ctx.payload).map_err(|e| alloc::format!("{e:?}"))?;
        Ok(InPlace::new(ctx))
    });
    l.register::<InPlace<AnimationClip>>(type_info::<AnimationClip>().id.0, |ctx| {
        AnimationView::new(ctx.payload).map_err(|e| alloc::format!("{e:?}"))?;
        Ok(InPlace::new(ctx))
    });
    l
}
