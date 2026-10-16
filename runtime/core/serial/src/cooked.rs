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
//! Cooked data is platform-specific (native endianness, layout and alignment), like everything
//! the cooker produces.
//!
//! ```ignore
//! #[derive(Clone, Copy)]
//! #[repr(C)]
//! struct Mesh {
//!     vertices: RelSlice<[f32; 3]>,
//!     name: RelSlice<u8>,
//! }
//! // SAFETY: two `RelSlice`s (8 bytes each, align 4): no padding.
//! unsafe impl Pod for Mesh {}
//!
//! let view = CookedView::new(&bytes);
//! let mesh: &Mesh = view.root()?;
//! let vertices: &[[f32; 3]] = view.slice(&mesh.vertices)?;
//! ```

use alloc::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::ops::Deref;
use core::ptr::NonNull;

/// Plain data that can be copied to and from bytes.
///
/// # Safety
///
/// The type must be `Copy`, valid for every bit pattern, without padding bytes (so its bytes can
/// be written out) and without interior mutability.
pub unsafe trait Pod: Copy + 'static {}

macro_rules! pod {
    ($($ty:ty),*) => {$(
        // SAFETY: integers and floats are valid for every bit pattern and have no padding.
        unsafe impl Pod for $ty {}
    )*};
}

pod!(u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64);

// SAFETY: an array of `Pod` has no padding between elements and every bit pattern is valid.
unsafe impl<T: Pod, const N: usize> Pod for [T; N] {}

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
unsafe impl<T: 'static> Pod for RelPtr<T> {}
// SAFETY: two 4-byte integers, no padding; any values are valid to check later.
unsafe impl<T: 'static> Pod for RelSlice<T> {}

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
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutOfBounds => "cooked data reference out of bounds",
            Self::Misaligned => "cooked data reference misaligned",
            Self::NotInView => "relative pointer is not inside the cooked buffer",
            Self::InvalidUtf8 => "cooked string is not UTF-8",
            Self::OffsetOverflow => "relative offset does not fit in 32 bits",
        })
    }
}

/// Checked read access to a cooked buffer.
#[derive(Clone, Copy, Debug)]
pub struct CookedView<'a> {
    bytes: &'a [u8],
}

impl<'a> CookedView<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// `len` `T`s at byte `offset` of the buffer.
    pub fn slice_at<T: Pod>(&self, offset: usize, len: usize) -> Result<&'a [T], LayoutError> {
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

    /// The `T` at the start of the buffer.
    pub fn root<T: Pod>(&self) -> Result<&'a T, LayoutError> {
        self.get(0)
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
}

impl CookedBuilder {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            align: 1,
        }
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

    /// The bytes in a buffer aligned for everything in it.
    pub fn finish(self) -> CookedBuffer {
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
