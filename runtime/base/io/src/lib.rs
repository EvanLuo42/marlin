//! Completion-based asynchronous file IO, paths, file system operations, memory-mapped files and
//! directory watching.
//!
//! The safe layer over `marlin_pal::file`, `fs` and `watch`:
//!
//! - [`IoQueue`]: the asynchronous core. Submit reads and writes of owned [`IoBuffer`]s, get
//!   [`Completed`] operations back with their buffers, cancel them. IoRing or IOCP on Windows,
//!   io_uring on Linux, blocking `pread` inside the queue on macOS.
//! - [`File`]: open, size, sync, and synchronous positional reads and writes; [`read`] and
//!   [`write`] for whole files.
//! - [`fs`]: metadata, creating, removing and renaming, [`fs::read_dir`] and [`fs::walk_dir`].
//! - [`Mmap`] / [`MmapMut`]: memory-mapped views.
//! - [`watch::Watcher`]: change notifications under a directory, for hot reload.
//! - [`Path`] / [`PathBuf`]: UTF-8 paths with `/` separators.
//!
//! Unbuffered files ([`OpenOptions::unbuffered`]) need buffers, offsets and lengths aligned to
//! [`File::alignment`]; [`IoBuffer::for_file`] makes such buffers.
//!
//! There is no VFS here: mounting packages and patches is `marlin_asset::Vfs`, at L3.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod buffer;
mod file;
pub mod fs;
mod mmap;
pub mod path;
mod queue;
pub mod watch;

pub use buffer::IoBuffer;
pub use file::{File, read, write};
pub use marlin_pal::file::{
    Disposition, Engine, MAX_IO_LEN, MAX_PATH_LEN, MAX_QUEUE_CAPACITY, OpenOptions,
};
pub use marlin_pal::{Error, ErrorKind, Result};
pub use mmap::{Mmap, MmapMut};
pub use path::{Path, PathBuf};
pub use queue::{Completed, IoQueue, OpKind, RequestId, SubmitError};
