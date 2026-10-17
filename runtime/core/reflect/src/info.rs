//! Static type descriptions.
//!
//! Everything here is built in `const` context, so a type's description is read-only data. Types
//! refer to other types through `fn() -> &'static TypeInfo` rather than `&'static TypeInfo`: a
//! recursive type (`struct Node { children: Vec<Node> }`) would otherwise make its own constant
//! depend on itself.
//!
//! The operations are `unsafe fn` pointers over untyped bytes; [`Ref`](crate::Ref),
//! [`Mut`](crate::Mut) and [`Value`](crate::Value) wrap them in a safe API.

use alloc::vec::Vec;
use core::any::{Any, TypeId as RustTypeId};
use core::fmt;
use core::mem::{align_of, size_of};
use core::ptr;

use crate::id::{TypeId, TypeName};
use crate::traits::{
    Reflect, ReflectEnum, ReflectList, ReflectMap, ReflectPointer, ReflectSet, ReflectString,
};

/// One item of a type's static type data: any `'static` value, found again by its Rust type with
/// [`TypeInfo::data`]. See [`crate::ReflectTrait`] and [`crate::ReflectProxy`].
pub type TypeData = &'static (dyn Any + Send + Sync);

/// Description of a reflected type.
pub struct TypeInfo {
    pub name: &'static TypeName,
    pub id: TypeId,
    pub size: usize,
    pub align: usize,
    pub kind: TypeKind,
    pub ops: TypeOps,
    /// Type-level hints from `#[reflect(...)]` and doc comments.
    pub attrs: TypeAttrs,
    /// Static type data, such as trait casts from `#[reflect(impl_trait(...))]` and values from
    /// `#[reflect(data(...))]`. More can be attached at run time through the
    /// [`registry`](crate::registry).
    pub data: &'static [TypeData],
}

impl TypeInfo {
    /// The description of `T` with the given kind; the rest comes from `T` itself. No attributes
    /// and no type data: set those with struct update syntax,
    /// `TypeInfo { data: &[...], ..TypeInfo::new::<Self>(kind) }`.
    pub const fn new<T: Reflect>(kind: TypeKind) -> Self {
        Self {
            name: const { &T::TYPE_NAME },
            id: TypeId::of_type_name(&T::TYPE_NAME),
            size: size_of::<T>(),
            align: align_of::<T>(),
            kind,
            ops: TypeOps::of::<T>(),
            attrs: TypeAttrs::NONE,
            data: &[],
        }
    }

    /// The static type data of type `D`, if the type declares one. The registry's
    /// [`data`](crate::Registry::data) also sees data attached at run time.
    pub fn data<D: Any>(&self) -> Option<&'static D> {
        self.data.iter().find_map(|d| {
            let d: &'static (dyn Any + Send + Sync) = *d;
            d.downcast_ref::<D>()
        })
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

    /// The fields of a struct or tuple; empty for other kinds.
    pub fn fields(&self) -> &'static [FieldInfo] {
        match &self.kind {
            TypeKind::Struct(s) => s.fields,
            TypeKind::Tuple(t) => t.fields,
            _ => &[],
        }
    }

    /// A struct (or tuple) field by reflected name.
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
            .field("attrs", &self.attrs)
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
    /// A tuple `(A, B, ...)`, with fields named `"0"`, `"1"`, ... at their offsets.
    Tuple(TupleInfo),
    /// Key-value map such as `HashMap<K, V>` or `BTreeMap<K, V>`.
    Map(MapInfo),
    /// Set such as `HashSet<T>` or `BTreeSet<T>`.
    Set(SetInfo),
    /// An owning pointer to one value: `Box<T>`, `Rc<T>`, `Arc<T>`.
    Pointer(PointerInfo),
    /// Named and sized, but with no visible structure (handles, OS objects). It may still carry a
    /// [`ReflectProxy`](crate::ReflectProxy) through which it converts to and from a reflected
    /// type by value.
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

