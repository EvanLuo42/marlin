//! The reflection traits and their implementations for core and alloc types.

use alloc::string::String;
use alloc::vec::Vec;

use crate::id::TypeName;
use crate::info::{
    ArrayInfo, ListInfo, OptionInfo, Primitive, StringInfo, StructInfo, StructStyle, TypeInfo,
    TypeKind,
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
