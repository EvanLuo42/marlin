//! Reading serialized data incrementally from a [`Source`] (a file, a socket, a decompressor)
//! instead of a buffer holding all of it.
//!
//! Scope: the data is the ordinary format, written by [`to_bytes`](crate::to_bytes) or
//! [`write`](crate::write); there is no separate stream format. [`StreamReader`] reads data
//! whose root is a list, array or set (`Vec<Record>`, say) one element at a time: only the bytes
//! of the current element are held in memory, in a buffer reused between elements. [`read_from`]
//! reads any root value, holding the bytes of that value only (not the whole input).
//!
//! Each element is located by walking its encoding (lengths and counts, as the format stores
//! them) and then decoded with the usual checks. Memory grows only with bytes actually read from
//! the source, never with a count or length the data merely claims, so corrupt input fails with
//! [`Error::Truncated`] when the source ends instead of allocating huge buffers.
//!
//! ```ignore
//! let file = marlin_std::io::File::open("events.bin")?;
//! let mut events = StreamReader::new(FileSource::new(&file))?;
//! while let Some(event) = events.read_next::<Event>()? {
//!     handle(event);
//! }
//! ```

use alloc::vec::Vec;

use marlin_reflect::{Mut, Reflect};
use marlin_std::io::File;

use crate::read::Reader;
use crate::wire::{
    self, ARRAY, CUSTOM, ENUM, Input, LIST, MAP, OPTION, SET, STRING, STRUCT, Sig, TUPLE,
    VERSIONED, scalar_size,
};
use crate::{Error, HEADER_LEN, Header, ReadOptions};

/// A source of bytes, read front to back.
pub trait Source {
    /// Reads up to `buf.len()` bytes into `buf` and returns how many; 0 only at the end of the
    /// input.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error>;
}

impl Source for &[u8] {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let n = buf.len().min(self.len());
        let (head, tail) = self.split_at(n);
        buf[..n].copy_from_slice(head);
        *self = tail;
        Ok(n)
    }
}

impl<S: Source + ?Sized> Source for &mut S {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        (**self).read(buf)
    }
}

/// A [`File`] read sequentially from an offset with positional reads.
pub struct FileSource<'f> {
    file: &'f File,
    offset: u64,
}

impl<'f> FileSource<'f> {
    /// Reads `file` from its start.
    pub fn new(file: &'f File) -> Self {
        Self::at(file, 0)
    }

    /// Reads `file` from byte `offset`.
    pub fn at(file: &'f File, offset: u64) -> Self {
        Self { file, offset }
    }

    /// The offset of the next byte to read.
    pub fn offset(&self) -> u64 {
        self.offset
    }
}

impl Source for FileSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let n = self.file.read_at(buf, self.offset).map_err(Error::Io)?;
        self.offset += n as u64;
        Ok(n)
    }
}

/// Largest read issued at once; buffers grow by at most this much beyond the bytes received.
const CHUNK: usize = 64 * 1024;

/// Appends exactly `n` bytes from `source` to `buf`.
fn fill<S: Source>(source: &mut S, buf: &mut Vec<u8>, n: usize) -> Result<(), Error> {
    let mut left = n;
    while left > 0 {
        let chunk = left.min(CHUNK);
        let start = buf.len();
        buf.resize(start + chunk, 0);
        let mut filled = 0;
        while filled < chunk {
            let got = source.read(&mut buf[start + filled..start + chunk])?;
            if got == 0 {
                buf.truncate(start + filled);
                return Err(Error::Truncated);
            }
            filled += got;
        }
        left -= chunk;
    }
    Ok(())
}

fn pull_u8<S: Source>(source: &mut S, buf: &mut Vec<u8>) -> Result<u8, Error> {
    fill(source, buf, 1)?;
    Ok(buf[buf.len() - 1])
}

fn pull_u32<S: Source>(source: &mut S, buf: &mut Vec<u8>) -> Result<u32, Error> {
    fill(source, buf, 4)?;
    let at = buf.len() - 4;
    Ok(u32::from_le_bytes([
        buf[at],
        buf[at + 1],
        buf[at + 2],
        buf[at + 3],
    ]))
}

