//! Bounding volumes, planes, rays and view frusta, with the intersection tests culling and
//! picking need.

use crate::{Mat4, Vec3, Vec4, scalar};

/// Axis-aligned bounding box. [`Aabb::EMPTY`] is inverted so growing it by any point works.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Self = Self {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };

    #[inline]
    pub const fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    #[inline]
    pub fn from_center_half_extents(center: Vec3, half_extents: Vec3) -> Self {
        Self::new(center - half_extents, center + half_extents)
    }

    pub fn from_points(points: impl IntoIterator<Item = Vec3>) -> Self {
        points.into_iter().fold(Self::EMPTY, Self::grow)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    #[inline]
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    #[inline]
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    #[inline]
    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    #[inline]
    pub fn grow(self, p: Vec3) -> Self {
        Self::new(self.min.min(p), self.max.max(p))
    }

    #[inline]
    pub fn union(self, rhs: Self) -> Self {
        Self::new(self.min.min(rhs.min), self.max.max(rhs.max))
    }

    #[inline]
    pub fn contains_point(&self, p: Vec3) -> bool {
        p.x >= self.min.x
            && p.y >= self.min.y
            && p.z >= self.min.z
            && p.x <= self.max.x
            && p.y <= self.max.y
            && p.z <= self.max.z
    }

    #[inline]
    pub fn intersects(&self, rhs: &Self) -> bool {
        self.min.x <= rhs.max.x
            && self.max.x >= rhs.min.x
            && self.min.y <= rhs.max.y
            && self.max.y >= rhs.min.y
            && self.min.z <= rhs.max.z
            && self.max.z >= rhs.min.z
    }

    /// Smallest box around this one after an affine transform (Arvo's method).
    pub fn transformed(&self, m: &Mat4) -> Self {
        let center = m.transform_point3(self.center());
        let e = self.half_extents();
        let half = m.x_axis.truncate().abs() * e.x
            + m.y_axis.truncate().abs() * e.y
            + m.z_axis.truncate().abs() * e.z;
        Self::from_center_half_extents(center, half)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Sphere {
    pub center: Vec3,
    pub radius: f32,
}

impl Sphere {
    #[inline]
    pub const fn new(center: Vec3, radius: f32) -> Self {
        Self { center, radius }
    }

    #[inline]
    pub fn contains_point(&self, p: Vec3) -> bool {
        self.center.distance_squared(p) <= self.radius * self.radius
    }

    #[inline]
    pub fn intersects(&self, rhs: &Self) -> bool {
        let r = self.radius + rhs.radius;
        self.center.distance_squared(rhs.center) <= r * r
    }
}

/// The plane `normal · p + d = 0`; points with positive distance are in front.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Plane {
    pub normal: Vec3,
    pub d: f32,
}

impl Plane {
    #[inline]
    pub const fn new(normal: Vec3, d: f32) -> Self {
        Self { normal, d }
    }

    /// Through `point`, facing unit `normal`.
    #[inline]
    pub fn from_point_normal(point: Vec3, normal: Vec3) -> Self {
        Self::new(normal, -normal.dot(point))
    }

    /// Normalizes `(a, b, c, d)` coefficients. A zero normal (e.g. the far plane of an infinite
    /// projection) yields a plane every point is in front of.
    pub fn from_coefficients(v: Vec4) -> Self {
        let normal = v.truncate();
        let len = normal.length();
        if len < 1e-12 {
            Self::new(Vec3::ZERO, 1.0)
        } else {
            Self::new(normal / len, v.w / len)
        }
    }

    #[inline]
    pub fn signed_distance(&self, p: Vec3) -> f32 {
        self.normal.dot(p) + self.d
    }
}

/// A half-line from `origin` along `direction` (not necessarily unit; `t` is in its units).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Ray {
    pub origin: Vec3,
    pub direction: Vec3,
}

impl Ray {
    #[inline]
    pub const fn new(origin: Vec3, direction: Vec3) -> Self {
        Self { origin, direction }
    }

    #[inline]
    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.direction * t
    }

    /// Entry and exit `t` against a box (slab method). The entry is negative when the origin is
    /// inside; `None` if the box is missed or entirely behind.
    pub fn intersect_aabb(&self, aabb: &Aabb) -> Option<(f32, f32)> {
        let inv = self.direction.recip();
        let t0 = (aabb.min - self.origin) * inv;
        let t1 = (aabb.max - self.origin) * inv;
        let enter = t0.min(t1).max_element();
        let exit = t0.max(t1).min_element();
        (enter <= exit && exit >= 0.0).then_some((enter, exit))
    }

    /// Nearest non-negative `t` hitting the plane, from either side.
    pub fn intersect_plane(&self, plane: &Plane) -> Option<f32> {
        let denom = plane.normal.dot(self.direction);
        if denom.abs() < 1e-12 {
            return None;
        }
        let t = -plane.signed_distance(self.origin) / denom;
        (t >= 0.0).then_some(t)
    }

    /// Nearest non-negative `t` hitting the sphere (0 if the origin is inside).
    pub fn intersect_sphere(&self, sphere: &Sphere) -> Option<f32> {
        let oc = self.origin - sphere.center;
        let a = self.direction.length_squared();
        let b = oc.dot(self.direction);
        let c = oc.length_squared() - sphere.radius * sphere.radius;
        if c <= 0.0 {
            return Some(0.0);
        }
        let disc = b * b - a * c;
        if disc < 0.0 || b > 0.0 {
            return None;
        }
        Some((-b - scalar::sqrt(disc)) / a)
    }
}

/// Six inward-facing planes: left, right, bottom, top, near, far.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frustum {
    pub planes: [Plane; 6],
}

impl Frustum {
    /// Extracts the planes of a reverse-Z, [0, 1]-depth view-projection matrix (Gribb/Hartmann).
    /// Works for infinite projections, whose far plane accepts everything.
    pub fn from_view_projection(m: &Mat4) -> Self {
        let (r0, r1, r2, r3) = (m.row(0), m.row(1), m.row(2), m.row(3));
        Self {
            planes: [
                Plane::from_coefficients(r3 + r0),
                Plane::from_coefficients(r3 - r0),
                Plane::from_coefficients(r3 + r1),
                Plane::from_coefficients(r3 - r1),
                // Reverse-Z: near is depth <= 1 (w - z >= 0), far is depth >= 0 (z >= 0).
                Plane::from_coefficients(r3 - r2),
                Plane::from_coefficients(r2),
            ],
        }
    }

    #[inline]
    pub fn contains_point(&self, p: Vec3) -> bool {
        self.planes.iter().all(|pl| pl.signed_distance(p) >= 0.0)
    }

    #[inline]
    pub fn intersects_sphere(&self, sphere: &Sphere) -> bool {
        self.planes
            .iter()
            .all(|pl| pl.signed_distance(sphere.center) >= -sphere.radius)
    }

    /// Conservative: may accept boxes just outside a frustum corner, never rejects visible ones.
    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool {
        self.planes.iter().all(|pl| {
            // The corner farthest along the plane normal.
            let p = Vec3::new(
                if pl.normal.x >= 0.0 {
                    aabb.max.x
                } else {
                    aabb.min.x
                },
                if pl.normal.y >= 0.0 {
                    aabb.max.y
                } else {
                    aabb.min.y
                },
                if pl.normal.z >= 0.0 {
                    aabb.max.z
                } else {
                    aabb.min.z
                },
            );
            pl.signed_distance(p) >= 0.0
        })
    }
}
