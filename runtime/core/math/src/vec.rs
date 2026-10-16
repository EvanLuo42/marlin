//! Float and integer vectors.

use core::fmt;
use core::ops::{
    Add, AddAssign, Deref, DerefMut, Div, DivAssign, Index, IndexMut, Mul, MulAssign, Neg, Sub,
    SubAssign,
};
use core::simd::prelude::*;
use core::simd::simd_swizzle;

use crate::scalar;

/// Implements the API shared by all float vectors on top of a few primitives.
macro_rules! float_vec_common {
    ($T:ident, $N:literal) => {
        impl $T {
            pub const ZERO: Self = Self::splat(0.0);
            pub const ONE: Self = Self::splat(1.0);
            pub const NAN: Self = Self::splat(f32::NAN);

            #[inline]
            pub fn length_squared(self) -> f32 {
                self.dot(self)
            }

            #[inline]
            pub fn length(self) -> f32 {
                scalar::sqrt(self.dot(self))
            }

            #[inline]
            pub fn length_recip(self) -> f32 {
                1.0 / self.length()
            }

            #[inline]
            pub fn distance(self, rhs: Self) -> f32 {
                (self - rhs).length()
            }

            #[inline]
            pub fn distance_squared(self, rhs: Self) -> f32 {
                (self - rhs).length_squared()
            }

            /// Unit vector in the same direction; NaN for zero or non-finite input.
            #[inline]
            pub fn normalize(self) -> Self {
                self * self.length_recip()
            }

            /// `None` for zero, tiny or non-finite input.
            #[inline]
            pub fn try_normalize(self) -> Option<Self> {
                let recip = self.length_recip();
                (recip.is_finite() && recip > 0.0).then(|| self * recip)
            }

            #[inline]
            pub fn normalize_or_zero(self) -> Self {
                self.try_normalize().unwrap_or(Self::ZERO)
            }

            #[inline]
            pub fn is_normalized(self) -> bool {
                (self.length_squared() - 1.0).abs() <= 2e-4
            }

            #[inline]
            pub fn lerp(self, rhs: Self, t: f32) -> Self {
                self + (rhs - self) * t
            }

            #[inline]
            pub fn clamp(self, min: Self, max: Self) -> Self {
                self.max(min).min(max)
            }

            /// Clamps the length to at most `max`.
            #[inline]
            pub fn clamp_length_max(self, max: f32) -> Self {
                let len_sq = self.length_squared();
                if len_sq > max * max {
                    self * (max / scalar::sqrt(len_sq))
                } else {
                    self
                }
            }

            /// Component of `self` along `rhs`.
            #[inline]
            pub fn project_onto(self, rhs: Self) -> Self {
                rhs * (self.dot(rhs) / rhs.length_squared())
            }

            /// Component of `self` perpendicular to `rhs`.
            #[inline]
            pub fn reject_from(self, rhs: Self) -> Self {
                self - self.project_onto(rhs)
            }

            /// Reflection off a surface with unit `normal`.
            #[inline]
            pub fn reflect(self, normal: Self) -> Self {
                self - normal * (2.0 * self.dot(normal))
            }

            #[inline]
            pub fn abs_diff_eq(self, rhs: Self, epsilon: f32) -> bool {
                (self - rhs).abs().max_element() <= epsilon
            }

            #[inline]
            pub fn is_finite(self) -> bool {
                self.to_array().iter().all(|v| v.is_finite())
            }

            #[inline]
            pub fn is_nan(self) -> bool {
                self.to_array().iter().any(|v| v.is_nan())
            }

            #[inline]
            pub fn floor(self) -> Self {
                self.map(scalar::floor)
            }

            #[inline]
            pub fn ceil(self) -> Self {
                self.map(scalar::ceil)
            }

            #[inline]
            pub fn round(self) -> Self {
                self.map(scalar::round)
            }

            #[inline]
            pub fn fract(self) -> Self {
                self.map(scalar::fract)
            }

            #[inline]
            pub fn recip(self) -> Self {
                Self::ONE / self
            }

            #[inline]
            pub fn map(self, f: impl Fn(f32) -> f32) -> Self {
                Self::from_array(self.to_array().map(f))
            }
        }

        impl Mul<$T> for f32 {
            type Output = $T;
            #[inline]
            fn mul(self, rhs: $T) -> $T {
                rhs * self
            }
        }

        impl Div<$T> for f32 {
            type Output = $T;
            #[inline]
            fn div(self, rhs: $T) -> $T {
                $T::splat(self) / rhs
            }
        }

        impl AddAssign for $T {
            #[inline]
            fn add_assign(&mut self, rhs: Self) {
                *self = *self + rhs;
            }
        }
        impl SubAssign for $T {
            #[inline]
            fn sub_assign(&mut self, rhs: Self) {
                *self = *self - rhs;
            }
        }
        impl MulAssign for $T {
            #[inline]
            fn mul_assign(&mut self, rhs: Self) {
                *self = *self * rhs;
            }
        }
        impl DivAssign for $T {
            #[inline]
            fn div_assign(&mut self, rhs: Self) {
                *self = *self / rhs;
            }
        }
        impl MulAssign<f32> for $T {
            #[inline]
            fn mul_assign(&mut self, rhs: f32) {
                *self = *self * rhs;
            }
        }
        impl DivAssign<f32> for $T {
            #[inline]
            fn div_assign(&mut self, rhs: f32) {
                *self = *self / rhs;
            }
        }

        impl From<[f32; $N]> for $T {
            #[inline]
            fn from(a: [f32; $N]) -> Self {
                Self::from_array(a)
            }
        }
        impl From<$T> for [f32; $N] {
            #[inline]
            fn from(v: $T) -> Self {
                v.to_array()
            }
        }

        impl core::iter::Sum for $T {
            fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
                iter.fold(Self::ZERO, |a, b| a + b)
            }
        }
    };
}

