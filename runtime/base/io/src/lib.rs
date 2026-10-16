//! Completion-based asynchronous file IO and paths (memory-mapped files later).
//!
//! The safe layer over `marlin_pal::file`:
//!
//! - [`IoQueue`]: the asynchronous core. Submit reads and writes of owned [`IoBuffer`]s, get
//!   [`Completed`] operations back with their buffers. IoRing or IOCP on Windows, io_uring on
//!   Linux, blocking `pread` inside the queue on macOS.
//! - [`File`]: open, size, and synchronous positional reads and writes; [`read`] and [`write`]
//!   for whole files.
//! - [`Path`] / [`PathBuf`]: UTF-8 paths with `/` separators.
//!
//! Unbuffered files ([`OpenOptions::unbuffered`]) need buffers, offsets and lengths aligned to
//! [`File::alignment`]; [`IoBuffer::for_file`] makes such buffers.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod buffer;
mod file;
pub mod path;
mod queue;

pub use buffer::IoBuffer;
pub use file::{File, read, write};
pub use marlin_pal::file::{
    Disposition, Engine, MAX_IO_LEN, MAX_PATH_LEN, MAX_QUEUE_CAPACITY, OpenOptions,
};
pub use marlin_pal::{Error, ErrorKind, Result};
pub use path::{Path, PathBuf};
pub use queue::{Completed, IoQueue, OpKind, RequestId, SubmitError};
