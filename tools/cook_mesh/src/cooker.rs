//! The cook pipeline's cookers for glTF content: [`Mesh`], [`Skeleton`] and [`AnimationClip`].

use core::mem::offset_of;

use marlin_anim::asset::{
    AnimationClip, AnimationFlags, CookedAnimation, CookedSkeleton, Joint, JointTrack, Skeleton,
    encode_rotation,
};
use marlin_asset::SourceFile;
use marlin_asset::cooked::{BulkKind, Encoding};
use marlin_cook_core::{CookContext, Cooked, Cooker, SourceObject};
use marlin_reflect::{Reflect, type_info};
use marlin_render::asset::Mesh;
use marlin_serial::cooked::{CookedBuilder, RelSlice};

use crate::build::build;
use crate::import::{self, Document, MeshOptions};

/// Opens the glTF of `source`, reading external buffers through the context.
fn open(source: &SourceFile, ctx: &mut CookContext<'_>) -> Result<Document, String> {
    let path = &source.path;
    if path.is_empty() {
        return Err("`Source` is not set".into());
    }
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".gltf") || lower.ends_with(".glb")) {
        return Err(format!("`{path}`: not a glTF (.gltf or .glb)"));
    }
    let bytes = ctx.read_source(path)?;
    import::open(&bytes, path, &mut |p| ctx.read_source(p))
}

fn downcast<'a, T: Reflect>(object: &'a SourceObject<'_>) -> Result<&'a T, String> {
    object
        .value
        .as_ref()
        .downcast()
        .ok_or_else(|| format!("the object is not a {}", object.type_name))
}

/// Cooks [`Mesh`] assets.
pub struct MeshCooker;

impl Cooker for MeshCooker {
    fn version(&self) -> u32 {
        4
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let mesh: &Mesh = downcast(object)?;
        if !(mesh.scale.is_finite() && mesh.scale > 0.0) {
            return Err(format!("`Scale` must be positive, not {}", mesh.scale));
        }
        let doc = open(&mesh.source, ctx)?;
        let (geometry, warnings) = import::import_mesh(
            &doc,
            &MeshOptions {
                mesh: &mesh.mesh,
                scale: mesh.scale,
                max_uv_sets: mesh.max_uv_sets.min(4) as usize,
                colors: mesh.vertex_colors,
            },
        )?;
        for w in warnings {
            ctx.warn(w);
        }
        if geometry.influences.is_some() && mesh.skeleton.is_none() {
            ctx.warn("the mesh is skinned but `Skeleton` is not set; it will not find its joints at runtime");
        }
        let built = build(&geometry, mesh)?;
        for page in built.pages {
            ctx.add_bulk(BulkKind::Vertices, page);
        }
        Ok(Cooked {
            type_id: type_info::<Mesh>().id,
            encoding: Encoding::Cooked,
            payload: built.payload,
        })
    }
}

/// Cooks [`Skeleton`] assets.
pub struct SkeletonCooker;

impl Cooker for SkeletonCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let skeleton: &Skeleton = downcast(object)?;
        let doc = open(&skeleton.source, ctx)?;
        let joints = import::import_skeleton(&doc, &skeleton.skin, skeleton.scale)?;
        if joints.len() > u16::MAX as usize {
            return Err(format!("{} joints: at most 65535", joints.len()));
        }
        let mut names = String::new();
        let records: Vec<Joint> = joints
            .iter()
            .map(|j| {
                let name_offset = names.len() as u32;
                names.push_str(&j.name);
                Joint {
                    parent: j.parent,
                    name_offset,
                    name_len: j.name.len() as u32,
                    translation: j.translation,
                    rotation: j.rotation,
                    scale: j.scale,
                    inverse_bind: j.inverse_bind,
                }
            })
            .collect();
        let mut b = CookedBuilder::new();
        let root = b.push(&CookedSkeleton {
            joints: RelSlice::EMPTY,
            names: RelSlice::EMPTY,
        });
        let j = b.push_slice(&records);
        let n = b.push_str(&names);
        let err = |e: marlin_serial::cooked::LayoutError| e.to_string();
        b.link_slice(
            root.field(offset_of!(CookedSkeleton, joints), |s| &s.joints),
            j,
        )
        .map_err(err)?;
        b.link_slice(
            root.field(offset_of!(CookedSkeleton, names), |s| &s.names),
            n,
        )
        .map_err(err)?;
        Ok(Cooked {
            type_id: type_info::<Skeleton>().id,
            encoding: Encoding::Cooked,
            payload: b.bytes().to_vec(),
        })
    }
}

