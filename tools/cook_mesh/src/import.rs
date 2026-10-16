//! glTF 2.0 import: meshes, skins and animations into plain arrays.
//!
//! Parsing is the `gltf` crate's; resolving buffers is ours, so every byte read goes through the
//! cook context's declared sources: GLB binary chunks, external buffers (which must be listed in
//! the asset's `ExtraSources`) and `data:` URIs.

use std::collections::HashMap;

use gltf::Gltf;
use gltf::animation::Interpolation;
use gltf::animation::util::ReadOutputs;
use gltf::mesh::Mode;

use crate::math::{self, Mat4};
use crate::tangent;

/// A parsed glTF with its buffers.
pub struct Document {
    pub gltf: Gltf,
    pub buffers: Vec<Vec<u8>>,
}

/// Parses `bytes` (a `.gltf` or `.glb` at asset path `path`). `read` returns another source
/// file by asset path, for external buffers.
pub fn open(
    bytes: &[u8],
    path: &str,
    read: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
) -> Result<Document, String> {
    let gltf = Gltf::from_slice(bytes).map_err(|e| format!("`{path}`: {e}"))?;
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut buffers = Vec::new();
    for buffer in gltf.buffers() {
        let data = match buffer.source() {
            gltf::buffer::Source::Bin => gltf.blob.clone().ok_or_else(|| {
                format!(
                    "`{path}`: buffer {} refers to a missing GLB binary chunk",
                    buffer.index()
                )
            })?,
            gltf::buffer::Source::Uri(uri) => {
                if let Some(data) = uri.strip_prefix("data:") {
                    let (_, encoded) = data.split_once(";base64,").ok_or_else(|| {
                        format!(
                            "`{path}`: buffer {} has a data URI that is not base64",
                            buffer.index()
                        )
                    })?;
                    base64(encoded)
                        .ok_or_else(|| format!("`{path}`: buffer {}: bad base64", buffer.index()))?
                } else {
                    let rel = percent_decode(uri);
                    let target = join(dir, &rel).ok_or_else(|| {
                        format!("`{path}`: buffer URI `{uri}` leaves the asset root")
                    })?;
                    read(&target).map_err(|e| {
                        format!("{e} (external buffer of `{path}`: add `source:{target}` to `ExtraSources`)")
                    })?
                }
            }
        };
        if (data.len() as u64) < buffer.length() as u64 {
            return Err(format!(
                "`{path}`: buffer {} is {} bytes, shorter than the {} it declares",
                buffer.index(),
                data.len(),
                buffer.length()
            ));
        }
        buffers.push(data);
    }
    Ok(Document { gltf, buffers })
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut acc = 0u32;
        for &c in chunk {
            acc = acc << 6 | value(c)?;
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]),
            3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
            2 => out.push((acc >> 4) as u8),
            _ => return None,
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `dir/rel` with `.` and `..` resolved; `None` if it climbs out of the root.
fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// A joint of a skin: name and bind matrix.
#[derive(Clone, PartialEq, Debug)]
pub struct SkinJoint {
    pub name: String,
    pub inverse_bind: Mat4,
}

/// Imported triangle geometry. Every array is per vertex but `indices` (three per triangle) and
/// `triangle_sections` (one per triangle).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Geometry {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// xyz and the bitangent sign.
    pub tangents: Vec<[f32; 4]>,
    pub uvs: Vec<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[f32; 4]>>,
    /// Up to eight influences: joint indices into `skin` and weights, by descending weight.
    pub influences: Option<Vec<[(u16, f32); 8]>>,
    pub skin: Vec<SkinJoint>,
    pub section_names: Vec<String>,
    pub indices: Vec<u32>,
    pub triangle_sections: Vec<u16>,
}

impl Geometry {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }
}

/// What to import from a document.
pub struct MeshOptions<'a> {
    /// A mesh by name, or every mesh of the default scene.
    pub mesh: &'a str,
    pub scale: f32,
    pub max_uv_sets: usize,
    pub colors: bool,
}

