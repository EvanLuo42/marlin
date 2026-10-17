//! Zero-copy layouts for cooked data.
//!
//! Cooked data is a byte buffer of `#[repr(C)]` [`Pod`] structs that refer to each other through
//! [`RelPtr`] and [`RelSlice`]: signed 32-bit offsets relative to the pointer's own address. The
//! buffer has no absolute addresses, so it can be loaded or mmapped anywhere and used in place,
//! and sub-buffers can be moved or concatenated without fixups.
//!
//! [`CookedBuilder`] writes such a buffer (in the cooker); [`CookedView`] reads it. Every access
//! through the view checks that the target lies inside the buffer and is aligned for its type, so
//! a corrupt or truncated file yields a [`LayoutError`], never undefined behavior. The checks are
//! a few comparisons per access; nothing is validated up front.
//!
//! Cooked data is platform-specific (native layout and alignment), like everything the cooker
//! produces. Every supported target is little-endian (checked at compile time), so there is no
//! byte swapping. Offsets are 32-bit: a buffer that links across more than 2 GiB is refused with
//! [`LayoutError::OffsetOverflow`], and a buffer with a header is limited to 4 GiB.
//!
//! # Header
//!
//! [`CookedBuilder::with_header`] starts the buffer with a [`CookedHeader`]: magic, format
//! version, the [layout hash](layout_hash) of the root type, the total length and the alignment
//! the buffer needs. [`CookedView::with_header`] checks all of it before handing out the root,
//! so data cooked for another layout of the type (an older engine, another platform) is refused
//! with a clear error instead of being misread. Headerless buffers ([`CookedBuilder::new`],
//! [`CookedView::new`]) are for sub-buffers embedded in a larger format that has its own header.
//!
//! ```ignore
//! #[derive(Pod, Clone, Copy)]
//! #[repr(C)]
//! struct Mesh {
//!     vertices: RelSlice<[f32; 3]>,
//!     name: RelSlice<u8>,
//! }
//!
//! let (mut builder, mesh) = CookedBuilder::with_header(&Mesh { .. });
//! // push the vertices and the name, link them into `mesh` ...
//! let buffer = builder.finish();
//!
//! let view = CookedView::with_header::<Mesh>(&buffer)?;
//! let mesh: &Mesh = view.root()?;
//! let vertices: &[[f32; 3]] = view.slice(&mesh.vertices)?;
//! ```

use alloc::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use alloc::vec::Vec;
use core::any::TypeId as RustTypeId;
use core::fmt;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::ops::Deref;
use core::ptr::NonNull;

pub use marlin_serial_derive::Pod;

/// Plain data that can be copied to and from bytes.
///
/// Derive it with `#[derive(Pod)]`, which checks the requirements below at compile time and
/// describes the fields in [`LAYOUT`](Pod::LAYOUT).
///
/// # Safety
///
/// The type must be `Copy`, valid for every bit pattern, without padding bytes (so its bytes can
/// be written out) and without interior mutability. `LAYOUT` must describe the type (its default,
/// [`PodLayout::opaque`], always does).
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not `Pod`",
    note = "derive it with `#[derive(Pod)]` on a `#[repr(C)]` struct of `Pod` fields"
)]
pub unsafe trait Pod: Copy + 'static {
    /// Description of the layout, hashed into the cooked header. Hand-written impls default to
    /// size and alignment only; `#[derive(Pod)]` lists every field.
    const LAYOUT: PodLayout = PodLayout::opaque::<Self>();

    /// Compile-time checks of `#[derive(Pod)]` (no padding), evaluated for every type the
    /// builder and the view are used with.
    #[doc(hidden)]
    const CHECK: () = ();
}

/// How a [`Pod`] type is laid out: what its [`layout_hash`] is computed from.
#[derive(Clone, Copy, Debug)]
pub struct PodLayout {
    /// Rust type id; only used to recognize a type seen before while hashing, so recursive
    /// layouts (a node with a `RelSlice` of nodes) hash finitely. Not part of the hash.
    pub id: fn() -> RustTypeId,
    pub size: usize,
    pub align: usize,
    pub kind: PodKind,
}

