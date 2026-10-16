//! Rotation quaternions.

use core::fmt;
use core::ops::{Deref, DerefMut, Mul, MulAssign, Neg};
use core::simd::prelude::*;
use core::simd::simd_swizzle;

use crate::{Mat3, Vec3, Vec4, Xyzw, scalar};

/// A rotation as a unit quaternion `(x, y, z, w)`, `w` the scalar part. Fields are reachable as
/// `q.x` through `Deref`.
#[derive(Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct Quat(pub(crate) f32x4);

impl Deref for Quat {
    type Target = Xyzw;
    #[inline]
    fn deref(&self) -> &Xyzw {
        // SAFETY: f32x4 has the layout of [f32; 4] (with more alignment), as does Xyzw.
        unsafe { &*(self as *const Self).cast() }
    }
}

impl DerefMut for Quat {
    #[inline]
    fn deref_mut(&mut self) -> &mut Xyzw {
        // SAFETY: as above.
        unsafe { &mut *(self as *mut Self).cast() }
    }
}

impl Default for Quat {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Quat {
    pub const IDENTITY: Self = Self::from_xyzw(0.0, 0.0, 0.0, 1.0);

    #[inline]
    pub const fn from_xyzw(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self(f32x4::from_array([x, y, z, w]))
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
    pub const fn from_vec4(v: Vec4) -> Self {
        Self(v.0)
    }

    #[inline]
    pub const fn to_vec4(self) -> Vec4 {
        Vec4(self.0)
    }

    /// Rotation of `angle` radians about unit `axis`.
    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        let (s, c) = scalar::sin_cos(angle * 0.5);
        let v = axis * s;
        Self::from_xyzw(v.x, v.y, v.z, c)
    }

    #[inline]
    pub fn from_rotation_x(angle: f32) -> Self {
        let (s, c) = scalar::sin_cos(angle * 0.5);
        Self::from_xyzw(s, 0.0, 0.0, c)
    }

    #[inline]
    pub fn from_rotation_y(angle: f32) -> Self {
        let (s, c) = scalar::sin_cos(angle * 0.5);
        Self::from_xyzw(0.0, s, 0.0, c)
    }

    #[inline]
    pub fn from_rotation_z(angle: f32) -> Self {
        let (s, c) = scalar::sin_cos(angle * 0.5);
        Self::from_xyzw(0.0, 0.0, s, c)
    }

    /// Yaw about Y, then pitch about X, then roll about Z, applied to the object in reverse:
    /// `Ry(yaw) * Rx(pitch) * Rz(roll)`. The usual camera/character order for Y-up.
    #[inline]
    pub fn from_euler_yxz(yaw: f32, pitch: f32, roll: f32) -> Self {
        Self::from_rotation_y(yaw) * Self::from_rotation_x(pitch) * Self::from_rotation_z(roll)
    }

    /// Inverse of [`from_euler_yxz`](Self::from_euler_yxz): `(yaw, pitch, roll)`, pitch in
    /// [-π/2, π/2]. At ±90° pitch (gimbal lock) roll is reported as 0.
    pub fn to_euler_yxz(self) -> (f32, f32, f32) {
        let m = Mat3::from_quat(self);
        let sin_pitch = -m.z_axis.y;
        if sin_pitch.abs() >= 0.999_999 {
            let pitch = scalar::FRAC_PI_2.copysign(sin_pitch);
            (scalar::atan2(-m.x_axis.z, m.x_axis.x), pitch, 0.0)
        } else {
            (
                scalar::atan2(m.z_axis.x, m.z_axis.z),
                scalar::asin(sin_pitch),
                scalar::atan2(m.x_axis.y, m.y_axis.y),
            )
        }
    }

    /// Shortest-arc rotation taking unit `from` to unit `to`.
    pub fn from_rotation_arc(from: Vec3, to: Vec3) -> Self {
        let d = from.dot(to);
        if d < -1.0 + 1e-6 {
            // Opposite: any perpendicular axis will do.
            return Self::from_axis_angle(from.any_orthonormal_vector(), scalar::PI);
        }
        let c = from.cross(to);
        Self::from_xyzw(c.x, c.y, c.z, 1.0 + d).normalize()
    }

    /// Rotation turning [`Vec3::FORWARD`] (-Z) toward `forward` and +Y toward `up`, as far as
    /// they allow. Neither needs to be normalized; they must not be parallel.
    pub fn look_to(forward: Vec3, up: Vec3) -> Self {
        let f = forward.normalize();
        let right = f.cross(up).normalize();
        let up = right.cross(f);
        Self::from_mat3(&Mat3::from_cols(right, up, -f))
    }

