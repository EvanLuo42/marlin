//! Translation-rotation-scale transforms, as stored by the scene hierarchy.

use core::ops::Mul;

use crate::{Mat4, Quat, Vec3};

/// Scale, then rotate, then translate.
///
/// Composing two transforms is exact when the parent's scale is uniform. With non-uniform
/// parent scale and rotation the true result has shear, which TRS cannot represent; like most
/// engines, composition then approximates it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    #[inline]
    pub const fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            ..Self::IDENTITY
        }
    }

    #[inline]
    pub const fn from_rotation(rotation: Quat) -> Self {
        Self {
            rotation,
            ..Self::IDENTITY
        }
    }

    #[inline]
    pub const fn from_scale(scale: Vec3) -> Self {
        Self {
            scale,
            ..Self::IDENTITY
        }
    }

    /// Decomposes an affine matrix; see [`Mat4::to_scale_rotation_translation`].
    #[inline]
    pub fn from_matrix(m: &Mat4) -> Self {
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        Self {
            translation,
            rotation,
            scale,
        }
    }

    #[inline]
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// This transform turned so [`Vec3::FORWARD`] points at `target`.
    #[inline]
    pub fn looking_at(self, target: Vec3, up: Vec3) -> Self {
        Self {
            rotation: Quat::look_to(target - self.translation, up),
            ..self
        }
    }

    #[inline]
    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::FORWARD
    }

    #[inline]
    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::RIGHT
    }

    #[inline]
    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::UP
    }

    #[inline]
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        self.rotation * (p * self.scale) + self.translation
    }

    #[inline]
    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        self.rotation * (v * self.scale)
    }

    /// Exact for uniform scale; see the type docs otherwise.
    pub fn inverse(&self) -> Self {
        let rotation = self.rotation.conjugate();
        let scale = self.scale.recip();
        Self {
            translation: rotation * (-self.translation) * scale,
            rotation,
            scale,
        }
    }
}

impl Mul for Transform {
    type Output = Self;

    /// `parent * child`: the child's transform expressed in the parent's space.
    #[inline]
    fn mul(self, child: Self) -> Self {
        Self {
            translation: self.transform_point(child.translation),
            rotation: self.rotation * child.rotation,
            scale: self.scale * child.scale,
        }
    }
}

impl Mul<Vec3> for Transform {
    type Output = Vec3;
    #[inline]
    fn mul(self, p: Vec3) -> Vec3 {
        self.transform_point(p)
    }
}