impl PodLayout {
    /// Size and alignment of `T` only: the default for hand-written `Pod` impls.
    pub const fn opaque<T: 'static>() -> Self {
        Self {
            id: RustTypeId::of::<T>,
            size: size_of::<T>(),
            align: align_of::<T>(),
            kind: PodKind::Opaque,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PodKind {
    Scalar(PodScalar),
    Array {
        element: fn() -> &'static PodLayout,
        len: usize,
    },
    /// Fields in declaration order.
    Struct(&'static [PodField]),
    /// A [`RelPtr`] to the type.
    RelPtr(fn() -> &'static PodLayout),
    /// A [`RelSlice`] of the type.
    RelSlice(fn() -> &'static PodLayout),
    /// Known only by size and alignment.
    Opaque,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PodScalar {
    U8,
    U16,
    U32,
    U64,
    U128,
    I8,
    I16,
    I32,
    I64,
    I128,
    F32,
    F64,
}

#[derive(Clone, Copy, Debug)]
pub struct PodField {
    pub name: &'static str,
    pub offset: usize,
    pub ty: fn() -> &'static PodLayout,
}

/// The layout description of `T`. A `fn` so layouts can refer to each other lazily.
pub fn pod_layout<T: Pod>() -> &'static PodLayout {
    const { &T::LAYOUT }
}

/// Stable hash of `T`'s layout: sizes, alignments, field names and offsets and scalar kinds,
/// through arrays and relative pointers, recursively. It is the same on every build and
/// platform that lays `T` out the same way, and changes when a field is added, removed,
/// renamed, moved or retyped. Type names are not part of it, so moving a type between modules
/// keeps it. Types with hand-written `Pod` impls contribute only their size and alignment.
pub fn layout_hash<T: Pod>() -> u64 {
    hash_layout(pod_layout::<T>())
}

/// [`layout_hash`] of a layout description.
pub fn hash_layout(layout: &'static PodLayout) -> u64 {
    let mut out = Vec::new();
    encode_layout(layout, &mut Vec::new(), &mut out);
    marlin_hash::xxh3_64(&out)
}

/// Appends a canonical encoding of `layout` to `out`. A type met before is encoded as a
/// back-reference to its position in `seen`, which is filled in traversal order, so the encoding
/// is deterministic and finite for recursive layouts.
fn encode_layout(layout: &'static PodLayout, seen: &mut Vec<RustTypeId>, out: &mut Vec<u8>) {
    let id = (layout.id)();
    if let Some(index) = seen.iter().position(|s| *s == id) {
        out.push(0xff);
        out.extend_from_slice(&(index as u32).to_le_bytes());
        return;
    }
    seen.push(id);
    out.extend_from_slice(&(layout.size as u64).to_le_bytes());
    out.extend_from_slice(&(layout.align as u64).to_le_bytes());
    match layout.kind {
        PodKind::Scalar(s) => out.extend_from_slice(&[1, s as u8]),
        PodKind::Array { element, len } => {
            out.push(2);
            out.extend_from_slice(&(len as u64).to_le_bytes());
            encode_layout(element(), seen, out);
        }
        PodKind::Struct(fields) => {
            out.push(3);
            out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
            for field in fields {
                out.extend_from_slice(&(field.name.len() as u32).to_le_bytes());
                out.extend_from_slice(field.name.as_bytes());
                out.extend_from_slice(&(field.offset as u64).to_le_bytes());
                encode_layout((field.ty)(), seen, out);
            }
        }
        PodKind::RelPtr(target) => {
            out.push(4);
            encode_layout(target(), seen, out);
        }
        PodKind::RelSlice(element) => {
            out.push(5);
            encode_layout(element(), seen, out);
        }
        PodKind::Opaque => out.push(6),
    }
}

macro_rules! pod {
    ($($ty:ident => $scalar:ident),*) => {$(
        // SAFETY: integers and floats are valid for every bit pattern and have no padding.
        unsafe impl Pod for $ty {
            const LAYOUT: PodLayout = PodLayout {
                kind: PodKind::Scalar(PodScalar::$scalar),
                ..PodLayout::opaque::<Self>()
            };
        }
    )*};
}

pod!(
    u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64, i128 => I128,
    f32 => F32, f64 => F64
);

// SAFETY: an array of `Pod` has no padding between elements and every bit pattern is valid.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {
    const LAYOUT: PodLayout = PodLayout {
        kind: PodKind::Array {
            element: pod_layout::<T>,
            len: N,
        },
        ..PodLayout::opaque::<Self>()
    };
    const CHECK: () = T::CHECK;
}

// SAFETY: zero-sized, no bytes at all.
unsafe impl<T: ?Sized + 'static> Pod for PhantomData<T> {
    const LAYOUT: PodLayout = PodLayout {
        kind: PodKind::Struct(&[]),
        ..PodLayout::opaque::<Self>()
    };
}

/// Offset to a `T`, relative to the address of the `RelPtr` itself. Zero means null.
#[repr(C)]
pub struct RelPtr<T> {
    offset: i32,
    _target: PhantomData<fn() -> T>,
}

/// Offset to `len` contiguous `T`s, relative to the address of the `RelSlice` itself.
#[repr(C)]
pub struct RelSlice<T> {
    offset: i32,
    len: u32,
    _target: PhantomData<fn() -> T>,
}

// Manual impls: derives would require `T: Copy`.
impl<T> Clone for RelPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for RelPtr<T> {}

impl<T> Clone for RelSlice<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for RelSlice<T> {}

impl<T> PartialEq for RelPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
    }
}

impl<T> Eq for RelPtr<T> {}

impl<T> PartialEq for RelSlice<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.offset, self.len) == (other.offset, other.len)
    }
}

