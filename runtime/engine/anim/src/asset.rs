//! Skeleton and animation clip assets: the source settings authored in `.masset` text and the
//! cooked forms the runtime samples.
//!
//! Both cook from glTF skins and animations (`tools/cook_mesh`). Cooked joints are ordered so
//! every parent precedes its children. Skinned meshes and clips refer to joints **by name**, as
//! they may come from other files than the skeleton (a character's meshes, its skeleton and its
//! hundreds of clips are usually exported separately): the runtime binds them to a skeleton's
//! joint indices once, at load ([`AnimationView::bind`]).

use alloc::string::String;
use alloc::vec::Vec;

use marlin_asset::{Link, SourceFile};
use marlin_math::scalar::{floor, round, sqrt};
use marlin_reflect::Reflect;
use marlin_serial::cooked::{CookedView, LayoutError, Pod, RelSlice};

/// A skeleton: the joint hierarchy and bind pose of a glTF skin.
#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct Skeleton {
    /// A `.gltf` or `.glb`.
    pub source: SourceFile,
    /// The external buffers a `.gltf` refers to.
    pub extra_sources: Vec<SourceFile>,
    /// The glTF skin's name. Empty: the file's first skin.
    pub skin: String,
    /// Uniform scale applied to translations, as the skinned meshes apply it to positions.
    pub scale: f32,
}

impl Default for Skeleton {
    fn default() -> Self {
        Self {
            source: SourceFile::default(),
            extra_sources: Vec::new(),
            skin: String::new(),
            scale: 1.0,
        }
    }
}

/// An animation clip, sampled per joint of a skeleton.
#[derive(Clone, PartialEq, Debug, Reflect)]
#[reflect(default)]
pub struct AnimationClip {
    pub source: SourceFile,
    pub extra_sources: Vec<SourceFile>,
    /// The glTF animation's name. Empty: the file's first animation.
    pub animation: String,
    pub skeleton: Link<Skeleton>,
    /// Frames per second the clip is resampled at.
    pub sample_rate: f32,
    pub looping: bool,
    /// Must match the skeleton's.
    pub scale: f32,
}

impl Default for AnimationClip {
    fn default() -> Self {
        Self {
            source: SourceFile::default(),
            extra_sources: Vec::new(),
            animation: String::new(),
            skeleton: Link::NONE,
            sample_rate: 30.0,
            looping: true,
            scale: 1.0,
        }
    }
}

/// The payload root of a cooked skeleton.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CookedSkeleton {
    /// Parents before children.
    pub joints: RelSlice<Joint>,
    /// Joint names, concatenated.
    pub names: RelSlice<u8>,
}

// SAFETY: two RelSlices: 16 bytes, align 4, no padding; any bits are valid.
unsafe impl Pod for CookedSkeleton {}

#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct Joint {
    /// Index of the parent joint, or -1 for a root.
    pub parent: i32,
    /// Byte range of the name in [`CookedSkeleton::names`].
    pub name_offset: u32,
    pub name_len: u32,
    /// Rest pose, relative to the parent.
    pub translation: [f32; 3],
    /// Quaternion x, y, z, w.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    /// Mesh space to joint space at bind time, column-major.
    pub inverse_bind: [f32; 16],
}

// SAFETY: an i32, two u32 and 26 f32: 116 bytes, align 4, no padding; any bits are valid.
unsafe impl Pod for Joint {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnimAssetError {
    Layout(LayoutError),
    /// Counts or references that do not describe a skeleton or clip.
    Malformed,
}

impl From<LayoutError> for AnimAssetError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A checked cooked skeleton.
#[derive(Clone, Copy, Debug)]
pub struct SkeletonView<'a> {
    pub joints: &'a [Joint],
    names: &'a [u8],
}

impl<'a> SkeletonView<'a> {
    pub fn new(payload: &'a [u8]) -> Result<Self, AnimAssetError> {
        let view = CookedView::new(payload);
        let root: &CookedSkeleton = view.root()?;
        let joints = view.slice(&root.joints)?;
        let names = view.slice(&root.names)?;
        for (i, j) in joints.iter().enumerate() {
            let name_end = j.name_offset as usize + j.name_len as usize;
            if !(j.parent == -1 || (0..i as i32).contains(&j.parent))
                || name_end > names.len()
                || core::str::from_utf8(&names[j.name_offset as usize..name_end]).is_err()
            {
                return Err(AnimAssetError::Malformed);
            }
        }
        Ok(Self { joints, names })
    }

