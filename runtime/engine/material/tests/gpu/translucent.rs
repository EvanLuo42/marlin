//! Translucent surfaces (0009 §8, R4e): panes of `Material.Glass` drawn by the forward pass over
//! a wall, lit by a uniform sky alone (no sun, no lights, no GI), so every value is known: the
//! wall reflects albedo · L plus its specular albedo · L; a pane sends its tint · L times its
//! opacity, its specular albedo · L whole, and lets 1 − opacity of what is behind it through.
//! Two overlapping panes blend back to front; a pane behind the wall is not drawn.

use marlin_material::DefaultMaterial;
use marlin_math::{Transform, Vec3};
use marlin_render::Environment;
use marlin_render::scene::MaterialDesc;

use crate::support::cook::{cube, uv_quad};
use crate::support::{GLASS, HEIGHT, Harness, WIDTH};

const SKY: f32 = 1.0;
const WALL: f32 = 0.6;

/// Karis's analytic specular albedo (`Lighting/Brdf.mshader`'s `SpecularAlbedo`).
fn specular_albedo(f0: f32, roughness: f32, nov: f32) -> f32 {
    let c0 = [-1.0, -0.0275, -0.572, 0.022];
    let c1 = [1.0, 0.0425, 1.04, -0.04];
    let r: [f32; 4] = core::array::from_fn(|i| c0[i] * roughness + c1[i]);
    let a004 = (r[0] * r[0]).min((-9.28 * nov).exp2()) * r[0] + r[1];
    let (a, b) = (-1.04 * a004 + r[2], 1.04 * a004 + r[3]);
    f0 * a + b
}

/// The 8-bit square-root encoding the material buffer keeps albedos in.
fn encoded(c: f32) -> f32 {
    ((c.sqrt() * 255.0).round() / 255.0).powi(2)
}

/// A wall filling the view behind z = −1.9, white-ish, rough; a uniform sky.
fn wall(h: &mut Harness) {
    let c = h.upload(&cube());
    let m = h.default_material(DefaultMaterial {
        base_color: [WALL; 3],
        roughness: 1.0,
        metallic: 0.0,
    });
    h.add(
        c,
        m,
        Transform {
            translation: Vec3::new(0.0, 0.0, -2.0),
            rotation: marlin_math::Quat::IDENTITY,
            scale: Vec3::new(20.0, 20.0, 0.2),
        },
        [1.0; 4],
    );
    h.updates.environment = Some(Environment {
        sun_illuminance: 0.0,
        ambient: [SKY; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
        ..Environment::default()
    });
}

/// A `Material.Glass` pane: its parameters.
#[derive(Clone, Copy)]
struct Glass {
    tint: [f32; 3],
    opacity: f32,
    thickness: f32,
    transmission: [f32; 3],
}

impl Glass {
    fn tinted(tint: [f32; 3], opacity: f32) -> Self {
        Self {
            tint,
            opacity,
            thickness: 0.0,
            transmission: [1.0; 3],
        }
    }
}

/// A pane (a quad facing the camera) of tint `tint` and opacity `opacity` at depth `z`.
fn pane(h: &mut Harness, tint: [f32; 3], opacity: f32, z: f32) {
    glass(
        h,
        Glass::tinted(tint, opacity),
        z,
        marlin_math::Quat::IDENTITY,
    );
}

/// A pane of `g` at depth `z`, turned by `rotation`.
fn glass(h: &mut Harness, g: Glass, z: f32, rotation: marlin_math::Quat) {
    let q = h.upload(&uv_quad());
    let [t0, t1, t2] = g.tint;
    let [r, gg, b] = g.transmission;
    let glass = h.material(MaterialDesc {
        permutation: GLASS,
        params: [t0, t1, t2, 0.05, g.opacity, g.thickness, r, gg, b]
            .map(f32::to_bits)
            .to_vec(),
    });
    h.add(
        q,
        glass,
        Transform {
            translation: Vec3::new(0.0, 0.0, z),
            rotation,
            scale: Vec3::splat(1.5),
        },
        [1.0; 4],
    );
}

/// What the wall sends to the eye, seen head-on.
fn wall_light() -> f32 {
    encoded(WALL) * SKY + specular_albedo(0.04, 1.0, 1.0) * SKY
}

/// A pane of `tint` and `opacity` over `below`, seen head-on (premultiplied blending).
fn over(tint: f32, opacity: f32, below: f32) -> f32 {
    encoded(tint) * SKY * opacity + specular_albedo(0.04, 0.05, 1.0) * SKY + (1.0 - opacity) * below
}

/// The view's center pixel.
fn center(picture: &[[f32; 3]]) -> [f32; 3] {
    picture[((HEIGHT / 2) * WIDTH + WIDTH / 2) as usize]
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= 0.02 * b[i].max(0.05))
}