/// Component-wise operators and helpers for the plain `repr(C)` vectors.
macro_rules! scalar_vec {
    ($T:ident, $N:literal, $f0:ident $(, $f:ident)*) => {
        impl $T {
            #[inline]
            pub const fn new($f0: f32 $(, $f: f32)*) -> Self {
                Self { $f0 $(, $f)* }
            }

            #[inline]
            pub const fn splat(v: f32) -> Self {
                Self { $f0: v $(, $f: v)* }
            }

            #[inline]
            pub const fn from_array(a: [f32; $N]) -> Self {
                let [$f0 $(, $f)*] = a;
                Self { $f0 $(, $f)* }
            }

            #[inline]
            pub const fn to_array(self) -> [f32; $N] {
                [self.$f0 $(, self.$f)*]
            }

            #[inline]
            pub fn dot(self, rhs: Self) -> f32 {
                self.$f0 * rhs.$f0 $(+ self.$f * rhs.$f)*
            }

            #[inline]
            pub fn min(self, rhs: Self) -> Self {
                Self { $f0: self.$f0.min(rhs.$f0) $(, $f: self.$f.min(rhs.$f))* }
            }

            #[inline]
            pub fn max(self, rhs: Self) -> Self {
                Self { $f0: self.$f0.max(rhs.$f0) $(, $f: self.$f.max(rhs.$f))* }
            }

            #[inline]
            pub fn abs(self) -> Self {
                Self { $f0: self.$f0.abs() $(, $f: self.$f.abs())* }
            }

            #[inline]
            pub fn min_element(self) -> f32 {
                self.$f0 $(.min(self.$f))*
            }

            #[inline]
            pub fn max_element(self) -> f32 {
                self.$f0 $(.max(self.$f))*
            }

            #[inline]
            pub fn element_sum(self) -> f32 {
                self.$f0 $(+ self.$f)*
            }
        }

        impl Add for $T {
            type Output = Self;
            #[inline]
            fn add(self, rhs: Self) -> Self {
                Self { $f0: self.$f0 + rhs.$f0 $(, $f: self.$f + rhs.$f)* }
            }
        }
        impl Sub for $T {
            type Output = Self;
            #[inline]
            fn sub(self, rhs: Self) -> Self {
                Self { $f0: self.$f0 - rhs.$f0 $(, $f: self.$f - rhs.$f)* }
            }
        }
        impl Mul for $T {
            type Output = Self;
            #[inline]
            fn mul(self, rhs: Self) -> Self {
                Self { $f0: self.$f0 * rhs.$f0 $(, $f: self.$f * rhs.$f)* }
            }
        }
        impl Div for $T {
            type Output = Self;
            #[inline]
            fn div(self, rhs: Self) -> Self {
                Self { $f0: self.$f0 / rhs.$f0 $(, $f: self.$f / rhs.$f)* }
            }
        }
        impl Mul<f32> for $T {
            type Output = Self;
            #[inline]
            fn mul(self, rhs: f32) -> Self {
                Self { $f0: self.$f0 * rhs $(, $f: self.$f * rhs)* }
            }
        }
        impl Div<f32> for $T {
            type Output = Self;
            #[inline]
            fn div(self, rhs: f32) -> Self {
                Self { $f0: self.$f0 / rhs $(, $f: self.$f / rhs)* }
            }
        }
        impl Neg for $T {
            type Output = Self;
            #[inline]
            fn neg(self) -> Self {
                Self { $f0: -self.$f0 $(, $f: -self.$f)* }
            }
        }

        impl Index<usize> for $T {
            type Output = f32;
            #[inline]
            fn index(&self, i: usize) -> &f32 {
                &self.as_ref()[i]
            }
        }
        impl IndexMut<usize> for $T {
            #[inline]
            fn index_mut(&mut self, i: usize) -> &mut f32 {
                &mut self.as_mut()[i]
            }
        }
        impl AsRef<[f32; $N]> for $T {
            #[inline]
            fn as_ref(&self) -> &[f32; $N] {
                // SAFETY: `repr(C)` with $N f32 fields has the layout of [f32; $N].
                unsafe { &*(self as *const Self).cast() }
            }
        }
        impl AsMut<[f32; $N]> for $T {
            #[inline]
            fn as_mut(&mut self) -> &mut [f32; $N] {
                // SAFETY: as above.
                unsafe { &mut *(self as *mut Self).cast() }
            }
        }

        impl fmt::Debug for $T {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($T)).field(&self.$f0) $(.field(&self.$f))* .finish()
            }
        }

        float_vec_common!($T, $N);
    };
}

