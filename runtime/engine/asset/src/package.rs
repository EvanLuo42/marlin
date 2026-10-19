//! Packages (`.mpak`): cooked units for shipping, compressed in chunks (0003 §7).
//!
//! ```text
//! header   PackageHeader (64 bytes) at offset 0
//! data     chunks, each compressed on its own
//! index    units, blobs, chunks, objects, names; then xxh3_64 of the index
//! ```
//!
//! A unit is stored as its cooked bytes cut into chunks of at most [`MAX_CHUNK`] raw bytes: the
//! head (header, tables, payload) and then each bulk blob separately, every blob starting a new
//! chunk at a 4 KiB-aligned file offset. Reading a unit reads its chunks in one range and
//! decodes them back into the unit's bytes; a blob can also be read on its own, for streaming
//! texture mips and mesh LODs. Each chunk records its codec, both sizes and a checksum of the
//! stored bytes, so a damaged package is an error, never undefined behavior.
//!
//! Units appear in the order the packager is given (asset path order), so a patch that changes
//! a few units leaves the rest of the file's bytes in place. All integers are little-endian.

use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

use marlin_compress::Codec;
use marlin_hash::xxh3_64;

use crate::Uuid;
use crate::cooked::BULK_ALIGN;
use crate::le::{u16_at, u32_at, u64_at, u128_at};

pub const PACKAGE_MAGIC: [u8; 4] = *b"MPK1";
pub const PACKAGE_VERSION: u16 = 3;
/// Largest raw chunk: big enough for good ratios and few IO requests, small enough that a
/// texture mip or mesh page decodes in parallel pieces.
pub const MAX_CHUNK: u32 = 256 << 10;
/// Alignment of the first chunk of every blob in the file.
pub const BLOB_FILE_ALIGN: u64 = BULK_ALIGN;
/// Alignment of everything else in the file.
pub const CHUNK_FILE_ALIGN: u64 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PackageHeader {
    /// [`crate::cooked::platform_id`] of the platform the units are cooked for.
    pub platform: u32,
    pub unit_count: u32,
    pub blob_count: u32,
    pub chunk_count: u32,
    pub object_count: u32,
    pub name_count: u32,
    /// The build manifest's key the package was made from.
    pub build: u128,
    pub index_offset: u64,
    /// Including the trailing checksum.
    pub index_size: u64,
}

impl PackageHeader {
    pub const SIZE: usize = 64;
}

/// A unit: its chunks reproduce its cooked bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct UnitRecord {
    pub unit: u64,
    /// Size of the cooked unit.
    pub size: u64,
    pub first_chunk: u32,
    pub chunk_count: u32,
    pub first_blob: u32,
    pub blob_count: u32,
}

impl UnitRecord {
    pub const SIZE: usize = 32;
}

/// A bulk blob of a unit, for reading it alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BlobRecord {
    /// Offset in the cooked unit.
    pub unit_offset: u64,
    pub size: u64,
    pub first_chunk: u32,
    pub chunk_count: u32,
    /// The unit's bulk entry flags (`BULK_STREAMED`): a streamed blob is not read with its unit.
    pub flags: u32,
}