fn node_name(node: &gltf::Node<'_>) -> String {
    node.name()
        .map_or_else(|| format!("Node{}", node.index()), str::to_owned)
}

/// World transforms of every node of the default scene (or the first, or all roots).
fn world_transforms(doc: &Document) -> (Vec<Option<Mat4>>, Vec<usize>) {
    let g = &doc.gltf;
    let count = g.nodes().count();
    let mut world = vec![None; count];
    let mut order = Vec::new();
    let roots: Vec<gltf::Node<'_>> = match g.default_scene().or_else(|| g.scenes().next()) {
        Some(scene) => scene.nodes().collect(),
        None => {
            let mut child = vec![false; count];
            for n in g.nodes() {
                for c in n.children() {
                    child[c.index()] = true;
                }
            }
            g.nodes().filter(|n| !child[n.index()]).collect()
        }
    };
    let mut stack: Vec<(gltf::Node<'_>, Mat4)> = roots
        .into_iter()
        .rev()
        .map(|n| (n, math::IDENTITY))
        .collect();
    while let Some((node, parent)) = stack.pop() {
        if world[node.index()].is_some() {
            continue; // a malformed graph with shared children
        }
        let m = math::mul(&parent, &math::from_cols(node.transform().matrix()));
        world[node.index()] = Some(m);
        order.push(node.index());
        let children: Vec<_> = node.children().collect();
        for c in children.into_iter().rev() {
            stack.push((c, m));
        }
    }
    (world, order)
}

