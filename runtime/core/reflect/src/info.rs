//! Static type descriptions.
//!
//! Everything here is built in `const` context, so a type's description is read-only data. Types
//! refer to other types through `fn() -> &'static TypeInfo` rather than `&'static TypeInfo`: a
//! recursive type (`struct Node { children: Vec<Node> }`) would otherwise make its own constant
//! depend on itself.
//!
//! The operations are `unsafe fn` pointers over untyped bytes; [`Ref`](crate::Ref),
//! [`Mut`](crate::Mut) and [`Value`](crate::Value) wrap them in a safe API.

use core::any::TypeId as RustTypeId;
use core::fmt;
use core::mem::{align_of, size_of};
use core::ptr;

use crate::id::{TypeId, TypeName};
use crate::traits::{Reflect, ReflectEnum, ReflectList, ReflectString};

/// Description of a reflected type.
pub struct TypeInfo {
    pub name: &'static TypeName,
    pub id: TypeId,
    pub size: usize,
    pub align: usize,
    pub kind: TypeKind,
    pub ops: TypeOps,
}

impl TypeInfo {
    /// The description of `T` with the given kind; the rest comes from `T` itself.
    pub const fn new<T: Reflect>(kind: TypeKind) -> Self {
        Self {
            name: const { &T::TYPE_NAME },
            id: TypeId::of_type_name(&T::TYPE_NAME),
            size: size_of::<T>(),
            align: align_of::<T>(),
            kind,
            ops: TypeOps::of::<T>(),
        }
    }

    /// Whether this describes `T`. Compares the Rust type, not the stable id, so two types that
    /// share a name are never confused.
    pub fn is<T: Reflect>(&self) -> bool {
        (self.ops.rust_id)() == RustTypeId::of::<T>()
    }

    /// Whether `self` and `other` describe the same Rust type.
    pub fn same_type(&self, other: &TypeInfo) -> bool {
        (self.ops.rust_id)() == (other.ops.rust_id)()
    }

    /// The fields of a struct; empty for other kinds.
    pub fn fields(&self) -> &'static [FieldInfo] {
        match &self.kind {
            TypeKind::Struct(s) => s.fields,
            _ => &[],
        }
    }

    /// A struct field by reflected name.
    pub fn field(&self, name: &str) -> Option<&'static FieldInfo> {
        self.fields().iter().find(|f| f.name == name)
    }

    /// The variants of an enum; empty for other kinds.
    pub fn variants(&self) -> &'static [VariantInfo] {
        match &self.kind {
            TypeKind::Enum(e) => e.variants,
            _ => &[],
        }
    }
}

impl fmt::Debug for TypeInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeInfo")
            .field("name", &format_args!("{}", self.name))
            .field("id", &format_args!("{}", self.id))
            .field("size", &self.size)
            .field("align", &self.align)
            .finish_non_exhaustive()
    }
}

/// Type-erased lifecycle operations.
///
/// Every pointer argument must be aligned for the described type; `dst` points to uninitialized
/// storage of the type's size, and every other pointer to a live value.
#[derive(Clone, Copy)]
pub struct TypeOps {
    /// `core::any::TypeId` of the described type, used for safe downcasts.
    pub rust_id: fn() -> RustTypeId,
    /// Drops the value in place.
    pub drop: unsafe fn(ptr: *mut u8),
    /// Writes a default value to `dst`; returns false (writing nothing) if the type has none.
    pub default: unsafe fn(dst: *mut u8) -> bool,
    /// Writes a clone of `src` to `dst`; returns false (writing nothing) if the type is not
    /// cloneable.
    pub clone: unsafe fn(src: *const u8, dst: *mut u8) -> bool,
}

impl TypeOps {
    pub const fn of<T: Reflect>() -> Self {
        Self {
            rust_id: RustTypeId::of::<T>,
            drop: drop_op::<T>,
            default: default_op::<T>,
            clone: clone_op::<T>,
        }
    }
}

unsafe fn drop_op<T>(ptr: *mut u8) {
    // SAFETY: the caller passes a live, aligned `T` that is not used afterwards.
    unsafe { ptr::drop_in_place(ptr.cast::<T>()) }
}

unsafe fn default_op<T: Reflect>(dst: *mut u8) -> bool {
    let Some(value) = T::reflect_default() else {
        return false;
    };
    // SAFETY: the caller passes aligned storage for a `T`.
    unsafe { dst.cast::<T>().write(value) };
    true
}