#[test]
fn a_pane_blends_over_the_wall() {
    let mut h = Harness::new();
    wall(&mut h);
    pane(&mut h, [0.8, 0.3, 0.2], 0.4, 0.0);
    let got = center(&h.restir_frame(1));
    let want = [0.8, 0.3, 0.2].map(|t| over(t, 0.4, wall_light()));
    assert!(close(got, want), "{got:?} against {want:?}");
}

#[test]
fn overlapping_panes_blend_back_to_front() {
    // Red behind, blue in front: drawn back to front, the blue pane covers half the red one.
    let mut h = Harness::new();
    wall(&mut h);
    pane(&mut h, [0.9, 0.1, 0.1], 0.5, 0.5);
    pane(&mut h, [0.1, 0.1, 0.9], 0.5, 1.0);
    let got = center(&h.restir_frame(1));
    let red = [0.9, 0.1, 0.1].map(|t| over(t, 0.5, wall_light()));
    let want: [f32; 3] = core::array::from_fn(|i| over([0.1, 0.1, 0.9][i], 0.5, red[i]));
    assert!(close(got, want), "{got:?} against {want:?}");
    // The other order would differ: a test that sees it.
    let blue = [0.1, 0.1, 0.9].map(|t| over(t, 0.5, wall_light()));
    let wrong: [f32; 3] = core::array::from_fn(|i| over([0.9, 0.1, 0.1][i], 0.5, blue[i]));
    assert!(!close(got, wrong));
}

#[test]
fn a_pane_behind_the_wall_is_not_drawn() {
    let mut h = Harness::new();
    wall(&mut h);
    pane(&mut h, [0.8, 0.3, 0.2], 0.4, -3.0);
    let got = center(&h.restir_frame(1));
    let want = [wall_light(); 3];
    assert!(close(got, want), "{got:?} against {want:?}");
}

#[test]
fn local_lights_light_a_pane_through_the_grid() {
    // A pane in the dark lit by two point lights, unshadowed (0010 §5.4): at the center pixel's
    // point, what the CPU reference's formulas give its diffuse lobe (times the opacity) and its
    // GGX lobe.
    use marlin_test_scenes::reference::{light_contribution, light_specular, pixel_ray};
    use marlin_test_scenes::scenes::point_light;
    let mut h = Harness::new();
    let (tint, opacity) = ([0.7, 0.5, 0.3], 0.6);
    pane(&mut h, tint, opacity, 0.0);
    h.updates.environment = Some(Environment {
        sun_illuminance: 0.0,
        ambient: [0.0; 3],
        clear_color: [0.0, 0.0, 0.0, 1.0],
        ..Environment::default()
    });
    let lights = [
        point_light(Vec3::new(0.5, 0.3, 1.0), 10.0, [1.0, 0.9, 0.8], 0.0, 10.0),
        point_light(Vec3::new(-0.4, -0.2, 0.7), 5.0, [0.6, 0.8, 1.0], 0.0, 10.0),
    ];
    for light in lights {
        let id = h.scene.ids().lights.reserve();
        h.updates
            .lights
            .push(id, marlin_render::scene::LightUpdate::Add(light));
    }
    let got = center(&h.restir_frame(1));
    let (origin, dir) = pixel_ray(h.camera, WIDTH, HEIGHT, WIDTH / 2, HEIGHT / 2);
    let x = origin + dir * (-origin.z / dir.z);
    let (n, v) = (Vec3::Z, -dir);
    let albedo = tint.map(encoded);
    let mut want = [0.0f32; 3];
    for light in &lights {
        let d = light_contribution(light, x, n, albedo);
        let s = light_specular(light, x, n, v, 0.05, [0.04; 3]);
        for i in 0..3 {
            want[i] += d[i] * opacity + s[i];
        }
    }
    assert!(close(got, want), "{got:?} against {want:?}");
}

#[test]
fn a_pane_writes_its_opacity_to_the_reactive_mask() {
    let mut h = Harness::new();
    wall(&mut h);
    pane(&mut h, [0.8, 0.3, 0.2], 0.4, 0.0);
    h.restir_frame(1);
    let reactive = h.reactive.take().expect("translucent surfaces were drawn");
    let at = |x: u32, y: u32| reactive[(y * WIDTH + x) as usize];
    // The pane at the center, the opacity in 8 bits; the wall's corner, nothing.
    assert!((at(WIDTH / 2, HEIGHT / 2) - 0.4).abs() <= 1.0 / 255.0);
    assert_eq!(at(0, 0), 0.0);
}

