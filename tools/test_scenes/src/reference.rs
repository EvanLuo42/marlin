//! CPU references of a scene's lighting, direct and ([`global_illumination`]) with every bounce:
//! rays traced against every triangle of the scene (brute force: the scenes are small), each
//! surface lit by every light with the renderer's formula (`Lighting/Restir.mshader`: albedo / π ·
//! I · cos θ / d², UE's range window, a spot's cone) and its visibility over the half of the
//! light's sphere facing it.

use marlin_math::{Mat4, Rng, Vec3, Vec4};
use marlin_render::Camera;
use marlin_render::scene::{LightDesc, LightKind};

use crate::{Instance, Scene};

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

/// The GGX specular BRDF times cos θ_l, toward `v` from `l` about `n` (height-correlated Smith,
/// Schlick Fresnel; α = roughness²): the renderer's `SpecularCos` (`Lighting/Brdf.mshader`).
pub fn specular_cos(n: Vec3, v: Vec3, l: Vec3, roughness: f32, f0: [f32; 3]) -> [f32; 3] {
    let (nol, nov) = (n.dot(l), n.dot(v));
    if nol <= 0.0 || nov <= 0.0 {
        return [0.0; 3];
    }
    let a = (roughness * roughness).max(1e-4);
    let a2 = a * a;
    let h = (v + l).normalize();
    let noh = n.dot(h).clamp(0.0, 1.0);
    let d = noh * noh * (a2 - 1.0) + 1.0;
    let ggx = a2 / (core::f32::consts::PI * d * d);
    let vis = 0.5
        / (nol * (nov * nov * (1.0 - a2) + a2).sqrt() + nov * (nol * nol * (1.0 - a2) + a2).sqrt())
            .max(1e-6);
    let f = (1.0 - v.dot(h).clamp(0.0, 1.0)).powi(5);
    f0.map(|c| (c + (1.0 - c) * f) * ggx * vis * nol)
}

/// The least roughness a light's highlight takes (the renderer's `MinLightRoughness`).
pub const MIN_LIGHT_ROUGHNESS: f32 = 0.05;

/// The roughness below which the specular lobe is traced, not resampled (the renderer's
/// `SpecularTau`, 0010 §2.7).
pub const SPECULAR_TAU: f32 = 0.2;

/// What `light` gives the specular lobe (`roughness`, `f0`) at `x` facing `n` seen along `v`,
/// unshadowed.
pub fn light_specular(
    light: &LightDesc,
    x: Vec3,
    n: Vec3,
    v: Vec3,
    roughness: f32,
    f0: [f32; 3],
) -> [f32; 3] {
    // The diffuse formula for a white surface over cos θ / π is the incident light.
    let toward = (Vec3::from(light.position) - x).normalize();
    let lambert = light_contribution(light, x, toward, [1.0; 3]);
    let s = specular_cos(n, v, toward, roughness.max(MIN_LIGHT_ROUGHNESS), f0);
    [0, 1, 2].map(|i| lambert[i] * core::f32::consts::PI * s[i])
}