/// Imports triangle geometry. Returns it with warnings.
pub fn import_mesh(
    doc: &Document,
    options: &MeshOptions<'_>,
) -> Result<(Geometry, Vec<String>), String> {
    let g = &doc.gltf;
    let mut warnings = Vec::new();
    // (mesh, transform, skin)
    let mut instances: Vec<(gltf::Mesh<'_>, Mat4, Option<gltf::Skin<'_>>)> = Vec::new();
    if options.mesh.is_empty() {
        let (world, order) = world_transforms(doc);
        let nodes: Vec<gltf::Node<'_>> = g.nodes().collect();
        for i in order {
            let node = &nodes[i];
            if let Some(mesh) = node.mesh() {
                // Skinned meshes are placed by their joints; the node transform is ignored.
                let skin = node.skin();
                let m = if skin.is_some() {
                    math::IDENTITY
                } else {
                    world[i].unwrap_or(math::IDENTITY)
                };
                instances.push((mesh, m, skin));
            }
        }
        if instances.is_empty() {
            return Err("the default scene has no meshes".into());
        }
    } else {
        let matches: Vec<_> = g
            .meshes()
            .filter(|m| m.name() == Some(options.mesh))
            .collect();
        let mesh = match matches.as_slice() {
            [m] => m.clone(),
            [] => {
                let names: Vec<_> = g
                    .meshes()
                    .filter_map(|m| m.name().map(str::to_owned))
                    .collect();
                return Err(format!(
                    "no mesh named `{}` (the file has {names:?})",
                    options.mesh
                ));
            }
            _ => return Err(format!("several meshes are named `{}`", options.mesh)),
        };
        let skin = g
            .nodes()
            .find(|n| n.mesh().is_some_and(|m| m.index() == mesh.index()))
            .and_then(|n| n.skin());
        instances.push((mesh, math::IDENTITY, skin));
    }
    let skins: Vec<Option<usize>> = instances
        .iter()
        .map(|i| i.2.as_ref().map(|s| s.index()))
        .collect();
    let skin = skins[0];
    if skins.iter().any(|s| *s != skin) {
        return Err(
            "the meshes merged here use different skins (or some none); import them separately"
                .into(),
        );
    }

    let mut out = Geometry::default();
    let mut section_of_material: HashMap<Option<usize>, u16> = HashMap::new();
    let uv_sets = instances
        .iter()
        .flat_map(|(m, _, _)| m.primitives())
        .map(|p| {
            (0..)
                .take_while(|&s| p.get(&gltf::Semantic::TexCoords(s)).is_some())
                .count()
        })
        .max()
        .unwrap_or(0)
        .min(options.max_uv_sets);
    let any_color = options.colors
        && instances
            .iter()
            .flat_map(|(m, _, _)| m.primitives())
            .any(|p| p.get(&gltf::Semantic::Colors(0)).is_some());
    out.uvs = vec![Vec::new(); uv_sets];
    if any_color {
        out.colors = Some(Vec::new());
    }
    let buffers = |b: gltf::Buffer<'_>| doc.buffers.get(b.index()).map(Vec::as_slice);

    if let Some(skin) = instances[0].2.clone() {
        let reader = skin.reader(buffers);
        let ibms: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
            Some(it) => it.map(math::from_cols).collect(),
            None => vec![math::IDENTITY; skin.joints().count()],
        };
        out.skin = skin
            .joints()
            .zip(ibms)
            .map(|(j, mut ibm)| {
                for t in &mut ibm[12..15] {
                    *t *= options.scale;
                }
                SkinJoint {
                    name: node_name(&j),
                    inverse_bind: ibm,
                }
            })
            .collect();
        out.influences = Some(Vec::new());
    }

    for (mesh, transform, _) in &instances {
        if mesh.weights().is_some_and(|w| !w.is_empty())
            || mesh
                .primitives()
                .any(|p| p.morph_targets().next().is_some())
        {
            warnings.push(format!(
                "mesh `{}` has morph targets, which are not imported yet",
                mesh.name().unwrap_or("?")
            ));
        }
        for primitive in mesh.primitives() {
            let reader = primitive.reader(buffers);
            let Some(positions) = reader.read_positions() else {
                warnings.push("a primitive without positions was skipped".into());
                continue;
            };
            let mut positions: Vec<[f32; 3]> = positions.collect();
            let count = positions.len() as u32;
            let raw: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..count).collect(),
            };
            if raw.iter().any(|&i| i >= count) {
                return Err("a primitive has indices past its vertices".into());
            }
            let mut indices: Vec<u32> = match primitive.mode() {
                Mode::Triangles => raw.as_chunks::<3>().0.iter().flatten().copied().collect(),
                Mode::TriangleStrip => (2..raw.len())
                    .flat_map(|i| {
                        if i % 2 == 0 {
                            [raw[i - 2], raw[i - 1], raw[i]]
                        } else {
                            [raw[i - 1], raw[i - 2], raw[i]]
                        }
                    })
                    .collect(),
                Mode::TriangleFan => (2..raw.len())
                    .flat_map(|i| [raw[0], raw[i - 1], raw[i]])
                    .collect(),
                mode => {
                    warnings.push(format!(
                        "a {mode:?} primitive was skipped: only triangles are meshes"
                    ));
                    continue;
                }
            };
            let mut normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
            let mut tangents: Option<Vec<[f32; 4]>> = reader.read_tangents().map(Iterator::collect);
            let mut uvs: Vec<Vec<[f32; 2]>> = (0..uv_sets as u32)
                .map(|s| match reader.read_tex_coords(s) {
                    Some(t) => t.into_f32().collect(),
                    None => vec![[0.0; 2]; count as usize],
                })
                .collect();
            let mut colors: Option<Vec<[f32; 4]>> =
                any_color.then(|| match reader.read_colors(0) {
                    Some(c) => c.into_rgba_f32().collect(),
                    None => vec![[1.0; 4]; count as usize],
                });
            let mut influences: Option<Vec<[(u16, f32); 8]>> =
                out.influences.is_some().then(|| {
                    let mut inf = vec![[(0u16, 0.0f32); 8]; count as usize];
                    for set in 0..2u32 {
                        if let (Some(j), Some(w)) =
                            (reader.read_joints(set), reader.read_weights(set))
                        {
                            for (v, (j, w)) in j.into_u16().zip(w.into_f32()).enumerate() {
                                for k in 0..4 {
                                    inf[v][set as usize * 4 + k] = (j[k], w[k]);
                                }
                            }
                        }
                    }
                    for i in &mut inf {
                        i.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                    }
                    inf
                });
            if let Some(inf) = &influences
                && inf
                    .iter()
                    .flatten()
                    .any(|&(j, w)| w > 0.0 && j as usize >= out.skin.len())
            {
                return Err("a vertex is weighted to a joint the skin does not have".into());
            }

            // Missing normals: flat shading, as glTF specifies. Unweld so each triangle owns its
            // corners.
            if normals.is_none() {
                let unweld = |v: Vec<[f32; 2]>| -> Vec<[f32; 2]> {
                    indices.iter().map(|&i| v[i as usize]).collect()
                };
                positions = indices.iter().map(|&i| positions[i as usize]).collect();
                uvs = uvs.into_iter().map(unweld).collect();
                colors = colors.map(|c| indices.iter().map(|&i| c[i as usize]).collect());
                influences = influences.map(|c| indices.iter().map(|&i| c[i as usize]).collect());
                tangents = tangents.map(|c| indices.iter().map(|&i| c[i as usize]).collect());
                indices = (0..positions.len() as u32).collect();
                let flat = positions
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|t| {
                        let e1 = std::array::from_fn(|i| t[1][i] - t[0][i]);
                        let e2 = std::array::from_fn(|i| t[2][i] - t[0][i]);
                        let n = math::normalize3(cross(e1, e2));
                        [n; 3]
                    })
                    .collect();
                normals = Some(flat);
            }
            let mut normals = normals.unwrap_or_default();

            // Into the merged space.
            let mirror = math::det3(transform) < 0.0;
            let nm = math::normal_matrix(transform);
            for p in &mut positions {
                *p = math::transform_point(transform, *p).map(|c| c * options.scale);
            }
            for n in &mut normals {
                *n = math::normalize3(math::transform_vector(&nm, *n));
            }
            if let Some(t) = &mut tangents {
                for t in t.iter_mut() {
                    let v = math::normalize3(math::transform_vector(transform, [t[0], t[1], t[2]]));
                    let w = if mirror { -t[3] } else { t[3] };
                    *t = [v[0], v[1], v[2], if w < 0.0 { -1.0 } else { 1.0 }];
                }
            }
            if mirror {
                for t in indices.as_chunks_mut::<3>().0 {
                    t.swap(1, 2);
                }
            }

            let tangents = match tangents {
                Some(t) => t,
                None => {
                    let generated = tangent::generate(
                        &positions,
                        &normals,
                        uvs.first().map(Vec::as_slice),
                        &indices,
                    );
                    for &v in &generated.splits {
                        let v = v as usize;
                        positions.push(positions[v]);
                        normals.push(normals[v]);
                        for set in &mut uvs {
                            set.push(set[v]);
                        }
                        if let Some(c) = &mut colors {
                            c.push(c[v]);
                        }
                        if let Some(c) = &mut influences {
                            c.push(c[v]);
                        }
                    }
                    indices = generated.indices;
                    generated.tangents
                }
            };

            let material = primitive.material().index();
            let next = section_of_material.len() as u16;
            let section = *section_of_material.entry(material).or_insert_with(|| {
                out.section_names.push(match material {
                    Some(i) => primitive
                        .material()
                        .name()
                        .map_or_else(|| format!("Material{i}"), str::to_owned),
                    None => "Default".to_owned(),
                });
                next
            });
            let base = out.positions.len() as u32;
            out.indices.extend(indices.iter().map(|i| i + base));
            out.triangle_sections
                .extend(std::iter::repeat_n(section, indices.len() / 3));
            out.positions.extend(positions);
            out.normals.extend(normals);
            out.tangents.extend(tangents);
            for (set, data) in out.uvs.iter_mut().zip(uvs) {
                set.extend(data);
            }
            if let (Some(all), Some(c)) = (&mut out.colors, colors) {
                all.extend(c);
            }
            if let (Some(all), Some(c)) = (&mut out.influences, influences) {
                all.extend(c);
            }
        }
    }
    if out.indices.is_empty() {
        return Err("nothing to import: no triangles".into());
    }
    if section_of_material.len() > u16::MAX as usize {
        return Err("too many materials".into());
    }
    Ok((out, warnings))
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// A skeleton joint in parent-first order.
#[derive(Clone, PartialEq, Debug)]
pub struct Joint {
    pub name: String,
    pub parent: i32,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub inverse_bind: Mat4,
}

