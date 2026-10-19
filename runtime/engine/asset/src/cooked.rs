//! The cooked unit: what the cooker makes of one `.masset` file and the runtime loads in one
//! piece (`docs/design/0003-asset-pipeline.md` §4.3).
//!
//! ```text
//! header      UnitHeader
//! objects     [ObjectEntry; object_count]    sorted by UUID
//! imports     [ImportEntry; import_count]    sorted by UUID: objects of other units linked here
//! bulk table  [BulkEntry; bulk_count]
//! payload     each object's payload, 16-byte aligned
//! bulk        GPU-bound blobs (vertices, texture mips), each 4 KiB aligned
//! ```
//!
//! All integers little-endian, offsets relative to the start of the unit. Readers check every
//! range, so a damaged unit is an error, never undefined behavior. Cooked data is
//! platform-specific: a unit is cooked for one platform.
//!
//! A blob marked [`BULK_STREAMED`] is read on its own when wanted (a mesh's finer cluster pages):
//! sources that can read blobs alone load a unit without them, so the unit's bytes may end
//! before them. [`UnitView::bulk_present`] says whether a blob's bytes are there.

use core::mem::size_of;

use crate::Uuid;
use crate::le::{u16_at, u32_at, u64_at, u128_at};

pub const UNIT_MAGIC: [u8; 4] = *b"MCU1";
pub const UNIT_VERSION: u16 = 1;
/// Payload alignment.
pub const PAYLOAD_ALIGN: u64 = 16;
/// Bulk alignment: a page, as GPU uploads and DirectStorage want.
pub const BULK_ALIGN: u64 = 4096;

/// How an object's payload is encoded.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum Encoding {
    /// marlin-serial's evolvable reflection format, read into a value of the object's type.
    Serial = 0,
    /// A marlin-serial cooked layout (`#[repr(C)]` with relative pointers), used in place.
    Cooked = 1,
}

/// What a bulk blob holds, so loaders route it without parsing the payload.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u32)]
pub enum BulkKind {
    Raw = 0,
    /// Texture data in the RHI's copy layout.
    Texture = 1,
    Vertices = 2,
    Indices = 3,
}

/// A unit's identity: a hash of its asset path (the `.masset` file's path without extension).
pub fn unit_id(asset_path: &str) -> u64 {
    marlin_hash::xxh3_64(asset_path.as_bytes())
}

