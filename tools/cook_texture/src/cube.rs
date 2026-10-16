//! Cube maps from equirectangular (latitude-longitude) images.
//!
//! Faces come in D3D order (+X, -X, +Y, -Y, +Z, -Z), each seen from inside the cube with the
//! usual cube map orientation. The equirectangular image maps longitude to x and latitude to y:
//! its center column looks down -Z, its top row is +Y.

use std::f32::consts::{PI, TAU};

use crate::image::{ColorSpace, Image};
use crate::par;

/// The direction through face `face` at face coordinates `u`, `v` in [-1, 1] (u right, v down).
pub fn face_direction(face: usize, u: f32, v: f32) -> [f32; 3] {
    let d = match face {
        0 => [1.0, -v, -u],
        1 => [-1.0, -v, u],
        2 => [u, 1.0, v],
        3 => [u, -1.0, -v],
        4 => [u, -v, 1.0],
        _ => [-u, -v, -1.0],
    };
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    d.map(|c| c / len)
}

/// Where direction `d` lands in an equirectangular image, in [0, 1]².
pub fn equirect_coords(d: [f32; 3]) -> (f32, f32) {
    let u = 0.5 + d[0].atan2(-d[2]) / TAU;
    let v = d[1].clamp(-1.0, 1.0).acos() / PI;
    (u, v)
}

/// Bilinear sample at `u`, `v` in [0, 1]²: wrapping across longitude, clamped at the poles.
fn sample(texels: &[[f32; 4]], width: u32, height: u32, u: f32, v: f32) -> [f32; 4] {
    let x = u * width as f32 - 0.5;
    let y = (v * height as f32 - 0.5).clamp(0.0, height as f32 - 1.0);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let wrap = |x: f32| (x as i64).rem_euclid(width as i64) as usize;
    let clamp = |y: f32| (y as i64).clamp(0, height as i64 - 1) as usize;
    let (xa, xb) = (wrap(x0), wrap(x0 + 1.0));
    let (ya, yb) = (clamp(y0), clamp(y0 + 1.0));
    let at = |x: usize, y: usize| texels[y * width as usize + x];
    let (a, b, c, d) = (at(xa, ya), at(xb, ya), at(xa, yb), at(xb, yb));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

/// Samples per texel side: each face texel averages 3×3 bilinear samples, enough that a face as
/// wide as a quarter of the source neither aliases nor blurs visibly.
const SUPERSAMPLE: u32 = 3;

/// The six faces of `size` × `size` texels, resampled from `image` in linear light. Faces keep
/// the image's color space, usage and storage.
pub fn equirect_to_cube(image: &Image, size: u32) -> [Image; 6] {
    assert!(size > 0, "empty cube face");
    let linear = image.texels_in(ColorSpace::Linear);
    let (w, h) = (image.width, image.height);
    std::array::from_fn(|face| {
        let mut texels = vec![[0.0f32; 4]; (size * size) as usize];
        par::for_each_row(&mut texels, size as usize, |row, out| {
            for (x, texel) in out.iter_mut().enumerate() {
                let mut sum = [0.0f32; 4];
                for sy in 0..SUPERSAMPLE {
                    for sx in 0..SUPERSAMPLE {
                        let fu = (x as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                        let fv =
                            (row as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32) / size as f32;
                        let d = face_direction(face, fu * 2.0 - 1.0, fv * 2.0 - 1.0);
                        let (u, v) = equirect_coords(d);
                        let s = sample(&linear, w, h, u, v);
                        for i in 0..4 {
                            sum[i] += s[i];
                        }
                    }
                }
                *texel = sum.map(|c| c / (SUPERSAMPLE * SUPERSAMPLE) as f32);
            }
        });
        image.with_texels(size, size, ColorSpace::Linear, texels)
    })
}