impl BlobRecord {
    pub const SIZE: usize = 32;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChunkRecord {
    /// Where the stored bytes are in the file.
    pub file_offset: u64,
    /// Where the raw bytes go in the cooked unit.
    pub unit_offset: u64,
    pub stored_size: u32,
    pub raw_size: u32,
    pub codec: Codec,
    /// Low 32 bits of xxh3_64 of the stored bytes.
    pub checksum: u32,
}

impl ChunkRecord {
    pub const SIZE: usize = 32;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjectRecord {
    pub uuid: Uuid,
    pub unit: u64,
}

impl ObjectRecord {
    pub const SIZE: usize = 24;
}

/// A named object (a shader package, 0003 §4.4): found by name rather than by link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NameRecord {
    /// [`crate::name_hash`] of the name; the cook refuses builds where two names collide.
    pub name: u64,
    pub uuid: Uuid,
}

impl NameRecord {
    pub const SIZE: usize = 24;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PackageError {
    BadMagic,
    Version(u16),
    Truncated,
    /// The index does not match its checksum.
    IndexChecksum,
    /// Tables out of order or ranges that do not fit.
    Malformed,
    /// A chunk's stored bytes do not match their checksum.
    ChunkChecksum {
        chunk: u32,
    },
    UnknownCodec {
        chunk: u32,
    },
    Decompress {
        chunk: u32,
        error: marlin_compress::ErrorKind,
    },
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("not a package"),
            Self::Version(v) => write!(f, "package version {v}, expected {PACKAGE_VERSION}"),
            Self::Truncated => f.write_str("package truncated"),
            Self::IndexChecksum => f.write_str("package index damaged"),
            Self::Malformed => f.write_str("package index malformed"),
            Self::ChunkChecksum { chunk } => write!(f, "package chunk {chunk} damaged"),
            Self::UnknownCodec { chunk } => write!(f, "package chunk {chunk} has an unknown codec"),
            Self::Decompress { chunk, error } => {
                write!(f, "package chunk {chunk} does not decompress: {error:?}")
            }
        }
    }
}

impl core::error::Error for PackageError {}

/// Reads the header at the start of a package.
pub fn read_header(bytes: &[u8]) -> Result<PackageHeader, PackageError> {
    if bytes.len() < PackageHeader::SIZE {
        return Err(PackageError::Truncated);
    }
    if bytes[..4] != PACKAGE_MAGIC {
        return Err(PackageError::BadMagic);
    }
    let version = u16_at(bytes, 4);
    if version != PACKAGE_VERSION {
        return Err(PackageError::Version(version));
    }
    Ok(PackageHeader {
        platform: u32_at(bytes, 8),
        unit_count: u32_at(bytes, 12),
        blob_count: u32_at(bytes, 16),
        chunk_count: u32_at(bytes, 20),
        object_count: u32_at(bytes, 24),
        name_count: u32_at(bytes, 28),
        build: u128_at(bytes, 32),
        index_offset: u64_at(bytes, 48),
        index_size: u64_at(bytes, 56),
    })
}

/// A package's tables, checked: every range in order and in bounds.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PackageIndex {
    pub header: PackageHeader,
    /// Sorted by unit id (for lookup; file order is the packager's).
    pub units: Vec<UnitRecord>,
    pub blobs: Vec<BlobRecord>,
    pub chunks: Vec<ChunkRecord>,
    /// Sorted by UUID.
    pub objects: Vec<ObjectRecord>,
    /// Sorted by name hash; every object is in `objects`.
    pub names: Vec<NameRecord>,
}

