//! Column-major 3×3 and 4×4 matrices.

use core::ops::{Add, Mul, MulAssign, Sub};

use crate::{Quat, Vec3, Vec4, scalar};

// --- Mat3 ---

/// 3×3 matrix stored as three column vectors.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct Mat3 {
    pub x_axis: Vec3,
    pub y_axis: Vec3,
    pub z_axis: Vec3,
}

impl Default for Mat3 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat3 {
    pub const IDENTITY: Self = Self::from_cols(Vec3::X, Vec3::Y, Vec3::Z);
    pub const ZERO: Self = Self::from_cols(Vec3::ZERO, Vec3::ZERO, Vec3::ZERO);

    #[inline]
    pub const fn from_cols(x_axis: Vec3, y_axis: Vec3, z_axis: Vec3) -> Self {
        Self {
            x_axis,
            y_axis,
            z_axis,
        }
    }

    /// From nine floats in column-major order.
    #[inline]
    pub const fn from_cols_array(m: &[f32; 9]) -> Self {
        Self::from_cols(
            Vec3::new(m[0], m[1], m[2]),
            Vec3::new(m[3], m[4], m[5]),
            Vec3::new(m[6], m[7], m[8]),
        )
    }

    #[inline]
    pub const fn to_cols_array(&self) -> [f32; 9] {
        let (x, y, z) = (self.x_axis, self.y_axis, self.z_axis);
        [x.x, x.y, x.z, y.x, y.y, y.z, z.x, z.y, z.z]
    }

    #[inline]
    pub fn from_diagonal(d: Vec3) -> Self {
        Self::from_cols(
            Vec3::new(d.x, 0.0, 0.0),
            Vec3::new(0.0, d.y, 0.0),
            Vec3::new(0.0, 0.0, d.z),
        )
    }

    #[inline]
    pub fn from_scale(scale: Vec3) -> Self {
        Self::from_diagonal(scale)
    }

    pub fn from_quat(q: Quat) -> Self {
        let (x2, y2, z2) = (q.x + q.x, q.y + q.y, q.z + q.z);
        let (xx, xy, xz) = (q.x * x2, q.x * y2, q.x * z2);
        let (yy, yz, zz) = (q.y * y2, q.y * z2, q.z * z2);
        let (wx, wy, wz) = (q.w * x2, q.w * y2, q.w * z2);
        Self::from_cols(
            Vec3::new(1.0 - (yy + zz), xy + wz, xz - wy),
            Vec3::new(xy - wz, 1.0 - (xx + zz), yz + wx),
            Vec3::new(xz + wy, yz - wx, 1.0 - (xx + yy)),
        )
    }

    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        Self::from_quat(Quat::from_axis_angle(axis, angle))
    }

    /// Upper-left 3×3 of `m`.
    #[inline]
    pub fn from_mat4(m: &Mat4) -> Self {
        Self::from_cols(
            m.x_axis.truncate(),
            m.y_axis.truncate(),
            m.z_axis.truncate(),
        )
    }

    #[inline]
    pub fn col(&self, i: usize) -> Vec3 {
        [self.x_axis, self.y_axis, self.z_axis][i]
    }

    #[inline]
    pub fn row(&self, i: usize) -> Vec3 {
        Vec3::new(self.x_axis[i], self.y_axis[i], self.z_axis[i])
    }

    #[inline]
    pub fn transpose(&self) -> Self {
        Self::from_cols(self.row(0), self.row(1), self.row(2))
    }

    #[inline]
    pub fn determinant(&self) -> f32 {
        self.x_axis.dot(self.y_axis.cross(self.z_axis))
    }

    /// Inverse; non-finite for singular matrices.
    pub fn inverse(&self) -> Self {
        // The rows of the inverse are the pairwise cross products of the columns over the
        // determinant.
        let (a, b, c) = (self.x_axis, self.y_axis, self.z_axis);
        let rows = Self::from_cols(b.cross(c), c.cross(a), a.cross(b));
        rows.transpose() * (1.0 / a.dot(b.cross(c)))
    }

    #[inline]
    pub fn mul_vec3(&self, v: Vec3) -> Vec3 {
        self.x_axis * v.x + self.y_axis * v.y + self.z_axis * v.z
    }

    #[inline]
    pub fn abs_diff_eq(&self, rhs: &Self, epsilon: f32) -> bool {
        self.x_axis.abs_diff_eq(rhs.x_axis, epsilon)
            && self.y_axis.abs_diff_eq(rhs.y_axis, epsilon)
            && self.z_axis.abs_diff_eq(rhs.z_axis, epsilon)
    }
}