impl<T> Eq for RelSlice<T> {}

// SAFETY: an `i32` (plus a zero-sized marker); any value is a valid offset to check later.
unsafe impl<T: Pod> Pod for RelPtr<T> {
    const LAYOUT: PodLayout = PodLayout {
        kind: PodKind::RelPtr(pod_layout::<T>),
        ..PodLayout::opaque::<Self>()
    };
}
// SAFETY: two 4-byte integers, no padding; any values are valid to check later.
unsafe impl<T: Pod> Pod for RelSlice<T> {
    const LAYOUT: PodLayout = PodLayout {
        kind: PodKind::RelSlice(pod_layout::<T>),
        ..PodLayout::opaque::<Self>()
    };
}

impl<T> RelPtr<T> {
    pub const NULL: Self = Self {
        offset: 0,
        _target: PhantomData,
    };

    pub fn is_null(&self) -> bool {
        self.offset == 0
    }

    pub fn offset(&self) -> i32 {
        self.offset
    }
}

impl<T> RelSlice<T> {
    pub const EMPTY: Self = Self {
        offset: 0,
        len: 0,
        _target: PhantomData,
    };

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn offset(&self) -> i32 {
        self.offset
    }
}

impl<T> fmt::Debug for RelPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RelPtr({:+})", self.offset)
    }
}