impl PackageIndex {
    /// Parses and checks the index bytes (`header.index_size` of them, read at
    /// `header.index_offset`).
    pub fn parse(header: PackageHeader, index: &[u8]) -> Result<Self, PackageError> {
        let counts = [
            (header.unit_count as u64, UnitRecord::SIZE as u64),
            (header.blob_count as u64, BlobRecord::SIZE as u64),
            (header.chunk_count as u64, ChunkRecord::SIZE as u64),
            (header.object_count as u64, ObjectRecord::SIZE as u64),
            (header.name_count as u64, NameRecord::SIZE as u64),
        ];
        let tables: u64 = counts.iter().map(|(n, s)| n * s).sum();
        if index.len() as u64 != header.index_size || header.index_size != tables + 8 {
            return Err(PackageError::Truncated);
        }
        let body = &index[..tables as usize];
        if xxh3_64(body) != u64_at(index, tables as usize) {
            return Err(PackageError::IndexChecksum);
        }
        let mut at = 0usize;
        let mut units = Vec::with_capacity(header.unit_count as usize);
        for _ in 0..header.unit_count {
            units.push(UnitRecord {
                unit: u64_at(body, at),
                size: u64_at(body, at + 8),
                first_chunk: u32_at(body, at + 16),
                chunk_count: u32_at(body, at + 20),
                first_blob: u32_at(body, at + 24),
                blob_count: u32_at(body, at + 28),
            });
            at += UnitRecord::SIZE;
        }
        let mut blobs = Vec::with_capacity(header.blob_count as usize);
        for _ in 0..header.blob_count {
            blobs.push(BlobRecord {
                unit_offset: u64_at(body, at),
                size: u64_at(body, at + 8),
                first_chunk: u32_at(body, at + 16),
                chunk_count: u32_at(body, at + 20),
                flags: u32_at(body, at + 24),
            });
            at += BlobRecord::SIZE;
        }
        let mut chunks = Vec::with_capacity(header.chunk_count as usize);
        for i in 0..header.chunk_count {
            let codec = body[at + 24];
            chunks.push(ChunkRecord {
                file_offset: u64_at(body, at),
                unit_offset: u64_at(body, at + 8),
                stored_size: u32_at(body, at + 16),
                raw_size: u32_at(body, at + 20),
                codec: Codec::from_id(codec).ok_or(PackageError::UnknownCodec { chunk: i })?,
                checksum: u32_at(body, at + 28),
            });
            at += ChunkRecord::SIZE;
        }
        let mut objects = Vec::with_capacity(header.object_count as usize);
        for _ in 0..header.object_count {
            objects.push(ObjectRecord {
                uuid: Uuid::from_u128(u128_at(body, at)),
                unit: u64_at(body, at + 16),
            });
            at += ObjectRecord::SIZE;
        }
        let mut names = Vec::with_capacity(header.name_count as usize);
        for _ in 0..header.name_count {
            names.push(NameRecord {
                name: u64_at(body, at),
                uuid: Uuid::from_u128(u128_at(body, at + 8)),
            });
            at += NameRecord::SIZE;
        }
        let index = Self {
            header,
            units,
            blobs,
            chunks,
            objects,
            names,
        };
        index.check()?;
        Ok(index)
    }

    fn check(&self) -> Result<(), PackageError> {
        let bad = Err(PackageError::Malformed);
        if !self.units.windows(2).all(|w| w[0].unit < w[1].unit)
            || !self.objects.windows(2).all(|w| w[0].uuid < w[1].uuid)
            || !self.names.windows(2).all(|w| w[0].name < w[1].name)
        {
            return bad;
        }
        let data_end = self.header.index_offset;
        for c in &self.chunks {
            if c.raw_size == 0
                || c.raw_size > MAX_CHUNK
                || c.file_offset < PackageHeader::SIZE as u64
                || c.file_offset
                    .checked_add(c.stored_size as u64)
                    .is_none_or(|e| e > data_end)
                || (c.codec == Codec::None && c.stored_size != c.raw_size)
            {
                return bad;
            }
        }
        let chunk_range = |first: u32, count: u32| {
            (first as usize)
                .checked_add(count as usize)
                .filter(|&e| e <= self.chunks.len())
                .map(|e| first as usize..e)
        };
        for u in &self.units {
            let Some(range) = chunk_range(u.first_chunk, u.chunk_count) else {
                return bad;
            };
            // Chunks in unit order, inside the unit, not overlapping.
            let mut end = 0u64;
            for c in &self.chunks[range] {
                if c.unit_offset < end || c.unit_offset + c.raw_size as u64 > u.size {
                    return bad;
                }
                end = c.unit_offset + c.raw_size as u64;
            }
            let blobs_end = u.first_blob as usize + u.blob_count as usize;
            if blobs_end > self.blobs.len() {
                return bad;
            }
            for b in &self.blobs[u.first_blob as usize..blobs_end] {
                let Some(range) = chunk_range(b.first_chunk, b.chunk_count) else {
                    return bad;
                };
                let unit_chunks = u.first_chunk as usize..(u.first_chunk + u.chunk_count) as usize;
                let covered: u64 = self.chunks[range.clone()]
                    .iter()
                    .map(|c| c.raw_size as u64)
                    .sum();
                if !(unit_chunks.start <= range.start && range.end <= unit_chunks.end)
                    || b.unit_offset.checked_add(b.size).is_none_or(|e| e > u.size)
                    || covered != b.size
                    || self
                        .chunks
                        .get(range.start)
                        .is_some_and(|c| c.unit_offset != b.unit_offset)
                {
                    return bad;
                }
            }
        }
        if self.objects.iter().any(|o| self.unit(o.unit).is_none())
            || self
                .names
                .iter()
                .any(|n| self.object_unit(n.uuid).is_none())
        {
            return bad;
        }
        Ok(())
    }