unsafe fn clone_op<T: Reflect>(src: *const u8, dst: *mut u8) -> bool {
    // SAFETY: the caller passes a live, aligned `T`.
    let src = unsafe { &*src.cast::<T>() };
    let Some(value) = src.reflect_clone() else {
        return false;
    };
    // SAFETY: the caller passes aligned storage for a `T`.
    unsafe { dst.cast::<T>().write(value) };
    true
}

/// What a type looks like.
#[derive(Clone, Copy, Debug)]
pub enum TypeKind {
    Primitive(Primitive),
    String(StringInfo),
    Struct(StructInfo),
    Enum(EnumInfo),
    /// Fixed-size array `[T; N]`: `len` elements of the element type, laid out contiguously.
    Array(ArrayInfo),
    /// Resizable sequence such as `Vec<T>`.
    List(ListInfo),
    Option(OptionInfo),
    /// Named and sized, but with no visible structure (handles, OS objects).
    Opaque,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Primitive {
    Bool,
    Char,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    F32,
    F64,
}

impl Primitive {
    pub const fn is_integer(self) -> bool {
        !matches!(self, Self::Bool | Self::Char | Self::F32 | Self::F64)
    }

    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    pub const fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StructStyle {
    /// `struct S { a: A }`
    Named,
    /// `struct S(A);` Fields are named `"0"`, `"1"`, ...
    Tuple,
    /// `struct S;`
    Unit,
}

#[derive(Clone, Copy, Debug)]
pub struct StructInfo {
    pub style: StructStyle,
    pub fields: &'static [FieldInfo],
}

/// `FieldInfo::offset` of enum variant fields, whose position depends on the active variant;
/// they are reached through [`EnumInfo::field_ptr`] instead.
pub const VARIANT_FIELD: usize = usize::MAX;

#[derive(Clone, Copy)]
pub struct FieldInfo {
    /// Reflected name: `#[reflect(rename)]` if given, else the Rust name (`"0"` for tuple fields).
    /// Serialized data matches fields by the hash of this name.
    pub name: &'static str,
    /// Name in Rust source.
    pub ident: &'static str,
    /// [`name_hash`](crate::name_hash) of `name`.
    pub name_hash: u32,
    /// Byte offset in the struct, or [`VARIANT_FIELD`].
    pub offset: usize,
    pub ty: fn() -> &'static TypeInfo,
    pub attrs: FieldAttrs,
}

impl fmt::Debug for FieldInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FieldInfo")
            .field("name", &self.name)
            .field("ty", &format_args!("{}", (self.ty)().name))
            .field("offset", &self.offset)
            .field("attrs", &self.attrs)
            .finish()
    }
}

/// Editor and serialization hints from `#[reflect(...)]`. Reflection itself does not enforce them:
/// `Mut` can still write a `read_only` field or a value outside `range`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldAttrs {
    pub hidden: bool,
    pub read_only: bool,
    /// Never serialized; keeps its default when loading.
    pub transient: bool,
    pub tooltip: Option<&'static str>,
    pub range: Option<RangeAttr>,
}

impl FieldAttrs {
    pub const NONE: Self = Self {
        hidden: false,
        read_only: false,
        transient: false,
        tooltip: None,
        range: None,
    };
}

/// `#[reflect(range = min..max)]` or `min..=max`. A missing bound is infinite; `min` is always
/// inclusive.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RangeAttr {
    pub min: f64,
    pub max: f64,
    pub max_inclusive: bool,
}

impl RangeAttr {
    pub fn contains(&self, value: f64) -> bool {
        value >= self.min
            && if self.max_inclusive {
                value <= self.max
            } else {
                value < self.max
            }
    }
}

#[derive(Clone, Copy)]
pub struct EnumInfo {
    pub variants: &'static [VariantInfo],
    /// Index of the active variant.
    pub variant_index: unsafe fn(ptr: *const u8) -> usize,
    /// Field `field` of the active variant, or null.
    pub field_ptr: unsafe fn(ptr: *const u8, field: usize) -> *const u8,
    /// Same as `field_ptr` for writing.
    pub field_ptr_mut: unsafe fn(ptr: *mut u8, field: usize) -> *mut u8,
    /// Replaces the value with variant `index` holding default fields; false (unchanged) if a
    /// field has no default or the index is out of range.
    pub set_variant: unsafe fn(ptr: *mut u8, index: usize) -> bool,
}

