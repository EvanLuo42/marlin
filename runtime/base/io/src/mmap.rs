//! Memory-mapped files.

use core::ops::{Deref, DerefMut};

use marlin_pal::fs::Mapping;

use crate::{File, Result};

/// A read-only memory-mapped view of part of a file. Dropping it unmaps the view; the file may be
/// closed first.
pub struct Mmap(Mapping);

impl Mmap {
    /// Maps `len` bytes of `file` from `offset` (no alignment needed). `InvalidInput` for an
    /// empty or out-of-range view.
    ///
    /// # Safety
    ///
    /// The view shows the file's bytes as they are now and later: nothing (in this process or
    /// another) may modify or truncate the mapped range while the view is alive, or reads of it
    /// are undefined behavior. Cooked packages, which are written once and never touched again,
    /// are the intended use.
    pub unsafe fn map(file: &File, offset: u64, len: usize) -> Result<Self> {
        Mapping::new(&file.0, offset, len, false).map(Self)
    }

    /// Maps the whole file. `InvalidInput` for an empty file.
    ///
    /// # Safety
    /// As for [`map`](Self::map).
    pub unsafe fn map_all(file: &File) -> Result<Self> {
        let len = usize::try_from(file.size()?)
            .map_err(|_| crate::Error::from_kind(crate::ErrorKind::OutOfMemory))?;
        // SAFETY: forwarded from the caller.
        unsafe { Self::map(file, 0, len) }
    }
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: the view is `len` readable bytes; `map`'s caller guarantees nobody changes them.
        unsafe { core::slice::from_raw_parts(self.0.as_ptr(), self.0.len()) }
    }
}

/// A writable memory-mapped view: writes reach the file (flush them with [`flush`](Self::flush)).
pub struct MmapMut(Mapping);

impl MmapMut {
    /// Maps `len` bytes of `file`, which must be open for writing, from `offset`.
    ///
    /// # Safety
    /// As for [`Mmap::map`]: nothing else may modify or truncate the range while the view is
    /// alive.
    pub unsafe fn map(file: &File, offset: u64, len: usize) -> Result<Self> {
        Mapping::new(&file.0, offset, len, true).map(Self)
    }

    /// Writes modified pages back to the file. Follow with [`File::sync_all`] for durability.
    pub fn flush(&self) -> Result<()> {
        self.0.flush()
    }
}

impl Deref for MmapMut {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: as for `Mmap`.
        unsafe { core::slice::from_raw_parts(self.0.as_ptr(), self.0.len()) }
    }
}

impl DerefMut for MmapMut {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as for `Mmap`; `&mut self` makes the access exclusive within this process.
        unsafe { core::slice::from_raw_parts_mut(self.0.as_ptr(), self.0.len()) }
    }
}
