//! Double-precision positions for large worlds (docs/design/0019-engine-framework-and-world.md
//! §6).
//!
//! World positions are [`DVec3`]; directions, rotations and scales stay `f32`. Code that works in
//! `f32` (rendering, physics) takes positions relative to an origin near them with
//! [`DVec3::relative_to`]: the difference is exact in `f64` and then rounded once, so it keeps
//! `f32` precision however far from the world's origin both points are.

use core::f64::math as fm;
use core::fmt;
use core::ops::{
    Add, AddAssign, Div, DivAssign, Index, IndexMut, Mul, MulAssign, Neg, Sub, SubAssign,
};

use crate::{Quat, Vec3};

#[derive(Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct DVec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl DVec3 {
    pub const ZERO: Self = Self::splat(0.0);
    pub const ONE: Self = Self::splat(1.0);
    pub const X: Self = Self::new(1.0, 0.0, 0.0);
    pub const Y: Self = Self::new(0.0, 1.0, 0.0);
    pub const Z: Self = Self::new(0.0, 0.0, 1.0);

    #[inline]
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    #[inline]
    pub const fn splat(v: f64) -> Self {
        Self { x: v, y: v, z: v }
    }

    #[inline]
    pub const fn from_array(a: [f64; 3]) -> Self {
        let [x, y, z] = a;
        Self { x, y, z }
    }

    #[inline]
    pub const fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    /// Widens an `f32` vector (exact).
    #[inline]
    pub const fn from_vec3(v: Vec3) -> Self {
        Self::new(v.x as f64, v.y as f64, v.z as f64)
    }

    /// Rounds each component to the nearest `f32`: only for values known to be small, such as
    /// offsets; positions go through [`relative_to`](Self::relative_to).
    #[inline]
    pub const fn as_vec3(self) -> Vec3 {
        Vec3::new(self.x as f32, self.y as f32, self.z as f32)
    }

    /// `self - origin` rounded to `f32`: this position as seen from a nearby origin.
    #[inline]
    pub fn relative_to(self, origin: Self) -> Vec3 {
        (self - origin).as_vec3()
    }

    #[inline]
    pub fn dot(self, rhs: Self) -> f64 {
        self.x * rhs.x + self.y * rhs.y + self.z * rhs.z
    }

    #[inline]
    pub fn length_squared(self) -> f64 {
        self.dot(self)
    }

    #[inline]
    pub fn length(self) -> f64 {
        fm::sqrt(self.dot(self))
    }

    #[inline]
    pub fn distance(self, rhs: Self) -> f64 {
        (self - rhs).length()
    }

    #[inline]
    pub fn distance_squared(self, rhs: Self) -> f64 {
        (self - rhs).length_squared()
    }

    #[inline]
    pub fn min(self, rhs: Self) -> Self {
        Self::new(self.x.min(rhs.x), self.y.min(rhs.y), self.z.min(rhs.z))
    }

    #[inline]
    pub fn max(self, rhs: Self) -> Self {
        Self::new(self.x.max(rhs.x), self.y.max(rhs.y), self.z.max(rhs.z))
    }

    #[inline]
    pub fn abs(self) -> Self {
        Self::new(self.x.abs(), self.y.abs(), self.z.abs())
    }

    /// `self` at `t = 0`, `rhs` at `t = 1`.
    #[inline]
    pub fn lerp(self, rhs: Self, t: f64) -> Self {
        self + (rhs - self) * t
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }

    #[inline]
    pub fn abs_diff_eq(self, rhs: Self, epsilon: f64) -> bool {
        (self - rhs).abs().max_element() <= epsilon
    }

    #[inline]
    pub fn max_element(self) -> f64 {
        self.x.max(self.y).max(self.z)
    }

    /// Scales each component by `scale`.
    #[inline]
    pub fn scale(self, scale: Vec3) -> Self {
        self * Self::from_vec3(scale)
    }

    /// Rotated by `q`, evaluated in `f64` so large offsets keep their precision.
    #[inline]
    pub fn rotate(self, q: Quat) -> Self {
        let qv = Self::new(q.x as f64, q.y as f64, q.z as f64);
        let t = cross(qv, self) * 2.0;
        self + t * q.w as f64 + cross(qv, t)
    }
}

#[inline]
fn cross(a: DVec3, b: DVec3) -> DVec3 {
    DVec3::new(
        a.y * b.z - a.z * b.y,
        a.z * b.x - a.x * b.z,
        a.x * b.y - a.y * b.x,
    )
}

impl From<Vec3> for DVec3 {
    #[inline]
    fn from(v: Vec3) -> Self {
        Self::from_vec3(v)
    }
}

impl From<[f64; 3]> for DVec3 {
    #[inline]
    fn from(a: [f64; 3]) -> Self {
        Self::from_array(a)
    }
}

impl Mul<DVec3> for Quat {
    type Output = DVec3;
    #[inline]
    fn mul(self, rhs: DVec3) -> DVec3 {
        rhs.rotate(self)
    }
}

macro_rules! binop {
    ($Trait:ident, $fn:ident, $TraitA:ident, $fna:ident, $op:tt) => {
        impl $Trait for DVec3 {
            type Output = Self;
            #[inline]
            fn $fn(self, rhs: Self) -> Self {
                Self::new(self.x $op rhs.x, self.y $op rhs.y, self.z $op rhs.z)
            }
        }
        impl $Trait<f64> for DVec3 {
            type Output = Self;
            #[inline]
            fn $fn(self, rhs: f64) -> Self {
                Self::new(self.x $op rhs, self.y $op rhs, self.z $op rhs)
            }
        }
        impl $TraitA for DVec3 {
            #[inline]
            fn $fna(&mut self, rhs: Self) {
                *self = *self $op rhs;
            }
        }
        impl $TraitA<f64> for DVec3 {
            #[inline]
            fn $fna(&mut self, rhs: f64) {
                *self = *self $op rhs;
            }
        }
    };
}

binop!(Add, add, AddAssign, add_assign, +);
binop!(Sub, sub, SubAssign, sub_assign, -);
binop!(Mul, mul, MulAssign, mul_assign, *);
binop!(Div, div, DivAssign, div_assign, /);

/// Offsets a position by an `f32` vector.
impl Add<Vec3> for DVec3 {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Vec3) -> Self {
        self + Self::from_vec3(rhs)
    }
}

impl AddAssign<Vec3> for DVec3 {
    #[inline]
    fn add_assign(&mut self, rhs: Vec3) {
        *self = *self + rhs;
    }
}

impl Sub<Vec3> for DVec3 {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Vec3) -> Self {
        self - Self::from_vec3(rhs)
    }
}

impl SubAssign<Vec3> for DVec3 {
    #[inline]
    fn sub_assign(&mut self, rhs: Vec3) {
        *self = *self - rhs;
    }
}

impl Neg for DVec3 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl Index<usize> for DVec3 {
    type Output = f64;
    #[inline]
    fn index(&self, i: usize) -> &f64 {
        match i {
            0 => &self.x,
            1 => &self.y,
            2 => &self.z,
            _ => panic!("DVec3 index {i} out of range"),
        }
    }
}

impl IndexMut<usize> for DVec3 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f64 {
        match i {
            0 => &mut self.x,
            1 => &mut self.y,
            2 => &mut self.z,
            _ => panic!("DVec3 index {i} out of range"),
        }
    }
}

impl fmt::Debug for DVec3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DVec3")
            .field(&self.x)
            .field(&self.y)
            .field(&self.z)
            .finish()
    }
}
