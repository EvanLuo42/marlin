//! The reflection traits and their implementations for core and alloc types.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::id::TypeName;
use crate::info::{
    ArrayInfo, FieldInfo, ListInfo, MapInfo, OptionInfo, PointerInfo, PointerKind, Primitive,
    SetInfo, StringInfo, StructInfo, StructStyle, TupleInfo, TypeInfo, TypeKind,
};

/// A type with a static description.
///
/// Usually derived with `#[derive(Reflect)]`. The two constructors return `None` when the type
/// (or one of its fields) cannot be built that way; deserialization needs `reflect_default`, and
/// `Mut::assign` and `Value::try_clone` need `reflect_clone`.
///
/// # Safety
///
/// `TYPE_INFO` must describe `Self` exactly: [`TypeInfo::new::<Self>`] for the sizes and
/// operations, field offsets from `offset_of!(Self, ...)` with each field's real type, and the
/// kind-specific operations built from `Self` (`EnumInfo::of::<Self>`, `ListInfo::of::<Self>`,
/// ...). Type-erased access trusts it completely.
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not implement `Reflect`",
    note = "derive it with `#[derive(Reflect)]`, or mark the field `#[reflect(skip)]`"
)]
pub unsafe trait Reflect: Sized + 'static {
    const TYPE_NAME: TypeName;
    const TYPE_INFO: TypeInfo;

    fn reflect_default() -> Option<Self>;
    fn reflect_clone(&self) -> Option<Self>;
}

/// The description of `T`. A `fn` (rather than the constant) so it can be stored as a lazy
/// reference in other descriptions.
pub fn type_info<T: Reflect>() -> &'static TypeInfo {
    const { &T::TYPE_INFO }
}

/// Typed access used to build [`EnumInfo`](crate::EnumInfo). Implemented by the derive.
pub trait ReflectEnum: Reflect {
    fn variant_index(&self) -> usize;
    /// Field `field` (counting reflected fields only) of the active variant, or null.
    fn field_ptr(&self, field: usize) -> *const u8;
    fn field_ptr_mut(&mut self, field: usize) -> *mut u8;
    /// Variant `index` with every field default-constructed.
    fn default_variant(index: usize) -> Option<Self>;
}

/// Typed access used to build [`ListInfo`].
pub trait ReflectList: Reflect {
    type Item: Reflect;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, index: usize) -> Option<&Self::Item>;
    fn get_mut(&mut self, index: usize) -> Option<&mut Self::Item>;
    /// Truncates, or grows with default items. Returns false, leaving the list unchanged, if it
    /// has to grow and the item type has no default.
    fn resize(&mut self, len: usize) -> bool;
    /// Inserts a default item at `index <= len`. False (unchanged) on failure.
    fn insert_default(&mut self, index: usize) -> bool;
    /// Removes the item at `index`. False if out of bounds.
    fn remove(&mut self, index: usize) -> bool;
}

/// Typed access used to build [`StringInfo`].
pub trait ReflectString: Reflect {
    fn as_str(&self) -> &str;
    fn set_str(&mut self, value: &str);
}

/// Typed access used to build [`MapInfo`](crate::MapInfo).
pub trait ReflectMap: Reflect {
    type Key: Reflect;
    type Value: Reflect;
    /// Whether iteration order depends only on the keys (sorted maps).
    const ORDERED: bool;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get(&self, key: &Self::Key) -> Option<&Self::Value>;
    fn get_mut(&mut self, key: &Self::Key) -> Option<&mut Self::Value>;
    /// Inserts, or replaces the value of an equal key.
    fn insert(&mut self, key: Self::Key, value: Self::Value);
    /// Removes the entry of `key`; false if there was none.
    fn remove(&mut self, key: &Self::Key) -> bool;
    fn clear(&mut self);
    fn for_each(&self, f: &mut dyn FnMut(&Self::Key, &Self::Value));
    fn for_each_mut(&mut self, f: &mut dyn FnMut(&Self::Key, &mut Self::Value));
}

/// Typed access used to build [`SetInfo`](crate::SetInfo).
pub trait ReflectSet: Reflect {
    type Item: Reflect;
    /// Whether iteration order depends only on the elements (sorted sets).
    const ORDERED: bool;

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn contains(&self, item: &Self::Item) -> bool;
    /// False (dropping `item`) if an equal item was already present.
    fn insert(&mut self, item: Self::Item) -> bool;
    fn remove(&mut self, item: &Self::Item) -> bool;
    fn clear(&mut self);
    fn for_each(&self, f: &mut dyn FnMut(&Self::Item));
}