// --- Vec2 ---

#[derive(Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

scalar_vec!(Vec2, 2, x, y);

impl Vec2 {
    pub const X: Self = Self::new(1.0, 0.0);
    pub const Y: Self = Self::new(0.0, 1.0);

    #[inline]
    pub const fn extend(self, z: f32) -> Vec3 {
        Vec3::new(self.x, self.y, z)
    }

    /// Rotated 90° counter-clockwise.
    #[inline]
    pub const fn perp(self) -> Self {
        Self::new(-self.y, self.x)
    }

    /// z of the 3D cross product: positive when `rhs` is counter-clockwise from `self`.
    #[inline]
    pub fn perp_dot(self, rhs: Self) -> f32 {
        self.x * rhs.y - self.y * rhs.x
    }

    /// Unit vector at `angle` radians from +X.
    #[inline]
    pub fn from_angle(angle: f32) -> Self {
        let (s, c) = scalar::sin_cos(angle);
        Self::new(c, s)
    }

    /// Angle from +X, in [-π, π].
    #[inline]
    pub fn to_angle(self) -> f32 {
        scalar::atan2(self.y, self.x)
    }
}

// --- Vec3 ---

#[derive(Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

scalar_vec!(Vec3, 3, x, y, z);

impl Vec3 {
    pub const X: Self = Self::new(1.0, 0.0, 0.0);
    pub const Y: Self = Self::new(0.0, 1.0, 0.0);
    pub const Z: Self = Self::new(0.0, 0.0, 1.0);
    pub const NEG_X: Self = Self::new(-1.0, 0.0, 0.0);
    pub const NEG_Y: Self = Self::new(0.0, -1.0, 0.0);
    pub const NEG_Z: Self = Self::new(0.0, 0.0, -1.0);

    pub const RIGHT: Self = Self::X;
    pub const LEFT: Self = Self::NEG_X;
    pub const UP: Self = Self::Y;
    pub const DOWN: Self = Self::NEG_Y;
    /// Where cameras and models look: -Z.
    pub const FORWARD: Self = Self::NEG_Z;
    pub const BACK: Self = Self::Z;

    #[inline]
    pub fn cross(self, rhs: Self) -> Self {
        Self::new(
            self.y * rhs.z - self.z * rhs.y,
            self.z * rhs.x - self.x * rhs.z,
            self.x * rhs.y - self.y * rhs.x,
        )
    }

    #[inline]
    pub const fn extend(self, w: f32) -> Vec4 {
        Vec4::new(self.x, self.y, self.z, w)
    }

