//! The engine's asset loaders: what each cooked type becomes at runtime.
//!
//! - [`Level`]: as authored (marlin-serial).
//! - `Mesh` → [`LoadedMesh`]: LOD 0 decoded for the forward renderer, shared with the render side
//!   (which uploads it) through an `Arc`.
//! - `Texture`, `Skeleton`, `AnimationClip` → [`InPlace`]: the cooked layout checked once and
//!   kept in the unit's bytes, read through its view without copying. Texture upload arrives
//!   with the renderer that samples textures.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::marker::PhantomData;
use core::ops::Range;

use marlin_anim::asset::{AnimationClip, AnimationView, Skeleton, SkeletonView};
use marlin_asset::{LoadContext, Loaders, UnitBytes};
use marlin_reflect::type_info;
use marlin_render::asset::mesh::MeshView;
use marlin_render::asset::texture::TextureView;
use marlin_render::asset::{Mesh, Texture};
use marlin_render::{MeshData, Vertex};

use crate::level::Level;

/// A mesh's LOD 0 in the forward renderer's form.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct MeshGeometry {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Bounding sphere center and radius.
    pub sphere: [f32; 4],
}

impl MeshGeometry {
    pub fn data(&self) -> MeshData<'_> {
        MeshData {
            vertices: &self.vertices,
            indices: &self.indices,
        }
    }
}

/// A loaded `Mesh`.
#[derive(Clone, Debug)]
pub struct LoadedMesh(pub Arc<MeshGeometry>);

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

fn load_mesh(ctx: &mut LoadContext<'_>) -> Result<LoadedMesh, alloc::string::String> {
    let sizes: Vec<u64> = ctx.bulk.iter().map(|(_, b)| b.len() as u64).collect();
    let view = MeshView::new(ctx.payload, &sizes).map_err(|e| alloc::format!("{e:?}"))?;
    let page = ctx
        .bulk
        .get(view.lods[0].bulk as usize)
        .ok_or("LOD 0 page missing")?
        .1;
    let (vertices, indices) = view
        .decode_lod(0, page)
        .map_err(|e| alloc::format!("{e:?}"))?;
    Ok(LoadedMesh(Arc::new(MeshGeometry {
        vertices,
        indices,
        sphere: view.header.sphere,
    })))
}

/// Every engine asset type's loader.
pub fn engine_loaders() -> Loaders {
    let mut l = Loaders::new();
    l.register_serial::<Level>();
    l.register::<LoadedMesh>(type_info::<Mesh>().id.0, load_mesh);
    l.register::<InPlace<Texture>>(type_info::<Texture>().id.0, |ctx| {
        let sizes: Vec<u64> = ctx.bulk.iter().map(|(_, b)| b.len() as u64).collect();
        TextureView::new(ctx.payload, &sizes).map_err(|e| alloc::format!("{e:?}"))?;
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