/// Typed access used to build [`PointerInfo`](crate::PointerInfo).
pub trait ReflectPointer: Reflect {
    type Pointee: Reflect;
    const KIND: PointerKind;

    fn pointee(&self) -> &Self::Pointee;
    /// `None` while the pointee is shared.
    fn pointee_mut(&mut self) -> Option<&mut Self::Pointee>;
    /// Unshares the pointee first by cloning it (`Reflect::reflect_clone`) into a new
    /// allocation; `None` if it is shared and cannot be cloned.
    fn make_mut(&mut self) -> Option<&mut Self::Pointee>;
}

macro_rules! primitives {
    ($($ty:ty => $kind:ident),* $(,)?) => {$(
        // SAFETY: `TypeInfo::new::<Self>` with a primitive kind matching the type.
        unsafe impl Reflect for $ty {
            const TYPE_NAME: TypeName = TypeName::simple(stringify!($ty));
            const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Primitive(Primitive::$kind));

            fn reflect_default() -> Option<Self> {
                Some(Default::default())
            }

            fn reflect_clone(&self) -> Option<Self> {
                Some(*self)
            }
        }
    )*};
}

primitives! {
    bool => Bool, char => Char,
    u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128, usize => Usize,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64, i128 => I128, isize => Isize,
    f32 => F32, f64 => F64,
}

// SAFETY: a field-less unit struct.
unsafe impl Reflect for () {
    const TYPE_NAME: TypeName = TypeName::simple("()");
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Struct(StructInfo {
        style: StructStyle::Unit,
        fields: &[],
    }));

    fn reflect_default() -> Option<Self> {
        Some(())
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(())
    }
}

// SAFETY: string operations built from `Self`.
unsafe impl Reflect for String {
    const TYPE_NAME: TypeName = TypeName::simple("String");
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::String(StringInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(String::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(self.clone())
    }
}

impl ReflectString for String {
    fn as_str(&self) -> &str {
        self
    }

    fn set_str(&mut self, value: &str) {
        self.clear();
        self.push_str(value);
    }
}

// SAFETY: option operations built from `T`, which is what `Option<T>`'s operations expect.
unsafe impl<T: Reflect> Reflect for Option<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "Option",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Option(OptionInfo::of::<T>()));

    fn reflect_default() -> Option<Self> {
        Some(None)
    }

    fn reflect_clone(&self) -> Option<Self> {
        match self {
            None => Some(None),
            Some(value) => Some(Some(value.reflect_clone()?)),
        }
    }
}

// SAFETY: list operations built from `Self`.
unsafe impl<T: Reflect> Reflect for Vec<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "Vec",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::List(ListInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(Vec::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        self.iter().map(T::reflect_clone).collect()
    }
}

impl<T: Reflect> ReflectList for Vec<T> {
    type Item = T;

    fn len(&self) -> usize {
        self.len()
    }