#[test]
fn a_tinting_pane_filters_what_is_behind_it() {
    // Clear (opacity 0), no thickness, a transmission tint: it lets the wall through, filtered,
    // and reflects its specular whole. The wall is uniform: the copy's blur changes nothing.
    let mut h = Harness::new();
    wall(&mut h);
    let transmission = [1.0, 0.5, 0.25];
    let g = Glass {
        transmission,
        ..Glass::tinted([1.0; 3], 0.0)
    };
    glass(&mut h, g, 0.0, marlin_math::Quat::IDENTITY);
    let got = center(&h.restir_frame(1));
    let want = transmission.map(|t| specular_albedo(0.04, 0.05, 1.0) * SKY + wall_light() * t);
    assert!(close(got, want), "{got:?} against {want:?}");
    // A refracting surface draws the pixel whole, and the reactive mask asks for at least ½.
    let reactive = h.reactive.take().unwrap();
    let r = reactive[((HEIGHT / 2) * WIDTH + WIDTH / 2) as usize];
    assert!((r - 0.5).abs() <= 1.0 / 255.0, "{r}");
}

/// The column, on the center row, where the dark half of the background (x < 0) gives way to
/// the bright: where the luminance first passes halfway.
fn edge(picture: &[[f32; 3]], dark: f32, bright: f32) -> f32 {
    let row = &picture[((HEIGHT / 2) * WIDTH) as usize..][..WIDTH as usize];
    let lum = |p: [f32; 3]| (p[0] + p[1] + p[2]) / 3.0;
    let half = 0.5 * (dark + bright);
    let x = row.iter().position(|&p| lum(p) > half).expect("an edge") as f32;
    // Linearly between the two pixels around it.
    let (a, b) = (lum(row[x as usize - 1]), lum(row[x as usize]));
    x - 1.0 + (half - a) / (b - a)
}

#[test]
fn a_thick_pane_shifts_what_is_behind_it() {
    // A dark block over the left half of the wall, a clear pane turned 40° about y. Without
    // thickness the edge stays; with thickness the bent ray's offset moves it, by what the
    // shader's model gives (the background point moved by (bent − incident) · thickness).
    use marlin_math::{Quat, Vec4};
    let thickness = 1.5;
    let turn = Quat::from_rotation_y(40f32.to_radians());
    let frame = |thickness: f32| {
        let mut h = Harness::new();
        wall(&mut h);
        let c = h.upload(&cube());
        let black = h.default_material(DefaultMaterial {
            base_color: [0.0; 3],
            roughness: 1.0,
            metallic: 0.0,
        });
        h.add(
            c,
            black,
            Transform {
                translation: Vec3::new(-5.0, 0.0, -1.7),
                rotation: Quat::IDENTITY,
                scale: Vec3::new(10.0, 20.0, 0.2),
            },
            [1.0; 4],
        );
        let g = Glass {
            thickness,
            ..Glass::tinted([1.0; 3], 0.0)
        };
        glass(&mut h, g, 0.0, turn);
        (h.restir_frame(1), h.camera)
    };
    let (flat, camera) = frame(0.0);
    let (bent, _) = frame(thickness);
    // Dark and bright: the row's ends, outside the pane.
    let row = |x: u32| {
        let p = flat[((HEIGHT / 2) * WIDTH + x) as usize];
        (p[0] + p[1] + p[2]) / 3.0
    };
    let (dark, bright) = (row(0), row(WIDTH - 1));
    let (e0, e1) = (edge(&flat, dark, bright), edge(&bent, dark, bright));
    // The model at the edge's point: x = 0 on the block's face, z = −1.6.
    let eye = camera.0.translation;
    let x = Vec3::new(0.0, 0.0, -1.6);
    let wo = (eye - x).normalize();
    let mut n = turn * Vec3::Z;
    if n.dot(wo) < 0.0 {
        n = -n;
    }
    // The pane's F0 (0.04) gives its index: 1.5.
    let eta = 1.0 / 1.5;
    let i = -wo;
    let cos = -n.dot(i);
    let k = 1.0 - eta * eta * (1.0 - cos * cos);
    let refracted = i * eta + n * (eta * cos - k.sqrt());
    let offset = (refracted + wo) * thickness;
    // The pixel that samples x is the one whose background point is x − offset.
    let vp = marlin_render::view_projection(camera, WIDTH, HEIGHT);
    let column = |p: Vec3| {
        let c = vp * Vec4::new(p.x, p.y, p.z, 1.0);
        (c.x / c.w * 0.5 + 0.5) * WIDTH as f32
    };
    let shift = column(x - offset) - column(x);
    assert!(shift.abs() > 1.0, "the test needs a visible shift: {shift}");
    let moved = e1 - e0;
    assert!(
        (moved - shift).abs() <= 1.0,
        "the edge moved {moved} pixels, the model {shift}"
    );
}