impl Mul for Mat3 {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        Self::from_cols(
            self.mul_vec3(rhs.x_axis),
            self.mul_vec3(rhs.y_axis),
            self.mul_vec3(rhs.z_axis),
        )
    }
}

impl Mul<Vec3> for Mat3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, rhs: Vec3) -> Vec3 {
        self.mul_vec3(rhs)
    }
}

impl Mul<f32> for Mat3 {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self {
        Self::from_cols(self.x_axis * rhs, self.y_axis * rhs, self.z_axis * rhs)
    }
}

// --- Mat4 ---

/// 4×4 matrix stored as four SIMD column vectors (64 bytes, 16-byte aligned).
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
pub struct Mat4 {
    pub x_axis: Vec4,
    pub y_axis: Vec4,
    pub z_axis: Vec4,
    pub w_axis: Vec4,
}

impl Default for Mat4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat4 {
    pub const IDENTITY: Self = Self::from_cols(Vec4::X, Vec4::Y, Vec4::Z, Vec4::W);
    pub const ZERO: Self = Self::from_cols(Vec4::ZERO, Vec4::ZERO, Vec4::ZERO, Vec4::ZERO);

    #[inline]
    pub const fn from_cols(x_axis: Vec4, y_axis: Vec4, z_axis: Vec4, w_axis: Vec4) -> Self {
        Self {
            x_axis,
            y_axis,
            z_axis,
            w_axis,
        }
    }

    /// From sixteen floats in column-major order.
    #[inline]
    pub const fn from_cols_array(m: &[f32; 16]) -> Self {
        Self::from_cols(
            Vec4::new(m[0], m[1], m[2], m[3]),
            Vec4::new(m[4], m[5], m[6], m[7]),
            Vec4::new(m[8], m[9], m[10], m[11]),
            Vec4::new(m[12], m[13], m[14], m[15]),
        )
    }

    #[inline]
    pub const fn to_cols_array(&self) -> [f32; 16] {
        let (x, y, z, w) = (
            self.x_axis.to_array(),
            self.y_axis.to_array(),
            self.z_axis.to_array(),
            self.w_axis.to_array(),
        );
        [
            x[0], x[1], x[2], x[3], y[0], y[1], y[2], y[3], z[0], z[1], z[2], z[3], w[0], w[1],
            w[2], w[3],
        ]
    }

    #[inline]
    pub fn from_mat3(m: &Mat3) -> Self {
        Self::from_cols(
            m.x_axis.extend(0.0),
            m.y_axis.extend(0.0),
            m.z_axis.extend(0.0),
            Vec4::W,
        )
    }

    #[inline]
    pub fn from_translation(t: Vec3) -> Self {
        Self::from_cols(Vec4::X, Vec4::Y, Vec4::Z, t.extend(1.0))
    }

    #[inline]
    pub fn from_scale(s: Vec3) -> Self {
        Self::from_mat3(&Mat3::from_scale(s))
    }

