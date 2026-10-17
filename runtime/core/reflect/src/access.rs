//! Type-erased borrows: navigate and modify values knowing only their [`TypeInfo`].

use alloc::vec::Vec;
use core::any::Any;
use core::fmt;
use core::marker::PhantomData;

use crate::data::ReflectTrait;
use crate::info::{FieldInfo, TypeInfo, TypeKind, VariantInfo};
use crate::traits::{Reflect, type_info};
use crate::value::Value;

/// Shared type-erased borrow of a reflected value.
#[derive(Clone, Copy)]
pub struct Ref<'a> {
    /// Invariant: points to a live value described by `info`, shared-borrowed for `'a`.
    ptr: *const u8,
    info: &'static TypeInfo,
    _borrow: PhantomData<&'a ()>,
}

/// Exclusive type-erased borrow of a reflected value.
pub struct Mut<'a> {
    /// Invariant: points to a live value described by `info`, exclusively borrowed for `'a`.
    ptr: *mut u8,
    info: &'static TypeInfo,
    _borrow: PhantomData<&'a mut ()>,
}

impl fmt::Debug for Ref<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ref<{}>({:p})", self.info.name, self.ptr)
    }
}

impl fmt::Debug for Mut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mut<{}>({:p})", self.info.name, self.ptr)
    }
}

/// Why a path lookup failed, and at which byte of the path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PathError {
    pub kind: PathErrorKind,
    pub position: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathErrorKind {
    /// Not of the form `a.b[2].c`.
    Syntax,
    /// No such field (in the active variant, for enums), or the value has no fields.
    NoField,
    /// The value is not an array or list.
    NotIndexable,
    IndexOutOfBounds,
    /// The path goes through an `Option` that is `None`.
    NoneOption,
    /// The value at the path is not of the requested type.
    TypeMismatch,
    /// A mutable path goes through an `Rc`/`Arc` whose pointee is shared.
    SharedPointer,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            PathErrorKind::Syntax => "invalid path syntax",
            PathErrorKind::NoField => "no such field",
            PathErrorKind::NotIndexable => "value cannot be indexed",
            PathErrorKind::IndexOutOfBounds => "index out of bounds",
            PathErrorKind::NoneOption => "option is None",
            PathErrorKind::TypeMismatch => "type mismatch",
            PathErrorKind::SharedPointer => "shared pointer cannot be written through",
        };
        write!(f, "{what} at byte {} of the path", self.position)
    }
}

// Raw navigation shared by `Ref` and `Mut`. `mutable` selects the `_mut` operations so pointers
// handed to `Mut` carry write permission; `Ref` never writes through the result.

/// # Safety
/// `ptr` points to a live value described by `info` (exclusively borrowed if `mutable`).
unsafe fn field_raw(
    ptr: *mut u8,
    info: &'static TypeInfo,
    index: usize,
    mutable: bool,
) -> Option<(*mut u8, &'static FieldInfo)> {
    match &info.kind {
        TypeKind::Struct(_) | TypeKind::Tuple(_) => {
            let field = info.fields().get(index)?;
            // SAFETY: `TYPE_INFO` gives in-bounds offsets of fields of this type.
            Some((unsafe { ptr.add(field.offset) }, field))
        }
        TypeKind::Enum(e) => {
            // SAFETY: the caller's guarantee.
            let variant = e.variants.get(unsafe { (e.variant_index)(ptr) })?;
            let field = variant.fields.get(index)?;
            let child = if mutable {
                // SAFETY: the caller's guarantee.
                unsafe { (e.field_ptr_mut)(ptr, index) }
            } else {
                // SAFETY: the caller's guarantee.
                unsafe { (e.field_ptr)(ptr, index) }.cast_mut()
            };
            (!child.is_null()).then_some((child, field))
        }
        _ => None,
    }
}

/// # Safety
/// As for [`field_raw`].
unsafe fn active_fields(ptr: *const u8, info: &TypeInfo) -> &'static [FieldInfo] {
    match &info.kind {
        TypeKind::Struct(s) => s.fields,
        TypeKind::Tuple(t) => t.fields,
        TypeKind::Enum(e) => {
            // SAFETY: the caller's guarantee.
            let index = unsafe { (e.variant_index)(ptr) };
            e.variants.get(index).map_or(&[], |v| v.fields)
        }
        _ => &[],
    }
}