impl<T> fmt::Debug for RelSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RelSlice({:+}, len {})", self.offset, self.len)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutError {
    /// The target extends outside the buffer.
    OutOfBounds,
    /// The target's address is not aligned for its type.
    Misaligned,
    /// The `RelPtr`/`RelSlice` passed to the view does not lie inside its buffer.
    NotInView,
    InvalidUtf8,
    /// Builder: a relative offset does not fit in 32 bits.
    OffsetOverflow,
    /// Header: not a cooked buffer with a header (or not one written by this crate).
    BadMagic,
    /// Header: written by a newer format version, or with flags this version does not know.
    UnsupportedVersion(u16),
    /// Header: the buffer's length differs from the length it was written with (truncated, or
    /// part of a larger buffer).
    LengthMismatch {
        header: u32,
        actual: usize,
    },
    /// Header: the root type's layout differs from the one the data was cooked for. Recook it.
    LayoutMismatch {
        expected: u64,
        found: u64,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds => f.write_str("cooked data reference out of bounds"),
            Self::Misaligned => f.write_str("cooked data reference misaligned"),
            Self::NotInView => f.write_str("relative pointer is not inside the cooked buffer"),
            Self::InvalidUtf8 => f.write_str("cooked string is not UTF-8"),
            Self::OffsetOverflow => f.write_str("relative offset does not fit in 32 bits"),
            Self::BadMagic => f.write_str("not a cooked buffer (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported cooked format version {v}"),
            Self::LengthMismatch { header, actual } => write!(
                f,
                "cooked buffer is {actual} bytes long but its header says {header}"
            ),
            Self::LayoutMismatch { expected, found } => write!(
                f,
                "cooked data was written for another layout of its root type (layout hash \
                 {found:016x}, expected {expected:016x}); recook it"
            ),
        }
    }
}

impl core::error::Error for LayoutError {}

/// First bytes of a cooked buffer with a header.
pub const COOKED_MAGIC: [u8; 4] = *b"MRLC";
/// Cooked header version written by this crate; views accept versions up to it.
pub const COOKED_VERSION: u16 = 1;

/// The start of a cooked buffer written by [`CookedBuilder::with_header`]. 32 bytes, all
/// integers in the platform's (little-endian) byte order.
#[derive(Pod, Clone, Copy, PartialEq, Eq, Debug)]
#[pod(crate = "crate")]
#[repr(C)]
pub struct CookedHeader {
    /// [`COOKED_MAGIC`].
    pub magic: [u8; 4],
    /// [`COOKED_VERSION`].
    pub version: u16,
    /// Zero; reserved for format options.
    pub flags: u16,
    /// [`layout_hash`] of the root type.
    pub layout_hash: u64,
    /// Length of the whole buffer, header included.
    pub len: u32,
    /// Byte offset of the root value.
    pub root: u32,
    /// Alignment the buffer must be loaded at: the largest alignment of anything in it.
    pub align: u32,
    pub reserved: u32,
}

/// Checked read access to a cooked buffer.
#[derive(Clone, Copy, Debug)]
pub struct CookedView<'a> {
    bytes: &'a [u8],
    /// Byte offset of the root value: 0 without a header.
    root: usize,
    has_header: bool,
}