impl FieldInfo {
    /// A field with no attributes, for hand-written descriptions: `name` is also the Rust name.
    pub const fn new(name: &'static str, offset: usize, ty: fn() -> &'static TypeInfo) -> Self {
        Self {
            name,
            ident: name,
            name_hash: crate::id::name_hash(name),
            offset,
            ty,
            attrs: FieldAttrs::NONE,
        }
    }

    /// The name to show in an editor: `display_name` if given, else the reflected name.
    pub fn label(&self) -> &'static str {
        self.attrs.display_name.unwrap_or(self.name)
    }
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

/// Editor and serialization hints from `#[reflect(...)]` and doc comments. Reflection itself does
/// not enforce them: `Mut` can still write a `read_only` field or a value outside `range`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldAttrs {
    pub hidden: bool,
    pub read_only: bool,
    /// Never serialized; keeps its default when loading.
    pub transient: bool,
    pub tooltip: Option<&'static str>,
    pub range: Option<RangeAttr>,
    /// Label for the editor instead of the field name.
    pub display_name: Option<&'static str>,
    /// Group in a property grid, such as `"Lighting"`.
    pub category: Option<&'static str>,
    /// The field's `///` comment (one line per line, the leading space removed), or an explicit
    /// `#[reflect(doc = "...")]`.
    pub doc: Option<&'static str>,
    /// Unit of the value, such as `"m"`, `"deg"` or `"s"`.
    pub units: Option<&'static str>,
    /// Increment of a numeric widget.
    pub step: Option<f64>,
    /// Which editor widget to use, when the type alone does not say.
    pub widget: Option<Widget>,
    /// Free-form `#[reflect(meta(key = "value"))]` pairs, in declaration order.
    pub meta: &'static [(&'static str, &'static str)],
}

impl FieldAttrs {
    pub const NONE: Self = Self {
        hidden: false,
        read_only: false,
        transient: false,
        tooltip: None,
        range: None,
        display_name: None,
        category: None,
        doc: None,
        units: None,
        step: None,
        widget: None,
        meta: &[],
    };

    /// The value of custom attribute `key`.
    pub fn meta_value(&self, key: &str) -> Option<&'static str> {
        meta_value(self.meta, key)
    }
}

fn meta_value(meta: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    meta.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Type-level hints from `#[reflect(...)]` and doc comments.
#[derive(Clone, Copy, Debug, Default)]
pub struct TypeAttrs {
    /// Name for the editor instead of the type name.
    pub display_name: Option<&'static str>,
    /// Group in type pickers, such as `"Rendering/Lights"`.
    pub category: Option<&'static str>,
    /// The type's `///` comment, or an explicit `#[reflect(doc = "...")]`.
    pub doc: Option<&'static str>,
    /// Free-form `#[reflect(meta(key = "value"))]` pairs.
    pub meta: &'static [(&'static str, &'static str)],
    /// Data version from `#[reflect(version = N)]`, for serializers that migrate old data. 0 means
    /// unversioned.
    pub version: u32,
}

impl TypeAttrs {
    pub const NONE: Self = Self {
        display_name: None,
        category: None,
        doc: None,
        meta: &[],
        version: 0,
    };

    /// The value of custom attribute `key`.
    pub fn meta_value(&self, key: &str) -> Option<&'static str> {
        meta_value(self.meta, key)
    }
}

