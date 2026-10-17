//! `Reflect` for the engine's L2 data types: `marlin_math`, `marlin_text` and
//! `marlin_containers`.
//!
//! - Math types are structs with named fields (`x`, `y`, ..., `x_axis`, ..., `translation`, ...).
//!   `Vec4` and `Quat` hold a SIMD `f32x4`, which has the layout of `[f32; 4]`, so their `x`/`y`/
//!   `z`/`w` fields are real `f32`s at offsets 0, 4, 8 and 12, exactly what their `Deref` to
//!   `Xyzw` exposes.
//! - `InlineString<N>` is a string; setting one longer than `N` bytes truncates it at a char
//!   boundary. `Name` is opaque with a [`ReflectProxy`] to `String` (a name with a number suffix
//!   has no contiguous `&str` to lend); `Handle<T>` is opaque with a proxy to its `u64` bits.
//! - `SmallVec` and `ArrayVec` are lists (an `ArrayVec` refuses to grow past `N`); `HashMap` and
//!   `HashSet` are maps and sets. `SlotMap` is not reflected: no kind can recreate its handles.

use alloc::string::{String, ToString};
use core::hash::Hash;
use core::mem::offset_of;

use marlin_containers::{ArrayVec, Handle, HashMap, HashMapExt, HashSet, HashSetExt, SmallVec};
use marlin_math::{
    Aabb, Frustum, IVec2, IVec3, Mat3, Mat4, Plane, Quat, Ray, Sphere, Transform, UVec2, Vec2,
    Vec3, Vec4, Xyzw,
};
use marlin_text::{InlineString, Name};

use crate::data::{Proxied, ReflectProxy};
use crate::id::{ConstArg, TypeName};
use crate::info::{
    FieldInfo, ListInfo, MapInfo, SetInfo, StringInfo, StructInfo, StructStyle, TypeInfo, TypeKind,
};
use crate::traits::{
    Reflect, ReflectList, ReflectMap, ReflectSet, ReflectString, grow_with_defaults, type_info,
};

/// `Reflect` for a `Copy` struct of public fields, described field by field. `via Xyzw` takes the
/// offsets from `Xyzw`, for the SIMD-backed types.
macro_rules! plain_struct {
    ($ty:ident = $name:literal, default $default:expr, { $($field:ident: $fty:ty),* $(,)? }) => {
        plain_struct!(@impl $ty, $name, $default, $ty, { $($field: $fty),* });
    };
    ($ty:ident = $name:literal via Xyzw, default $default:expr) => {
        plain_struct!(@impl $ty, $name, $default, Xyzw, { x: f32, y: f32, z: f32, w: f32 });
    };
    (@impl $ty:ident, $name:literal, $default:expr, $layout:ident, { $($field:ident: $fty:ty),* }) => {
        // SAFETY: every field is at its `offset_of!` offset with its declared type (for
        // `via Xyzw`, the `f32x4` inside has the layout of `Xyzw`, as its `Deref` relies on).
        unsafe impl Reflect for $ty {
            const TYPE_NAME: TypeName = TypeName::simple($name);
            const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Struct(StructInfo {
                style: StructStyle::Named,
                fields: &[$(FieldInfo::new(
                    stringify!($field),
                    offset_of!($layout, $field),
                    type_info::<$fty>,
                )),*],
            }));

            fn reflect_default() -> Option<Self> {
                Some($default)
            }

            fn reflect_clone(&self) -> Option<Self> {
                Some(*self)
            }
        }
    };
}