    fn get(&self, index: usize) -> Option<&T> {
        <[T]>::get(self, index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        <[T]>::get_mut(self, index)
    }

    fn resize(&mut self, len: usize) -> bool {
        let old = self.len();
        if len <= old {
            self.truncate(len);
            return true;
        }
        self.reserve(len - old);
        for _ in old..len {
            match T::reflect_default() {
                Some(item) => self.push(item),
                None => {
                    self.truncate(old);
                    return false;
                }
            }
        }
        true
    }

    fn insert_default(&mut self, index: usize) -> bool {
        if index > self.len() {
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
        if index >= self.len() {
            return false;
        }
        self.remove(index);
        true
    }
}

// SAFETY: `N` contiguous `T`s, which is the layout of `[T; N]`.
unsafe impl<T: Reflect, const N: usize> Reflect for [T; N] {
    const TYPE_NAME: TypeName = TypeName::Array {
        element: &T::TYPE_NAME,
        len: N,
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Array(ArrayInfo {
        element: type_info::<T>,
        len: N,
    }));

    fn reflect_default() -> Option<Self> {
        try_array(|_| T::reflect_default())
    }

    fn reflect_clone(&self) -> Option<Self> {
        try_array(|i| self[i].reflect_clone())
    }
}

/// `core::array::try_from_fn` without the nightly feature. Items built before a failure are
/// dropped.
fn try_array<T, const N: usize>(mut f: impl FnMut(usize) -> Option<T>) -> Option<[T; N]> {
    let mut items = Vec::with_capacity(N);
    for i in 0..N {
        items.push(f(i)?);
    }
    items.try_into().ok()
}

/// Pushes `count` default items through `push`, which returns false when the container is full.
/// False if an item had no default or did not fit; the caller then truncates back.
pub(crate) fn grow_with_defaults<T: Reflect>(
    count: usize,
    mut push: impl FnMut(T) -> bool,
) -> bool {
    for _ in 0..count {
        let Some(item) = T::reflect_default() else {
            return false;
        };
        if !push(item) {
            return false;
        }
    }
    true
}

// SAFETY: list operations built from `Self`.
unsafe impl<T: Reflect> Reflect for VecDeque<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "VecDeque",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::List(ListInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(VecDeque::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        self.iter().map(T::reflect_clone).collect()
    }
}

impl<T: Reflect> ReflectList for VecDeque<T> {
    type Item = T;

    fn len(&self) -> usize {
        self.len()
    }

    fn get(&self, index: usize) -> Option<&T> {
        VecDeque::get(self, index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        VecDeque::get_mut(self, index)
    }

    fn resize(&mut self, len: usize) -> bool {
        let old = self.len();
        if len <= old {
            self.truncate(len);
            return true;
        }
        self.reserve(len - old);
        let ok = grow_with_defaults(len - old, |item| {
            self.push_back(item);
            true
        });
        if !ok {
            self.truncate(old);
        }
        ok
    }

    fn insert_default(&mut self, index: usize) -> bool {
        if index > self.len() {
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
        self.remove(index).is_some()
    }
}

macro_rules! tuples {
    ($(($($T:ident $i:tt),+))*) => {$(
        // SAFETY: each field at its `offset_of!` offset with its own type.
        unsafe impl<$($T: Reflect),+> Reflect for ($($T,)+) {
            const TYPE_NAME: TypeName = TypeName::Tuple(&[$(&$T::TYPE_NAME),+]);
            const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Tuple(TupleInfo {
                fields: &[$(FieldInfo::new(
                    stringify!($i),
                    core::mem::offset_of!(Self, $i),
                    type_info::<$T>,
                )),+],
            }));

            fn reflect_default() -> Option<Self> {
                Some(($($T::reflect_default()?,)+))
            }

            fn reflect_clone(&self) -> Option<Self> {
                Some(($(self.$i.reflect_clone()?,)+))
            }
        }
    )*};
}

tuples! {
    (A 0)
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11)
}

// SAFETY: pointer operations built from `Self`.
unsafe impl<T: Reflect> Reflect for Box<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "Box",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Pointer(PointerInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(Box::new(T::reflect_default()?))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Box::new((**self).reflect_clone()?))
    }
}

impl<T: Reflect> ReflectPointer for Box<T> {
    type Pointee = T;
    const KIND: PointerKind = PointerKind::Box;

    fn pointee(&self) -> &T {
        self
    }

    fn pointee_mut(&mut self) -> Option<&mut T> {
        Some(self)
    }

    fn make_mut(&mut self) -> Option<&mut T> {
        Some(self)
    }
}

macro_rules! shared_pointer {
    ($($ptr:ident => $kind:ident),*) => {$(
        // SAFETY: pointer operations built from `Self`.
        unsafe impl<T: Reflect> Reflect for $ptr<T> {
            const TYPE_NAME: TypeName = TypeName::Path {
                path: stringify!($ptr),
                args: &[&T::TYPE_NAME],
            };
            const TYPE_INFO: TypeInfo =
                TypeInfo::new::<Self>(TypeKind::Pointer(PointerInfo::of::<Self>()));

            fn reflect_default() -> Option<Self> {
                Some($ptr::new(T::reflect_default()?))
            }

            /// Shares the pointee, as `Clone` does.
            fn reflect_clone(&self) -> Option<Self> {
                Some($ptr::clone(self))
            }
        }

        impl<T: Reflect> ReflectPointer for $ptr<T> {
            type Pointee = T;
            const KIND: PointerKind = PointerKind::$kind;

            fn pointee(&self) -> &T {
                self
            }

            fn pointee_mut(&mut self) -> Option<&mut T> {
                $ptr::get_mut(self)
            }

            fn make_mut(&mut self) -> Option<&mut T> {
                if $ptr::get_mut(self).is_none() {
                    let copy = (**self).reflect_clone()?;
                    *self = $ptr::new(copy);
                }
                $ptr::get_mut(self)
            }
        }
    )*};
}