/// Appends the bytes of one signature; validated afterwards by `Input::sig`.
fn pull_sig<S: Source>(source: &mut S, buf: &mut Vec<u8>, depth: u32) -> Result<(), Error> {
    if depth == 0 {
        return Err(Error::TooDeep);
    }
    match pull_u8(source, buf)? {
        LIST | OPTION | SET => pull_sig(source, buf, depth - 1),
        ARRAY => {
            fill(source, buf, 4)?;
            pull_sig(source, buf, depth - 1)
        }
        MAP => {
            pull_sig(source, buf, depth - 1)?;
            pull_sig(source, buf, depth - 1)
        }
        CUSTOM => fill(source, buf, 8),
        VERSIONED => fill(source, buf, 5),
        _ => Ok(()),
    }
}

/// Appends the bytes of one value with signature `sig`.
fn pull_value<S: Source>(
    source: &mut S,
    buf: &mut Vec<u8>,
    sig: Sig<'_>,
    depth: u32,
) -> Result<(), Error> {
    if depth == 0 {
        return Err(Error::TooDeep);
    }
    let (_, sig) = sig.unversioned();
    match sig.kind() {
        STRING | CUSTOM => {
            let len = pull_u32(source, buf)? as usize;
            fill(source, buf, len)
        }
        STRUCT | TUPLE => pull_fields(source, buf),
        ENUM => {
            fill(source, buf, 4)?;
            pull_fields(source, buf)
        }
        LIST | SET => {
            let count = pull_u32(source, buf)?;
            pull_elements(source, buf, sig.element(), count as usize, depth)
        }
        ARRAY => pull_elements(source, buf, sig.element(), sig.array_len() as usize, depth),
        MAP => {
            let count = pull_u32(source, buf)?;
            for _ in 0..count {
                pull_value(source, buf, sig.element(), depth - 1)?;
                pull_value(source, buf, sig.map_value(), depth - 1)?;
            }
            Ok(())
        }
        OPTION => match pull_u8(source, buf)? {
            0 => Ok(()),
            1 => pull_value(source, buf, sig.element(), depth - 1),
            _ => Err(Error::InvalidTag),
        },
        kind => fill(source, buf, scalar_size(kind).unwrap_or(0)),
    }
}

fn pull_elements<S: Source>(
    source: &mut S,
    buf: &mut Vec<u8>,
    element: Sig<'_>,
    count: usize,
    depth: u32,
) -> Result<(), Error> {
    // Elements whose encoding is always empty need no reads (and a huge count of them no loop).
    if element.min_size() == 0 {
        return Ok(());
    }
    for _ in 0..count {
        pull_value(source, buf, element, depth - 1)?;
    }
    Ok(())
}

fn pull_fields<S: Source>(source: &mut S, buf: &mut Vec<u8>) -> Result<(), Error> {
    let count = pull_u32(source, buf)?;
    for _ in 0..count {
        fill(source, buf, 4)?;
        let len = pull_u32(source, buf)? as usize;
        fill(source, buf, len)?;
    }
    Ok(())
}

/// Reads the header and root signature; returns them with the signature's bytes.
fn start<S: Source>(source: &mut S, max_depth: u32) -> Result<(Header, Vec<u8>), Error> {
    let mut bytes = Vec::new();
    fill(source, &mut bytes, HEADER_LEN).map_err(|e| match e {
        Error::Truncated => Error::BadMagic,
        e => e,
    })?;
    let header = crate::header(&mut Input::new(&bytes))?;
    let mut sig = Vec::new();
    pull_sig(source, &mut sig, max_depth)?;
    Input::new(&sig).sig(max_depth)?;
    Ok((header, sig))
}

/// Decodes `bytes`, the whole encoding of one value with signature `sig`, into `target`.
fn decode_all(reader: Reader, bytes: &[u8], target: Mut<'_>, sig: Sig<'_>) -> Result<(), Error> {
    if !wire::compatible(target.info(), sig) {
        return Err(Error::Incompatible);
    }
    let mut input = Input::new(bytes);
    reader.decode(&mut input, target, sig, reader.max_depth)?;
    if input.remaining() != 0 {
        return Err(Error::InvalidLength);
    }
    Ok(())
}