/// A platform's identity in unit headers: a hash of its name (`windows`, `ps5`, …).
pub fn platform_id(name: &str) -> u32 {
    marlin_hash::fnv1a_32(name.as_bytes())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UnitHeader {
    pub unit: u64,
    pub platform: u32,
    pub object_count: u32,
    pub import_count: u32,
    pub bulk_count: u32,
    pub payload_offset: u64,
    pub payload_size: u64,
    pub bulk_offset: u64,
    pub bulk_size: u64,
}

impl UnitHeader {
    pub const SIZE: usize = 64;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObjectEntry {
    pub uuid: Uuid,
    /// marlin-reflect stable type id of the runtime type the payload holds.
    pub type_id: u64,
    pub encoding: Encoding,
    /// Relative to the payload region.
    pub payload_offset: u64,
    pub payload_size: u64,
    /// The object's bulk blobs: `bulk_first..bulk_first + bulk_count` in the bulk table.
    pub bulk_first: u32,
    pub bulk_count: u32,
}

impl ObjectEntry {
    pub const SIZE: usize = 64;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImportEntry {
    /// An object of another unit that this unit links to.
    pub uuid: Uuid,
    /// The unit holding it.
    pub unit: u64,
}

impl ImportEntry {
    pub const SIZE: usize = 32;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BulkEntry {
    /// Relative to the bulk region; a multiple of [`BULK_ALIGN`].
    pub offset: u64,
    pub size: u64,
    pub kind: BulkKind,
    /// [`BULK_STREAMED`] or 0.
    pub flags: u32,
}

/// [`BulkEntry::flags`]: the blob is read on its own when wanted, not with its unit.
pub const BULK_STREAMED: u32 = 1;

impl BulkEntry {
    pub const SIZE: usize = 24;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::Error)]
pub enum UnitError {
    #[error("not a cooked unit")]
    BadMagic,
    #[error("cooked unit version {0} is not supported")]
    Version(u16),
    #[error("cooked unit is truncated or has an out-of-range table")]
    Truncated,
    #[error("cooked unit has an unknown encoding or bulk kind")]
    BadEnum,
    #[error("cooked unit tables are not sorted by UUID")]
    Unsorted,
}

/// A checked view of a cooked unit's bytes.
#[derive(Clone, Copy, Debug)]
pub struct UnitView<'a> {
    bytes: &'a [u8],
    header: UnitHeader,
}

impl<'a> UnitView<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, UnitError> {
        if bytes.len() < UnitHeader::SIZE {
            return Err(UnitError::Truncated);
        }
        if bytes[..4] != UNIT_MAGIC {
            return Err(UnitError::BadMagic);
        }
        let version = u16_at(bytes, 4);
        if version != UNIT_VERSION {
            return Err(UnitError::Version(version));
        }
        let header = UnitHeader {
            unit: u64_at(bytes, 8),
            platform: u32_at(bytes, 16),
            object_count: u32_at(bytes, 20),
            import_count: u32_at(bytes, 24),
            bulk_count: u32_at(bytes, 28),
            payload_offset: u64_at(bytes, 32),
            payload_size: u64_at(bytes, 40),
            bulk_offset: u64_at(bytes, 48),
            bulk_size: u64_at(bytes, 56),
        };
        let view = Self { bytes, header };
        let tables_end =
            view.bulk_table_offset() as u64 + u64::from(header.bulk_count) * BulkEntry::SIZE as u64;
        let len = bytes.len() as u64;
        let in_range = |offset: u64, size: u64| offset.checked_add(size).is_some_and(|e| e <= len);
        // The bulk region may run past the bytes: streamed blobs at its end can be absent.
        if tables_end > len
            || !in_range(header.payload_offset, header.payload_size)
            || header.bulk_offset.checked_add(header.bulk_size).is_none()
        {
            return Err(UnitError::Truncated);
        }
        let mut previous = None;
        for i in 0..header.object_count as usize {
            let entry = view.object(i)?;
            if entry
                .payload_offset
                .checked_add(entry.payload_size)
                .is_none_or(|e| e > header.payload_size)
                || u64::from(entry.bulk_first) + u64::from(entry.bulk_count)
                    > u64::from(header.bulk_count)
            {
                return Err(UnitError::Truncated);
            }
            if previous.is_some_and(|p| p >= entry.uuid) {
                return Err(UnitError::Unsorted);
            }
            previous = Some(entry.uuid);
        }
        for i in 0..header.bulk_count as usize {
            let bulk = view.bulk(i)?;
            let end = bulk.offset.checked_add(bulk.size);
            if end.is_none_or(|e| e > header.bulk_size)
                || (bulk.flags & BULK_STREAMED == 0 && !view.bulk_present(&bulk))
            {
                return Err(UnitError::Truncated);
            }
        }
        Ok(view)
    }

    pub fn header(&self) -> &UnitHeader {
        &self.header
    }

    fn objects_offset(&self) -> usize {
        UnitHeader::SIZE
    }

    fn imports_offset(&self) -> usize {
        self.objects_offset() + self.header.object_count as usize * ObjectEntry::SIZE
    }

    fn bulk_table_offset(&self) -> usize {
        self.imports_offset() + self.header.import_count as usize * ImportEntry::SIZE
    }

    pub fn object_count(&self) -> usize {
        self.header.object_count as usize
    }

    pub fn object(&self, index: usize) -> Result<ObjectEntry, UnitError> {
        if index >= self.object_count() {
            return Err(UnitError::Truncated);
        }
        let at = self.objects_offset() + index * ObjectEntry::SIZE;
        let b = self
            .bytes
            .get(at..at + ObjectEntry::SIZE)
            .ok_or(UnitError::Truncated)?;
        let encoding = match u32_at(b, 24) {
            0 => Encoding::Serial,
            1 => Encoding::Cooked,
            _ => return Err(UnitError::BadEnum),
        };
        Ok(ObjectEntry {
            uuid: Uuid::from_u128(u128_at(b, 0)),
            type_id: u64_at(b, 16),
            encoding,
            payload_offset: u64_at(b, 32),
            payload_size: u64_at(b, 40),
            bulk_first: u32_at(b, 48),
            bulk_count: u32_at(b, 52),
        })
    }

    /// The object with `uuid`, by binary search.
    pub fn find(&self, uuid: Uuid) -> Option<(usize, ObjectEntry)> {
        let (mut lo, mut hi) = (0, self.object_count());
        while lo < hi {
            let mid = (lo + hi) / 2;
            let entry = self.object(mid).ok()?;
            match entry.uuid.cmp(&uuid) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some((mid, entry)),
            }
        }
        None
    }

    pub fn import_count(&self) -> usize {
        self.header.import_count as usize
    }

    pub fn import(&self, index: usize) -> Result<ImportEntry, UnitError> {
        if index >= self.import_count() {
            return Err(UnitError::Truncated);
        }
        let at = self.imports_offset() + index * ImportEntry::SIZE;
        let b = self
            .bytes
            .get(at..at + ImportEntry::SIZE)
            .ok_or(UnitError::Truncated)?;
        Ok(ImportEntry {
            uuid: Uuid::from_u128(u128_at(b, 0)),
            unit: u64_at(b, 16),
        })
    }

    pub fn bulk_count(&self) -> usize {
        self.header.bulk_count as usize
    }

    pub fn bulk(&self, index: usize) -> Result<BulkEntry, UnitError> {
        if index >= self.bulk_count() {
            return Err(UnitError::Truncated);
        }
        let at = self.bulk_table_offset() + index * BulkEntry::SIZE;
        let b = self
            .bytes
            .get(at..at + BulkEntry::SIZE)
            .ok_or(UnitError::Truncated)?;
        let kind = match u32_at(b, 16) {
            0 => BulkKind::Raw,
            1 => BulkKind::Texture,
            2 => BulkKind::Vertices,
            3 => BulkKind::Indices,
            _ => return Err(UnitError::BadEnum),
        };
        Ok(BulkEntry {
            offset: u64_at(b, 0),
            size: u64_at(b, 8),
            kind,
            flags: u32_at(b, 20),
        })
    }

    /// The payload bytes of object `entry`.
    pub fn payload(&self, entry: &ObjectEntry) -> &'a [u8] {
        let start = (self.header.payload_offset + entry.payload_offset) as usize;
        &self.bytes[start..start + entry.payload_size as usize]
    }

    /// Whether bulk blob `entry`'s bytes are in the unit (a streamed blob may be absent).
    pub fn bulk_present(&self, entry: &BulkEntry) -> bool {
        self.header
            .bulk_offset
            .checked_add(entry.offset)
            .and_then(|s| s.checked_add(entry.size))
            .is_some_and(|e| e <= self.bytes.len() as u64)
    }

    /// The bytes of bulk blob `entry`; empty if it is a streamed blob the unit was loaded
    /// without.
    pub fn bulk_bytes(&self, entry: &BulkEntry) -> &'a [u8] {
        if !self.bulk_present(entry) {
            return &[];
        }
        let start = (self.header.bulk_offset + entry.offset) as usize;
        &self.bytes[start..start + entry.size as usize]
    }

    /// Bulk blobs of object `entry`.
    pub fn object_bulk(&self, entry: &ObjectEntry) -> impl Iterator<Item = BulkEntry> + '_ {
        (entry.bulk_first..entry.bulk_first + entry.bulk_count)
            .filter_map(|i| self.bulk(i as usize).ok())
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Writes the fixed-size records; [`UnitView`] reads them back. Used by the cooker.
pub mod write {
    use alloc::vec::Vec;