/// Every node's parent.
fn parents(doc: &Document) -> Vec<Option<usize>> {
    let mut parent = vec![None; doc.gltf.nodes().count()];
    for n in doc.gltf.nodes() {
        for c in n.children() {
            parent[c.index()] = Some(n.index());
        }
    }
    parent
}

fn local(node: &gltf::Node<'_>) -> Mat4 {
    math::from_cols(node.transform().matrix())
}

/// The transform from `ancestor` (exclusive, or the scene root for `None`) down to `node`
/// (inclusive).
fn relative(
    doc: &Document,
    parent: &[Option<usize>],
    node: usize,
    ancestor: Option<usize>,
) -> Mat4 {
    let nodes: Vec<gltf::Node<'_>> = doc.gltf.nodes().collect();
    let mut m = local(&nodes[node]);
    let mut at = parent[node];
    while at != ancestor {
        let Some(p) = at else { break };
        m = math::mul(&local(&nodes[p]), &m);
        at = parent[p];
    }
    m
}

/// Imports the skin named `skin` (or the first) as a skeleton.
pub fn import_skeleton(doc: &Document, skin: &str, scale: f32) -> Result<Vec<Joint>, String> {
    let g = &doc.gltf;
    let skin = if skin.is_empty() {
        g.skins().next().ok_or("the file has no skins")?
    } else {
        g.skins()
            .find(|s| s.name() == Some(skin))
            .ok_or_else(|| format!("no skin named `{skin}`"))?
    };
    let buffers = |b: gltf::Buffer<'_>| doc.buffers.get(b.index()).map(Vec::as_slice);
    let joints: Vec<gltf::Node<'_>> = skin.joints().collect();
    let ibms: Vec<Mat4> = match skin.reader(buffers).read_inverse_bind_matrices() {
        Some(it) => it.map(math::from_cols).collect(),
        None => vec![math::IDENTITY; joints.len()],
    };
    let parent = parents(doc);
    let index_of: HashMap<usize, usize> = joints
        .iter()
        .enumerate()
        .map(|(i, j)| (j.index(), i))
        .collect();
    // Nearest joint ancestor of each joint.
    let joint_parent: Vec<Option<usize>> = joints
        .iter()
        .map(|j| {
            let mut at = parent[j.index()];
            while let Some(p) = at {
                if index_of.contains_key(&p) {
                    return Some(p);
                }
                at = parent[p];
            }
            None
        })
        .collect();
    let depth = |mut i: usize| {
        let mut d = 0;
        while let Some(p) = joint_parent[i] {
            d += 1;
            i = index_of[&p];
            if d > joints.len() {
                break;
            }
        }
        d
    };
    let mut order: Vec<usize> = (0..joints.len()).collect();
    order.sort_by_key(|&i| (depth(i), i));
    let position: HashMap<usize, usize> = order.iter().enumerate().map(|(k, &i)| (i, k)).collect();

    let mut names = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(joints.len());
    for &i in &order {
        let j = &joints[i];
        let name = node_name(j);
        if !names.insert(name.clone()) {
            return Err(format!(
                "two joints are named `{name}`; joints bind by name"
            ));
        }
        let m = relative(doc, &parent, j.index(), joint_parent[i]);
        let (mut translation, rotation, s) = math::decompose(&m);
        translation = translation.map(|c| c * scale);
        let mut ibm = ibms.get(i).copied().unwrap_or(math::IDENTITY);
        for t in &mut ibm[12..15] {
            *t *= scale;
        }
        out.push(Joint {
            name,
            parent: joint_parent[i].map_or(-1, |p| position[&index_of[&p]] as i32),
            translation,
            rotation,
            scale: s,
            inverse_bind: ibm,
        });
    }
    Ok(out)
}