/// # Safety
/// As for [`field_raw`].
unsafe fn index_raw(
    ptr: *mut u8,
    info: &'static TypeInfo,
    index: usize,
    mutable: bool,
) -> Result<(*mut u8, &'static TypeInfo), PathErrorKind> {
    match &info.kind {
        TypeKind::Array(a) => {
            if index >= a.len {
                return Err(PathErrorKind::IndexOutOfBounds);
            }
            let element = (a.element)();
            // SAFETY: arrays are `len` contiguous elements and `index < len`.
            Ok((unsafe { ptr.add(index * element.size) }, element))
        }
        TypeKind::List(l) => {
            let child = if mutable {
                // SAFETY: the caller's guarantee.
                unsafe { (l.element_ptr_mut)(ptr, index) }
            } else {
                // SAFETY: the caller's guarantee.
                unsafe { (l.element_ptr)(ptr, index) }.cast_mut()
            };
            if child.is_null() {
                Err(PathErrorKind::IndexOutOfBounds)
            } else {
                Ok((child, (l.element)()))
            }
        }
        _ => Err(PathErrorKind::NotIndexable),
    }
}

/// Steps into `Some` and through pointers, so paths see through options, boxes and `Rc`s.
///
/// # Safety
/// As for [`field_raw`].
unsafe fn some_raw(
    mut ptr: *mut u8,
    mut info: &'static TypeInfo,
    mutable: bool,
) -> Result<(*mut u8, &'static TypeInfo), PathErrorKind> {
    loop {
        match &info.kind {
            TypeKind::Option(o) => {
                ptr = if mutable {
                    // SAFETY: the caller's guarantee.
                    unsafe { (o.get_mut)(ptr) }
                } else {
                    // SAFETY: the caller's guarantee.
                    unsafe { (o.get)(ptr) }.cast_mut()
                };
                if ptr.is_null() {
                    return Err(PathErrorKind::NoneOption);
                }
                info = (o.some)();
            }
            TypeKind::Pointer(p) => {
                ptr = if mutable {
                    // SAFETY: the caller's guarantee.
                    unsafe { (p.get_mut)(ptr) }
                } else {
                    // SAFETY: the caller's guarantee.
                    unsafe { (p.get)(ptr) }.cast_mut()
                };
                if ptr.is_null() {
                    return Err(PathErrorKind::SharedPointer);
                }
                info = (p.pointee)();
            }
            _ => return Ok((ptr, info)),
        }
    }
}

enum Segment<'p> {
    Field(&'p str),
    Index(usize),
}

