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

/// A pane (a quad facing the camera) of tint `tint` and opacity `opacity` at depth `z`.
fn pane(h: &mut Harness, tint: [f32; 3], opacity: f32, z: f32) {
    let q = h.upload(&uv_quad());
    let glass = h.material(MaterialDesc {
        permutation: GLASS,
        params: [tint[0], tint[1], tint[2], 0.05, opacity]
            .map(f32::to_bits)
            .to_vec(),
    });
    h.add(
        q,
        glass,
        Transform {
            translation: Vec3::new(0.0, 0.0, z),
            rotation: marlin_math::Quat::IDENTITY,
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
