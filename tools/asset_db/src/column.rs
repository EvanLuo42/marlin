//! Columns that are either owned or borrowed straight from a memory-mapped snapshot.
//!
//! Opening an index maps its snapshot and points every column into the mapping: nothing is read
//! or copied until a query touches the pages. The first mutation of a column copies it into an
//! owned `Vec` (copy on write), so updates never write to the file.

use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;

use memmap2::Mmap;

/// Plain data: every bit pattern is a valid value, no padding, no pointers.
///
/// # Safety
/// The type must satisfy the above, so a column can be viewed from raw snapshot bytes.
pub(crate) unsafe trait Pod: Copy + Send + Sync + 'static {}

// SAFETY: integers have no invalid bit patterns or padding.
unsafe impl Pod for u8 {}
// SAFETY: as above.
unsafe impl Pod for u16 {}
// SAFETY: as above.
unsafe impl Pod for u32 {}
// SAFETY: as above.
unsafe impl Pod for u64 {}
// SAFETY: as above.
unsafe impl Pod for u128 {}

pub(crate) enum Column<T: Pod> {
    Owned(Vec<T>),
    Mapped {
        map: Arc<Mmap>,
        offset: usize,
        len: usize,
        _items: PhantomData<T>,
    },
}

impl<T: Pod> Default for Column<T> {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}

impl<T: Pod> Column<T> {
    /// A column over `len` items at `offset` in `map`. Returns `None` when the range is out of
    /// bounds or misaligned (a corrupt or foreign file).
    pub(crate) fn mapped(map: &Arc<Mmap>, offset: usize, len: usize) -> Option<Self> {
        let bytes = len.checked_mul(size_of::<T>())?;
        let end = offset.checked_add(bytes)?;
        if end > map.len() || !(map.as_ptr() as usize + offset).is_multiple_of(align_of::<T>()) {
            return None;
        }
        Some(Self::Mapped {
            map: map.clone(),
            offset,
            len,
            _items: PhantomData,
        })
    }

    /// The owned vector, copying a mapped column first.
    pub(crate) fn to_mut(&mut self) -> &mut Vec<T> {
        if let Self::Mapped { .. } = self {
            *self = Self::Owned(self.to_vec());
        }
        match self {
            Self::Owned(v) => v,
            Self::Mapped { .. } => unreachable!("converted above"),
        }
    }

    pub(crate) fn push(&mut self, value: T) {
        self.to_mut().push(value);
    }

    pub(crate) fn set(&mut self, index: usize, value: T) {
        self.to_mut()[index] = value;
    }

    /// The raw bytes, for writing snapshots.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        let items: &[T] = self;
        // SAFETY: `T: Pod` has no padding, so its bytes are all initialized.
        unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), size_of_val(items)) }
    }
}

impl<T: Pod> Deref for Column<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match self {
            Self::Owned(v) => v,
            Self::Mapped {
                map, offset, len, ..
            } => {
                // SAFETY: `mapped` checked bounds and alignment; the map is read-only and lives as
                // long as `self`; `T: Pod` accepts any bytes.
                unsafe { std::slice::from_raw_parts(map.as_ptr().add(*offset).cast(), *len) }
            }
        }
    }
}

impl<T: Pod> From<Vec<T>> for Column<T> {
    fn from(v: Vec<T>) -> Self {
        Self::Owned(v)
    }
}