    /// The object called `name` (by [`crate::name_hash`]).
    pub fn named(&self, name: u64) -> Option<Uuid> {
        self.names
            .binary_search_by_key(&name, |n| n.name)
            .ok()
            .map(|i| self.names[i].uuid)
    }

    pub fn unit(&self, unit: u64) -> Option<&UnitRecord> {
        self.units
            .binary_search_by_key(&unit, |u| u.unit)
            .ok()
            .map(|i| &self.units[i])
    }

    pub fn object_unit(&self, uuid: Uuid) -> Option<u64> {
        self.objects
            .binary_search_by_key(&uuid, |o| o.uuid)
            .ok()
            .map(|i| self.objects[i].unit)
    }

    /// The file range holding `chunks`.
    pub fn file_range(&self, chunks: Range<usize>) -> Range<u64> {
        let c = &self.chunks[chunks];
        match (c.first(), c.last()) {
            (Some(a), Some(b)) => a.file_offset..b.file_offset + b.stored_size as u64,
            _ => 0..0,
        }
    }

    /// The chunks of `unit`, and the file range they occupy.
    pub fn unit_read(&self, unit: &UnitRecord) -> (Range<usize>, Range<u64>) {
        let chunks = unit.first_chunk as usize..(unit.first_chunk + unit.chunk_count) as usize;
        let range = self.file_range(chunks.clone());
        (chunks, range)
    }

    /// What loading `unit` without its streamed blobs reads: its chunks up to the last one that
    /// is not a streamed blob's, the file range they occupy, and the bytes they decode to (the
    /// unit cut after them). Streamed blobs between resident ones are read along.
    pub fn unit_resident_read(&self, unit: &UnitRecord) -> (Range<usize>, Range<u64>, u64) {
        let first = unit.first_chunk as usize;
        let all = first..first + unit.chunk_count as usize;
        let blobs_end = (unit.first_blob + unit.blob_count) as usize;
        let streamed = |chunk: usize| {
            self.blobs[unit.first_blob as usize..blobs_end]
                .iter()
                .any(|b| {
                    b.flags & crate::cooked::BULK_STREAMED != 0
                        && (b.first_chunk as usize..(b.first_chunk + b.chunk_count) as usize)
                            .contains(&chunk)
                })
        };
        let end = all
            .clone()
            .rev()
            .find(|&c| !streamed(c))
            .map_or(first, |c| c + 1);
        let chunks = first..end;
        let size = self.chunks[chunks.clone()]
            .iter()
            .map(|c| c.unit_offset + u64::from(c.raw_size))
            .max()
            .unwrap_or(0);
        let range = self.file_range(chunks.clone());
        (chunks, range, size)
    }

    /// Decodes `chunks`, whose stored bytes are `stored` (read from `file_start` on), into `out`
    /// at each chunk's unit offset minus `out_base`. Bytes no chunk covers (alignment padding)
    /// are left as they are; pass zeroed `out`.
    pub fn decode(
        &self,
        chunks: Range<usize>,
        stored: &[u8],
        file_start: u64,
        out: &mut [u8],
        out_base: u64,
    ) -> Result<(), PackageError> {
        for i in chunks {
            let c = &self.chunks[i];
            let chunk = i as u32;
            let src_at = c
                .file_offset
                .checked_sub(file_start)
                .ok_or(PackageError::Truncated)? as usize;
            let src = stored
                .get(src_at..src_at + c.stored_size as usize)
                .ok_or(PackageError::Truncated)?;
            if xxh3_64(src) as u32 != c.checksum {
                return Err(PackageError::ChunkChecksum { chunk });
            }
            let dst_at = c
                .unit_offset
                .checked_sub(out_base)
                .ok_or(PackageError::Malformed)? as usize;
            let dst = out
                .get_mut(dst_at..dst_at + c.raw_size as usize)
                .ok_or(PackageError::Malformed)?;
            let n = marlin_compress::decompress(c.codec, src, dst).map_err(|e| {
                PackageError::Decompress {
                    chunk,
                    error: e.kind(),
                }
            })?;
            if n != c.raw_size as usize {
                return Err(PackageError::Decompress {
                    chunk,
                    error: marlin_compress::ErrorKind::Corrupt,
                });
            }
        }
        Ok(())
    }
}