/// Editor widget hint from `#[reflect(widget = ...)]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Widget {
    /// A slider over the field's `range`.
    Slider,
    /// A number dragged with the mouse.
    Drag,
    /// A color picker (an `[f32; 3]`/`[f32; 4]` or color type in linear sRGB).
    Color,
    /// A color picker allowing values above 1.
    HdrColor,
    /// An angle stored in radians, edited in degrees.
    Angle,
    /// A multi-line text box.
    Multiline,
    /// An asset picker.
    Asset,
    /// A file path picker.
    Path,
    /// A widget the editor registers under this name (`widget = "knob"`).
    Custom(&'static str),
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
    /// Label for the editor instead of the variant name.
    pub display_name: Option<&'static str>,
    /// The variant's `///` comment, or an explicit `#[reflect(doc = "...")]`.
    pub doc: Option<&'static str>,
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

/// Fields `"0"`, `"1"`, ... of a tuple, with real offsets.
#[derive(Clone, Copy, Debug)]
pub struct TupleInfo {
    pub fields: &'static [FieldInfo],
}

#[derive(Clone, Copy)]
pub struct MapInfo {
    pub key: fn() -> &'static TypeInfo,
    pub value: fn() -> &'static TypeInfo,
    /// Iteration order depends only on the keys (sorted maps), so equal maps iterate alike. Hash
    /// maps iterate in an order that depends on their history.
    pub ordered: bool,
    pub len: unsafe fn(ptr: *const u8) -> usize,
    /// Appends a `(key, value)` pointer pair per entry to `out`, in iteration order.
    pub entries: unsafe fn(ptr: *const u8, out: &mut Vec<(*const u8, *const u8)>),
    /// Same as `entries`, with writable values. Keys are never writable.
    pub entries_mut: unsafe fn(ptr: *mut u8, out: &mut Vec<(*const u8, *mut u8)>),
    /// The value for the key at `key` (a live value of the key type), or null.
    pub get: unsafe fn(ptr: *const u8, key: *const u8) -> *const u8,
    pub get_mut: unsafe fn(ptr: *mut u8, key: *const u8) -> *mut u8,
    /// Moves a key and a value out of `key` and `value` (which hold no live value afterwards)
    /// into the map, replacing the value of an equal key.
    pub insert: unsafe fn(ptr: *mut u8, key: *mut u8, value: *mut u8),
    /// Removes the entry for the key at `key`; false if there was none.
    pub remove: unsafe fn(ptr: *mut u8, key: *const u8) -> bool,
    pub clear: unsafe fn(ptr: *mut u8),
}

impl MapInfo {
    pub const fn of<M: ReflectMap>() -> Self {
        Self {
            key: crate::type_info::<M::Key>,
            value: crate::type_info::<M::Value>,
            ordered: M::ORDERED,
            len: map_len_op::<M>,
            entries: map_entries_op::<M>,
            entries_mut: map_entries_mut_op::<M>,
            get: map_get_op::<M>,
            get_mut: map_get_mut_op::<M>,
            insert: map_insert_op::<M>,
            remove: map_remove_op::<M>,
            clear: map_clear_op::<M>,
        }
    }
}

impl fmt::Debug for MapInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MapInfo")
            .field("key", &format_args!("{}", (self.key)().name))
            .field("value", &format_args!("{}", (self.value)().name))
            .field("ordered", &self.ordered)
            .finish_non_exhaustive()
    }
}

unsafe fn map_len_op<M: ReflectMap>(ptr: *const u8) -> usize {
    // SAFETY: the caller passes a live `M`.
    unsafe { &*ptr.cast::<M>() }.len()
}

unsafe fn map_entries_op<M: ReflectMap>(ptr: *const u8, out: &mut Vec<(*const u8, *const u8)>) {
    // SAFETY: the caller passes a live `M`.
    let map = unsafe { &*ptr.cast::<M>() };
    out.reserve(map.len());
    map.for_each(&mut |k, v| out.push((ptr::from_ref(k).cast(), ptr::from_ref(v).cast())));
}

unsafe fn map_entries_mut_op<M: ReflectMap>(ptr: *mut u8, out: &mut Vec<(*const u8, *mut u8)>) {
    // SAFETY: the caller passes a live `M` it may mutate.
    let map = unsafe { &mut *ptr.cast::<M>() };
    out.reserve(map.len());
    map.for_each_mut(&mut |k, v| out.push((ptr::from_ref(k).cast(), ptr::from_mut(v).cast())));
}