    use super::*;

    pub fn header(out: &mut Vec<u8>, h: &UnitHeader) {
        out.extend_from_slice(&UNIT_MAGIC);
        out.extend_from_slice(&UNIT_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&h.unit.to_le_bytes());
        out.extend_from_slice(&h.platform.to_le_bytes());
        out.extend_from_slice(&h.object_count.to_le_bytes());
        out.extend_from_slice(&h.import_count.to_le_bytes());
        out.extend_from_slice(&h.bulk_count.to_le_bytes());
        out.extend_from_slice(&h.payload_offset.to_le_bytes());
        out.extend_from_slice(&h.payload_size.to_le_bytes());
        out.extend_from_slice(&h.bulk_offset.to_le_bytes());
        out.extend_from_slice(&h.bulk_size.to_le_bytes());
        debug_assert_eq!(out.len() % UnitHeader::SIZE, 0);
    }

    pub fn object(out: &mut Vec<u8>, e: &ObjectEntry) {
        out.extend_from_slice(&e.uuid.as_u128().to_le_bytes());
        out.extend_from_slice(&e.type_id.to_le_bytes());
        out.extend_from_slice(&(e.encoding as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&e.payload_offset.to_le_bytes());
        out.extend_from_slice(&e.payload_size.to_le_bytes());
        out.extend_from_slice(&e.bulk_first.to_le_bytes());
        out.extend_from_slice(&e.bulk_count.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
    }

    pub fn import(out: &mut Vec<u8>, e: &ImportEntry) {
        out.extend_from_slice(&e.uuid.as_u128().to_le_bytes());
        out.extend_from_slice(&e.unit.to_le_bytes());
        out.extend_from_slice(&[0; 8]);
    }

    pub fn bulk(out: &mut Vec<u8>, e: &BulkEntry) {
        out.extend_from_slice(&e.offset.to_le_bytes());
        out.extend_from_slice(&e.size.to_le_bytes());
        out.extend_from_slice(&(e.kind as u32).to_le_bytes());
        out.extend_from_slice(&e.flags.to_le_bytes());
    }

    const _: () = assert!(size_of::<u128>() + 8 + 4 + 4 + 8 + 8 + 4 + 4 + 8 == ObjectEntry::SIZE);

    /// One object of a unit to assemble.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct UnitObject {
        pub uuid: Uuid,
        pub type_id: u64,
        pub encoding: Encoding,
        pub payload: Vec<u8>,
        pub bulk: Vec<UnitBulk>,
    }

    /// A bulk blob of an object to assemble.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct UnitBulk {
        pub kind: BulkKind,
        /// Read on its own when wanted ([`BULK_STREAMED`]).
        pub streamed: bool,
        pub data: Vec<u8>,
    }

