//! Test geometry and a minimal glTF writer.
#![allow(dead_code, reason = "each test file uses a subset")]

use std::fmt::Write;

/// xorshift64*.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A `segments` × `segments` grid over [0, 1]² in the XZ plane, +Y up, UVs = (x, z).
pub fn grid(segments: u32) -> (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>) {
    let n = segments + 1;
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    for z in 0..n {
        for x in 0..n {
            let (u, v) = (x as f32 / segments as f32, z as f32 / segments as f32);
            positions.push([u, 0.0, v]);
            uvs.push([u, v]);
        }
    }
    let mut indices = Vec::new();
    for z in 0..segments {
        for x in 0..segments {
            let i = z * n + x;
            // Counter-clockwise seen from +Y.
            indices.extend([i, i + n, i + 1, i + 1, i + n, i + n + 1]);
        }
    }
    (positions, uvs, indices)
}

/// A UV sphere of radius 1: `rings` latitude bands, `segments` longitude steps, with the UV seam
/// duplicated (so it has an attribute seam) and one vertex per pole per segment.
pub struct Sphere {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

pub fn sphere(rings: u32, segments: u32) -> Sphere {
    let mut s = Sphere {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
    };
    for r in 0..=rings {
        let theta = r as f32 / rings as f32 * std::f32::consts::PI;
        for k in 0..=segments {
            let phi = k as f32 / segments as f32 * std::f32::consts::TAU;
            let p = [
                theta.sin() * phi.cos(),
                theta.cos(),
                -theta.sin() * phi.sin(),
            ];
            // Exact poles, so all pole vertices share a position.
            let p = if r == 0 {
                [0.0, 1.0, 0.0]
            } else if r == rings {
                [0.0, -1.0, 0.0]
            } else {
                p
            };
            s.positions.push(p);
            s.normals.push(p);
            s.uvs
                .push([k as f32 / segments as f32, r as f32 / rings as f32]);
        }
    }
    let w = segments + 1;
    for r in 0..rings {
        for k in 0..segments {
            let a = r * w + k;
            let (b, c, d) = (a + 1, a + w, a + w + 1);
            if r != 0 {
                s.indices.extend([a, c, b]);
            }
            if r != rings - 1 {
                s.indices.extend([b, c, d]);
            }
        }
    }
    s
}

fn push_f32(bin: &mut Vec<u8>, values: impl IntoIterator<Item = f32>) {
    for v in values {
        bin.extend(v.to_le_bytes());
    }
}

/// A minimal glTF document builder: one buffer.
#[derive(Default)]
pub struct Gltf {
    pub bin: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
    pub meshes: Vec<String>,
    pub nodes: Vec<String>,
    pub skins: Vec<String>,
    pub animations: Vec<String>,
    pub materials: Vec<String>,
    pub scene_nodes: Vec<usize>,
}

pub enum Container {
    /// A `.glb` with the buffer in its binary chunk.
    Glb,
    /// A `.gltf` with the buffer in an external file of this name.
    External(&'static str),
    /// A `.gltf` with the buffer in a data URI.
    DataUri,
}

impl Gltf {
    fn accessor(
        &mut self,
        bytes: Vec<u8>,
        component: u32,
        count: usize,
        ty: &str,
        minmax: Option<(Vec<f32>, Vec<f32>)>,
    ) -> usize {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend(&bytes);
        self.views.push(format!(
            r#"{{"buffer":0,"byteOffset":{offset},"byteLength":{}}}"#,
            bytes.len()
        ));
        let mut a = format!(
            r#"{{"bufferView":{},"componentType":{component},"count":{count},"type":"{ty}""#,
            self.views.len() - 1
        );
        if let Some((min, max)) = minmax {
            let list = |v: &[f32]| {
                v.iter()
                    .map(|x| format!("{x:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            write!(a, r#","min":[{}],"max":[{}]"#, list(&min), list(&max)).unwrap();
        }
        a.push('}');
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    pub fn vec3(&mut self, v: &[[f32; 3]]) -> usize {
        let mut b = Vec::new();
        push_f32(&mut b, v.iter().flatten().copied());
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for p in v {
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        self.accessor(b, 5126, v.len(), "VEC3", Some((min.to_vec(), max.to_vec())))
    }

    pub fn vec2(&mut self, v: &[[f32; 2]]) -> usize {
        let mut b = Vec::new();
        push_f32(&mut b, v.iter().flatten().copied());
        self.accessor(b, 5126, v.len(), "VEC2", None)
    }

    pub fn vec4(&mut self, v: &[[f32; 4]]) -> usize {
        let mut b = Vec::new();
        push_f32(&mut b, v.iter().flatten().copied());
        self.accessor(b, 5126, v.len(), "VEC4", None)
    }

    pub fn scalars(&mut self, v: &[f32]) -> usize {
        let mut b = Vec::new();
        push_f32(&mut b, v.iter().copied());
        let min = v.iter().copied().fold(f32::MAX, f32::min);
        let max = v.iter().copied().fold(f32::MIN, f32::max);
        self.accessor(b, 5126, v.len(), "SCALAR", Some((vec![min], vec![max])))
    }

    pub fn joints(&mut self, v: &[[u16; 4]]) -> usize {
        let b = v.iter().flatten().flat_map(|j| j.to_le_bytes()).collect();
        self.accessor(b, 5123, v.len(), "VEC4", None)
    }

    pub fn mat4s(&mut self, v: &[[f32; 16]]) -> usize {
        let mut b = Vec::new();
        push_f32(&mut b, v.iter().flatten().copied());
        self.accessor(b, 5126, v.len(), "MAT4", None)
    }

    pub fn indices(&mut self, v: &[u32]) -> usize {
        let b = v.iter().flat_map(|i| i.to_le_bytes()).collect();
        self.accessor(b, 5125, v.len(), "SCALAR", None)
    }

    pub fn material(&mut self, name: &str) -> usize {
        self.materials.push(format!(r#"{{"name":"{name}"}}"#));
        self.materials.len() - 1
    }

    /// The JSON (and the binary buffer, for GLB and external buffers).
    pub fn finish(&self, container: Container) -> (Vec<u8>, Vec<u8>) {
        let join = |v: &[String]| v.join(",");
        let uri = match &container {
            Container::Glb => String::new(),
            Container::External(name) => format!(r#","uri":"{name}""#),
            Container::DataUri => format!(
                r#","uri":"data:application/octet-stream;base64,{}""#,
                base64(&self.bin)
            ),
        };
        let scene: Vec<String> = self.scene_nodes.iter().map(|n| n.to_string()).collect();
        let mut json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[{}]}}],"nodes":[{}],"meshes":[{}],"buffers":[{{"byteLength":{}{uri}}}],"bufferViews":[{}],"accessors":[{}]"#,
            scene.join(","),
            join(&self.nodes),
            join(&self.meshes),
            self.bin.len(),
            join(&self.views),
            join(&self.accessors),
        );
        if !self.materials.is_empty() {
            write!(json, r#","materials":[{}]"#, join(&self.materials)).unwrap();
        }
        if !self.skins.is_empty() {
            write!(json, r#","skins":[{}]"#, join(&self.skins)).unwrap();
        }
        if !self.animations.is_empty() {
            write!(json, r#","animations":[{}]"#, join(&self.animations)).unwrap();
        }
        json.push('}');
        match container {
            Container::Glb => {
                let mut j = json.into_bytes();
                while j.len() % 4 != 0 {
                    j.push(b' ');
                }
                let mut bin = self.bin.clone();
                while bin.len() % 4 != 0 {
                    bin.push(0);
                }
                let total = 12 + 8 + j.len() + 8 + bin.len();
                let mut out = Vec::with_capacity(total);
                out.extend(b"glTF");
                out.extend(2u32.to_le_bytes());
                out.extend((total as u32).to_le_bytes());
                out.extend((j.len() as u32).to_le_bytes());
                out.extend(b"JSON");
                out.extend(j);
                out.extend((bin.len() as u32).to_le_bytes());
                out.extend(b"BIN\0");
                out.extend(bin);
                (out, Vec::new())
            }
            Container::External(_) => (json.into_bytes(), self.bin.clone()),
            Container::DataUri => (json.into_bytes(), Vec::new()),
        }
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}