    pub fn name(&self, joint: usize) -> &'a str {
        let j = &self.joints[joint];
        let bytes = &self.names[j.name_offset as usize..][..j.name_len as usize];
        // Checked in `new`.
        core::str::from_utf8(bytes).unwrap_or_default()
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        (0..self.joints.len()).find(|&i| self.name(i) == name)
    }
}

/// The payload root of a cooked clip.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CookedAnimation {
    /// Seconds.
    pub duration: f32,
    pub sample_rate: f32,
    /// Samples per animated track: `duration * sample_rate` rounded up, plus one.
    pub frame_count: u32,
    /// [`AnimationFlags`].
    pub flags: u32,
    /// One per animated joint.
    pub tracks: RelSlice<JointTrack>,
    pub translations: RelSlice<[f32; 3]>,
    /// Quantized with [`encode_rotation`].
    pub rotations: RelSlice<[u16; 3]>,
    pub scales: RelSlice<[f32; 3]>,
}

// SAFETY: four 4-byte scalars and four RelSlices: 48 bytes, align 4, no padding.
unsafe impl Pod for CookedAnimation {}

pub struct AnimationFlags;

impl AnimationFlags {
    pub const LOOPING: u32 = 1;
}

/// One joint's keys. Each channel has either one key (constant over the clip) or `frame_count`
/// keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct JointTrack {
    /// The joint's name, as in the skeleton.
    pub name: RelSlice<u8>,
    pub translation_first: u32,
    pub translation_count: u32,
    pub rotation_first: u32,
    pub rotation_count: u32,
    pub scale_first: u32,
    pub scale_count: u32,
}

// SAFETY: a RelSlice and six u32: 32 bytes, align 4, no padding.
unsafe impl Pod for JointTrack {}

/// A joint's local transform.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct JointPose {
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl JointPose {
    pub const IDENTITY: Self = Self {
        translation: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    };
}

/// A checked cooked clip.
#[derive(Clone, Copy, Debug)]
pub struct AnimationView<'a> {
    view: CookedView<'a>,
    pub header: &'a CookedAnimation,
    pub tracks: &'a [JointTrack],
    translations: &'a [[f32; 3]],
    rotations: &'a [[u16; 3]],
    scales: &'a [[f32; 3]],
}

impl<'a> AnimationView<'a> {
    pub fn new(payload: &'a [u8]) -> Result<Self, AnimAssetError> {
        let view = CookedView::new(payload);
        let header: &CookedAnimation = view.root()?;
        let tracks = view.slice(&header.tracks)?;
        let translations = view.slice(&header.translations)?;
        let rotations = view.slice(&header.rotations)?;
        let scales = view.slice(&header.scales)?;
        let frames = header.frame_count;
        let ok = |first: u32, count: u32, len: usize| {
            (count == 1 || count == frames) && (first as u64 + count as u64) <= len as u64
        };
        if frames == 0
            || !(header.duration.is_finite() && header.duration >= 0.0)
            || !(header.sample_rate.is_finite() && header.sample_rate > 0.0)
            || !tracks.iter().all(|t| {
                ok(t.translation_first, t.translation_count, translations.len())
                    && ok(t.rotation_first, t.rotation_count, rotations.len())
                    && ok(t.scale_first, t.scale_count, scales.len())
                    && view.str(&t.name).is_ok()
            })
        {
            return Err(AnimAssetError::Malformed);
        }
        Ok(Self {
            view,
            header,
            tracks,
            translations,
            rotations,
            scales,
        })
    }

