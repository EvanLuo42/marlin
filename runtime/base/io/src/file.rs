use alloc::vec;
use alloc::vec::Vec;

use marlin_pal::file as pal;

use crate::{Disposition, Error, ErrorKind, OpenOptions, Path, Result};

/// An open file with positional reads and writes. Dropping it closes the file.
///
/// Paths are passed to the OS as they are: use `/` separators (Windows accepts `\` too).
/// [`PathBuf`](crate::PathBuf) normalizes.
pub struct File(pub(crate) pal::File);

impl File {
    /// Opens an existing file read-only.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_options(path, &OpenOptions::read())
    }

    /// Opens a file read-write, creating it or truncating it to zero length.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::with_options(
            path,
            &OpenOptions::read_write().disposition(Disposition::CreateOrTruncate),
        )
    }

    /// `InvalidInput` for an empty path, one longer than [`MAX_PATH_LEN`](crate::MAX_PATH_LEN)
    /// or containing NUL, and for truncation without write access.
    pub fn with_options(path: impl AsRef<Path>, options: &OpenOptions) -> Result<Self> {
        pal::File::open(path.as_ref().as_str(), options).map(Self)
    }

    /// Required alignment of buffers, offsets and lengths: 1 unless opened unbuffered. See
    /// [`IoBuffer::for_file`](crate::IoBuffer::for_file).
    #[inline]
    pub fn alignment(&self) -> usize {
        self.0.alignment()
    }

    pub fn size(&self) -> Result<u64> {
        self.0.size()
    }

    /// Truncates or zero-extends the file. Needs write access.
    pub fn set_len(&self, len: u64) -> Result<()> {
        self.0.set_len(len)
    }

    /// Reads up to `buf.len()` bytes at `offset`; fewer near the end of the file, 0 past it.
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.0.read_at(buf, offset)
    }

    /// Writes up to `buf.len()` bytes at `offset`, returning how many were written.
    pub fn write_at(&self, buf: &[u8], offset: u64) -> Result<usize> {
        self.0.write_at(buf, offset)
    }

    /// Fills `buf` from `offset`, or fails with `UnexpectedEof` if the file ends first.
    pub fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> Result<()> {
        while !buf.is_empty() {
            match self.read_at(buf, offset)? {
                0 => return Err(Error::from_kind(ErrorKind::UnexpectedEof)),
                n => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
            }
        }
        Ok(())
    }

    /// Writes all of `buf` at `offset`.
    pub fn write_all_at(&self, mut buf: &[u8], mut offset: u64) -> Result<()> {
        while !buf.is_empty() {
            match self.write_at(buf, offset)? {
                0 => return Err(Error::from_kind(ErrorKind::Other)),
                n => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
            }
        }
        Ok(())
    }
}

/// Reads a whole file. Copes with the file changing size while it is read.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    let size =
        usize::try_from(file.size()?).map_err(|_| Error::from_kind(ErrorKind::OutOfMemory))?;
    // One spare byte, so reaching the end costs no reallocation when the size was right.
    let mut buf = vec![0; size + 1];
    let mut len = 0;
    loop {
        if len == buf.len() {
            buf.resize(len * 2, 0);
        }
        match file.read_at(&mut buf[len..], len as u64)? {
            0 => break,
            n => len += n,
        }
    }
    buf.truncate(len);
    Ok(buf)
}

/// Creates or truncates a file and writes `data` to it.
pub fn write(path: impl AsRef<Path>, data: &[u8]) -> Result<()> {
    File::create(path)?.write_all_at(data, 0)
}