/// A clip resampled at a fixed rate: per animated node, `frames` keys per channel.
#[derive(Clone, PartialEq, Debug)]
pub struct Clip {
    pub duration: f32,
    pub frames: u32,
    pub tracks: Vec<Track>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Track {
    pub name: String,
    pub translations: Vec<[f32; 3]>,
    pub rotations: Vec<[f32; 4]>,
    pub scales: Vec<[f32; 3]>,
}

/// Keys of one channel, as read.
enum Keys {
    Vec3(Vec<[f32; 3]>),
    Quat(Vec<[f32; 4]>),
}

fn sample_channel(times: &[f32], keys: &Keys, interpolation: Interpolation, t: f32) -> [f32; 4] {
    let cubic = interpolation == Interpolation::CubicSpline;
    let value = |i: usize| -> [f32; 4] {
        let k = if cubic { i * 3 + 1 } else { i };
        match keys {
            Keys::Vec3(v) => [v[k][0], v[k][1], v[k][2], 0.0],
            Keys::Quat(q) => q[k],
        }
    };
    let n = times.len();
    if n == 0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    if t <= times[0] {
        return value(0);
    }
    if t >= times[n - 1] {
        return value(n - 1);
    }
    let i = times.partition_point(|&x| x <= t) - 1;
    let (t0, t1) = (times[i], times[i + 1]);
    let dt = t1 - t0;
    let u = if dt > 0.0 { (t - t0) / dt } else { 0.0 };
    let is_quat = matches!(keys, Keys::Quat(_));
    match interpolation {
        Interpolation::Step => value(i),
        Interpolation::Linear => {
            let (a, b) = (value(i), value(i + 1));
            if is_quat {
                math::slerp(a, b, u)
            } else {
                std::array::from_fn(|c| a[c] + (b[c] - a[c]) * u)
            }
        }
        Interpolation::CubicSpline => {
            let tangent = |k: usize| -> [f32; 4] {
                match keys {
                    Keys::Vec3(v) => [v[k][0], v[k][1], v[k][2], 0.0],
                    Keys::Quat(q) => q[k],
                }
            };
            let (p0, m0) = (value(i), tangent(i * 3 + 2).map(|c| c * dt));
            let (p1, m1) = (value(i + 1), tangent((i + 1) * 3).map(|c| c * dt));
            let (u2, u3) = (u * u, u * u * u);
            let (h00, h10, h01, h11) = (
                2.0 * u3 - 3.0 * u2 + 1.0,
                u3 - 2.0 * u2 + u,
                -2.0 * u3 + 3.0 * u2,
                u3 - u2,
            );
            let r = std::array::from_fn(|c| h00 * p0[c] + h10 * m0[c] + h01 * p1[c] + h11 * m1[c]);
            if is_quat { math::quat_normalize(r) } else { r }
        }
    }
}

/// Imports the animation named `name` (or the first), resampled at `rate` frames per second.
/// Root joints (whose parent is no joint of any skin) take their parent's rest transform into
/// their keys, as the skeleton does into their rest pose.
pub fn import_animation(
    doc: &Document,
    name: &str,
    rate: f32,
    scale: f32,
) -> Result<(Clip, Vec<String>), String> {
    if !(rate.is_finite() && rate > 0.0) {
        return Err(format!("`SampleRate` must be positive, not {rate}"));
    }
    let g = &doc.gltf;
    let anim = if name.is_empty() {
        g.animations().next().ok_or("the file has no animations")?
    } else {
        g.animations()
            .find(|a| a.name() == Some(name))
            .ok_or_else(|| format!("no animation named `{name}`"))?
    };
    let mut warnings = Vec::new();
    let buffers = |b: gltf::Buffer<'_>| doc.buffers.get(b.index()).map(Vec::as_slice);
    // node -> (translation, rotation, scale) channels.
    type Channel = (Vec<f32>, Keys, Interpolation);
    let mut channels: HashMap<usize, [Option<Channel>; 3]> = HashMap::new();
    let mut duration = 0.0f32;
    for channel in anim.channels() {
        let reader = channel.reader(buffers);
        let (Some(inputs), Some(outputs)) = (reader.read_inputs(), reader.read_outputs()) else {
            return Err("an animation channel has no keys".into());
        };
        let times: Vec<f32> = inputs.collect();
        if times.windows(2).any(|w| w[1] < w[0]) {
            return Err("an animation channel's key times decrease".into());
        }
        duration = duration.max(times.last().copied().unwrap_or(0.0));
        let interpolation = channel.sampler().interpolation();
        let (slot, keys) = match outputs {
            ReadOutputs::Translations(t) => (0, Keys::Vec3(t.collect())),
            ReadOutputs::Rotations(r) => (1, Keys::Quat(r.into_f32().collect())),
            ReadOutputs::Scales(s) => (2, Keys::Vec3(s.collect())),
            ReadOutputs::MorphTargetWeights(_) => {
                warnings.push("morph target weight channels are not imported yet".into());
                continue;
            }
        };
        let expected = times.len()
            * if interpolation == Interpolation::CubicSpline {
                3
            } else {
                1
            };
        let have = match &keys {
            Keys::Vec3(v) => v.len(),
            Keys::Quat(q) => q.len(),
        };
        if have != expected {
            return Err(format!(
                "an animation channel has {have} values for {} keys",
                times.len()
            ));
        }
        channels.entry(channel.target().node().index()).or_default()[slot] =
            Some((times, keys, interpolation));
    }
    let frames = (duration * rate).ceil() as u32 + 1;
    let parent = parents(doc);
    let joint_nodes: std::collections::HashSet<usize> = g
        .skins()
        .flat_map(|s| s.joints().map(|j| j.index()).collect::<Vec<_>>())
        .collect();
    let nodes: Vec<gltf::Node<'_>> = g.nodes().collect();
    let mut node_ids: Vec<usize> = channels.keys().copied().collect();
    node_ids.sort_unstable();
    let mut tracks = Vec::with_capacity(node_ids.len());
    for node in node_ids {
        let slots = &channels[&node];
        let (rest_t, rest_r, rest_s) = nodes[node].transform().decomposed();
        // A root joint's keys are relative to its non-joint ancestors' rest transform.
        let root_parent = match parent[node] {
            Some(p) if joint_nodes.contains(&node) && !joint_nodes.contains(&p) => {
                let mut m = local(&nodes[p]);
                let mut at = parent[p];
                while let Some(a) = at {
                    m = math::mul(&local(&nodes[a]), &m);
                    at = parent[a];
                }
                Some(m)
            }
            _ => None,
        };
        let mut track = Track {
            name: node_name(&nodes[node]),
            translations: Vec::with_capacity(frames as usize),
            rotations: Vec::with_capacity(frames as usize),
            scales: Vec::with_capacity(frames as usize),
        };
        for f in 0..frames {
            let t = (f as f32 / rate).min(duration);
            let get = |slot: usize, rest: [f32; 4]| match &slots[slot] {
                Some((times, keys, interp)) => sample_channel(times, keys, *interp, t),
                None => rest,
            };
            let tr = get(0, [rest_t[0], rest_t[1], rest_t[2], 0.0]);
            let ro = get(1, rest_r);
            let sc = get(2, [rest_s[0], rest_s[1], rest_s[2], 0.0]);
            let (mut tr, ro, sc) = (
                [tr[0], tr[1], tr[2]],
                math::quat_normalize(ro),
                [sc[0], sc[1], sc[2]],
            );
            let (tr2, ro2, sc2) = match &root_parent {
                Some(p) => math::decompose(&math::mul(p, &math::compose(tr, ro, sc))),
                None => (tr, ro, sc),
            };
            tr = tr2.map(|c| c * scale);
            track.translations.push(tr);
            track.rotations.push(ro2);
            track.scales.push(sc2);
        }
        tracks.push(track);
    }
    Ok((
        Clip {
            duration,
            frames,
            tracks,
        },
        warnings,
    ))
}
