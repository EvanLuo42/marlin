//! Gradients (§2.1, §8.2): linear, radial and angle, their stops interpolated in sRGB-encoded
//! space or in Oklab, between premultiplied colors.
//!
//! Each gradient is baked when recorded into a ramp of [`GRADIENT_TEXELS`] premultiplied RGBA8
//! texels, so shading takes one interpolated lookup whatever the stops and the space. Its
//! geometry is kept on the screen. (§3.1 puts the ramps in a texture; a buffer the shader
//! interpolates is the same lookup without an upload pass.)

use alloc::vec::Vec;

use marlin_math::Vec2;
use marlin_math::scalar::{powf, sqrt};

use crate::command::{GRADIENT_TEXELS, GRADIENT_WORDS, GradientWords};
use crate::shape::{linear_to_srgb, srgb_to_linear};
use crate::style::{Affine2, Color};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GradientShape {
    /// Along the line from `from` (0) to `to` (1).
    Linear { from: Vec2, to: Vec2 },
    /// Out from `center` (0) to `radius` (1).
    Radial { center: Vec2, radius: f32 },
    /// Clockwise around `center` (y down), from `start` radians (0) once round (1).
    Angle { center: Vec2, start: f32 },
}

/// The space the stops are interpolated in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GradientSpace {
    /// sRGB-encoded values, as browsers and design tools do (§8.1).
    #[default]
    Srgb,
    /// Oklab: perceptually even, no grey middle between saturated colors.
    Oklab,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Gradient {
    pub shape: GradientShape,
    /// Positions 0–1 with their colors, in order. Before the first and after the last, the end
    /// colors go on.
    pub stops: Vec<(f32, Color)>,
    pub space: GradientSpace,
}

impl Gradient {
    pub fn linear(from: Vec2, to: Vec2, stops: &[(f32, Color)]) -> Self {
        Self {
            shape: GradientShape::Linear { from, to },
            stops: stops.to_vec(),
            space: GradientSpace::Srgb,
        }
    }

    pub fn radial(center: Vec2, radius: f32, stops: &[(f32, Color)]) -> Self {
        Self {
            shape: GradientShape::Radial { center, radius },
            stops: stops.to_vec(),
            space: GradientSpace::Srgb,
        }
    }

    pub fn angle(center: Vec2, start: f32, stops: &[(f32, Color)]) -> Self {
        Self {
            shape: GradientShape::Angle { center, start },
            stops: stops.to_vec(),
            space: GradientSpace::Srgb,
        }
    }

    pub fn in_oklab(mut self) -> Self {
        self.space = GradientSpace::Oklab;
        self
    }

    /// The premultiplied, sRGB-encoded color at `t` (0–1), as the ramp bakes it.
    pub fn color_at(&self, t: f32) -> [f32; 4] {
        let stops = &self.stops;
        let Some(&(_, first)) = stops.first() else {
            return [0.0; 4];
        };
        let to = |c: Color| self.encode(c);
        if t <= stops[0].0 {
            return self.decode(to(first));
        }
        for w in stops.windows(2) {
            let ((t0, c0), (t1, c1)) = (w[0], w[1]);
            if t <= t1 {
                let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                let (a, b) = (to(c0), to(c1));
                let mix: [f32; 4] = core::array::from_fn(|i| a[i] + (b[i] - a[i]) * k);
                return self.decode(mix);
            }
        }
        self.decode(to(stops[stops.len() - 1].1))
    }

    /// A stop color in the interpolation space, premultiplied: sRGB-encoded or Oklab values
    /// times alpha, then alpha.
    fn encode(&self, c: Color) -> [f32; 4] {
        let a = f32::from(c.a) / 255.0;
        let rgb = [c.r, c.g, c.b].map(|v| f32::from(v) / 255.0);
        let v = match self.space {
            GradientSpace::Srgb => rgb,
            GradientSpace::Oklab => oklab(rgb.map(srgb_to_linear)),
        };
        [v[0] * a, v[1] * a, v[2] * a, a]
    }

    /// Back from the space to premultiplied sRGB-encoded.
    fn decode(&self, v: [f32; 4]) -> [f32; 4] {
        let a = v[3];
        if a <= 0.0 {
            return [0.0; 4];
        }
        let s = [v[0] / a, v[1] / a, v[2] / a];
        let rgb = match self.space {
            GradientSpace::Srgb => s,
            GradientSpace::Oklab => linear_from_oklab(s).map(|c| linear_to_srgb(c.clamp(0.0, 1.0))),
        };
        [rgb[0] * a, rgb[1] * a, rgb[2] * a, a]
    }
}

/// Linear sRGB to Oklab (Björn Ottosson, 2020).
fn oklab(c: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_47 * c[0] + 0.536_332_55 * c[1] + 0.051_445_995 * c[2];
    let m = 0.211_903_5 * c[0] + 0.680_699_5 * c[1] + 0.107_396_96 * c[2];
    let s = 0.088_302_46 * c[0] + 0.281_718_85 * c[1] + 0.629_978_7 * c[2];
    let (l, m, s) = (cbrt(l), cbrt(m), cbrt(s));
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn linear_from_oklab(c: [f32; 3]) -> [f32; 3] {
    let l = c[0] + 0.396_337_78 * c[1] + 0.215_803_76 * c[2];
    let m = c[0] - 0.105_561_346 * c[1] - 0.063_854_17 * c[2];
    let s = c[0] - 0.089_484_18 * c[1] - 1.291_485_5 * c[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

fn cbrt(x: f32) -> f32 {
    if x < 0.0 {
        -powf(-x, 1.0 / 3.0)
    } else {
        powf(x, 1.0 / 3.0)
    }
}

/// `g`'s record: its geometry through `to_screen` (physical pixels), `opacity`, and its ramp.
pub(crate) fn bake(g: &Gradient, to_screen: Affine2, opacity: f32) -> GradientWords {
    let mut w: GradientWords = [0; GRADIENT_WORDS];
    let scale = sqrt(to_screen.determinant().abs());
    let (kind, params) = match g.shape {
        GradientShape::Linear { from, to } => {
            let (a, b) = (to_screen.apply(from), to_screen.apply(to));
            (0, [a.x, a.y, b.x, b.y])
        }
        GradientShape::Radial { center, radius } => {
            let c = to_screen.apply(center);
            (1, [c.x, c.y, radius * scale, 0.0])
        }
        GradientShape::Angle { center, start } => {
            // The screen angle of the start direction.
            let c = to_screen.apply(center);
            let (sin, cos) = marlin_math::scalar::sin_cos(start);
            let d = to_screen.apply(center + Vec2::new(cos, sin)) - c;
            (2, [c.x, c.y, marlin_math::scalar::atan2(d.y, d.x), 0.0])
        }
    };
    w[0] = kind;
    for (i, v) in params.into_iter().enumerate() {
        w[1 + i] = v.to_bits();
    }
    w[5] = opacity.to_bits();
    let mut ramp = Vec::with_capacity(GRADIENT_TEXELS);
    for i in 0..GRADIENT_TEXELS {
        let c = g.color_at(i as f32 / (GRADIENT_TEXELS - 1) as f32);
        let q = c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32);
        ramp.push(q[0] | q[1] << 8 | q[2] << 16 | q[3] << 24);
    }
    w[8..].copy_from_slice(&ramp);
    w
}