/// Writing packages (for the packager in `tools/cook_core`): the records in their on-disk form.
pub mod write {
    use alloc::vec::Vec;

    use super::{
        BlobRecord, ChunkRecord, NameRecord, ObjectRecord, PACKAGE_MAGIC, PACKAGE_VERSION,
        PackageHeader, UnitRecord,
    };

    pub fn header(h: &PackageHeader) -> [u8; PackageHeader::SIZE] {
        let mut out = [0u8; PackageHeader::SIZE];
        out[..4].copy_from_slice(&PACKAGE_MAGIC);
        out[4..6].copy_from_slice(&PACKAGE_VERSION.to_le_bytes());
        out[8..12].copy_from_slice(&h.platform.to_le_bytes());
        out[12..16].copy_from_slice(&h.unit_count.to_le_bytes());
        out[16..20].copy_from_slice(&h.blob_count.to_le_bytes());
        out[20..24].copy_from_slice(&h.chunk_count.to_le_bytes());
        out[24..28].copy_from_slice(&h.object_count.to_le_bytes());
        out[28..32].copy_from_slice(&h.name_count.to_le_bytes());
        out[32..48].copy_from_slice(&h.build.to_le_bytes());
        out[48..56].copy_from_slice(&h.index_offset.to_le_bytes());
        out[56..64].copy_from_slice(&h.index_size.to_le_bytes());
        out
    }

    pub fn unit(out: &mut Vec<u8>, u: &UnitRecord) {
        out.extend_from_slice(&u.unit.to_le_bytes());
        out.extend_from_slice(&u.size.to_le_bytes());
        out.extend_from_slice(&u.first_chunk.to_le_bytes());
        out.extend_from_slice(&u.chunk_count.to_le_bytes());
        out.extend_from_slice(&u.first_blob.to_le_bytes());
        out.extend_from_slice(&u.blob_count.to_le_bytes());
    }

    pub fn blob(out: &mut Vec<u8>, b: &BlobRecord) {
        out.extend_from_slice(&b.unit_offset.to_le_bytes());
        out.extend_from_slice(&b.size.to_le_bytes());
        out.extend_from_slice(&b.first_chunk.to_le_bytes());
        out.extend_from_slice(&b.chunk_count.to_le_bytes());
        out.extend_from_slice(&b.flags.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
    }

    pub fn chunk(out: &mut Vec<u8>, c: &ChunkRecord) {
        out.extend_from_slice(&c.file_offset.to_le_bytes());
        out.extend_from_slice(&c.unit_offset.to_le_bytes());
        out.extend_from_slice(&c.stored_size.to_le_bytes());
        out.extend_from_slice(&c.raw_size.to_le_bytes());
        out.push(c.codec.id());
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&c.checksum.to_le_bytes());
    }

    pub fn object(out: &mut Vec<u8>, o: &ObjectRecord) {
        out.extend_from_slice(&o.uuid.as_u128().to_le_bytes());
        out.extend_from_slice(&o.unit.to_le_bytes());
    }

    pub fn name(out: &mut Vec<u8>, n: &NameRecord) {
        out.extend_from_slice(&n.name.to_le_bytes());
        out.extend_from_slice(&n.uuid.as_u128().to_le_bytes());
    }

    /// The index: tables in order, then the checksum.
    pub fn index(
        units: &[UnitRecord],
        blobs: &[BlobRecord],
        chunks: &[ChunkRecord],
        objects: &[ObjectRecord],
        names: &[NameRecord],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        units.iter().for_each(|u| unit(&mut out, u));
        blobs.iter().for_each(|b| blob(&mut out, b));
        chunks.iter().for_each(|c| chunk(&mut out, c));
        objects.iter().for_each(|o| object(&mut out, o));
        names.iter().for_each(|n| name(&mut out, n));
        let checksum = marlin_hash::xxh3_64(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }
}