shared_pointer!(Rc => Rc, Arc => Arc);

// SAFETY: string operations built from `Self`.
unsafe impl Reflect for Cow<'static, str> {
    const TYPE_NAME: TypeName = TypeName::simple("Cow<str>");
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::String(StringInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(Cow::Borrowed(""))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(self.clone())
    }
}

impl ReflectString for Cow<'static, str> {
    fn as_str(&self) -> &str {
        self
    }

    fn set_str(&mut self, value: &str) {
        match self {
            Cow::Owned(s) => {
                s.clear();
                s.push_str(value);
            }
            Cow::Borrowed(_) => *self = Cow::Owned(value.into()),
        }
    }
}

// SAFETY: a field-less unit struct.
unsafe impl<T: Reflect> Reflect for PhantomData<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "PhantomData",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Struct(StructInfo {
        style: StructStyle::Unit,
        fields: &[],
    }));

    fn reflect_default() -> Option<Self> {
        Some(PhantomData)
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(PhantomData)
    }
}

// SAFETY: map operations built from `Self`.
unsafe impl<K: Reflect + Ord, V: Reflect> Reflect for BTreeMap<K, V> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "BTreeMap",
        args: &[&K::TYPE_NAME, &V::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Map(MapInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(BTreeMap::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        self.iter()
            .map(|(k, v)| Some((k.reflect_clone()?, v.reflect_clone()?)))
            .collect()
    }
}

impl<K: Reflect + Ord, V: Reflect> ReflectMap for BTreeMap<K, V> {
    type Key = K;
    type Value = V;
    const ORDERED: bool = true;

    fn len(&self) -> usize {
        self.len()
    }

    fn get(&self, key: &K) -> Option<&V> {
        BTreeMap::get(self, key)
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        BTreeMap::get_mut(self, key)
    }

    fn insert(&mut self, key: K, value: V) {
        BTreeMap::insert(self, key, value);
    }

    fn remove(&mut self, key: &K) -> bool {
        BTreeMap::remove(self, key).is_some()
    }

    fn clear(&mut self) {
        BTreeMap::clear(self);
    }

    fn for_each(&self, f: &mut dyn FnMut(&K, &V)) {
        self.iter().for_each(|(k, v)| f(k, v));
    }

    fn for_each_mut(&mut self, f: &mut dyn FnMut(&K, &mut V)) {
        self.iter_mut().for_each(|(k, v)| f(k, v));
    }
}

// SAFETY: set operations built from `Self`.
unsafe impl<T: Reflect + Ord> Reflect for BTreeSet<T> {
    const TYPE_NAME: TypeName = TypeName::Path {
        path: "BTreeSet",
        args: &[&T::TYPE_NAME],
    };
    const TYPE_INFO: TypeInfo = TypeInfo::new::<Self>(TypeKind::Set(SetInfo::of::<Self>()));

    fn reflect_default() -> Option<Self> {
        Some(BTreeSet::new())
    }

    fn reflect_clone(&self) -> Option<Self> {
        self.iter().map(T::reflect_clone).collect()
    }
}

impl<T: Reflect + Ord> ReflectSet for BTreeSet<T> {
    type Item = T;
    const ORDERED: bool = true;

    fn len(&self) -> usize {
        self.len()
    }

    fn contains(&self, item: &T) -> bool {
        BTreeSet::contains(self, item)
    }

    fn insert(&mut self, item: T) -> bool {
        BTreeSet::insert(self, item)
    }

    fn remove(&mut self, item: &T) -> bool {
        BTreeSet::remove(self, item)
    }

    fn clear(&mut self) {
        BTreeSet::clear(self);
    }

    fn for_each(&self, f: &mut dyn FnMut(&T)) {
        self.iter().for_each(f);
    }
}