    #[inline]
    pub const fn truncate(self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }

    /// Angle between two vectors, in [0, π].
    #[inline]
    pub fn angle_between(self, rhs: Self) -> f32 {
        scalar::atan2(self.cross(rhs).length(), self.dot(rhs))
    }

    /// Some unit vector orthogonal to unit `self` (Duff et al., "Building an Orthonormal Basis,
    /// Revisited").
    pub fn any_orthonormal_vector(self) -> Self {
        let sign = 1.0f32.copysign(self.z);
        let a = -1.0 / (sign + self.z);
        let b = self.x * self.y * a;
        Self::new(b, sign + self.y * self.y * a, -self.y)
    }
}

// --- Vec4 ---

/// Four `f32`s in a SIMD register, 16-byte aligned. Fields are reachable as `v.x` through
/// `Deref`.
#[derive(Clone, Copy, PartialEq, Default)]
#[repr(transparent)]
pub struct Vec4(pub(crate) f32x4);

/// Field view of a `Vec4` or `Quat`.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct Xyzw {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Deref for Vec4 {
    type Target = Xyzw;
    #[inline]
    fn deref(&self) -> &Xyzw {
        // SAFETY: f32x4 has the layout of [f32; 4] (with more alignment), as does Xyzw.
        unsafe { &*(self as *const Self).cast() }
    }
}

impl DerefMut for Vec4 {
    #[inline]
    fn deref_mut(&mut self) -> &mut Xyzw {
        // SAFETY: as above.
        unsafe { &mut *(self as *mut Self).cast() }
    }
}

impl Vec4 {
    pub const X: Self = Self::new(1.0, 0.0, 0.0, 0.0);
    pub const Y: Self = Self::new(0.0, 1.0, 0.0, 0.0);
    pub const Z: Self = Self::new(0.0, 0.0, 1.0, 0.0);
    pub const W: Self = Self::new(0.0, 0.0, 0.0, 1.0);

    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self(f32x4::from_array([x, y, z, w]))
    }

    #[inline]
    pub const fn splat(v: f32) -> Self {
        Self(f32x4::from_array([v; 4]))
    }

    #[inline]
    pub const fn from_array(a: [f32; 4]) -> Self {
        Self(f32x4::from_array(a))
    }

    #[inline]
    pub const fn to_array(self) -> [f32; 4] {
        self.0.to_array()
    }

    #[inline]
    pub fn dot(self, rhs: Self) -> f32 {
        (self.0 * rhs.0).reduce_sum()
    }

    #[inline]
    pub fn min(self, rhs: Self) -> Self {
        Self(self.0.simd_min(rhs.0))
    }

    #[inline]
    pub fn max(self, rhs: Self) -> Self {
        Self(self.0.simd_max(rhs.0))
    }

    #[inline]
    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    #[inline]
    pub fn min_element(self) -> f32 {
        self.0.reduce_min()
    }

    #[inline]
    pub fn max_element(self) -> f32 {
        self.0.reduce_max()
    }

    #[inline]
    pub fn element_sum(self) -> f32 {
        self.0.reduce_sum()
    }

    #[inline]
    pub fn truncate(self) -> Vec3 {
        Vec3::new(self.x, self.y, self.z)
    }

    // One lane broadcast to all four, staying in the register.
    #[inline]
    pub(crate) fn splat_x(self) -> f32x4 {
        simd_swizzle!(self.0, [0, 0, 0, 0])
    }

    #[inline]
    pub(crate) fn splat_y(self) -> f32x4 {
        simd_swizzle!(self.0, [1, 1, 1, 1])
    }

    #[inline]
    pub(crate) fn splat_z(self) -> f32x4 {
        simd_swizzle!(self.0, [2, 2, 2, 2])
    }

    #[inline]
    pub(crate) fn splat_w(self) -> f32x4 {
        simd_swizzle!(self.0, [3, 3, 3, 3])
    }
}

macro_rules! simd_binop {
    ($Trait:ident, $f:ident, $op:tt) => {
        impl $Trait for Vec4 {
            type Output = Self;
            #[inline]
            fn $f(self, rhs: Self) -> Self {
                Self(self.0 $op rhs.0)
            }
        }
        impl $Trait<f32> for Vec4 {
            type Output = Self;
            #[inline]
            fn $f(self, rhs: f32) -> Self {
                Self(self.0 $op f32x4::splat(rhs))
            }
        }
    };
}

