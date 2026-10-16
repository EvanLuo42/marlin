//! Small column-major matrix and quaternion helpers for import.

pub type Mat4 = [f32; 16];

pub const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

pub fn from_cols(m: [[f32; 4]; 4]) -> Mat4 {
    std::array::from_fn(|i| m[i / 4][i % 4])
}

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    std::array::from_fn(|i| {
        let (col, row) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum()
    })
}

pub fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

pub fn transform_vector(m: &Mat4, v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[r] * v[0] + m[4 + r] * v[1] + m[8 + r] * v[2])
}

/// Determinant of the upper 3×3.
pub fn det3(m: &Mat4) -> f32 {
    let c = |col: usize, row: usize| m[col * 4 + row];
    c(0, 0) * (c(1, 1) * c(2, 2) - c(2, 1) * c(1, 2))
        - c(1, 0) * (c(0, 1) * c(2, 2) - c(2, 1) * c(0, 2))
        + c(2, 0) * (c(0, 1) * c(1, 2) - c(1, 1) * c(0, 2))
}

/// The inverse transpose of the upper 3×3 (for normals), as a Mat4 without translation.
pub fn normal_matrix(m: &Mat4) -> Mat4 {
    let c = |col: usize, row: usize| m[col * 4 + row];
    let det = det3(m);
    let inv = if det.abs() > 1e-30 { 1.0 / det } else { 0.0 };
    // Cofactor matrix / det = inverse transpose.
    let cof = |r: usize, k: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (k1, k2) = ((k + 1) % 3, (k + 2) % 3);
        c(k1, r1) * c(k2, r2) - c(k2, r1) * c(k1, r2)
    };
    let mut out = IDENTITY;
    for col in 0..3 {
        for row in 0..3 {
            out[col * 4 + row] = cof(row, col) * inv;
        }
    }
    out
}

pub fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 0.0 { v.map(|c| c / l) } else { v }
}

pub fn quat_to_mat3(q: [f32; 4]) -> [[f32; 3]; 3] {
    let [x, y, z, w] = q;
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * w),
            2.0 * (x * z - y * w),
        ],
        [
            2.0 * (x * y - z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + x * w),
        ],
        [
            2.0 * (x * z + y * w),
            2.0 * (y * z - x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

/// T · R · S.
pub fn compose(t: [f32; 3], r: [f32; 4], s: [f32; 3]) -> Mat4 {
    let m = quat_to_mat3(r);
    let mut out = IDENTITY;
    for col in 0..3 {
        for row in 0..3 {
            out[col * 4 + row] = m[col][row] * s[col];
        }
    }
    out[12] = t[0];
    out[13] = t[1];
    out[14] = t[2];
    out
}

/// Translation, rotation (x, y, z, w) and scale of a matrix without shear. A mirror goes into
/// the scale's X.
pub fn decompose(m: &Mat4) -> ([f32; 3], [f32; 4], [f32; 3]) {
    let t = [m[12], m[13], m[14]];
    let col = |c: usize| [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]];
    let len = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let mut s = [len(col(0)), len(col(1)), len(col(2))];
    if det3(m) < 0.0 {
        s[0] = -s[0];
    }
    let r: [[f32; 3]; 3] = std::array::from_fn(|c| {
        let v = col(c);
        if s[c] != 0.0 {
            v.map(|x| x / s[c])
        } else {
            [0.0; 3]
        }
    });
    (t, mat3_to_quat(r), s)
}

/// Rotation matrix (columns) to a unit quaternion (Shepperd's method).
pub fn mat3_to_quat(m: [[f32; 3]; 3]) -> [f32; 4] {
    let e = |row: usize, col: usize| m[col][row];
    let trace = e(0, 0) + e(1, 1) + e(2, 2);
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            (e(2, 1) - e(1, 2)) / s,
            (e(0, 2) - e(2, 0)) / s,
            (e(1, 0) - e(0, 1)) / s,
            0.25 * s,
        ]
    } else if e(0, 0) > e(1, 1) && e(0, 0) > e(2, 2) {
        let s = (1.0 + e(0, 0) - e(1, 1) - e(2, 2)).sqrt() * 2.0;
        [
            0.25 * s,
            (e(0, 1) + e(1, 0)) / s,
            (e(0, 2) + e(2, 0)) / s,
            (e(2, 1) - e(1, 2)) / s,
        ]
    } else if e(1, 1) > e(2, 2) {
        let s = (1.0 + e(1, 1) - e(0, 0) - e(2, 2)).sqrt() * 2.0;
        [
            (e(0, 1) + e(1, 0)) / s,
            0.25 * s,
            (e(1, 2) + e(2, 1)) / s,
            (e(0, 2) - e(2, 0)) / s,
        ]
    } else {
        let s = (1.0 + e(2, 2) - e(0, 0) - e(1, 1)).sqrt() * 2.0;
        [
            (e(0, 2) + e(2, 0)) / s,
            (e(1, 2) + e(2, 1)) / s,
            0.25 * s,
            (e(1, 0) - e(0, 1)) / s,
        ]
    };
    quat_normalize(q)
}

pub fn quat_normalize(q: [f32; 4]) -> [f32; 4] {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if l > 0.0 {
        q.map(|c| c / l)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// Spherical interpolation along the shorter arc.
pub fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut d: f32 = (0..4).map(|i| a[i] * b[i]).sum();
    let b = if d < 0.0 {
        d = -d;
        b.map(|c| -c)
    } else {
        b
    };
    if d > 0.9995 {
        return quat_normalize(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t));
    }
    let theta = d.clamp(-1.0, 1.0).acos();
    let s = theta.sin();
    let (wa, wb) = (((1.0 - t) * theta).sin() / s, (t * theta).sin() / s);
    std::array::from_fn(|i| a[i] * wa + b[i] * wb)
}