/// Splits `a.b[2][3].c` into segments with their byte positions.
fn parse_path(
    path: &str,
    mut visit: impl FnMut(Segment<'_>, usize) -> Result<(), PathError>,
) -> Result<(), PathError> {
    let syntax = |position| PathError {
        kind: PathErrorKind::Syntax,
        position,
    };
    if path.is_empty() {
        return Ok(());
    }
    let bytes = path.as_bytes();
    let mut i = 0;
    loop {
        // A field name, which may be omitted only before an index (`[0]` at the start).
        let start = i;
        while i < bytes.len() && !matches!(bytes[i], b'.' | b'[') {
            i += 1;
        }
        if i > start {
            visit(Segment::Field(&path[start..i]), start)?;
        } else if i >= bytes.len() || bytes[i] != b'[' {
            return Err(syntax(i));
        }
        while i < bytes.len() && bytes[i] == b'[' {
            let open = i;
            i += 1;
            let digits = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == digits || i >= bytes.len() || bytes[i] != b']' {
                return Err(syntax(open));
            }
            let index = path[digits..i].parse().map_err(|_| syntax(digits))?;
            i += 1;
            visit(Segment::Index(index), open)?;
        }
        if i == bytes.len() {
            return Ok(());
        }
        if bytes[i] != b'.' {
            return Err(syntax(i));
        }
        i += 1;
    }
}

/// # Safety
/// As for [`field_raw`].
unsafe fn path_raw(
    mut ptr: *mut u8,
    mut info: &'static TypeInfo,
    path: &str,
    mutable: bool,
) -> Result<(*mut u8, &'static TypeInfo), PathError> {
    // Report syntax errors anywhere in the path before lookup errors.
    parse_path(path, |_, _| Ok(()))?;
    parse_path(path, |segment, position| {
        let error = |kind| PathError { kind, position };
        // SAFETY: (for every call below) `ptr`/`info` start as the caller's value and are only
        // replaced by children of it, which inherit its guarantee.
        (ptr, info) = unsafe { some_raw(ptr, info, mutable) }.map_err(error)?;
        match segment {
            Segment::Field(name) => {
                // SAFETY: see above.
                let fields = unsafe { active_fields(ptr, info) };
                let index = fields
                    .iter()
                    .position(|f| f.name == name)
                    .ok_or(error(PathErrorKind::NoField))?;
                // SAFETY: see above.
                let (child, field) = unsafe { field_raw(ptr, info, index, mutable) }
                    .ok_or(error(PathErrorKind::NoField))?;
                (ptr, info) = (child, (field.ty)());
            }
            Segment::Index(index) => {
                // SAFETY: see above.
                (ptr, info) = unsafe { index_raw(ptr, info, index, mutable) }.map_err(error)?;
            }
        }
        Ok(())
    })?;
    Ok((ptr, info))
}

impl<'a> Ref<'a> {
    pub fn new<T: Reflect>(value: &'a T) -> Self {
        Self {
            ptr: core::ptr::from_ref(value).cast(),
            info: type_info::<T>(),
            _borrow: PhantomData,
        }
    }

    /// # Safety
    /// `ptr` must point to a live value described by `info` that stays shared-borrowed for `'a`.
    pub unsafe fn from_raw(ptr: *const u8, info: &'static TypeInfo) -> Self {
        Self {
            ptr,
            info,
            _borrow: PhantomData,
        }
    }

    pub fn info(&self) -> &'static TypeInfo {
        self.info
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    pub fn downcast<T: Reflect>(self) -> Option<&'a T> {
        // SAFETY: the value is a `T` (checked by Rust type id) borrowed for `'a`.
        self.info
            .is::<T>()
            .then(|| unsafe { &*self.ptr.cast::<T>() })
    }

    /// Field `index` of a struct, or of the active variant of an enum.
    pub fn field_at(self, index: usize) -> Option<(&'static FieldInfo, Ref<'a>)> {
        // SAFETY: the invariant of `self`.
        let (ptr, field) = unsafe { field_raw(self.ptr.cast_mut(), self.info, index, false) }?;
        // SAFETY: a field of a value borrowed for `'a`.
        Some((field, unsafe { Ref::from_raw(ptr, (field.ty)()) }))
    }

    /// Field by reflected name, of a struct or of the active variant of an enum.
    pub fn field(self, name: &str) -> Option<Ref<'a>> {
        let index = self.fields_info().iter().position(|f| f.name == name)?;
        self.field_at(index).map(|(_, r)| r)
    }

    /// Descriptions of the fields `field_at` can reach.
    pub fn fields_info(&self) -> &'static [FieldInfo] {
        // SAFETY: the invariant of `self`.
        unsafe { active_fields(self.ptr, self.info) }
    }

    /// The fields of a struct, or of the active variant of an enum.
    pub fn fields(self) -> impl Iterator<Item = (&'static FieldInfo, Ref<'a>)> {
        (0..self.fields_info().len()).filter_map(move |i| self.field_at(i))
    }

    /// The active variant of an enum.
    pub fn variant(&self) -> Option<(usize, &'static VariantInfo)> {
        let TypeKind::Enum(e) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`.
        let index = unsafe { (e.variant_index)(self.ptr) };
        Some((index, e.variants.get(index)?))
    }

    /// Element count of an array, list, map or set.
    pub fn len(&self) -> Option<usize> {
        match &self.info.kind {
            TypeKind::Array(a) => Some(a.len),
            // SAFETY: the invariant of `self`.
            TypeKind::List(l) => Some(unsafe { (l.len)(self.ptr) }),
            // SAFETY: the invariant of `self`.
            TypeKind::Map(m) => Some(unsafe { (m.len)(self.ptr) }),
            // SAFETY: the invariant of `self`.
            TypeKind::Set(s) => Some(unsafe { (s.len)(self.ptr) }),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|n| n == 0)
    }

    /// Element of an array or list.
    pub fn index(self, index: usize) -> Option<Ref<'a>> {
        // SAFETY: the invariant of `self`.
        let (ptr, info) =
            unsafe { index_raw(self.ptr.cast_mut(), self.info, index, false) }.ok()?;
        // SAFETY: an element of a value borrowed for `'a`.
        Some(unsafe { Ref::from_raw(ptr, info) })
    }

    /// The contents of an `Option`: `None` if this is not an option, `Some(None)` if it is empty.
    pub fn option(self) -> Option<Option<Ref<'a>>> {
        let TypeKind::Option(o) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`.
        let ptr = unsafe { (o.get)(self.ptr) };
        // SAFETY: the contents of a value borrowed for `'a`.
        Some((!ptr.is_null()).then(|| unsafe { Ref::from_raw(ptr, (o.some)()) }))
    }

    pub fn as_str(self) -> Option<&'a str> {
        let TypeKind::String(s) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`; the string lives as long as the borrow.
        Some(unsafe { &*(s.as_str)(self.ptr) })
    }

    /// The value at `path`, such as `"transform.position[2]"` or `"items[0].name"`. Options are
    /// looked through; empty options are an error.
    pub fn path(self, path: &str) -> Result<Ref<'a>, PathError> {
        // SAFETY: the invariant of `self`.
        let (ptr, info) = unsafe { path_raw(self.ptr.cast_mut(), self.info, path, false) }?;
        // SAFETY: a part of a value borrowed for `'a`.
        Ok(unsafe { Ref::from_raw(ptr, info) })
    }

    /// Clones the value, if its type supports it.
    pub fn to_value(self) -> Option<Value> {
        Value::clone_of(self)
    }

    /// The pointee of a `Box`, `Rc` or `Arc`.
    pub fn pointee(self) -> Option<Ref<'a>> {
        let TypeKind::Pointer(p) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`; the pointee lives as long as the borrow of the pointer.
        Some(unsafe { Ref::from_raw((p.get)(self.ptr), (p.pointee)()) })
    }

    /// The `(key, value)` entries of a map, in its iteration order (sorted for ordered maps).
    pub fn map_entries(self) -> Option<Vec<(Ref<'a>, Ref<'a>)>> {
        let TypeKind::Map(m) = &self.info.kind else {
            return None;
        };
        let mut raw = Vec::new();
        // SAFETY: the invariant of `self`.
        unsafe { (m.entries)(self.ptr, &mut raw) };
        let (key, value) = ((m.key)(), (m.value)());
        let entries = raw.into_iter().map(|(k, v)| {
            // SAFETY: an entry of a map borrowed for `'a`.
            unsafe { (Ref::from_raw(k, key), Ref::from_raw(v, value)) }
        });
        Some(entries.collect())
    }

    /// The value of `key` in a map. `None` if this is not a map, `key` is not of its key type, or
    /// the key is absent.
    pub fn map_get(self, key: Ref<'_>) -> Option<Ref<'a>> {
        let TypeKind::Map(m) = &self.info.kind else {
            return None;
        };
        if !key.info.same_type((m.key)()) {
            return None;
        }
        // SAFETY: the invariant of `self`; `key` is a live key.
        let ptr = unsafe { (m.get)(self.ptr, key.ptr) };
        // SAFETY: a value of a map borrowed for `'a`.
        (!ptr.is_null()).then(|| unsafe { Ref::from_raw(ptr, (m.value)()) })
    }

    /// The elements of a set, in its iteration order (sorted for ordered sets).
    pub fn set_elements(self) -> Option<Vec<Ref<'a>>> {
        let TypeKind::Set(s) = &self.info.kind else {
            return None;
        };
        let mut raw = Vec::new();
        // SAFETY: the invariant of `self`.
        unsafe { (s.elements)(self.ptr, &mut raw) };
        let element = (s.element)();
        let elements = raw.into_iter().map(|e| {
            // SAFETY: an element of a set borrowed for `'a`.
            unsafe { Ref::from_raw(e, element) }
        });
        Some(elements.collect())
    }

    /// Whether a set contains `element`; `None` if this is not a set of `element`'s type.
    pub fn set_contains(self, element: Ref<'_>) -> Option<bool> {
        let TypeKind::Set(s) = &self.info.kind else {
            return None;
        };
        if !element.info.same_type((s.element)()) {
            return None;
        }
        // SAFETY: the invariant of `self`; `element` is a live element.
        Some(unsafe { (s.contains)(self.ptr, element.ptr) })
    }

    /// Type data of type `D` for this value's type, static or registered (see
    /// [`find_data`](crate::find_data)).
    pub fn data<D: Any>(&self) -> Option<&'static D> {
        crate::registry::find_data::<D>(self.info)
    }

    /// The value as the trait object `D` (`dyn Trait`), through the type's
    /// [`ReflectTrait<D>`] data: `value.cast::<dyn Visible>()`.
    pub fn cast<D: ?Sized + 'static>(self) -> Option<&'a D> {
        self.data::<ReflectTrait<D>>()?.get(self)
    }
}

