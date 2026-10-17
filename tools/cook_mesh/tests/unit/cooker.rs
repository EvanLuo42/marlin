//! Meshes, skeletons and clips through the cook pipeline, from generated glTF files.

use std::fs;

use marlin_anim::asset::{AnimationClip, AnimationView, JointPose, Skeleton, SkeletonView};
use marlin_asset::Uuid;
use marlin_asset::cooked::{BulkKind, Encoding, UnitView};
use marlin_cook_core::Cookers;
use marlin_cook_mesh::{AnimationCooker, MeshCooker, SkeletonCooker};
use marlin_math::scalar::abs_diff_eq;
use marlin_reflect::type_info;
use marlin_render::asset::Mesh;
use marlin_render::asset::mesh::{MeshFlags, MeshView, dequantize_position, read_attributes};
use marlin_serial::cooked::CookedBuffer;

use crate::support::{Container, Gltf, Project, sphere, uuid};

const MESH: Uuid = uuid(1);
const SKELETON: Uuid = uuid(2);
const CLIP: Uuid = uuid(3);

fn cookers() -> Cookers {
    let mut cookers = Cookers::new();
    cookers.register::<Mesh>(MeshCooker).unwrap();
    cookers.register::<Skeleton>(SkeletonCooker).unwrap();
    cookers.register::<AnimationClip>(AnimationCooker).unwrap();
    cookers
}