simd_binop!(Add, add, +);
simd_binop!(Sub, sub, -);
simd_binop!(Mul, mul, *);
simd_binop!(Div, div, /);

impl Neg for Vec4 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl Index<usize> for Vec4 {
    type Output = f32;
    #[inline]
    fn index(&self, i: usize) -> &f32 {
        &self.0.as_array()[i]
    }
}

impl IndexMut<usize> for Vec4 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f32 {
        &mut self.0.as_mut_array()[i]
    }
}

impl fmt::Debug for Vec4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Vec4")
            .field(&self.x)
            .field(&self.y)
            .field(&self.z)
            .field(&self.w)
            .finish()
    }
}

float_vec_common!(Vec4, 4);

// --- Integer vectors ---

macro_rules! int_vec {
    ($T:ident, $S:ty, $F:ident, $N:literal, $($f:ident),+) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
        #[repr(C)]
        pub struct $T {
            $(pub $f: $S),+
        }

        impl $T {
            pub const ZERO: Self = Self::splat(0);
            pub const ONE: Self = Self::splat(1);

            #[inline]
            pub const fn new($($f: $S),+) -> Self {
                Self { $($f),+ }
            }

            #[inline]
            pub const fn splat(v: $S) -> Self {
                Self { $($f: v),+ }
            }

            #[inline]
            pub const fn to_array(self) -> [$S; $N] {
                [$(self.$f),+]
            }

            #[inline]
            pub fn min(self, rhs: Self) -> Self {
                Self { $($f: self.$f.min(rhs.$f)),+ }
            }

            #[inline]
            pub fn max(self, rhs: Self) -> Self {
                Self { $($f: self.$f.max(rhs.$f)),+ }
            }

            #[inline]
            pub fn as_float(self) -> $F {
                $F { $($f: self.$f as f32),+ }
            }
        }

        impl $F {
            /// Truncates toward zero; saturates out-of-range values.
            #[inline]
            pub fn as_int(self) -> $T {
                $T { $($f: self.$f as $S),+ }
            }
        }

        impl From<[$S; $N]> for $T {
            #[inline]
            fn from(a: [$S; $N]) -> Self {
                let [$($f),+] = a;
                Self { $($f),+ }
            }
        }

        int_vec!(@op $T, $S, Add, add, AddAssign, add_assign, +, $($f),+);
        int_vec!(@op $T, $S, Sub, sub, SubAssign, sub_assign, -, $($f),+);
        int_vec!(@op $T, $S, Mul, mul, MulAssign, mul_assign, *, $($f),+);
        int_vec!(@op $T, $S, Div, div, DivAssign, div_assign, /, $($f),+);
    };
    (@op $T:ident, $S:ty, $Trait:ident, $fn:ident, $TraitA:ident, $fna:ident, $op:tt, $($f:ident),+) => {
        impl $Trait for $T {
            type Output = Self;
            #[inline]
            fn $fn(self, rhs: Self) -> Self {
                Self { $($f: self.$f $op rhs.$f),+ }
            }
        }
        impl $Trait<$S> for $T {
            type Output = Self;
            #[inline]
            fn $fn(self, rhs: $S) -> Self {
                Self { $($f: self.$f $op rhs),+ }
            }
        }
        impl $TraitA for $T {
            #[inline]
            fn $fna(&mut self, rhs: Self) {
                *self = *self $op rhs;
            }
        }
    };
}

int_vec!(IVec2, i32, Vec2, 2, x, y);
int_vec!(IVec3, i32, Vec3, 3, x, y, z);

/// Unsigned 2D extents and coordinates (texture sizes, tiles).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
#[repr(C)]
pub struct UVec2 {
    pub x: u32,
    pub y: u32,
}

impl UVec2 {
    pub const ZERO: Self = Self::new(0, 0);
    pub const ONE: Self = Self::new(1, 1);

    #[inline]
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn as_vec2(self) -> Vec2 {
        Vec2::new(self.x as f32, self.y as f32)
    }

    /// `x * y`, e.g. a texture's texel count.
    #[inline]
    pub const fn area(self) -> u64 {
        self.x as u64 * self.y as u64
    }
}

impl Neg for IVec2 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

impl Neg for IVec3 {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}