plain_struct!(Vec2 = "marlin_math::Vec2", default Vec2::ZERO, { x: f32, y: f32 });
plain_struct!(Vec3 = "marlin_math::Vec3", default Vec3::ZERO, { x: f32, y: f32, z: f32 });
plain_struct!(Vec4 = "marlin_math::Vec4" via Xyzw, default Vec4::ZERO);
plain_struct!(Quat = "marlin_math::Quat" via Xyzw, default Quat::IDENTITY);
plain_struct!(IVec2 = "marlin_math::IVec2", default IVec2::ZERO, { x: i32, y: i32 });
plain_struct!(IVec3 = "marlin_math::IVec3", default IVec3::ZERO, { x: i32, y: i32, z: i32 });
plain_struct!(UVec2 = "marlin_math::UVec2", default UVec2::ZERO, { x: u32, y: u32 });
plain_struct!(Mat3 = "marlin_math::Mat3", default Mat3::IDENTITY, {
    x_axis: Vec3,
    y_axis: Vec3,
    z_axis: Vec3,
});
plain_struct!(Mat4 = "marlin_math::Mat4", default Mat4::IDENTITY, {
    x_axis: Vec4,
    y_axis: Vec4,
    z_axis: Vec4,
    w_axis: Vec4,
});
plain_struct!(Transform = "marlin_math::Transform", default Transform::IDENTITY, {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
});
plain_struct!(Aabb = "marlin_math::Aabb", default Aabb::EMPTY, { min: Vec3, max: Vec3 });
plain_struct!(Sphere = "marlin_math::Sphere", default Sphere::new(Vec3::ZERO, 0.0), {
    center: Vec3,
    radius: f32,
});
plain_struct!(Plane = "marlin_math::Plane", default Plane::new(Vec3::Y, 0.0), {
    normal: Vec3,
    d: f32,
});
plain_struct!(Ray = "marlin_math::Ray", default Ray::new(Vec3::ZERO, Vec3::FORWARD), {
    origin: Vec3,
    direction: Vec3,
});
plain_struct!(Frustum = "marlin_math::Frustum",
    default Frustum { planes: [Plane::new(Vec3::Y, 0.0); 6] },
    { planes: [Plane; 6] }
);

// SAFETY: string operations built from `Self`.
unsafe impl<const N: usize> Reflect for InlineString<N> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "marlin_text::InlineString",
        args: &[&TypeName::Const(ConstArg::Uint(N as u128))],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::String(StringInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(Self::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(*self)
    }
}

impl<const N: usize> ReflectString for InlineString<N> {
    fn as_str(&self) -> &str {
        InlineString::as_str(self)
    }

    /// Truncates at a char boundary when `value` is longer than `N` bytes.
    fn set_str(&mut self, value: &str) {
        self.clear();
        self.push_str_truncating(value);
    }
}

// SAFETY: an opaque description of `Name` itself.
unsafe impl Reflect for Name {
    const TYPE_NAME: TypeName = TypeName::simple("marlin_text::Name");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectProxy::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Name::default())
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(*self)
    }
}

/// A name converts to its full text, number suffix included, and back by interning.
impl Proxied for Name {
    type Proxy = String;

    fn to_proxy(&self) -> String {
        self.to_string()
    }

    fn set_from_proxy(&mut self, proxy: &String) -> bool {
        *self = Name::new(proxy);
        true
    }
}

// SAFETY: an opaque description of `Handle<T>` itself.
unsafe impl<T: Reflect> Reflect for Handle<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "marlin_containers::Handle",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectProxy::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Handle::NULL)
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(*self)
    }
}

/// A handle converts to its bits ([`Handle::to_bits`]). Handles only mean something next to the
/// slot map that issued them.
impl<T: Reflect> Proxied for Handle<T> {
    type Proxy = u64;

    fn to_proxy(&self) -> u64 {
        self.to_bits()
    }

    fn set_from_proxy(&mut self, proxy: &u64) -> bool {
        *self = Handle::from_bits(*proxy);
        true
    }
}

// SAFETY: list operations built from `Self`.
unsafe impl<T: Reflect, const N: usize> Reflect for SmallVec<T, N> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "marlin_containers::SmallVec",
        args: &[&T::TYPE_NAME, &TypeName::Const(ConstArg::Uint(N as u128))],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::List(ListInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(SmallVec::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        self.iter().map(T::reflect_clone).collect()
    }
}

impl<T: Reflect, const N: usize> ReflectList for SmallVec<T, N> {
    type Item = T;

    fn len(&self) -> usize {
        SmallVec::len(self)
    }