    #[inline]
    pub fn from_quat(q: Quat) -> Self {
        Self::from_mat3(&Mat3::from_quat(q))
    }

    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        Self::from_quat(Quat::from_axis_angle(axis, angle))
    }

    #[inline]
    pub fn from_rotation_translation(rotation: Quat, translation: Vec3) -> Self {
        let mut m = Self::from_quat(rotation);
        m.w_axis = translation.extend(1.0);
        m
    }

    /// `T * R * S`: scales first, then rotates, then translates.
    pub fn from_scale_rotation_translation(scale: Vec3, rotation: Quat, translation: Vec3) -> Self {
        let r = Mat3::from_quat(rotation);
        Self::from_cols(
            (r.x_axis * scale.x).extend(0.0),
            (r.y_axis * scale.y).extend(0.0),
            (r.z_axis * scale.z).extend(0.0),
            translation.extend(1.0),
        )
    }

    /// Splits an affine `T * R * S` matrix back apart. A negative determinant (mirroring) is put
    /// in the X scale.
    pub fn to_scale_rotation_translation(&self) -> (Vec3, Quat, Vec3) {
        let sign = if self.determinant() < 0.0 { -1.0 } else { 1.0 };
        let (x, y, z) = (
            self.x_axis.truncate(),
            self.y_axis.truncate(),
            self.z_axis.truncate(),
        );
        let scale = Vec3::new(x.length() * sign, y.length(), z.length());
        let rotation = Quat::from_mat3(&Mat3::from_cols(x / scale.x, y / scale.y, z / scale.z));
        (scale, rotation.normalize(), self.w_axis.truncate())
    }

    /// View matrix for a camera at `eye` looking along `dir`, with `up` as the approximate up
    /// direction. The camera looks down -Z in view space.
    pub fn look_to_rh(eye: Vec3, dir: Vec3, up: Vec3) -> Self {
        let f = dir.normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);
        Self::from_cols(
            Vec4::new(s.x, u.x, -f.x, 0.0),
            Vec4::new(s.y, u.y, -f.y, 0.0),
            Vec4::new(s.z, u.z, -f.z, 0.0),
            Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
        )
    }

    #[inline]
    pub fn look_at_rh(eye: Vec3, target: Vec3, up: Vec3) -> Self {
        Self::look_to_rh(eye, target - eye, up)
    }

    /// Reverse-Z perspective projection: view-space depth `-near` maps to 1, `-far` to 0.
    /// `fov_y` is the full vertical field of view in radians; `aspect` is width / height.
    pub fn perspective_rh(fov_y: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / scalar::tan(0.5 * fov_y);
        let range = near / (far - near);
        Self::from_cols(
            Vec4::new(f / aspect, 0.0, 0.0, 0.0),
            Vec4::new(0.0, f, 0.0, 0.0),
            Vec4::new(0.0, 0.0, range, -1.0),
            Vec4::new(0.0, 0.0, far * range, 0.0),
        )
    }

    /// Reverse-Z perspective with the far plane at infinity: depth is `near / -z`. The best
    /// precision for large worlds.
    pub fn perspective_infinite_rh(fov_y: f32, aspect: f32, near: f32) -> Self {
        let f = 1.0 / scalar::tan(0.5 * fov_y);
        Self::from_cols(
            Vec4::new(f / aspect, 0.0, 0.0, 0.0),
            Vec4::new(0.0, f, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 0.0, -1.0),
            Vec4::new(0.0, 0.0, near, 0.0),
        )
    }

    /// Reverse-Z orthographic projection: depth `-near` maps to 1, `-far` to 0.
    pub fn orthographic_rh(
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let rw = 1.0 / (right - left);
        let rh = 1.0 / (top - bottom);
        let rd = 1.0 / (far - near);
        Self::from_cols(
            Vec4::new(2.0 * rw, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 2.0 * rh, 0.0, 0.0),
            Vec4::new(0.0, 0.0, rd, 0.0),
            Vec4::new(-(left + right) * rw, -(top + bottom) * rh, far * rd, 1.0),
        )
    }

    #[inline]
    pub fn col(&self, i: usize) -> Vec4 {
        [self.x_axis, self.y_axis, self.z_axis, self.w_axis][i]
    }

    #[inline]
    pub fn row(&self, i: usize) -> Vec4 {
        Vec4::new(
            self.x_axis[i],
            self.y_axis[i],
            self.z_axis[i],
            self.w_axis[i],
        )
    }

    #[inline]
    pub fn transpose(&self) -> Self {
        Self::from_cols(self.row(0), self.row(1), self.row(2), self.row(3))
    }

    #[inline]
    pub fn mul_vec4(&self, v: Vec4) -> Vec4 {
        Vec4(
            self.x_axis.0 * v.splat_x()
                + self.y_axis.0 * v.splat_y()
                + self.z_axis.0 * v.splat_z()
                + self.w_axis.0 * v.splat_w(),
        )
    }

    /// Applies an affine transform to a point (w = 1), ignoring the projective row.
    #[inline]
    pub fn transform_point3(&self, p: Vec3) -> Vec3 {
        (self.x_axis * p.x + self.y_axis * p.y + self.z_axis * p.z + self.w_axis).truncate()
    }

    /// Applies a transform to a direction (w = 0): no translation.
    #[inline]
    pub fn transform_vector3(&self, v: Vec3) -> Vec3 {
        (self.x_axis * v.x + self.y_axis * v.y + self.z_axis * v.z).truncate()
    }

    /// Applies a projective transform to a point, dividing by w.
    #[inline]
    pub fn project_point3(&self, p: Vec3) -> Vec3 {
        let r = self.mul_vec4(p.extend(1.0));
        r.truncate() / r.w
    }

    /// Cofactors of the transpose and the determinant (MESA's `gluInvertMatrix`).
    fn adjugate(&self) -> ([f32; 16], f32) {
        let m = self.to_cols_array();
        let mut inv = [0.0f32; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
            + m[9] * m[7] * m[14]
            + m[13] * m[6] * m[11]
            - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
            - m[8] * m[7] * m[14]
            - m[12] * m[6] * m[11]
            + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
            + m[8] * m[7] * m[13]
            + m[12] * m[5] * m[11]
            - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
            - m[8] * m[6] * m[13]
            - m[12] * m[5] * m[10]
            + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
            - m[9] * m[3] * m[14]
            - m[13] * m[2] * m[11]
            + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
            + m[8] * m[3] * m[14]
            + m[12] * m[2] * m[11]
            - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
            - m[8] * m[3] * m[13]
            - m[12] * m[1] * m[11]
            + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
            + m[8] * m[2] * m[13]
            + m[12] * m[1] * m[10]
            - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
            + m[5] * m[3] * m[14]
            + m[13] * m[2] * m[7]
            - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
            - m[4] * m[3] * m[14]
            - m[12] * m[2] * m[7]
            + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
            + m[4] * m[3] * m[13]
            + m[12] * m[1] * m[7]
            - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
            - m[4] * m[2] * m[13]
            - m[12] * m[1] * m[6]
            + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
            - m[5] * m[3] * m[10]
            - m[9] * m[2] * m[7]
            + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
            + m[4] * m[3] * m[10]
            + m[8] * m[2] * m[7]
            - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
            - m[4] * m[3] * m[9]
            - m[8] * m[1] * m[7]
            + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
            + m[4] * m[2] * m[9]
            + m[8] * m[1] * m[6]
            - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        (inv, det)
    }

    #[inline]
    pub fn determinant(&self) -> f32 {
        self.adjugate().1
    }

    /// General inverse; non-finite for singular matrices.
    pub fn inverse(&self) -> Self {
        let (inv, det) = self.adjugate();
        let r = 1.0 / det;
        Self::from_cols_array(&inv.map(|v| v * r))
    }

    /// Inverse of an affine matrix (last row 0, 0, 0, 1); cheaper than [`inverse`](Self::inverse).
    pub fn inverse_affine(&self) -> Self {
        let inv3 = Mat3::from_mat4(self).inverse();
        let t = -(inv3 * self.w_axis.truncate());
        Self::from_cols(
            inv3.x_axis.extend(0.0),
            inv3.y_axis.extend(0.0),
            inv3.z_axis.extend(0.0),
            t.extend(1.0),
        )
    }

    #[inline]
    pub fn abs_diff_eq(&self, rhs: &Self, epsilon: f32) -> bool {
        self.x_axis.abs_diff_eq(rhs.x_axis, epsilon)
            && self.y_axis.abs_diff_eq(rhs.y_axis, epsilon)
            && self.z_axis.abs_diff_eq(rhs.z_axis, epsilon)
            && self.w_axis.abs_diff_eq(rhs.w_axis, epsilon)
    }

    #[inline]
    pub fn is_finite(&self) -> bool {
        self.x_axis.is_finite()
            && self.y_axis.is_finite()
            && self.z_axis.is_finite()
            && self.w_axis.is_finite()
    }
}

impl Mul for Mat4 {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        Self::from_cols(
            self.mul_vec4(rhs.x_axis),
            self.mul_vec4(rhs.y_axis),
            self.mul_vec4(rhs.z_axis),
            self.mul_vec4(rhs.w_axis),
        )
    }
}

impl MulAssign for Mat4 {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

impl Mul<Vec4> for Mat4 {
    type Output = Vec4;
    #[inline]
    fn mul(self, rhs: Vec4) -> Vec4 {
        self.mul_vec4(rhs)
    }
}

impl Mul<f32> for Mat4 {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: f32) -> Self {
        Self::from_cols(
            self.x_axis * rhs,
            self.y_axis * rhs,
            self.z_axis * rhs,
            self.w_axis * rhs,
        )
    }
}

impl Add for Mat4 {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self::from_cols(
            self.x_axis + rhs.x_axis,
            self.y_axis + rhs.y_axis,
            self.z_axis + rhs.z_axis,
            self.w_axis + rhs.w_axis,
        )
    }
}

impl Sub for Mat4 {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self::from_cols(
            self.x_axis - rhs.x_axis,
            self.y_axis - rhs.y_axis,
            self.z_axis - rhs.z_axis,
            self.w_axis - rhs.w_axis,
        )
    }
}