    pub fn track_name(&self, track: usize) -> &'a str {
        // Checked in `new`.
        self.view.str(&self.tracks[track].name).unwrap_or_default()
    }

    /// The skeleton joint each track animates, matched by name; `None` for tracks of joints
    /// the skeleton lacks.
    pub fn bind(&self, skeleton: &SkeletonView<'_>) -> Vec<Option<u16>> {
        (0..self.tracks.len())
            .map(|t| skeleton.find(self.track_name(t)).map(|j| j as u16))
            .collect()
    }

    /// Samples the clip into a skeleton pose: the rest pose, overridden by every bound track.
    pub fn sample_pose(
        &self,
        time: f32,
        skeleton: &SkeletonView<'_>,
        binding: &[Option<u16>],
        scratch: &mut Vec<JointPose>,
        pose: &mut [JointPose],
    ) {
        for (p, j) in pose.iter_mut().zip(skeleton.joints) {
            *p = JointPose {
                translation: j.translation,
                rotation: j.rotation,
                scale: j.scale,
            };
        }
        scratch.clear();
        scratch.resize(self.tracks.len(), JointPose::IDENTITY);
        self.sample(time, scratch);
        for (s, b) in scratch.iter().zip(binding) {
            if let Some(p) = b.and_then(|j| pose.get_mut(j as usize)) {
                *p = *s;
            }
        }
    }

    /// Samples every joint at `time` seconds (wrapped when looping, else clamped) into `out`
    /// (one per track), interpolating between the neighboring frames.
    pub fn sample(&self, time: f32, out: &mut [JointPose]) {
        let h = self.header;
        let t = if h.flags & AnimationFlags::LOOPING != 0 && h.duration > 0.0 {
            time - floor(time / h.duration) * h.duration
        } else {
            time.clamp(0.0, h.duration)
        };
        let f = t * h.sample_rate;
        let last = h.frame_count - 1;
        let frame = (f as u32).min(last);
        let next = (frame + 1).min(last);
        let alpha = (f - frame as f32).clamp(0.0, 1.0);
        for (track, pose) in self.tracks.iter().zip(out) {
            let key =
                |first: u32, count: u32, i: u32| (first + if count == 1 { 0 } else { i }) as usize;
            let lerp3 =
                |a: [f32; 3], b: [f32; 3]| core::array::from_fn(|i| a[i] + (b[i] - a[i]) * alpha);
            let tr =
                |i| self.translations[key(track.translation_first, track.translation_count, i)];
            let sc = |i| self.scales[key(track.scale_first, track.scale_count, i)];
            let ro = |i| {
                decode_rotation(self.rotations[key(track.rotation_first, track.rotation_count, i)])
            };
            pose.translation = lerp3(tr(frame), tr(next));
            pose.scale = lerp3(sc(frame), sc(next));
            pose.rotation = nlerp(ro(frame), ro(next), alpha);
        }
    }
}

/// Normalized linear interpolation along the shorter arc.
pub fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot: f32 = (0..4).map(|i| a[i] * b[i]).sum();
    let sign = if dot < 0.0 { -1.0 } else { 1.0 };
    let q: [f32; 4] = core::array::from_fn(|i| a[i] + (b[i] * sign - a[i]) * t);
    let len = sqrt((0..4).map(|i| q[i] * q[i]).sum::<f32>());
    if len > 0.0 {
        q.map(|c| c / len)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

const ROTATION_BITS: u32 = 15;
const ROTATION_MAX: f32 = ((1 << ROTATION_BITS) - 1) as f32;
/// The three smallest components of a unit quaternion lie in ±1/√2.
const ROTATION_RANGE: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// Quantizes a unit quaternion to 48 bits ("smallest three"): the largest component is dropped
/// (its sign made positive, as q and -q are the same rotation) and rebuilt from the other three,
/// each stored in 15 bits over ±1/√2. The dropped component's index takes the remaining bits:
/// bit 0 of the first word holds its high bit, bit 0 of the second its low bit. Error is about
/// 0.004° per component.
pub fn encode_rotation(q: [f32; 4]) -> [u16; 3] {
    let len = sqrt((0..4).map(|i| q[i] * q[i]).sum::<f32>());
    let q = if len > 0.0 {
        q.map(|c| c / len)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    let mut largest = 0;
    for i in 1..4 {
        if q[i].abs() > q[largest].abs() {
            largest = i;
        }
    }
    let sign = if q[largest] < 0.0 { -1.0 } else { 1.0 };
    let mut out = [0u16; 3];
    let mut n = 0;
    for (i, &c) in q.iter().enumerate() {
        if i == largest {
            continue;
        }
        let unit = ((c * sign / ROTATION_RANGE).clamp(-1.0, 1.0) + 1.0) * 0.5;
        out[n] = (round(unit * ROTATION_MAX) as u16) << 1;
        n += 1;
    }
    out[0] |= (largest as u16 >> 1) & 1;
    out[1] |= largest as u16 & 1;
    out
}

/// Inverse of [`encode_rotation`]: a unit quaternion x, y, z, w.
pub fn decode_rotation(bits: [u16; 3]) -> [f32; 4] {
    let largest = (((bits[0] & 1) << 1) | (bits[1] & 1)) as usize;
    let small = bits.map(|b| ((b >> 1) as f32 / ROTATION_MAX * 2.0 - 1.0) * ROTATION_RANGE);
    let mut q = [0.0f32; 4];
    let mut n = 0;
    for (i, c) in q.iter_mut().enumerate() {
        if i != largest {
            *c = small[n];
            n += 1;
        }
    }
    let rest: f32 = small.iter().map(|c| c * c).sum();
    q[largest] = sqrt((1.0 - rest).max(0.0));
    q
}