/// What `light` gives `instance`'s surface at `x` facing `n` seen along `v`, unshadowed: its
/// diffuse and specular lobes.
pub fn light_lobes(light: &LightDesc, x: Vec3, n: Vec3, v: Vec3, instance: &Instance) -> [f32; 3] {
    let (albedo, f0) = instance.lobes();
    let d = light_contribution(light, x, n, albedo);
    let s = light_specular(light, x, n, v, instance.roughness, f0);
    [0, 1, 2].map(|i| d[i] + s[i])
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
            let instance = &scene.instances[hit.instance];
            let mut sum = [0.0f32; 3];
            for light in &scene.lights {
                let c = light_lobes(light, hit.point, hit.normal, -dir, instance);
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
/// each seen from one random point of its sphere's half facing `x`; with `specular` (the view
/// direction, roughness and F0), its GGX lobe's too.
fn direct_sample(
    tracer: &Tracer,
    scene: &Scene,
    x: Vec3,
    n: Vec3,
    albedo: [f32; 3],
    specular: Option<(Vec3, f32, [f32; 3])>,
    rng: &mut Rng,
) -> [f32; 3] {
    let mut sum = [0.0f32; 3];
    for light in &scene.lights {
        let mut c = light_contribution(light, x, n, albedo);
        if let Some((v, roughness, f0)) = specular {
            let s = light_specular(light, x, n, v, roughness, f0);
            c = [0, 1, 2].map(|i| c[i] + s[i]);
        }
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

/// A reflection direction about `n` for a surface seen along `v`, from the GGX lobe of
/// `roughness` (VNDF sampling, Heitz 2018): the renderer's `SampleSpecular`.
pub fn sample_specular(n: Vec3, v: Vec3, roughness: f32, u1: f32, u2: f32) -> Vec3 {
    let t = if n.y.abs() < 0.99 { Vec3::Y } else { Vec3::X }
        .cross(n)
        .normalize();
    let b = n.cross(t);
    let a = (roughness * roughness).max(1e-4);
    let ve = Vec3::new(v.dot(t), v.dot(b), v.dot(n));
    let vh = Vec3::new(a * ve.x, a * ve.y, ve.z).normalize();
    let lensq = vh.x * vh.x + vh.y * vh.y;
    let t1 = if lensq > 0.0 {
        Vec3::new(-vh.y, vh.x, 0.0) / lensq.sqrt()
    } else {
        Vec3::X
    };
    let t2 = vh.cross(t1);
    let r = u1.sqrt();
    let phi = core::f32::consts::TAU * u2;
    let p1 = r * phi.cos();
    let s = 0.5 * (1.0 + vh.z);
    let p2 = (1.0 - s) * (1.0 - p1 * p1).max(0.0).sqrt() + s * r * phi.sin();
    let nh = t1 * p1 + t2 * p2 + vh * (1.0 - p1 * p1 - p2 * p2).max(0.0).sqrt();
    let m = Vec3::new(a * nh.x, a * nh.y, nh.z.max(0.0)).normalize();
    let h = t * m.x + b * m.y + n * m.z;
    // Reflect -v about h.
    -v - h * (2.0 * (-v).dot(h))
}

/// What a direction `l` drawn by [`sample_specular`] carries: F · G2 / G1(v), the renderer's
/// `SpecularWeight`.
pub fn specular_weight(n: Vec3, v: Vec3, l: Vec3, roughness: f32, f0: [f32; 3]) -> [f32; 3] {
    let (nol, nov) = (n.dot(l), n.dot(v));
    if nol <= 0.0 || nov <= 0.0 {
        return [0.0; 3];
    }
    let a = (roughness * roughness).max(1e-4);
    let a2 = a * a;
    let vis = 0.5
        / (nol * (nov * nov * (1.0 - a2) + a2).sqrt() + nov * (nol * nol * (1.0 - a2) + a2).sqrt())
            .max(1e-6);
    let g2 = 4.0 * nov * nol * vis;
    let g1 = 2.0 * nov / (nov + (a2 + (1.0 - a2) * nov * nov).sqrt());
    let f = (1.0 - v.dot((v + l).normalize()).clamp(0.0, 1.0)).powi(5);
    f0.map(|c| (c + (1.0 - c) * f) * g2 / g1)
}

/// The light reaching `origin` (on a surface facing `n`) along `d`: the sky where nothing is,
/// else what the surface hit reflects toward it, Lambert all the way ([`path_radiance`]).
fn radiance_toward(
    tracer: &Tracer,
    scene: &Scene,
    origin: Vec3,
    n: Vec3,
    d: Vec3,
    rng: &mut Rng,
) -> [f32; 3] {
    match tracer.trace(origin + n * 1e-3, d, f32::INFINITY) {
        Some(hit) => path_radiance(tracer, scene, hit, rng),
        None => scene.environment.ambient,
    }
}

/// What the surface at `hit` reflects (Lambert): next-event estimation to every light, then
/// cosine bounces, Russian roulette after two, the sky where a bounce leaves.
fn path_radiance(tracer: &Tracer, scene: &Scene, first: Hit, rng: &mut Rng) -> [f32; 3] {
    let sky = scene.environment.ambient;
    let mut sum = [0.0f32; 3];
    let mut hit = first;
    let mut throughput = [1.0f32; 3];
    for bounce in 0.. {
        let albedo = scene.instances[hit.instance].lobes().0;
        let direct = direct_sample(tracer, scene, hit.point, hit.normal, albedo, None, rng);
        for i in 0..3 {
            sum[i] += throughput[i] * direct[i];
            throughput[i] *= albedo[i];
        }
        if bounce >= 2 {
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
    sum
}

/// The scene's global illumination at every `step`-th pixel (from `step / 2`) of a `width` ×
/// `height` view: `paths` paths a pixel, the sky (the environment's ambient luminance) where a
/// bounce leaves; `None` at the other pixels and where nothing is. At the surface the camera
/// sees, its GGX lobe as the renderer splits it (0010 §2.7): the lights' highlights; a cosine
/// bounce weighted by the diffuse lobe and, where the roughness is at least
/// [`SPECULAR_TAU`], the specular one; below it a second, specular bounce (VNDF). Beyond it,
/// Lambert (the renderer's hits shade with the simplified materials). Unbiased, so noisy:
/// compare averages, not pixels.
pub fn global_illumination(
    scene: &Scene,
    width: u32,
    height: u32,
    step: u32,
    paths: u32,
) -> Vec<Option<[f32; 3]>> {
    let tracer = Tracer::new(scene);
    let mut rng = Rng::new(0x5EED);
    (0..width * height)
        .map(|p| {
            let (x, y) = (p % width, p / width);
            if x % step != step / 2 || y % step != step / 2 {
                return None;
            }
            let (origin, dir) = pixel_ray(scene.camera, width, height, x, y);
            let hit = tracer.trace(origin, dir, f32::INFINITY)?;
            let instance = &scene.instances[hit.instance];
            let (albedo, f0) = instance.lobes();
            let (n, v, roughness) = (hit.normal, -dir, instance.roughness);
            let mut sum = [0.0f32; 3];
            for _ in 0..paths {
                let specular = Some((v, roughness, f0));
                let direct =
                    direct_sample(&tracer, scene, hit.point, n, albedo, specular, &mut rng);
                // Cosine-sampled: the weight is the BRDF times cos over the pdf (cos / π).
                let d = cosine_direction(n, rng.f32(), rng.f32());
                let mut weight = albedo;
                if roughness >= SPECULAR_TAU {
                    let s = specular_cos(n, v, d, roughness, f0);
                    let cos = n.dot(d).max(1e-6);
                    weight = [0, 1, 2].map(|i| weight[i] + core::f32::consts::PI * s[i] / cos);
                }
                let incoming = radiance_toward(&tracer, scene, hit.point, n, d, &mut rng);
                let mut reflected = [0.0f32; 3];
                if roughness < SPECULAR_TAU {
                    let d = sample_specular(n, v, roughness, rng.f32(), rng.f32());
                    let w = specular_weight(n, v, d, roughness, f0);
                    if w != [0.0; 3] {
                        let l = radiance_toward(&tracer, scene, hit.point, n, d, &mut rng);
                        reflected = [0, 1, 2].map(|i| w[i] * l[i]);
                    }
                }
                for i in 0..3 {
                    sum[i] += direct[i] + weight[i] * incoming[i] + reflected[i];
                }
            }
            Some(sum.map(|s| s / paths as f32))
        })
        .collect()
}
