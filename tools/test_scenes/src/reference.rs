//! CPU references of a scene's lighting, direct and ([`global_illumination`]) with every bounce:
//! rays traced against every triangle of the scene (brute force: the scenes are small), each
//! surface lit by every light with the renderer's formula (`Lighting/Restir.mshader`: albedo / π ·
//! I · cos θ / d², UE's range window, a spot's cone) and its visibility over the half of the
//! light's sphere facing it.

use marlin_math::{Mat4, Rng, Vec3, Vec4};
use marlin_render::Camera;
use marlin_render::scene::{LightDesc, LightKind};

use crate::Scene;

/// A ray's hit: where, the surface's normal there (facing the ray), whose.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Hit {
    pub t: f32,
    pub point: Vec3,
    pub normal: Vec3,
    /// Its index in [`Scene::instances`].
    pub instance: usize,
}

/// The scene's triangles in world space.
pub struct Tracer {
    triangles: Vec<([Vec3; 3], usize)>,
}

impl Tracer {
    pub fn new(scene: &Scene) -> Self {
        let mut triangles = Vec::new();
        for (i, inst) in scene.instances.iter().enumerate() {
            let m = inst.transform.to_matrix();
            let mesh = &scene.meshes[inst.mesh];
            for t in mesh.indices.as_chunks::<3>().0 {
                let v = |k: usize| {
                    let p = mesh.positions[t[k] as usize];
                    m.transform_point3(Vec3::new(p[0], p[1], p[2]))
                };
                triangles.push(([v(0), v(1), v(2)], i));
            }
        }
        Self { triangles }
    }

    /// The closest hit along `dir` (normalized) from `origin`, before `t_max`.
    pub fn trace(&self, origin: Vec3, dir: Vec3, t_max: f32) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for &([a, b, c], instance) in &self.triangles {
            // Möller–Trumbore.
            let (e1, e2) = (b - a, c - a);
            let p = dir.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv = 1.0 / det;
            let s = origin - a;
            let u = s.dot(p) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = dir.dot(q) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let t = e2.dot(q) * inv;
            if t <= 0.0 || t >= t_max || best.is_some_and(|h| h.t <= t) {
                continue;
            }
            let n = e1.cross(e2).normalize();
            best = Some(Hit {
                t,
                point: origin + dir * t,
                normal: if n.dot(dir) > 0.0 { -n } else { n },
                instance,
            });
        }
        best
    }

    /// Whether anything lies between `from` and `to`.
    pub fn occluded(&self, from: Vec3, to: Vec3) -> bool {
        let d = to - from;
        let len = d.length();
        len > 0.0 && self.trace(from, d / len, len * 0.999).is_some()
    }
}

