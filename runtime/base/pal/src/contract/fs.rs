use core::ptr::NonNull;

use crate::Result;
use crate::contract::File;
use crate::fs::{DirEntry, Metadata};

/// Path operations, metadata, directory listing and memory mapping. Paths are non-empty, at most
/// `MAX_PATH_LEN` bytes and contain no NUL (checked by the caller).
///
/// # Safety
///
/// `map` must return a pointer to `len` bytes of the file starting at `offset`, valid until the
/// mapping is dropped.
pub(crate) unsafe trait Fs: File {
    /// An open directory listing; dropping it closes it.
    type Dir: Send;
    /// A mapped view; dropping it unmaps the view.
    type Mapping: Send + Sync;

    /// Flushes the file's data (and, unless `data_only`, its metadata) to the storage device.
    fn sync(file: &Self::File, data_only: bool) -> Result<()>;
    fn file_metadata(file: &Self::File) -> Result<Metadata>;
    /// Follows symbolic links.
    fn metadata(path: &str) -> Result<Metadata>;
    fn remove_file(path: &str) -> Result<()>;
    fn create_dir(path: &str) -> Result<()>;
    /// The directory must be empty.
    fn remove_dir(path: &str) -> Result<()>;
    /// Replaces `to` if it is a file.
    fn rename(from: &str, to: &str) -> Result<()>;

    fn read_dir(path: &str) -> Result<Self::Dir>;
    /// The next entry other than `.` and `..`, its name written like `crate::process` strings.
    fn dir_next(dir: &mut Self::Dir, name: &mut [u8]) -> Result<Option<DirEntry>>;

    /// `len > 0`; `offset + len` does not overflow and lies within the file (checked by the
    /// caller).
    fn map(
        file: &Self::File,
        offset: u64,
        len: usize,
        writable: bool,
    ) -> Result<(NonNull<u8>, Self::Mapping)>;
    /// Writes modified pages in `ptr..ptr + len`, which lies inside the mapping, back to the file.
    fn map_flush(mapping: &Self::Mapping, ptr: NonNull<u8>, len: usize) -> Result<()>;
}