unsafe fn map_get_op<M: ReflectMap>(ptr: *const u8, key: *const u8) -> *const u8 {
    // SAFETY: the caller passes a live `M` and a live key.
    let (map, key) = unsafe { (&*ptr.cast::<M>(), &*key.cast::<M::Key>()) };
    map.get(key)
        .map_or(ptr::null(), |v| ptr::from_ref(v).cast())
}

unsafe fn map_get_mut_op<M: ReflectMap>(ptr: *mut u8, key: *const u8) -> *mut u8 {
    // SAFETY: the caller passes a live `M` it may mutate and a live key outside it.
    let (map, key) = unsafe { (&mut *ptr.cast::<M>(), &*key.cast::<M::Key>()) };
    map.get_mut(key)
        .map_or(ptr::null_mut(), |v| ptr::from_mut(v).cast())
}

unsafe fn map_insert_op<M: ReflectMap>(ptr: *mut u8, key: *mut u8, value: *mut u8) {
    // SAFETY: the caller passes a live `M` it may mutate, and a key and a value it gives up.
    unsafe {
        let (key, value) = (key.cast::<M::Key>().read(), value.cast::<M::Value>().read());
        (*ptr.cast::<M>()).insert(key, value);
    }
}

unsafe fn map_remove_op<M: ReflectMap>(ptr: *mut u8, key: *const u8) -> bool {
    // SAFETY: the caller passes a live `M` it may mutate and a live key outside it.
    let (map, key) = unsafe { (&mut *ptr.cast::<M>(), &*key.cast::<M::Key>()) };
    map.remove(key)
}

unsafe fn map_clear_op<M: ReflectMap>(ptr: *mut u8) {
    // SAFETY: the caller passes a live `M` it may mutate.
    unsafe { &mut *ptr.cast::<M>() }.clear();
}

#[derive(Clone, Copy)]
pub struct SetInfo {
    pub element: fn() -> &'static TypeInfo,
    /// See [`MapInfo::ordered`].
    pub ordered: bool,
    pub len: unsafe fn(ptr: *const u8) -> usize,
    /// Appends a pointer per element to `out`, in iteration order. Elements are never writable.
    pub elements: unsafe fn(ptr: *const u8, out: &mut Vec<*const u8>),
    /// Whether the set holds a value equal to the one at `element`.
    pub contains: unsafe fn(ptr: *const u8, element: *const u8) -> bool,
    /// Moves the value out of `element` (which holds no live value afterwards) into the set;
    /// false if an equal value was already there (the new one is dropped).
    pub insert: unsafe fn(ptr: *mut u8, element: *mut u8) -> bool,
    /// Removes the value equal to the one at `element`; false if there was none.
    pub remove: unsafe fn(ptr: *mut u8, element: *const u8) -> bool,
    pub clear: unsafe fn(ptr: *mut u8),
}

impl SetInfo {
    pub const fn of<S: ReflectSet>() -> Self {
        Self {
            element: crate::type_info::<S::Item>,
            ordered: S::ORDERED,
            len: set_len_op::<S>,
            elements: set_elements_op::<S>,
            contains: set_contains_op::<S>,
            insert: set_insert_op::<S>,
            remove: set_remove_op::<S>,
            clear: set_clear_op::<S>,
        }
    }
}

impl fmt::Debug for SetInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetInfo")
            .field("element", &format_args!("{}", (self.element)().name))
            .field("ordered", &self.ordered)
            .finish_non_exhaustive()
    }
}

unsafe fn set_len_op<S: ReflectSet>(ptr: *const u8) -> usize {
    // SAFETY: the caller passes a live `S`.
    unsafe { &*ptr.cast::<S>() }.len()
}