impl<'a> CookedView<'a> {
    /// A view of a buffer without a header, whose root is at offset 0.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            root: 0,
            has_header: false,
        }
    }

    /// A view of a buffer written by [`CookedBuilder::with_header`] for root type `T`. Checks the
    /// magic, version, length, the buffer's alignment and `T`'s [`layout_hash`], and that the
    /// root `T` is in bounds and aligned.
    pub fn with_header<T: Pod>(bytes: &'a [u8]) -> Result<Self, LayoutError> {
        let raw = Self::new(bytes);
        let header: &CookedHeader = raw.get(0).map_err(|e| match e {
            // Too short to hold a header at all.
            LayoutError::OutOfBounds => LayoutError::BadMagic,
            other => other,
        })?;
        if header.magic != COOKED_MAGIC {
            return Err(LayoutError::BadMagic);
        }
        if header.version == 0 || header.version > COOKED_VERSION || header.flags != 0 {
            return Err(LayoutError::UnsupportedVersion(header.version));
        }
        if header.len as usize != bytes.len() {
            return Err(LayoutError::LengthMismatch {
                header: header.len,
                actual: bytes.len(),
            });
        }
        let align = header.align as usize;
        if !align.is_power_of_two() || !bytes.as_ptr().addr().is_multiple_of(align) {
            return Err(LayoutError::Misaligned);
        }
        let expected = layout_hash::<T>();
        if header.layout_hash != expected {
            return Err(LayoutError::LayoutMismatch {
                expected,
                found: header.layout_hash,
            });
        }
        let view = Self {
            bytes,
            root: header.root as usize,
            has_header: true,
        };
        view.root::<T>()?;
        Ok(view)
    }

    /// The header, for views made with [`with_header`](Self::with_header).
    pub fn header(&self) -> Option<&'a CookedHeader> {
        if self.has_header {
            self.get(0).ok()
        } else {
            None
        }
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// `len` `T`s at byte `offset` of the buffer.
    pub fn slice_at<T: Pod>(&self, offset: usize, len: usize) -> Result<&'a [T], LayoutError> {
        const { T::CHECK };
        let size = len
            .checked_mul(size_of::<T>())
            .ok_or(LayoutError::OutOfBounds)?;
        let end = offset.checked_add(size).ok_or(LayoutError::OutOfBounds)?;
        if end > self.bytes.len() {
            return Err(LayoutError::OutOfBounds);
        }
        let ptr = self.bytes.as_ptr().wrapping_add(offset);
        if !ptr.cast::<T>().is_aligned() {
            return Err(LayoutError::Misaligned);
        }
        // SAFETY: the range is inside `bytes` (borrowed for 'a) and aligned for `T`; `T: Pod` is
        // valid for any bytes and has no interior mutability, so a shared view of them is sound.
        Ok(unsafe { core::slice::from_raw_parts(ptr.cast::<T>(), len) })
    }

    /// The `T` at byte `offset` of the buffer.
    pub fn get<T: Pod>(&self, offset: usize) -> Result<&'a T, LayoutError> {
        self.slice_at::<T>(offset, 1).map(|s| &s[0])
    }

    /// The root `T`: at the start of a headerless buffer, after the header otherwise.
    pub fn root<T: Pod>(&self) -> Result<&'a T, LayoutError> {
        self.get(self.root)
    }

    /// Byte offset of `field` (a `RelPtr`/`RelSlice`) within the buffer.
    fn position_of<F>(&self, field: &F) -> Result<usize, LayoutError> {
        let base = self.bytes.as_ptr().addr();
        let at = core::ptr::from_ref(field).addr();
        let position = at.checked_sub(base).ok_or(LayoutError::NotInView)?;
        match position.checked_add(size_of::<F>()) {
            Some(end) if end <= self.bytes.len() => Ok(position),
            _ => Err(LayoutError::NotInView),
        }
    }

    fn target(&self, position: usize, offset: i32) -> Result<usize, LayoutError> {
        let target = position as i64 + i64::from(offset);
        usize::try_from(target).map_err(|_| LayoutError::OutOfBounds)
    }

    /// Follows `ptr`, which must be inside this view's buffer (typically a field of a struct
    /// obtained from it). `Ok(None)` for null.
    pub fn deref<T: Pod>(&self, ptr: &RelPtr<T>) -> Result<Option<&'a T>, LayoutError> {
        if ptr.is_null() {
            return Ok(None);
        }
        let target = self.target(self.position_of(ptr)?, ptr.offset)?;
        self.get(target).map(Some)
    }

    /// Follows `slice`, which must be inside this view's buffer.
    pub fn slice<T: Pod>(&self, slice: &RelSlice<T>) -> Result<&'a [T], LayoutError> {
        if slice.len == 0 {
            return Ok(&[]);
        }
        let target = self.target(self.position_of(slice)?, slice.offset)?;
        self.slice_at(target, slice.len())
    }

    /// Follows a UTF-8 string slice.
    pub fn str(&self, slice: &RelSlice<u8>) -> Result<&'a str, LayoutError> {
        core::str::from_utf8(self.slice(slice)?).map_err(|_| LayoutError::InvalidUtf8)
    }
}

/// Byte offset of a `T` written by a [`CookedBuilder`].
pub struct Pos<T> {
    offset: usize,
    _target: PhantomData<fn() -> T>,
}

impl<T> Clone for Pos<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Pos<T> {}

impl<T> fmt::Debug for Pos<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pos({})", self.offset)
    }
}

impl<T> Pos<T> {
    pub fn offset(self) -> usize {
        self.offset
    }