impl EnumInfo {
    pub const fn of<T: ReflectEnum>(variants: &'static [VariantInfo]) -> Self {
        Self {
            variants,
            variant_index: variant_index_op::<T>,
            field_ptr: field_ptr_op::<T>,
            field_ptr_mut: field_ptr_mut_op::<T>,
            set_variant: set_variant_op::<T>,
        }
    }
}

impl fmt::Debug for EnumInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnumInfo")
            .field("variants", &self.variants)
            .finish_non_exhaustive()
    }
}

unsafe fn variant_index_op<T: ReflectEnum>(ptr: *const u8) -> usize {
    // SAFETY: the caller passes a live `T`.
    unsafe { &*ptr.cast::<T>() }.variant_index()
}

unsafe fn field_ptr_op<T: ReflectEnum>(ptr: *const u8, field: usize) -> *const u8 {
    // SAFETY: the caller passes a live `T`.
    unsafe { &*ptr.cast::<T>() }.field_ptr(field)
}

unsafe fn field_ptr_mut_op<T: ReflectEnum>(ptr: *mut u8, field: usize) -> *mut u8 {
    // SAFETY: the caller passes a live `T` it may mutate.
    unsafe { &mut *ptr.cast::<T>() }.field_ptr_mut(field)
}

unsafe fn set_variant_op<T: ReflectEnum>(ptr: *mut u8, index: usize) -> bool {
    let Some(value) = T::default_variant(index) else {
        return false;
    };
    // SAFETY: the caller passes a live `T` it may mutate; assignment drops the old value.
    unsafe { *ptr.cast::<T>() = value };
    true
}

#[derive(Clone, Copy, Debug)]
pub struct VariantInfo {
    pub name: &'static str,
    pub ident: &'static str,
    pub name_hash: u32,
    /// The discriminant, for enums whose variants are all unit variants.
    pub discriminant: Option<i64>,
    pub style: StructStyle,
    pub fields: &'static [FieldInfo],
    pub hidden: bool,
    pub tooltip: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub struct ArrayInfo {
    pub element: fn() -> &'static TypeInfo,
    pub len: usize,
}

impl fmt::Debug for ArrayInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArrayInfo")
            .field("element", &format_args!("{}", (self.element)().name))
            .field("len", &self.len)
            .finish()
    }
}

#[derive(Clone, Copy)]
pub struct ListInfo {
    pub element: fn() -> &'static TypeInfo,
    pub len: unsafe fn(ptr: *const u8) -> usize,
    /// Element `index`, or null if out of bounds.
    pub element_ptr: unsafe fn(ptr: *const u8, index: usize) -> *const u8,
    pub element_ptr_mut: unsafe fn(ptr: *mut u8, index: usize) -> *mut u8,
    /// See [`ReflectList::resize`].
    pub resize: unsafe fn(ptr: *mut u8, len: usize) -> bool,
    /// See [`ReflectList::insert_default`].
    pub insert_default: unsafe fn(ptr: *mut u8, index: usize) -> bool,
    /// See [`ReflectList::remove`].
    pub remove: unsafe fn(ptr: *mut u8, index: usize) -> bool,
}

impl ListInfo {
    pub const fn of<L: ReflectList>() -> Self {
        Self {
            element: crate::type_info::<L::Item>,
            len: list_len_op::<L>,
            element_ptr: list_get_op::<L>,
            element_ptr_mut: list_get_mut_op::<L>,
            resize: list_resize_op::<L>,
            insert_default: list_insert_op::<L>,
            remove: list_remove_op::<L>,
        }
    }
}

impl fmt::Debug for ListInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListInfo")
            .field("element", &format_args!("{}", (self.element)().name))
            .finish_non_exhaustive()
    }
}

unsafe fn list_len_op<L: ReflectList>(ptr: *const u8) -> usize {
    // SAFETY: the caller passes a live `L`.
    unsafe { &*ptr.cast::<L>() }.len()
}

unsafe fn list_get_op<L: ReflectList>(ptr: *const u8, index: usize) -> *const u8 {
    // SAFETY: the caller passes a live `L`.
    let list = unsafe { &*ptr.cast::<L>() };
    list.get(index)
        .map_or(ptr::null(), |item| ptr::from_ref(item).cast())
}