/// Reads the elements of a serialized list, array or set one by one.
pub struct StreamReader<S> {
    source: S,
    header: Header,
    /// The root signature.
    sig: Vec<u8>,
    remaining: usize,
    /// Bytes of the current element.
    buf: Vec<u8>,
    reader: Reader,
}

impl<S: Source> StreamReader<S> {
    pub fn new(source: S) -> Result<Self, Error> {
        Self::with_options(source, &ReadOptions::default())
    }

    /// Reads the header and the element count. The root must be a list, array or set
    /// ([`Error::Incompatible`] otherwise).
    pub fn with_options(mut source: S, options: &ReadOptions) -> Result<Self, Error> {
        let (header, sig) = start(&mut source, options.max_depth)?;
        let mut buf = Vec::new();
        let root = Sig::from_validated(&sig);
        let remaining = match root.kind() {
            LIST | SET => pull_u32(&mut source, &mut buf)? as usize,
            ARRAY => root.array_len() as usize,
            _ => return Err(Error::Incompatible),
        };
        buf.clear();
        Ok(Self {
            source,
            header,
            sig,
            remaining,
            buf,
            reader: Reader {
                max_depth: options.max_depth,
            },
        })
    }

    pub fn header(&self) -> Header {
        self.header
    }

    /// Elements not read yet.
    pub fn remaining(&self) -> usize {
        self.remaining
    }

    /// Reads the next element into `target`, like [`read_into`](crate::read_into): fields
    /// absent from the data keep their value. `Ok(false)` when every element has been read.
    pub fn read_into(&mut self, target: Mut<'_>) -> Result<bool, Error> {
        if self.remaining == 0 {
            return Ok(false);
        }
        let element = Sig::from_validated(&self.sig).element();
        self.buf.clear();
        pull_value(
            &mut self.source,
            &mut self.buf,
            element,
            self.reader.max_depth,
        )?;
        self.remaining -= 1;
        decode_all(self.reader, &self.buf, target, element)?;
        Ok(true)
    }

    /// The next element as a `T`, starting from its default; `Ok(None)` after the last one.
    pub fn read_next<T: Reflect>(&mut self) -> Result<Option<T>, Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut value = T::reflect_default().ok_or(Error::NoDefault)?;
        self.read_into(Mut::new(&mut value))?;
        Ok(Some(value))
    }

    /// Skips the elements not read yet and checks nothing follows the data
    /// ([`Error::TrailingBytes`]); gives the source back.
    pub fn finish(mut self) -> Result<S, Error> {
        let element = Sig::from_validated(&self.sig).element();
        while self.remaining > 0 {
            self.buf.clear();
            pull_value(
                &mut self.source,
                &mut self.buf,
                element,
                self.reader.max_depth,
            )?;
            self.remaining -= 1;
        }
        if self.source.read(&mut [0])? != 0 {
            return Err(Error::TrailingBytes);
        }
        Ok(self.source)
    }

    /// The source, positioned after the last element read.
    pub fn into_source(self) -> S {
        self.source
    }
}

/// Reads one serialized value of any kind from `source` into `target`, holding only that value's
/// bytes. The source may continue after the value; nothing more is read from it.
pub fn read_into_from<S: Source>(
    mut source: S,
    target: Mut<'_>,
    options: &ReadOptions,
) -> Result<(), Error> {
    let (_, sig) = start(&mut source, options.max_depth)?;
    let sig = Sig::from_validated(&sig);
    let mut bytes = Vec::new();
    pull_value(&mut source, &mut bytes, sig, options.max_depth)?;
    let reader = Reader {
        max_depth: options.max_depth,
    };
    decode_all(reader, &bytes, target, sig)
}

/// Reads one `T` from `source`, starting from its default; see [`read_into_from`].
pub fn read_from<T: Reflect, S: Source>(source: S) -> Result<T, Error> {
    let mut value = T::reflect_default().ok_or(Error::NoDefault)?;
    read_into_from(source, Mut::new(&mut value), &ReadOptions::default())?;
    Ok(value)
}
