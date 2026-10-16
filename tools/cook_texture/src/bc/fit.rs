//! Line fitting shared by the endpoint encoders.

pub(crate) type Vec4 = [f32; 4];

/// Mean of `points` over channels `lo..hi` (the rest 0).
pub(crate) fn mean(points: &[Vec4], lo: usize, hi: usize) -> Vec4 {
    let mut m = [0.0; 4];
    for p in points {
        for c in lo..hi {
            m[c] += p[c];
        }
    }
    let inv = 1.0 / points.len().max(1) as f32;
    m.map(|v| v * inv)
}

fn covariance(points: &[Vec4], mean: Vec4, lo: usize, hi: usize) -> [[f32; 4]; 4] {
    let mut cov = [[0.0f32; 4]; 4];
    for p in points {
        let d: Vec4 = core::array::from_fn(|c| p[c] - mean[c]);
        for i in lo..hi {
            for j in i..hi {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    for i in lo..hi {
        for j in lo..i {
            cov[i][j] = cov[j][i];
        }
    }
    cov
}

/// Unit direction of greatest variance over channels `lo..hi`, by power iteration; zero when
/// the points coincide.
pub(crate) fn principal_axis(points: &[Vec4], mean: Vec4, lo: usize, hi: usize) -> Vec4 {
    let cov = covariance(points, mean, lo, hi);
    axis_of(&cov, lo, hi, 8)
}

fn axis_of(cov: &[[f32; 4]; 4], lo: usize, hi: usize, iterations: u32) -> Vec4 {
    // Start from the covariance row of the widest channel: never orthogonal to the answer
    // unless the points have no spread at all.
    let widest = (lo..hi)
        .max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b]))
        .unwrap_or(lo);
    let mut v = cov[widest];
    for _ in 0..iterations {
        let len = dot(v, v, lo, hi).sqrt();
        if len < 1e-12 {
            return [0.0; 4];
        }
        let n = v.map(|x| x / len);
        v = [0.0; 4];
        for i in lo..hi {
            for j in lo..hi {
                v[i] += cov[i][j] * n[j];
            }
        }
    }
    let len = dot(v, v, lo, hi).sqrt();
    if len < 1e-12 {
        [0.0; 4]
    } else {
        v.map(|x| x / len)
    }
}

/// Each texel's first and second moments (`x0..x3`, then the products `xa * xb`, `a <= b`),
/// centered on the block mean. Summed over a subset's mask, they give its covariance without
/// touching the texels again: cheap enough to rank every partition.
pub(crate) struct Moments {
    texels: [[f32; 16]; 16],
    total: [f32; 16],
    /// The whole block's principal axis: the power iterations' start.
    axis: Vec4,
}

const PRODUCTS: [(usize, usize); 10] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 2),
    (2, 3),
    (3, 3),
];

impl Moments {
    /// Over channels `0..hi`.
    pub(crate) fn new(points: &[Vec4; 16], hi: usize) -> Self {
        let m = mean(points, 0, hi);
        let mut texels = [[0.0; 16]; 16];
        let mut total = [0.0; 16];
        for (t, p) in texels.iter_mut().zip(points) {
            let d: Vec4 = core::array::from_fn(|c| if c < hi { p[c] - m[c] } else { 0.0 });
            t[..4].copy_from_slice(&d);
            for (k, &(a, b)) in PRODUCTS.iter().enumerate() {
                t[4 + k] = d[a] * d[b];
            }
            for k in 0..16 {
                total[k] += t[k];
            }
        }
        let mut moments = Self {
            texels,
            total,
            axis: [0.0; 4],
        };
        let cov = moments.covariance(&total, 16);
        moments.axis = axis_of(&cov, 0, 4, 8);
        moments
    }

    fn sum(&self, mask: u16) -> [f32; 16] {
        let mut s = [0.0; 16];
        let mut bits = mask;
        while bits != 0 {
            let t = &self.texels[bits.trailing_zeros() as usize];
            bits &= bits - 1;
            for k in 0..16 {
                s[k] += t[k];
            }
        }
        s
    }