    fn get(&self, index: usize) -> Option<&T> {
        self.as_slice().get(index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.as_mut_slice().get_mut(index)
    }

    fn resize(&mut self, len: usize) -> bool {
        let old = SmallVec::len(self);
        if len <= old {
            self.truncate(len);
            return true;
        }
        self.reserve(len - old);
        let ok = grow_with_defaults(len - old, |item| {
            self.push(item);
            true
        });
        if !ok {
            self.truncate(old);
        }
        ok
    }

    fn insert_default(&mut self, index: usize) -> bool {
        if index > SmallVec::len(self) {
            return false;
        }
        match T::reflect_default() {
            Some(item) => {
                self.insert(index, item);
                true
            }
            None => false,
        }
    }

    fn remove(&mut self, index: usize) -> bool {
        if index >= SmallVec::len(self) {
            return false;
        }
        SmallVec::remove(self, index);
        true
    }
}

// SAFETY: list operations built from `Self`.
unsafe impl<T: Reflect, const N: usize> Reflect for ArrayVec<T, N> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "marlin_containers::ArrayVec",
        args: &[&T::TYPE_NAME, &TypeName::Const(ConstArg::Uint(N as u128))],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::List(ListInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(ArrayVec::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        let mut out = ArrayVec::new();
        for item in self.as_slice() {
            out.try_push(item.reflect_clone()?).ok()?;
        }
        Some(out)
    }
}

/// Never grows past `N`: `resize` and `insert_default` return false instead.
impl<T: Reflect, const N: usize> ReflectList for ArrayVec<T, N> {
    type Item = T;

    fn len(&self) -> usize {
        ArrayVec::len(self)
    }

    fn get(&self, index: usize) -> Option<&T> {
        self.as_slice().get(index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.as_mut_slice().get_mut(index)
    }

    fn resize(&mut self, len: usize) -> bool {
        let old = ArrayVec::len(self);
        if len <= old {
            self.truncate(len);
            return true;
        }
        if len > N {
            return false;
        }
        let ok = grow_with_defaults(len - old, |item| self.try_push(item).is_ok());
        if !ok {
            self.truncate(old);
        }
        ok
    }

    fn insert_default(&mut self, index: usize) -> bool {
        if index > ArrayVec::len(self) || self.is_full() {
            return false;
        }
        match T::reflect_default() {
            Some(item) => self.try_insert(index, item).is_ok(),
            None => false,
        }
    }

    fn remove(&mut self, index: usize) -> bool {
        if index >= ArrayVec::len(self) {
            return false;
        }
        ArrayVec::remove(self, index);
        true
    }
}

// SAFETY: map operations built from `Self`.
unsafe impl<K: Reflect + Eq + Hash, V: Reflect> Reflect for HashMap<K, V> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "HashMap",
        args: &[&K::TYPE_NAME, &V::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Map(MapInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(<Self as HashMapExt>::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        let mut out = <Self as HashMapExt>::with_capacity(self.len());
        for (k, v) in self {
            out.insert(k.reflect_clone()?, v.reflect_clone()?);
        }
        Some(out)
    }
}

impl<K: Reflect + Eq + Hash, V: Reflect> ReflectMap for HashMap<K, V> {
    type Key = K;
    type Value = V;
    const ORDERED: bool = false;

    fn len(&self) -> usize {
        HashMap::len(self)
    }

    fn get(&self, key: &K) -> Option<&V> {
        HashMap::get(self, key)
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        HashMap::get_mut(self, key)
    }

    fn insert(&mut self, key: K, value: V) {
        HashMap::insert(self, key, value);
    }

    fn remove(&mut self, key: &K) -> bool {
        HashMap::remove(self, key).is_some()
    }

    fn clear(&mut self) {
        HashMap::clear(self);
    }

    fn for_each(&self, f: &mut dyn FnMut(&K, &V)) {
        self.iter().for_each(|(k, v)| f(k, v));
    }

    fn for_each_mut(&mut self, f: &mut dyn FnMut(&K, &mut V)) {
        self.iter_mut().for_each(|(k, v)| f(k, v));
    }
}

// SAFETY: set operations built from `Self`.
unsafe impl<T: Reflect + Eq + Hash> Reflect for HashSet<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "HashSet",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Set(SetInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(<Self as HashSetExt>::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        let mut out = <Self as HashSetExt>::with_capacity(self.len());
        for item in self {
            out.insert(item.reflect_clone()?);
        }
        Some(out)
    }
}

impl<T: Reflect + Eq + Hash> ReflectSet for HashSet<T> {
    type Item = T;
    const ORDERED: bool = false;

    fn len(&self) -> usize {
        HashSet::len(self)
    }

    fn contains(&self, item: &T) -> bool {
        HashSet::contains(self, item)
    }

    fn insert(&mut self, item: T) -> bool {
        HashSet::insert(self, item)
    }

    fn remove(&mut self, item: &T) -> bool {
        HashSet::remove(self, item)
    }

    fn clear(&mut self) {
        HashSet::clear(self);
    }

    fn for_each(&self, f: &mut dyn FnMut(&T)) {
        self.iter().for_each(f);
    }
}