/// The world-space ray through pixel (`x`, `y`) of a `width` × `height` view from `camera`.
pub fn pixel_ray(
    (eye, cam): (marlin_math::Transform, Camera),
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> (Vec3, Vec3) {
    let aspect = width as f32 / height as f32;
    let vp = Mat4::perspective_infinite_rh(cam.fov_y, aspect, cam.near)
        * eye.to_matrix().inverse_affine();
    let inverse = vp.inverse();
    let at = |z: f32| {
        let w = inverse
            * Vec4::new(
                (x as f32 + 0.5) / width as f32 * 2.0 - 1.0,
                1.0 - (y as f32 + 0.5) / height as f32 * 2.0,
                z,
                1.0,
            );
        w.truncate() / w.w
    };
    // A point on the pixel's ray in front of the eye.
    let p = at(0.5);
    (eye.translation, (p - eye.translation).normalize())
}

/// What `light` gives a Lambert surface of `albedo` at `x` facing `n`, unshadowed: the
/// renderer's formula.
pub fn light_contribution(light: &LightDesc, x: Vec3, n: Vec3, albedo: [f32; 3]) -> [f32; 3] {
    if light.kind == LightKind::Directional {
        return [0.0; 3];
    }
    let d = Vec3::from(light.position) - x;
    let d2 = d.length_squared().max(1e-4);
    let dist = d2.sqrt();
    let toward = d / dist;
    let cos = n.dot(toward).clamp(0.0, 1.0);
    let window = (1.0 - (dist / light.range.max(1e-4)).powi(4)).clamp(0.0, 1.0);
    let mut spot = 1.0;
    if light.kind == LightKind::Spot {
        let c = (-toward).dot(Vec3::from(light.direction));
        let s = ((c - light.cone[1]) / (light.cone[0] - light.cone[1]).max(1e-4)).clamp(0.0, 1.0);
        spot = s * s;
    }
    let k = window * window / d2 * spot * cos / core::f32::consts::PI;
    [0, 1, 2].map(|i| albedo[i] * light.radiance[i] * k)
}

/// The fraction of the half of `light`'s sphere facing `x` that `x` (offset along `n`) sees:
/// `samples`² stratified points (the renderer draws one a frame); 1 or 0 for a light without
/// size.
pub fn visibility(tracer: &Tracer, light: &LightDesc, x: Vec3, n: Vec3, samples: u32) -> f32 {
    let center = Vec3::from(light.position);
    let origin = x + n * 1e-3;
    if light.radius <= 0.0 {
        return if tracer.occluded(origin, center) {
            0.0
        } else {
            1.0
        };
    }
    let toward = (x - center).normalize();
    let t = if toward.y.abs() < 0.99 {
        Vec3::Y
    } else {
        Vec3::X
    }
    .cross(toward)
    .normalize();
    let b = toward.cross(t);
    let mut seen = 0;
    for i in 0..samples {
        for j in 0..samples {
            let z = (i as f32 + 0.5) / samples as f32;
            let phi = (j as f32 + 0.5) / samples as f32 * core::f32::consts::TAU;
            let ring = (1.0 - z * z).max(0.0).sqrt();
            let p = center
                + (toward * z + t * (ring * phi.cos()) + b * (ring * phi.sin())) * light.radius;
            seen += u32::from(!tracer.occluded(origin, p));
        }
    }
    seen as f32 / (samples * samples) as f32
}

/// The scene's direct lighting from its lights, per pixel of a `width` × `height` view from its
/// camera: `None` where nothing is. Visibility from `samples`² points per light.
pub fn direct_lighting(
    scene: &Scene,
    width: u32,
    height: u32,
    samples: u32,
) -> Vec<Option<[f32; 3]>> {
    let tracer = Tracer::new(scene);
    (0..width * height)
        .map(|p| {
            let (origin, dir) = pixel_ray(scene.camera, width, height, p % width, p / width);
            let hit = tracer.trace(origin, dir, f32::INFINITY)?;
            let albedo = scene.instances[hit.instance].albedo;
            let mut sum = [0.0f32; 3];
            for light in &scene.lights {
                let c = light_contribution(light, hit.point, hit.normal, albedo);
                if c == [0.0; 3] {
                    continue;
                }
                let v = visibility(&tracer, light, hit.point, hit.normal, samples);
                for i in 0..3 {
                    sum[i] += c[i] * v;
                }
            }
            Some(sum)
        })
        .collect()
}

/// A direction about `n`, cosine-distributed, from two uniform numbers.
fn cosine_direction(n: Vec3, u: f32, v: f32) -> Vec3 {
    let r = u.sqrt();
    let phi = v * core::f32::consts::TAU;
    let t = if n.y.abs() < 0.99 { Vec3::Y } else { Vec3::X }
        .cross(n)
        .normalize();
    let b = n.cross(t);
    (t * (r * phi.cos()) + b * (r * phi.sin()) + n * (1.0 - u).max(0.0).sqrt()).normalize()
}

/// The light a Lambert surface of `albedo` at `x` facing `n` reflects from the scene's lights,
/// each seen from one random point of its sphere's half facing `x`.
fn direct_sample(
    tracer: &Tracer,
    scene: &Scene,
    x: Vec3,
    n: Vec3,
    albedo: [f32; 3],
    rng: &mut Rng,
) -> [f32; 3] {
    let mut sum = [0.0f32; 3];
    for light in &scene.lights {
        let c = light_contribution(light, x, n, albedo);
        if c == [0.0; 3] {
            continue;
        }
        let center = Vec3::from(light.position);
        let toward = (x - center).normalize();
        let t = if toward.y.abs() < 0.99 {
            Vec3::Y
        } else {
            Vec3::X
        }
        .cross(toward)
        .normalize();
        let b = toward.cross(t);
        let (z, phi) = (rng.f32(), rng.f32() * core::f32::consts::TAU);
        let ring = (1.0 - z * z).max(0.0).sqrt();
        let p =
            center + (toward * z + t * (ring * phi.cos()) + b * (ring * phi.sin())) * light.radius;
        if !tracer.occluded(x + n * 1e-3, p) {
            for i in 0..3 {
                sum[i] += c[i];
            }
        }
    }
    sum
}

/// The scene's global illumination, Lambert surfaces only, at every `step`-th pixel (from
/// `step / 2`) of a `width` × `height` view: `paths` paths a pixel, each bounce cosine-sampled
/// with next-event estimation to every light, Russian roulette after three bounces, the sky
/// (the environment's ambient luminance) where a bounce leaves; `None` at the other pixels and
/// where nothing is. Unbiased, so noisy: compare averages, not pixels.
pub fn global_illumination(
    scene: &Scene,
    width: u32,
    height: u32,
    step: u32,
    paths: u32,
) -> Vec<Option<[f32; 3]>> {
    let tracer = Tracer::new(scene);
    let sky = scene.environment.ambient;
    let mut rng = Rng::new(0x5EED);
    (0..width * height)
        .map(|p| {
            let (x, y) = (p % width, p / width);
            if x % step != step / 2 || y % step != step / 2 {
                return None;
            }
            let (origin, dir) = pixel_ray(scene.camera, width, height, x, y);
            let first = tracer.trace(origin, dir, f32::INFINITY)?;
            let mut sum = [0.0f32; 3];
            for _ in 0..paths {
                let mut hit = first;
                let mut throughput = [1.0f32; 3];
                for bounce in 0.. {
                    let albedo = scene.instances[hit.instance].albedo;
                    let direct =
                        direct_sample(&tracer, scene, hit.point, hit.normal, albedo, &mut rng);
                    for i in 0..3 {
                        sum[i] += throughput[i] * direct[i];
                        throughput[i] *= albedo[i];
                    }
                    if bounce >= 3 {
                        let keep = throughput.iter().fold(0.0f32, |a, &t| a.max(t)).min(0.95);
                        if rng.f32() >= keep {
                            break;
                        }
                        throughput = throughput.map(|t| t / keep);
                    }
                    let d = cosine_direction(hit.normal, rng.f32(), rng.f32());
                    match tracer.trace(hit.point + hit.normal * 1e-3, d, f32::INFINITY) {
                        Some(next) => hit = next,
                        None => {
                            for i in 0..3 {
                                sum[i] += throughput[i] * sky[i];
                            }
                            break;
                        }
                    }
                }
            }
            Some(sum.map(|s| s / paths as f32))
        })
        .collect()
}