impl<'a> Mut<'a> {
    pub fn new<T: Reflect>(value: &'a mut T) -> Self {
        Self {
            ptr: core::ptr::from_mut(value).cast(),
            info: type_info::<T>(),
            _borrow: PhantomData,
        }
    }

    /// # Safety
    /// `ptr` must point to a live value described by `info` that stays exclusively borrowed for
    /// `'a`.
    pub unsafe fn from_raw(ptr: *mut u8, info: &'static TypeInfo) -> Self {
        Self {
            ptr,
            info,
            _borrow: PhantomData,
        }
    }

    pub fn info(&self) -> &'static TypeInfo {
        self.info
    }

    pub fn as_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub fn as_ref(&self) -> Ref<'_> {
        // SAFETY: shared reborrow of the exclusive borrow.
        unsafe { Ref::from_raw(self.ptr, self.info) }
    }

    pub fn reborrow(&mut self) -> Mut<'_> {
        // SAFETY: exclusive reborrow; `self` is unusable while it lives.
        unsafe { Mut::from_raw(self.ptr, self.info) }
    }

    pub fn downcast<T: Reflect>(self) -> Option<&'a mut T> {
        // SAFETY: the value is a `T` (checked by Rust type id) exclusively borrowed for `'a`.
        self.info
            .is::<T>()
            .then(|| unsafe { &mut *self.ptr.cast::<T>() })
    }

    /// See [`Ref::field_at`].
    pub fn field_at(self, index: usize) -> Option<(&'static FieldInfo, Mut<'a>)> {
        // SAFETY: the invariant of `self`.
        let (ptr, field) = unsafe { field_raw(self.ptr, self.info, index, true) }?;
        // SAFETY: a field of a value exclusively borrowed for `'a`, and `self` is consumed.
        Some((field, unsafe { Mut::from_raw(ptr, (field.ty)()) }))
    }

    /// See [`Ref::field`].
    pub fn field(self, name: &str) -> Option<Mut<'a>> {
        let index = self
            .as_ref()
            .fields_info()
            .iter()
            .position(|f| f.name == name)?;
        self.field_at(index).map(|(_, m)| m)
    }

    /// See [`Ref::index`].
    pub fn index(self, index: usize) -> Option<Mut<'a>> {
        // SAFETY: the invariant of `self`.
        let (ptr, info) = unsafe { index_raw(self.ptr, self.info, index, true) }.ok()?;
        // SAFETY: an element of a value exclusively borrowed for `'a`, and `self` is consumed.
        Some(unsafe { Mut::from_raw(ptr, info) })
    }

    /// See [`Ref::option`].
    pub fn option(self) -> Option<Option<Mut<'a>>> {
        let TypeKind::Option(o) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`.
        let ptr = unsafe { (o.get_mut)(self.ptr) };
        // SAFETY: the contents of a value exclusively borrowed for `'a`; `self` is consumed.
        Some((!ptr.is_null()).then(|| unsafe { Mut::from_raw(ptr, (o.some)()) }))
    }

    /// See [`Ref::path`].
    pub fn path(self, path: &str) -> Result<Mut<'a>, PathError> {
        // SAFETY: the invariant of `self`.
        let (ptr, info) = unsafe { path_raw(self.ptr, self.info, path, true) }?;
        // SAFETY: a part of a value exclusively borrowed for `'a`, and `self` is consumed.
        Ok(unsafe { Mut::from_raw(ptr, info) })
    }

    /// Replaces the value; gives `value` back if this is not a `T`.
    pub fn set<T: Reflect>(&mut self, value: T) -> Result<(), T> {
        match self.reborrow().downcast::<T>() {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(value),
        }
    }

    /// Replaces the value with a clone of `src`. False (unchanged) if `src` is another type or
    /// cannot be cloned.
    pub fn assign(&mut self, src: Ref<'_>) -> bool {
        if !self.info.same_type(src.info()) {
            return false;
        }
        match Value::clone_of(src) {
            Some(value) => {
                self.replace(value);
                true
            }
            None => false,
        }
    }

    /// Replaces the value with its type's default. False (unchanged) if it has none.
    pub fn reset(&mut self) -> bool {
        match Value::default_of(self.info) {
            Some(value) => {
                self.replace(value);
                true
            }
            None => false,
        }
    }

    /// Moves `value`, which must be of this type, into place, dropping the old value.
    fn replace(&mut self, value: Value) {
        debug_assert!(self.info.same_type(value.info()));
        // SAFETY: the invariant of `self`; dropping in place and refilling with a value of the
        // same type keeps it valid.
        unsafe {
            (self.info.ops.drop)(self.ptr);
            value.move_into(self.ptr);
        }
    }

    /// Sets a string. False if this is not a string.
    pub fn set_str(&mut self, value: &str) -> bool {
        let TypeKind::String(s) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (s.set)(self.ptr, value) };
        true
    }

    /// Resizes a list; see [`ReflectList::resize`](crate::ReflectList::resize).
    pub fn resize(&mut self, len: usize) -> bool {
        let TypeKind::List(l) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (l.resize)(self.ptr, len) }
    }

    pub fn insert_default(&mut self, index: usize) -> bool {
        let TypeKind::List(l) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (l.insert_default)(self.ptr, index) }
    }

    pub fn remove(&mut self, index: usize) -> bool {
        let TypeKind::List(l) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (l.remove)(self.ptr, index) }
    }

    /// Switches an enum to variant `index` with default fields.
    pub fn set_variant(&mut self, index: usize) -> bool {
        let TypeKind::Enum(e) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (e.set_variant)(self.ptr, index) }
    }

    /// Sets an `Option` to `None`.
    pub fn set_none(&mut self) -> bool {
        let TypeKind::Option(o) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (o.set_none)(self.ptr) };
        true
    }

    /// Sets an `Option` to `Some(default)`.
    pub fn set_some_default(&mut self) -> bool {
        let TypeKind::Option(o) = &self.info.kind else {
            return false;
        };
        // SAFETY: the invariant of `self`.
        unsafe { (o.set_some_default)(self.ptr) }
    }

    /// The pointee of a `Box`, `Rc` or `Arc`: `None` if this is not a pointer, `Some(None)` if
    /// the pointee is shared (another `Rc`/`Arc` or `Weak` points to it).
    pub fn pointee(self) -> Option<Option<Mut<'a>>> {
        let TypeKind::Pointer(p) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`.
        let ptr = unsafe { (p.get_mut)(self.ptr) };
        // SAFETY: the pointee of a value exclusively borrowed for `'a`; `self` is consumed.
        Some((!ptr.is_null()).then(|| unsafe { Mut::from_raw(ptr, (p.pointee)()) }))
    }

    /// The pointee of a `Box`, `Rc` or `Arc`, first replacing a shared pointee with a clone of
    /// it, like `Rc::make_mut`. `None` if this is not a pointer, or the pointee is shared and
    /// cannot be cloned.
    pub fn pointee_make_mut(self) -> Option<Mut<'a>> {
        let TypeKind::Pointer(p) = &self.info.kind else {
            return None;
        };
        // SAFETY: the invariant of `self`.
        let ptr = unsafe { (p.make_mut)(self.ptr) };
        // SAFETY: the pointee of a value exclusively borrowed for `'a`; `self` is consumed.
        (!ptr.is_null()).then(|| unsafe { Mut::from_raw(ptr, (p.pointee)()) })
    }

    /// See [`Ref::map_get`].
    pub fn map_get_mut(self, key: Ref<'_>) -> Option<Mut<'a>> {
        let TypeKind::Map(m) = &self.info.kind else {
            return None;
        };
        if !key.info.same_type((m.key)()) {
            return None;
        }
        // SAFETY: the invariant of `self`; `key` is a live key, and not part of the map since the
        // map is exclusively borrowed.
        let ptr = unsafe { (m.get_mut)(self.ptr, key.ptr) };
        // SAFETY: a value of a map exclusively borrowed for `'a`; `self` is consumed.
        (!ptr.is_null()).then(|| unsafe { Mut::from_raw(ptr, (m.value)()) })
    }

    /// The entries of a map with writable values. Keys stay read-only: changing one would break
    /// the map.
    pub fn map_entries_mut(self) -> Option<Vec<(Ref<'a>, Mut<'a>)>> {
        let TypeKind::Map(m) = &self.info.kind else {
            return None;
        };
        let mut raw = Vec::new();
        // SAFETY: the invariant of `self`.
        unsafe { (m.entries_mut)(self.ptr, &mut raw) };
        let (key, value) = ((m.key)(), (m.value)());
        let entries = raw.into_iter().map(|(k, v)| {
            // SAFETY: disjoint parts of a map exclusively borrowed for `'a`; `self` is consumed,
            // and no operation that could move the entries can run while they are borrowed.
            unsafe { (Ref::from_raw(k, key), Mut::from_raw(v, value)) }
        });
        Some(entries.collect())
    }

    /// Inserts into a map, replacing the value of an equal key. Gives both values back if this is
    /// not a map of their types.
    pub fn map_insert(&mut self, key: Value, value: Value) -> Result<(), (Value, Value)> {
        let TypeKind::Map(m) = &self.info.kind else {
            return Err((key, value));
        };
        if !key.info().same_type((m.key)()) || !value.info().same_type((m.value)()) {
            return Err((key, value));
        }
        let map = self.ptr;
        // SAFETY: the invariant of `self`; both values are of the map's types, and `insert`
        // moves them out of their storage, as `consume` requires.
        unsafe { key.consume(|k| value.consume(|v| (m.insert)(map, k, v))) };
        Ok(())
    }

    /// Removes `key` from a map. False if this is not a map of `key`'s type or the key is absent.
    pub fn map_remove(&mut self, key: Ref<'_>) -> bool {
        let TypeKind::Map(m) = &self.info.kind else {
            return false;
        };
        if !key.info.same_type((m.key)()) {
            return false;
        }
        // SAFETY: the invariant of `self`; `key` is a live key outside the map.
        unsafe { (m.remove)(self.ptr, key.ptr) }
    }

    /// Inserts into a set: `Ok(false)` if an equal element was already present (`element` is
    /// dropped). Gives `element` back if this is not a set of its type.
    pub fn set_insert(&mut self, element: Value) -> Result<bool, Value> {
        let TypeKind::Set(s) = &self.info.kind else {
            return Err(element);
        };
        if !element.info().same_type((s.element)()) {
            return Err(element);
        }
        let set = self.ptr;
        // SAFETY: the invariant of `self`; the element is of the set's type, and `insert` moves
        // it out of its storage, as `consume` requires.
        Ok(unsafe { element.consume(|e| (s.insert)(set, e)) })
    }

    /// Removes `element` from a set. False if this is not a set of its type or it is absent.
    pub fn set_remove(&mut self, element: Ref<'_>) -> bool {
        let TypeKind::Set(s) = &self.info.kind else {
            return false;
        };
        if !element.info.same_type((s.element)()) {
            return false;
        }
        // SAFETY: the invariant of `self`; `element` is a live element outside the set.
        unsafe { (s.remove)(self.ptr, element.ptr) }
    }

    /// Empties a list, map or set. False for other kinds.
    pub fn clear(&mut self) -> bool {
        match &self.info.kind {
            // SAFETY: the invariant of `self`.
            TypeKind::List(l) => unsafe { (l.resize)(self.ptr, 0) },
            TypeKind::Map(m) => {
                // SAFETY: the invariant of `self`.
                unsafe { (m.clear)(self.ptr) };
                true
            }
            TypeKind::Set(s) => {
                // SAFETY: the invariant of `self`.
                unsafe { (s.clear)(self.ptr) };
                true
            }
            _ => false,
        }
    }

    /// See [`Ref::cast`].
    pub fn cast<D: ?Sized + 'static>(self) -> Option<&'a mut D> {
        crate::registry::find_data::<ReflectTrait<D>>(self.info)?.get_mut(self)
    }
}

/// `root.path` as a `V`.
pub fn get_path<'a, V: Reflect>(root: &'a impl Reflect, path: &str) -> Result<&'a V, PathError> {
    Ref::new(root).path(path)?.downcast().ok_or(PathError {
        kind: PathErrorKind::TypeMismatch,
        position: path.len(),
    })
}

/// Sets `root.path` to `value`.
pub fn set_path<V: Reflect>(
    root: &mut impl Reflect,
    path: &str,
    value: V,
) -> Result<(), PathError> {
    Mut::new(root)
        .path(path)?
        .set(value)
        .map_err(|_| PathError {
            kind: PathErrorKind::TypeMismatch,
            position: path.len(),
        })
}