unsafe fn set_elements_op<S: ReflectSet>(ptr: *const u8, out: &mut Vec<*const u8>) {
    // SAFETY: the caller passes a live `S`.
    let set = unsafe { &*ptr.cast::<S>() };
    out.reserve(set.len());
    set.for_each(&mut |e| out.push(ptr::from_ref(e).cast()));
}

unsafe fn set_contains_op<S: ReflectSet>(ptr: *const u8, element: *const u8) -> bool {
    // SAFETY: the caller passes a live `S` and a live element.
    let (set, element) = unsafe { (&*ptr.cast::<S>(), &*element.cast::<S::Item>()) };
    set.contains(element)
}

unsafe fn set_insert_op<S: ReflectSet>(ptr: *mut u8, element: *mut u8) -> bool {
    // SAFETY: the caller passes a live `S` it may mutate and an element it gives up.
    unsafe {
        let element = element.cast::<S::Item>().read();
        (*ptr.cast::<S>()).insert(element)
    }
}

unsafe fn set_remove_op<S: ReflectSet>(ptr: *mut u8, element: *const u8) -> bool {
    // SAFETY: the caller passes a live `S` it may mutate and a live element outside it.
    let (set, element) = unsafe { (&mut *ptr.cast::<S>(), &*element.cast::<S::Item>()) };
    set.remove(element)
}

unsafe fn set_clear_op<S: ReflectSet>(ptr: *mut u8) {
    // SAFETY: the caller passes a live `S` it may mutate.
    unsafe { &mut *ptr.cast::<S>() }.clear();
}

/// Which owning pointer a [`PointerInfo`] describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointerKind {
    Box,
    /// Shared: writable only while unique (no other `Rc` or `Weak` to the value).
    Rc,
    /// Shared: writable only while unique (no other `Arc` or `Weak` to the value).
    Arc,
}

#[derive(Clone, Copy)]
pub struct PointerInfo {
    pub pointee: fn() -> &'static TypeInfo,
    pub kind: PointerKind,
    pub get: unsafe fn(ptr: *const u8) -> *const u8,
    /// The pointee for writing, or null when it is shared.
    pub get_mut: unsafe fn(ptr: *mut u8) -> *mut u8,
    /// The pointee for writing, first replacing a shared pointee with an unshared clone of it
    /// (like `Rc::make_mut`); null if it is shared and cannot be cloned.
    pub make_mut: unsafe fn(ptr: *mut u8) -> *mut u8,
}

impl PointerInfo {
    pub const fn of<P: ReflectPointer>() -> Self {
        Self {
            pointee: crate::type_info::<P::Pointee>,
            kind: P::KIND,
            get: pointer_get_op::<P>,
            get_mut: pointer_get_mut_op::<P>,
            make_mut: pointer_make_mut_op::<P>,
        }
    }
}

impl fmt::Debug for PointerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PointerInfo")
            .field("pointee", &format_args!("{}", (self.pointee)().name))
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

unsafe fn pointer_get_op<P: ReflectPointer>(ptr: *const u8) -> *const u8 {
    // SAFETY: the caller passes a live `P`.
    ptr::from_ref(unsafe { &*ptr.cast::<P>() }.pointee()).cast()
}

unsafe fn pointer_get_mut_op<P: ReflectPointer>(ptr: *mut u8) -> *mut u8 {
    // SAFETY: the caller passes a live `P` it may mutate.
    unsafe { &mut *ptr.cast::<P>() }
        .pointee_mut()
        .map_or(ptr::null_mut(), |v| ptr::from_mut(v).cast())
}

unsafe fn pointer_make_mut_op<P: ReflectPointer>(ptr: *mut u8) -> *mut u8 {
    // SAFETY: the caller passes a live `P` it may mutate.
    unsafe { &mut *ptr.cast::<P>() }
        .make_mut()
        .map_or(ptr::null_mut(), |v| ptr::from_mut(v).cast())
}