    /// The field at `byte_offset` inside the `T`:
    /// `pos.field(offset_of!(Mesh, name), |m| &m.name)`. The accessor is never called; it ties
    /// the field's type to `T`, so a link cannot store a pointer of the wrong type.
    pub fn field<U>(self, byte_offset: usize, _accessor: fn(&T) -> &U) -> Pos<U> {
        Pos {
            offset: self.offset + byte_offset,
            _target: PhantomData,
        }
    }
}

/// Byte offset and length of `T`s written by a [`CookedBuilder`].
pub struct SlicePos<T> {
    offset: usize,
    len: usize,
    _target: PhantomData<fn() -> T>,
}

impl<T> Clone for SlicePos<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for SlicePos<T> {}

impl<T> SlicePos<T> {
    pub fn offset(self) -> usize {
        self.offset
    }

    pub fn len(self) -> usize {
        self.len
    }

    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Element `index` of the slice, to link pointers inside it. Panics when out of range.
    pub fn at(self, index: usize) -> Pos<T> {
        assert!(index < self.len, "slice element out of range");
        Pos {
            offset: self.offset + index * size_of::<T>(),
            _target: PhantomData,
        }
    }
}

/// Appends `Pod` values to a buffer and links relative pointers between them.
///
/// Each value is aligned relative to the buffer start, so the buffer must be loaded at an address
/// aligned to [`CookedBuilder::align`]; [`CookedBuilder::finish`] returns such a buffer.
#[derive(Default)]
pub struct CookedBuilder {
    bytes: Vec<u8>,
    align: usize,
    has_header: bool,
}

impl CookedBuilder {
    /// A builder for a headerless buffer: the first value pushed is the root.
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            align: 1,
            has_header: false,
        }
    }

    /// A builder whose buffer starts with a [`CookedHeader`] for root type `T`, followed by
    /// `root`. [`finish`](Self::finish) fills in the length and alignment; read the buffer with
    /// [`CookedView::with_header`].
    pub fn with_header<T: Pod>(root: &T) -> (Self, Pos<T>) {
        let mut builder = Self::new();
        builder.has_header = true;
        builder.push(&CookedHeader {
            magic: COOKED_MAGIC,
            version: COOKED_VERSION,
            flags: 0,
            layout_hash: layout_hash::<T>(),
            len: 0,
            root: 0,
            align: 0,
            reserved: 0,
        });
        let pos = builder.push(root);
        // The root follows the 32-byte header, so its offset fits.
        builder.patch_header(core::mem::offset_of!(CookedHeader, root), pos.offset as u32);
        (builder, pos)
    }

    fn patch_header(&mut self, field: usize, value: u32) {
        self.bytes[field..field + 4].copy_from_slice(&value.to_ne_bytes());
    }

    /// Largest alignment of anything written.
    pub fn align(&self) -> usize {
        self.align
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn align_to(&mut self, align: usize) -> usize {
        self.align = self.align.max(align);
        let offset = self.bytes.len().next_multiple_of(align);
        self.bytes.resize(offset, 0);
        offset
    }

    pub fn push<T: Pod>(&mut self, value: &T) -> Pos<T> {
        let slice = self.push_slice(core::slice::from_ref(value));
        Pos {
            offset: slice.offset,
            _target: PhantomData,
        }
    }

    pub fn push_slice<T: Pod>(&mut self, values: &[T]) -> SlicePos<T> {
        const { T::CHECK };
        let offset = self.align_to(align_of::<T>());
        // SAFETY: `T: Pod` has no padding, so all `size_of_val(values)` bytes are initialized.
        let bytes = unsafe {
            core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values))
        };
        self.bytes.extend_from_slice(bytes);
        SlicePos {
            offset,
            len: values.len(),
            _target: PhantomData,
        }
    }

    pub fn push_str(&mut self, s: &str) -> SlicePos<u8> {
        self.push_slice(s.as_bytes())
    }

    fn relative(&self, from: usize, size: usize, to: usize) -> Result<i32, LayoutError> {
        if from + size > self.bytes.len() {
            return Err(LayoutError::OutOfBounds);
        }
        i32::try_from(to as i64 - from as i64).map_err(|_| LayoutError::OffsetOverflow)
    }

    /// Points the `RelPtr` at `at` to `target`.
    pub fn link<T>(&mut self, at: Pos<RelPtr<T>>, target: Pos<T>) -> Result<(), LayoutError> {
        let offset = self.relative(at.offset, size_of::<RelPtr<T>>(), target.offset)?;
        self.bytes[at.offset..at.offset + 4].copy_from_slice(&offset.to_ne_bytes());
        Ok(())
    }

    /// Points the `RelSlice` at `at` to `target`.
    pub fn link_slice<T>(
        &mut self,
        at: Pos<RelSlice<T>>,
        target: SlicePos<T>,
    ) -> Result<(), LayoutError> {
        let offset = self.relative(at.offset, size_of::<RelSlice<T>>(), target.offset)?;
        let len = u32::try_from(target.len).map_err(|_| LayoutError::OffsetOverflow)?;
        self.bytes[at.offset..at.offset + 4].copy_from_slice(&offset.to_ne_bytes());
        self.bytes[at.offset + 4..at.offset + 8].copy_from_slice(&len.to_ne_bytes());
        Ok(())
    }

    /// The bytes in a buffer aligned for everything in it. With a header, fills in its length and
    /// alignment first; panics if such a buffer exceeds 4 GiB.
    pub fn finish(mut self) -> CookedBuffer {
        if self.has_header {
            let len = u32::try_from(self.bytes.len())
                .expect("a cooked buffer with a header is limited to 4 GiB");
            self.patch_header(core::mem::offset_of!(CookedHeader, len), len);
            // At most the largest alignment of a type, far below 4 GiB.
            self.patch_header(
                core::mem::offset_of!(CookedHeader, align),
                self.align as u32,
            );
        }
        CookedBuffer::copy_from(&self.bytes, self.align)
    }
}