    fn covariance(&self, s: &[f32; 16], n: u32) -> [[f32; 4]; 4] {
        let inv = 1.0 / n as f32;
        let mut cov = [[0.0; 4]; 4];
        for (k, &(a, b)) in PRODUCTS.iter().enumerate() {
            cov[a][b] = s[4 + k] - s[a] * s[b] * inv;
            cov[b][a] = cov[a][b];
        }
        cov
    }

    /// Squared distance of a subset's texels from their best-fit line.
    fn residual(&self, s: &[f32; 16], n: u32) -> f32 {
        if n < 2 {
            return 0.0;
        }
        let cov = self.covariance(s, n);
        let apply =
            |v: Vec4| -> Vec4 { core::array::from_fn(|i| (0..4).map(|j| cov[i][j] * v[j]).sum()) };
        let trace = cov[0][0] + cov[1][1] + cov[2][2] + cov[3][3];
        if trace < 1e-9 {
            return 0.0;
        }
        // Two power steps from the block's axis, where subsets mostly lie; a subset spread
        // across it needs the full iteration.
        let mut v = self.axis;
        for _ in 0..2 {
            v = apply(v);
            let len = dot(v, v, 0, 4).sqrt();
            if len < 1e-3 * trace {
                v = axis_of(&cov, 0, 4, 8);
                break;
            }
            v = v.map(|x| x / len);
        }
        (trace - dot(v, apply(v), 0, 4)).max(0.0)
    }

    /// Sum of the line residuals of a partition's subsets (`masks[..subsets]`).
    pub(crate) fn partition_residual(&self, masks: &[u16; 3], subsets: usize) -> f32 {
        let mut rest = self.total;
        let mut rest_n = 16;
        let mut residual = 0.0;
        for &mask in &masks[..subsets - 1] {
            let s = self.sum(mask);
            for k in 0..16 {
                rest[k] -= s[k];
            }
            rest_n -= mask.count_ones();
            residual += self.residual(&s, mask.count_ones());
        }
        residual + self.residual(&rest, rest_n)
    }
}

pub(crate) fn dot(a: Vec4, b: Vec4, lo: usize, hi: usize) -> f32 {
    (lo..hi).map(|c| a[c] * b[c]).sum()
}

/// Extremes of `points` projected on their principal axis: starting endpoints.
pub(crate) fn axis_endpoints(points: &[Vec4], lo: usize, hi: usize) -> (Vec4, Vec4) {
    let m = mean(points, lo, hi);
    let axis = principal_axis(points, m, lo, hi);
    let (mut tmin, mut tmax) = (f32::MAX, f32::MIN);
    for p in points {
        let d: Vec4 = core::array::from_fn(|c| p[c] - m[c]);
        let t = dot(d, axis, lo, hi);
        tmin = tmin.min(t);
        tmax = tmax.max(t);
    }
    if tmin > tmax {
        return (m, m);
    }
    let at = |t: f32| core::array::from_fn(|c| m[c] + axis[c] * t);
    (at(tmin), at(tmax))
}

/// Endpoints `(a, b)` minimizing `Σ |α a + (1 - α) b - p|²`, given each point's weight `α`
/// on `a`. `None` when the system is singular (every point at the same `α`).
pub(crate) fn least_squares(
    points: &[Vec4],
    alphas: &[f32],
    lo: usize,
    hi: usize,
) -> Option<(Vec4, Vec4)> {
    let (mut aa, mut ab, mut bb) = (0.0f32, 0.0f32, 0.0f32);
    let mut ax = [0.0f32; 4];
    let mut bx = [0.0f32; 4];
    for (p, &a) in points.iter().zip(alphas) {
        let b = 1.0 - a;
        aa += a * a;
        ab += a * b;
        bb += b * b;
        for c in lo..hi {
            ax[c] += a * p[c];
            bx[c] += b * p[c];
        }
    }
    let det = aa * bb - ab * ab;
    if det.abs() < 1e-5 {
        return None;
    }
    let inv = 1.0 / det;
    let mut e0 = [0.0; 4];
    let mut e1 = [0.0; 4];
    for c in lo..hi {
        e0[c] = (ax[c] * bb - bx[c] * ab) * inv;
        e1[c] = (bx[c] * aa - ax[c] * ab) * inv;
    }
    Some((e0, e1))
}
