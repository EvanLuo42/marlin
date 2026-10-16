//! Framing for the asset tools' TCP protocols: the shared cache (`marlin-store`) and the cook
//! service (`marlin-cookd`) and its game-side client ([`DevSource`](crate::DevSource)).
//!
//! A stream of frames, each a 16-byte header and a payload:
//!
//! ```text
//! magic "MW"  kind:u16  id:u32  len:u32  checksum:u32   payload[len]
//! ```
//!
//! `kind` is the protocol's message type, `id` matches a response to its request (requests
//! pipeline; responses may come back in any order; id 0 is for messages nobody asked for, such
//! as change notifications), and `checksum` is the low 32 bits of xxh3_64 of the payload. A frame
//! that fails any check ends the connection: the stream cannot be resynchronized.
//!
//! Payloads are built and read with [`Writer`] and [`Reader`]: little-endian integers, and byte
//! strings with a u32 length.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use marlin_hash::xxh3_64;

use crate::Uuid;

pub const FRAME_MAGIC: [u8; 2] = *b"MW";
pub const HEADER_SIZE: usize = 16;
/// Largest payload accepted: bigger than any cooked unit, small enough that a damaged length
/// cannot make a peer allocate the address space.
pub const MAX_PAYLOAD: u32 = 1 << 30;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub kind: u16,
    pub id: u32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WireError {
    BadMagic,
    TooLarge(u32),
    Checksum,
    /// A payload shorter than its message needs, or with bytes left over.
    Malformed,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not a frame"),
            Self::TooLarge(n) => write!(f, "frame of {n} bytes, over the limit"),
            Self::Checksum => f.write_str("frame damaged"),
            Self::Malformed => f.write_str("message malformed"),
        }
    }
}

impl core::error::Error for WireError {}

/// Appends a frame to `out`.
pub fn encode(out: &mut Vec<u8>, kind: u16, id: u32, payload: &[u8]) {
    assert!(
        payload.len() as u64 <= u64::from(MAX_PAYLOAD),
        "frame payload too large"
    );
    out.extend_from_slice(&FRAME_MAGIC);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&(xxh3_64(payload) as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

/// A frame as bytes.
pub fn frame(kind: u16, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    encode(&mut out, kind, id, payload);
    out
}

/// Cuts frames out of a byte stream as it arrives.
#[derive(Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
    start: usize,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        // Drop consumed bytes before growing.
        if self.start > 0 && self.start * 2 >= self.buffer.len() {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Bytes received but not yet returned as frames.
    pub fn pending(&self) -> usize {
        self.buffer.len() - self.start
    }

    /// The next whole frame, if one has arrived.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, WireError> {
        let b = &self.buffer[self.start..];
        if b.len() < HEADER_SIZE {
            return Ok(None);
        }
        if b[..2] != FRAME_MAGIC {
            return Err(WireError::BadMagic);
        }
        let word = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let kind = u16::from_le_bytes([b[2], b[3]]);
        let id = word(4);
        let len = word(8);
        let checksum = word(12);
        if len > MAX_PAYLOAD {
            return Err(WireError::TooLarge(len));
        }
        let end = HEADER_SIZE + len as usize;
        if b.len() < end {
            return Ok(None);
        }
        let payload = &b[HEADER_SIZE..end];
        if xxh3_64(payload) as u32 != checksum {
            return Err(WireError::Checksum);
        }
        let frame = Frame {
            kind,
            id,
            payload: payload.to_vec(),
        };
        self.start += end;
        Ok(Some(frame))
    }
}

/// Builds a payload.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u128(&mut self, v: u128) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn uuid(&mut self, v: Uuid) -> &mut Self {
        self.u128(v.as_u128())
    }

    /// A u32 length, then the bytes.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
        self
    }

    pub fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    /// Bytes without a length: the rest of the payload.
    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }

    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// Reads a payload; every read is checked.
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(n).ok_or(WireError::Malformed)?;
        let s = self.bytes.get(self.at..end).ok_or(WireError::Malformed)?;
        self.at = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| WireError::Malformed)?,
        ))
    }

    pub fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| WireError::Malformed)?,
        ))
    }

    pub fn u128(&mut self) -> Result<u128, WireError> {
        Ok(u128::from_le_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| WireError::Malformed)?,
        ))
    }

    pub fn uuid(&mut self) -> Result<Uuid, WireError> {
        self.u128().map(Uuid::from_u128)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], WireError> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    pub fn str(&mut self) -> Result<String, WireError> {
        core::str::from_utf8(self.bytes()?)
            .map(String::from)
            .map_err(|_| WireError::Malformed)
    }

    /// Everything not read yet.
    pub fn rest(&mut self) -> &'a [u8] {
        let r = &self.bytes[self.at..];
        self.at = self.bytes.len();
        r
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    /// Fails unless everything was read.
    pub fn end(&self) -> Result<(), WireError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(WireError::Malformed)
        }
    }
}