/// Keys closer than this to the first are the same: the channel is constant.
const CONSTANT_EPSILON: f32 = 1e-6;

/// Cooks [`AnimationClip`] assets.
pub struct AnimationCooker;

impl Cooker for AnimationCooker {
    fn version(&self) -> u32 {
        1
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let clip: &AnimationClip = downcast(object)?;
        let doc = open(&clip.source, ctx)?;
        let (imported, warnings) =
            import::import_animation(&doc, &clip.animation, clip.sample_rate, clip.scale)?;
        for w in warnings {
            ctx.warn(w);
        }
        if clip.skeleton.is_none() {
            ctx.warn("`Skeleton` is not set: the clip cannot be checked against or bound to one");
        }
        let mut translations: Vec<[f32; 3]> = Vec::new();
        let mut rotations: Vec<[u16; 3]> = Vec::new();
        let mut scales: Vec<[f32; 3]> = Vec::new();
        let mut tracks: Vec<JointTrack> = Vec::new();
        let constant3 = |keys: &[[f32; 3]]| {
            keys.iter()
                .all(|k| (0..3).all(|i| (k[i] - keys[0][i]).abs() <= CONSTANT_EPSILON))
        };
        for t in &imported.tracks {
            let (translation_first, translation_count) = {
                let first = translations.len() as u32;
                if constant3(&t.translations) {
                    translations.push(t.translations[0]);
                } else {
                    translations.extend_from_slice(&t.translations);
                }
                (first, translations.len() as u32 - first)
            };
            let (scale_first, scale_count) = {
                let first = scales.len() as u32;
                if constant3(&t.scales) {
                    scales.push(t.scales[0]);
                } else {
                    scales.extend_from_slice(&t.scales);
                }
                (first, scales.len() as u32 - first)
            };
            let (rotation_first, rotation_count) = {
                let first = rotations.len() as u32;
                let q: Vec<[u16; 3]> = t.rotations.iter().map(|&r| encode_rotation(r)).collect();
                if q.iter().all(|k| *k == q[0]) {
                    rotations.push(q[0]);
                } else {
                    rotations.extend_from_slice(&q);
                }
                (first, rotations.len() as u32 - first)
            };
            tracks.push(JointTrack {
                name: RelSlice::EMPTY,
                translation_first,
                translation_count,
                rotation_first,
                rotation_count,
                scale_first,
                scale_count,
            });
        }
        let mut b = CookedBuilder::new();
        let root = b.push(&CookedAnimation {
            duration: imported.duration,
            sample_rate: clip.sample_rate,
            frame_count: imported.frames,
            flags: if clip.looping {
                AnimationFlags::LOOPING
            } else {
                0
            },
            tracks: RelSlice::EMPTY,
            translations: RelSlice::EMPTY,
            rotations: RelSlice::EMPTY,
            scales: RelSlice::EMPTY,
        });
        let err = |e: marlin_serial::cooked::LayoutError| e.to_string();
        let tr = b.push_slice(&tracks);
        let t = b.push_slice(&translations);
        let r = b.push_slice(&rotations);
        let s = b.push_slice(&scales);
        b.link_slice(
            root.field(offset_of!(CookedAnimation, tracks), |a| &a.tracks),
            tr,
        )
        .map_err(err)?;
        b.link_slice(
            root.field(offset_of!(CookedAnimation, translations), |a| {
                &a.translations
            }),
            t,
        )
        .map_err(err)?;
        b.link_slice(
            root.field(offset_of!(CookedAnimation, rotations), |a| &a.rotations),
            r,
        )
        .map_err(err)?;
        b.link_slice(
            root.field(offset_of!(CookedAnimation, scales), |a| &a.scales),
            s,
        )
        .map_err(err)?;
        for (i, track) in imported.tracks.iter().enumerate() {
            let name = b.push_str(&track.name);
            b.link_slice(
                tr.at(i).field(offset_of!(JointTrack, name), |t| &t.name),
                name,
            )
            .map_err(err)?;
        }
        Ok(Cooked {
            type_id: type_info::<AnimationClip>().id,
            encoding: Encoding::Cooked,
            payload: b.bytes().to_vec(),
        })
    }
}