/// An owned byte buffer with a chosen alignment, for cooked data loaded into memory.
pub struct CookedBuffer {
    /// Invariant: `len` initialized bytes allocated with `layout` (dangling if `len == 0`).
    ptr: NonNull<u8>,
    layout: Layout,
}

// SAFETY: owns plain bytes, like `Vec<u8>`.
unsafe impl Send for CookedBuffer {}
// SAFETY: shared access only reads.
unsafe impl Sync for CookedBuffer {}

impl CookedBuffer {
    /// Zeroed buffer of `len` bytes aligned to `align` (at least 16), for loading data into.
    /// Panics if `align` is not a power of two.
    pub fn zeroed(len: usize, align: usize) -> Self {
        let layout = Layout::from_size_align(len, align.max(16)).expect("invalid alignment");
        let ptr = if len == 0 {
            NonNull::new(core::ptr::without_provenance_mut(layout.align()))
                .unwrap_or(NonNull::dangling())
        } else {
            // SAFETY: the size is non-zero.
            NonNull::new(unsafe { alloc_zeroed(layout) })
                .unwrap_or_else(|| handle_alloc_error(layout))
        };
        Self { ptr, layout }
    }

    pub fn copy_from(bytes: &[u8], align: usize) -> Self {
        let mut buffer = Self::zeroed(bytes.len(), align);
        buffer.as_mut_slice().copy_from_slice(bytes);
        buffer
    }

    pub fn align(&self) -> usize {
        self.layout.align()
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: the invariant.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.layout.size()) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the invariant; `&mut self` makes the access exclusive.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }

    pub fn view(&self) -> CookedView<'_> {
        CookedView::new(self.as_slice())
    }
}

impl Deref for CookedBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for CookedBuffer {
    fn drop(&mut self) {
        if self.layout.size() != 0 {
            // SAFETY: allocated with this layout in `zeroed`.
            unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
        }
    }
}

impl fmt::Debug for CookedBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CookedBuffer({} bytes, align {})",
            self.layout.size(),
            self.layout.align()
        )
    }
}