    /// Rotation part of an orthonormal matrix (Shepperd's method).
    pub fn from_mat3(m: &Mat3) -> Self {
        let (m00, m01, m02) = (m.x_axis.x, m.y_axis.x, m.z_axis.x);
        let (m10, m11, m12) = (m.x_axis.y, m.y_axis.y, m.z_axis.y);
        let (m20, m21, m22) = (m.x_axis.z, m.y_axis.z, m.z_axis.z);
        let trace = m00 + m11 + m22;
        if trace > 0.0 {
            let s = scalar::sqrt(trace + 1.0) * 2.0;
            Self::from_xyzw((m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s)
        } else if m00 > m11 && m00 > m22 {
            let s = scalar::sqrt(1.0 + m00 - m11 - m22) * 2.0;
            Self::from_xyzw(0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s)
        } else if m11 > m22 {
            let s = scalar::sqrt(1.0 + m11 - m00 - m22) * 2.0;
            Self::from_xyzw((m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s)
        } else {
            let s = scalar::sqrt(1.0 + m22 - m00 - m11) * 2.0;
            Self::from_xyzw((m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s)
        }
    }

    /// `(axis, angle)` with angle in [0, π]; the X axis for the identity.
    pub fn to_axis_angle(self) -> (Vec3, f32) {
        let q = if self.w < 0.0 { -self } else { self };
        let v = Vec3::new(q.x, q.y, q.z);
        let s = v.length();
        if s < 1e-7 {
            (Vec3::X, 0.0)
        } else {
            (v / s, 2.0 * scalar::atan2(s, q.w))
        }
    }

    #[inline]
    pub fn dot(self, rhs: Self) -> f32 {
        (self.0 * rhs.0).reduce_sum()
    }

    #[inline]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    #[inline]
    pub fn length(self) -> f32 {
        scalar::sqrt(self.dot(self))
    }

    #[inline]
    pub fn normalize(self) -> Self {
        Self(self.0 * f32x4::splat(1.0 / self.length()))
    }

    #[inline]
    pub fn is_normalized(self) -> bool {
        (self.length_squared() - 1.0).abs() <= 2e-4
    }

    /// The inverse rotation of a unit quaternion.
    #[inline]
    pub fn conjugate(self) -> Self {
        Self(self.0 * f32x4::from_array([-1.0, -1.0, -1.0, 1.0]))
    }

    /// Like [`conjugate`](Self::conjugate), but also correct for non-unit quaternions.
    #[inline]
    pub fn inverse(self) -> Self {
        Self(self.conjugate().0 / f32x4::splat(self.length_squared()))
    }

    /// Rotation angle from `self` to `rhs`, in [0, π].
    #[inline]
    pub fn angle_between(self, rhs: Self) -> f32 {
        2.0 * scalar::acos(self.dot(rhs).abs().min(1.0))
    }

    /// Normalized linear interpolation along the shortest path: cheap, but not constant speed.
    pub fn nlerp(self, end: Self, t: f32) -> Self {
        let end = if self.dot(end) < 0.0 { -end } else { end };
        Self(self.0 + (end.0 - self.0) * f32x4::splat(t)).normalize()
    }

    /// Spherical interpolation along the shortest path, at constant angular speed.
    pub fn slerp(self, end: Self, t: f32) -> Self {
        let mut d = self.dot(end);
        let end = if d < 0.0 {
            d = -d;
            -end
        } else {
            end
        };
        if d > 0.9995 {
            return self.nlerp(end, t);
        }
        let theta = scalar::acos(d);
        let s = 1.0 / scalar::sin(theta);
        let a = scalar::sin((1.0 - t) * theta) * s;
        let b = scalar::sin(t * theta) * s;
        Self(self.0 * f32x4::splat(a) + end.0 * f32x4::splat(b))
    }

    /// Rotates `v`: `v + w·t + q×t` with `t = 2·(q×v)`.
    #[inline]
    pub fn mul_vec3(self, v: Vec3) -> Vec3 {
        let q = Vec3::new(self.x, self.y, self.z);
        let t = q.cross(v) * 2.0;
        v + t * self.w + q.cross(t)
    }

    #[inline]
    pub fn abs_diff_eq(self, rhs: Self, epsilon: f32) -> bool {
        (self.0 - rhs.0).abs().reduce_max() <= epsilon
    }

    /// Whether both represent the same rotation (`q` and `-q` do).
    #[inline]
    pub fn rotation_eq(self, rhs: Self, epsilon: f32) -> bool {
        self.abs_diff_eq(rhs, epsilon) || self.abs_diff_eq(-rhs, epsilon)
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.to_array().iter().all(|v| v.is_finite())
    }
}

impl Mul for Quat {
    type Output = Self;

    /// Hamilton product: `(a * b) * v == a * (b * v)`.
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        let (a, b) = (Vec4(self.0), rhs.0);
        let r = a.splat_w() * b
            + a.splat_x()
                * simd_swizzle!(b, [3, 2, 1, 0])
                * f32x4::from_array([1.0, -1.0, 1.0, -1.0])
            + a.splat_y()
                * simd_swizzle!(b, [2, 3, 0, 1])
                * f32x4::from_array([1.0, 1.0, -1.0, -1.0])
            + a.splat_z()
                * simd_swizzle!(b, [1, 0, 3, 2])
                * f32x4::from_array([-1.0, 1.0, 1.0, -1.0]);
        Self(r)
    }
}

impl MulAssign for Quat {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl Mul<Vec3> for Quat {
    type Output = Vec3;
    #[inline]
    fn mul(self, rhs: Vec3) -> Vec3 {
        self.mul_vec3(rhs)
    }
}

impl Neg for Quat {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

impl fmt::Debug for Quat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Quat")
            .field(&self.x)
            .field(&self.y)
            .field(&self.z)
            .field(&self.w)
            .finish()
    }
}