    impl From<(BulkKind, Vec<u8>)> for UnitBulk {
        fn from((kind, data): (BulkKind, Vec<u8>)) -> Self {
            Self {
                kind,
                streamed: false,
                data,
            }
        }
    }

    fn pad(out: &mut Vec<u8>, align: u64) {
        let len = (out.len() as u64).next_multiple_of(align);
        out.resize(len as usize, 0);
    }

    /// Lays out a whole unit: objects sorted by UUID, `imports` (UUID, unit) sorted, payloads
    /// 16-aligned, blobs page-aligned.
    pub fn unit(
        unit: u64,
        platform: u32,
        mut objects: Vec<UnitObject>,
        imports: &[(Uuid, u64)],
    ) -> Vec<u8> {
        objects.sort_by_key(|o| o.uuid);
        let mut imports = imports.to_vec();
        imports.sort_unstable();
        imports.dedup_by_key(|i| i.0);
        let tables = UnitHeader::SIZE
            + objects.len() * ObjectEntry::SIZE
            + imports.len() * ImportEntry::SIZE
            + objects.iter().map(|o| o.bulk.len()).sum::<usize>() * BulkEntry::SIZE;
        let payload_offset = (tables as u64).next_multiple_of(PAYLOAD_ALIGN);

        // Payload and bulk regions first, to know the offsets.
        let mut payload = Vec::new();
        let mut bulk = Vec::new();
        let mut entries = Vec::with_capacity(objects.len());
        let mut bulk_entries = Vec::new();
        for o in &objects {
            pad(&mut payload, PAYLOAD_ALIGN);
            let payload_start = payload.len() as u64;
            payload.extend_from_slice(&o.payload);
            let bulk_first = bulk_entries.len() as u32;
            for b in &o.bulk {
                pad(&mut bulk, BULK_ALIGN);
                bulk_entries.push(BulkEntry {
                    offset: bulk.len() as u64,
                    size: b.data.len() as u64,
                    kind: b.kind,
                    flags: if b.streamed { BULK_STREAMED } else { 0 },
                });
                bulk.extend_from_slice(&b.data);
            }
            entries.push(ObjectEntry {
                uuid: o.uuid,
                type_id: o.type_id,
                encoding: o.encoding,
                payload_offset: payload_start,
                payload_size: o.payload.len() as u64,
                bulk_first,
                bulk_count: o.bulk.len() as u32,
            });
        }
        let bulk_offset = (payload_offset + payload.len() as u64).next_multiple_of(BULK_ALIGN);

        let mut out = Vec::with_capacity(bulk_offset as usize + bulk.len());
        header(
            &mut out,
            &UnitHeader {
                unit,
                platform,
                object_count: entries.len() as u32,
                import_count: imports.len() as u32,
                bulk_count: bulk_entries.len() as u32,
                payload_offset,
                payload_size: payload.len() as u64,
                bulk_offset: if bulk.is_empty() { 0 } else { bulk_offset },
                bulk_size: bulk.len() as u64,
            },
        );
        for e in &entries {
            object(&mut out, e);
        }
        for &(uuid, unit) in &imports {
            import(&mut out, &ImportEntry { uuid, unit });
        }
        for e in &bulk_entries {
            self::bulk(&mut out, e);
        }
        pad(&mut out, PAYLOAD_ALIGN);
        out.extend_from_slice(&payload);
        if !bulk.is_empty() {
            out.resize(bulk_offset as usize, 0);
            out.extend_from_slice(&bulk);
        }
        out
    }
}