/// A sphere in two primitives: the top half with material "Top", the bottom with "Bottom".
fn sphere_gltf() -> Gltf {
    let s = sphere(32, 64);
    let mut g = Gltf::default();
    let top = g.material("Top");
    let bottom = g.material("Bottom");
    let pos = g.vec3(&s.positions);
    let nrm = g.vec3(&s.normals);
    let uv = g.vec2(&s.uvs);
    let (mut upper, mut lower) = (Vec::new(), Vec::new());
    for t in s.indices.as_chunks::<3>().0 {
        let y: f32 = t.iter().map(|&v| s.positions[v as usize][1]).sum();
        if y >= 0.0 { &mut upper } else { &mut lower }.extend_from_slice(t);
    }
    let (iu, il) = (g.indices(&upper), g.indices(&lower));
    let attrs = format!(r#""attributes":{{"POSITION":{pos},"NORMAL":{nrm},"TEXCOORD_0":{uv}}}"#);
    g.meshes.push(format!(
        r#"{{"name":"Ball","primitives":[{{{attrs},"indices":{iu},"material":{top}}},{{{attrs},"indices":{il},"material":{bottom}}}]}}"#
    ));
    g.nodes
        .push(r#"{"mesh":0,"translation":[10.0,0.0,0.0]}"#.into());
    g.scene_nodes.push(0);
    g
}

fn mesh_object(source: &str, extra: &str) -> String {
    format!(
        "!Version 1\nMesh {{ !Uuid = \"{MESH}\" !Name = \"Ball\" Source = `source:{source}` {extra} }}\n"
    )
}

/// The single object of a unit with its blobs.
fn object(unit: &CookedBuffer, index: usize) -> (u64, Encoding, &[u8], Vec<&[u8]>) {
    let view = UnitView::new(unit.as_slice()).unwrap();
    let o = view.object(index).unwrap();
    let blobs = view.object_bulk(&o).map(|b| {
        assert_eq!(b.kind, BulkKind::Vertices);
        view.bulk_bytes(&b)
    });
    let blobs: Vec<&[u8]> = blobs.collect();
    (o.type_id, o.encoding, view.payload(&o), blobs)
}

fn check_mesh(unit: &CookedBuffer) {
    let (type_id, encoding, payload, pages) = object(unit, 0);
    assert_eq!(type_id, type_info::<Mesh>().id.0);
    assert_eq!(encoding, Encoding::Cooked);
    let sizes: Vec<u64> = pages.iter().map(|p| p.len() as u64).collect();
    let mesh = MeshView::new(payload, &sizes).unwrap();
    assert_eq!(mesh.sections.len(), 2);
    assert_eq!(
        (mesh.section_name(0), mesh.section_name(1)),
        ("Top", "Bottom")
    );
    assert!(mesh.lods.len() >= 3, "{} LODs", mesh.lods.len());
    let h = mesh.header;
    // The node transform is applied: centered at x = 10.
    assert!(
        abs_diff_eq(h.sphere[0], 10.0, 0.05) && abs_diff_eq(h.sphere[3], 1.0, 0.05),
        "{:?}",
        h.sphere
    );
    let mut previous = u32::MAX;
    let mut previous_error = -1.0;
    for (i, lod) in mesh.lods.iter().enumerate() {
        mesh.check_page(i, pages[i]).unwrap();
        assert!(lod.triangle_count < previous, "LOD {i} has fewer triangles");
        assert!(lod.error >= previous_error, "and no less error");
        previous = lod.triangle_count;
        previous_error = lod.error;
        // Decoded vertices lie on the sphere, normals pointing out.
        let page = pages[i];
        let positions = &page[lod.positions.offset as usize..][..lod.positions.size as usize];
        let attributes = &page[lod.attributes.offset as usize..][..lod.attributes.size as usize];
        for v in 0..lod.vertex_count as usize {
            let q = std::array::from_fn(|k| {
                u16::from_le_bytes([positions[v * 8 + k * 2], positions[v * 8 + k * 2 + 1]])
            });
            let p = dequantize_position(q, h.position_offset, h.position_scale);
            let d = [p[0] - 10.0, p[1], p[2]];
            let r = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!(abs_diff_eq(r, 1.0, 1e-3), "vertex at radius {r}");
            let a = read_attributes(
                attributes,
                h.attribute_stride as usize,
                v,
                h.uv_sets as usize,
            );
            let dot = (0..3).map(|k| a.normal[k] * d[k] / r).sum::<f32>();
            assert!(dot > 0.99, "normal off by {dot}");
            let ortho = (0..3).map(|k| a.normal[k] * a.tangent[k]).sum::<f32>();
            assert!(
                abs_diff_eq(ortho, 0.0, 0.01),
                "tangent orthogonal to the normal"
            );
        }
        let lod_sections = mesh.lod_sections(i);
        assert!(lod_sections.iter().all(|s| s.meshlet_count > 0));
    }
    assert_eq!(h.flags & MeshFlags::SKIN, 0);
    assert!(mesh.joints.is_empty());
}

#[test]
fn a_glb_cooks_to_meshlets_and_lods() {
    let p = Project::new("glb");
    let (glb, _) = sphere_gltf().finish(Container::Glb);
    p.write("ball.glb", &glb);
    p.write(
        "ball.masset",
        mesh_object("ball.glb", "Lods = { Count = 4 }").as_bytes(),
    );
    let units = p.cook(&cookers()).unwrap();
    check_mesh(&units[0].bytes);
}

#[test]
fn external_buffers_must_be_declared() {
    let p = Project::new("external");
    let (json, bin) = sphere_gltf().finish(Container::External("ball%20data.bin"));
    p.write("art/ball.gltf", &json);
    p.write("art/ball data.bin", &bin);
    p.write("ball.masset", mesh_object("art/ball.gltf", "").as_bytes());
    let errors = p.cook(&cookers()).unwrap_err().join("\n");
    assert!(
        errors.contains("ExtraSources") && errors.contains("art/ball data.bin"),
        "{errors}"
    );

    p.write(
        "ball.masset",
        mesh_object(
            "art/ball.gltf",
            "ExtraSources = [`source:art/ball data.bin`]",
        )
        .as_bytes(),
    );
    let units = p.cook(&cookers()).unwrap();
    check_mesh(&units[0].bytes);
}

#[test]
fn data_uris_need_nothing_else() {
    let p = Project::new("datauri");
    let (json, _) = sphere_gltf().finish(Container::DataUri);
    p.write("ball.gltf", &json);
    p.write("ball.masset", mesh_object("ball.gltf", "").as_bytes());
    let units = p.cook(&cookers()).unwrap();
    check_mesh(&units[0].bytes);
}

#[test]
fn cooking_is_deterministic() {
    let p = Project::new("determinism");
    let (glb, _) = sphere_gltf().finish(Container::Glb);
    p.write("ball.glb", &glb);
    p.write("ball.masset", mesh_object("ball.glb", "").as_bytes());
    let a = p.cook(&cookers()).unwrap();
    fs::remove_dir_all(p.root.join(".marlin")).unwrap();
    let b = p.cook(&cookers()).unwrap();
    assert_eq!(a[0].bytes.as_slice(), b[0].bytes.as_slice());
}

/// A two-joint rig: Armature (not a joint, offset 1 along Z) → Root → Tip (1 up), skinning a
/// 2×2×2 column; Tip turns 90° about Z over one second.
fn rig() -> Gltf {
    let mut g = Gltf::default();
    let mut positions = Vec::new();
    let mut joints = Vec::new();
    let mut weights = Vec::new();
    // Rings at y = 0, 1, 2 of a square column; the middle ring is shared half and half.
    for y in 0..3 {
        for &(x, z) in &[(-0.5f32, -0.5f32), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
            positions.push([x, y as f32, z]);
            joints.push([0u16, 1, 0, 0]);
            let w = y as f32 / 2.0;
            weights.push([1.0 - w, w, 0.0, 0.0]);
        }
    }
    let mut indices = Vec::new();
    for ring in 0..2u32 {
        for k in 0..4u32 {
            let (a, b) = (ring * 4 + k, ring * 4 + (k + 1) % 4);
            indices.extend([a, b, a + 4, b, b + 4, a + 4]);
        }
    }
    let pos = g.vec3(&positions);
    let jnt = g.joints(&joints);
    let wgt = g.vec4(&weights);
    let idx = g.indices(&indices);
    g.meshes.push(format!(
        r#"{{"name":"Column","primitives":[{{"attributes":{{"POSITION":{pos},"JOINTS_0":{jnt},"WEIGHTS_0":{wgt}}},"indices":{idx}}}]}}"#
    ));
    // Inverse bind: Root at (0, 0, 1) in the world, Tip at (0, 1, 1).
    let ibm = |y: f32| {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -y, -1.0, 1.0,
        ]
    };
    let ibms = g.mat4s(&[ibm(0.0), ibm(1.0)]);
    g.nodes
        .push(r#"{"name":"Armature","translation":[0.0,0.0,1.0],"children":[1]}"#.into());
    g.nodes.push(r#"{"name":"Root","children":[2]}"#.into());
    g.nodes
        .push(r#"{"name":"Tip","translation":[0.0,1.0,0.0]}"#.into());
    g.nodes.push(r#"{"name":"Body","mesh":0,"skin":0}"#.into());
    g.scene_nodes.extend([0, 3]);
    g.skins.push(format!(
        r#"{{"name":"Rig","joints":[1,2],"inverseBindMatrices":{ibms}}}"#
    ));
    let times = g.scalars(&[0.0, 1.0]);
    let half = std::f32::consts::FRAC_1_SQRT_2;
    let rotations = g.vec4(&[[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, half, half]]);
    g.animations.push(format!(
        r#"{{"name":"Bend","samplers":[{{"input":{times},"output":{rotations},"interpolation":"LINEAR"}}],"channels":[{{"sampler":0,"target":{{"node":2,"path":"rotation"}}}}]}}"#
    ));
    g
}

#[test]
fn skins_skeletons_and_clips() {
    let p = Project::new("rig");
    let (glb, _) = rig().finish(Container::Glb);
    p.write("rig.glb", &glb);
    p.write(
        "rig.masset",
        format!(
            "!Version 1\n\
             Mesh {{ !Uuid = \"{MESH}\" !Name = \"Body\" Source = `source:rig.glb` Skeleton = <:Skel #{SKELETON}> }}\n\
             Skeleton {{ !Uuid = \"{SKELETON}\" !Name = \"Skel\" Source = `source:rig.glb` }}\n\
             AnimationClip {{ !Uuid = \"{CLIP}\" !Name = \"Bend\" Source = `source:rig.glb` Skeleton = <:Skel #{SKELETON}> Looping = false }}\n"
        )
        .as_bytes(),
    );
    let units = p.cook(&cookers()).unwrap();
    let unit = &units[0].bytes;
    let view = UnitView::new(unit.as_slice()).unwrap();
    let find = |uuid: Uuid| view.find(uuid).unwrap().0;

    // The mesh: skinned, joints by name, weights summing to 255.
    let (_, _, payload, pages) = object(unit, find(MESH));
    let sizes: Vec<u64> = pages.iter().map(|p| p.len() as u64).collect();
    let mesh = MeshView::new(payload, &sizes).unwrap();
    assert_ne!(mesh.header.flags & MeshFlags::SKIN, 0);
    assert_eq!(mesh.joints.len(), 2);
    mesh.check_page(0, pages[0]).unwrap();
    let lod = &mesh.lods[0];
    let skin = &pages[0][lod.skin.offset as usize..][..lod.skin.size as usize];
    for v in skin.as_chunks::<12>().0 {
        assert_eq!(v[8..12].iter().map(|&w| w as u32).sum::<u32>(), 255);
    }

    // The skeleton: parents first, the armature's offset in the root's rest pose.
    let o = view.object(find(SKELETON)).unwrap();
    let skeleton = SkeletonView::new(view.payload(&o)).unwrap();
    assert_eq!(skeleton.joints.len(), 2);
    assert_eq!((skeleton.name(0), skeleton.name(1)), ("Root", "Tip"));
    assert_eq!(
        (skeleton.joints[0].parent, skeleton.joints[1].parent),
        (-1, 0)
    );
    assert_eq!(skeleton.joints[0].translation, [0.0, 0.0, 1.0]);
    assert_eq!(skeleton.joints[1].translation, [0.0, 1.0, 0.0]);

    // The clip: Tip at half time is turned 45°.
    let o = view.object(find(CLIP)).unwrap();
    let clip = AnimationView::new(view.payload(&o)).unwrap();
    assert!(abs_diff_eq(clip.header.duration, 1.0, 1e-6));
    assert_eq!(clip.header.frame_count, 31);
    let binding = clip.bind(&skeleton);
    let mut pose = vec![JointPose::IDENTITY; 2];
    let mut scratch = Vec::new();
    clip.sample_pose(0.5, &skeleton, &binding, &mut scratch, &mut pose);
    let angle = 2.0 * pose[1].rotation[2].atan2(pose[1].rotation[3]);
    assert!(
        abs_diff_eq(angle.to_degrees(), 45.0, 0.1),
        "{}°",
        angle.to_degrees()
    );
    assert_eq!(
        pose[0].translation,
        [0.0, 0.0, 1.0],
        "the root keeps its rest pose"
    );
    // Clamped past the end.
    clip.sample_pose(5.0, &skeleton, &binding, &mut scratch, &mut pose);
    let angle = 2.0 * pose[1].rotation[2].atan2(pose[1].rotation[3]);
    assert!(abs_diff_eq(angle.to_degrees(), 90.0, 0.1));
}

#[test]
fn import_errors() {
    let p = Project::new("errors");
    p.write("no.masset", mesh_object("nothing.glb", "").as_bytes());
    p.write("bad.txt", b"not a mesh");
    let mut g = sphere_gltf();
    g.nodes.clear();
    g.scene_nodes.clear();
    let (glb, _) = g.finish(Container::Glb);
    p.write("empty.glb", &glb);
    p.write("empty.masset", mesh_object("empty.glb", "").as_bytes());
    p.write(
        "named.masset",
        mesh_object("empty.glb", "Mesh = \"Nope\"").as_bytes(),
    );
    let errors = p.cook(&cookers()).unwrap_err().join("\n");
    assert!(errors.contains("nothing.glb"), "{errors}");
    assert!(errors.contains("no meshes"), "{errors}");
    assert!(
        errors.contains("no mesh named `Nope`") && errors.contains("Ball"),
        "{errors}"
    );
}

/// Armature (not a joint, 1 along Z) → Root → Socket (not a joint, 2 along X) → Tip (1 up),
/// with Root and Tip keyed at their rest translation.
fn rig_with_a_socket() -> marlin_cook_mesh::import::Document {
    let mut g = Gltf::default();
    g.nodes
        .push(r#"{"name":"Armature","translation":[0.0,0.0,1.0],"children":[1]}"#.into());
    g.nodes.push(r#"{"name":"Root","children":[2]}"#.into());
    g.nodes
        .push(r#"{"name":"Socket","translation":[2.0,0.0,0.0],"children":[3]}"#.into());
    g.nodes
        .push(r#"{"name":"Tip","translation":[0.0,1.0,0.0]}"#.into());
    g.scene_nodes.push(0);
    g.skins.push(r#"{"name":"Rig","joints":[1,3]}"#.into());
    let times = g.scalars(&[0.0, 1.0]);
    let root = g.vec3(&[[0.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
    let tip = g.vec3(&[[0.0, 1.0, 0.0], [0.0, 1.0, 0.0]]);
    g.animations.push(format!(
        r#"{{"name":"Rest","samplers":[{{"input":{times},"output":{root}}},{{"input":{times},"output":{tip}}}],"channels":[{{"sampler":0,"target":{{"node":1,"path":"translation"}}}},{{"sampler":1,"target":{{"node":3,"path":"translation"}}}}]}}"#
    ));
    let (glb, _) = g.finish(Container::Glb);
    marlin_cook_mesh::import::open(&glb, "rig.glb", &mut |p| Err(format!("no {p}"))).unwrap()
}

#[test]
fn clips_and_skeletons_agree_on_non_joint_nodes_between_joints() {
    use marlin_cook_mesh::import::{import_animation, import_skeleton};
    let doc = rig_with_a_socket();
    let skeleton = import_skeleton(&doc, "", 1.0).unwrap();
    let names: Vec<&str> = skeleton.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(names, ["Root", "Tip"]);
    assert_eq!(
        skeleton[0].translation,
        [0.0, 0.0, 1.0],
        "Armature folded in"
    );
    assert_eq!(
        skeleton[1].translation,
        [2.0, 1.0, 0.0],
        "Socket folded in, Root not"
    );

    // Keyed at rest, every frame is the skeleton's rest pose: the clip folds in exactly the
    // nodes the skeleton does, no more (Root's transform comes from Root's own track).
    let (clip, _) = import_animation(&doc, "", 4.0, 1.0).unwrap();
    for joint in &skeleton {
        let track = clip.tracks.iter().find(|t| t.name == joint.name).unwrap();
        for t in &track.translations {
            for (got, want) in t.iter().zip(joint.translation) {
                assert!(abs_diff_eq(*got, want, 1e-6), "{}: {t:?}", joint.name);
            }
        }
    }
}