unsafe fn list_get_mut_op<L: ReflectList>(ptr: *mut u8, index: usize) -> *mut u8 {
    // SAFETY: the caller passes a live `L` it may mutate.
    let list = unsafe { &mut *ptr.cast::<L>() };
    list.get_mut(index)
        .map_or(ptr::null_mut(), |item| ptr::from_mut(item).cast())
}

unsafe fn list_resize_op<L: ReflectList>(ptr: *mut u8, len: usize) -> bool {
    // SAFETY: the caller passes a live `L` it may mutate.
    unsafe { &mut *ptr.cast::<L>() }.resize(len)
}

unsafe fn list_insert_op<L: ReflectList>(ptr: *mut u8, index: usize) -> bool {
    // SAFETY: the caller passes a live `L` it may mutate.
    unsafe { &mut *ptr.cast::<L>() }.insert_default(index)
}

unsafe fn list_remove_op<L: ReflectList>(ptr: *mut u8, index: usize) -> bool {
    // SAFETY: the caller passes a live `L` it may mutate.
    unsafe { &mut *ptr.cast::<L>() }.remove(index)
}

#[derive(Clone, Copy)]
pub struct OptionInfo {
    pub some: fn() -> &'static TypeInfo,
    /// The contained value, or null for `None`.
    pub get: unsafe fn(ptr: *const u8) -> *const u8,
    pub get_mut: unsafe fn(ptr: *mut u8) -> *mut u8,
    pub set_none: unsafe fn(ptr: *mut u8),
    /// Sets `Some(default)`; false (unchanged) if the inner type has no default.
    pub set_some_default: unsafe fn(ptr: *mut u8) -> bool,
}

impl OptionInfo {
    pub const fn of<T: Reflect>() -> Self {
        Self {
            some: crate::type_info::<T>,
            get: option_get_op::<T>,
            get_mut: option_get_mut_op::<T>,
            set_none: option_set_none_op::<T>,
            set_some_default: option_set_some_op::<T>,
        }
    }
}

impl fmt::Debug for OptionInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OptionInfo")
            .field("some", &format_args!("{}", (self.some)().name))
            .finish_non_exhaustive()
    }
}

unsafe fn option_get_op<T>(ptr: *const u8) -> *const u8 {
    // SAFETY: the caller passes a live `Option<T>`.
    unsafe { &*ptr.cast::<Option<T>>() }
        .as_ref()
        .map_or(ptr::null(), |v| ptr::from_ref(v).cast())
}

unsafe fn option_get_mut_op<T>(ptr: *mut u8) -> *mut u8 {
    // SAFETY: the caller passes a live `Option<T>` it may mutate.
    unsafe { &mut *ptr.cast::<Option<T>>() }
        .as_mut()
        .map_or(ptr::null_mut(), |v| ptr::from_mut(v).cast())
}

unsafe fn option_set_none_op<T>(ptr: *mut u8) {
    // SAFETY: the caller passes a live `Option<T>` it may mutate.
    unsafe { *ptr.cast::<Option<T>>() = None };
}

unsafe fn option_set_some_op<T: Reflect>(ptr: *mut u8) -> bool {
    let Some(value) = T::reflect_default() else {
        return false;
    };
    // SAFETY: the caller passes a live `Option<T>` it may mutate.
    unsafe { *ptr.cast::<Option<T>>() = Some(value) };
    true
}

#[derive(Clone, Copy)]
pub struct StringInfo {
    pub as_str: unsafe fn(ptr: *const u8) -> *const str,
    pub set: unsafe fn(ptr: *mut u8, value: &str),
}

impl StringInfo {
    pub const fn of<S: ReflectString>() -> Self {
        Self {
            as_str: string_as_str_op::<S>,
            set: string_set_op::<S>,
        }
    }
}

impl fmt::Debug for StringInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StringInfo").finish_non_exhaustive()
    }
}

unsafe fn string_as_str_op<S: ReflectString>(ptr: *const u8) -> *const str {
    // SAFETY: the caller passes a live `S`.
    ptr::from_ref(unsafe { &*ptr.cast::<S>() }.as_str())
}

unsafe fn string_set_op<S: ReflectString>(ptr: *mut u8, value: &str) {
    // SAFETY: the caller passes a live `S` it may mutate.
    unsafe { &mut *ptr.cast::<S>() }.set_str(value);
}
