//! Where cooked units come from: sources (packages, memory, later the cook service) mounted in
//! layers (0003 §7).
//!
//! Units are found by unit id and objects by UUID; a layer mounted later overrides the units of
//! the layers below it (patches, DLC, mods, a developer's local changes over a shared build).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use marlin_containers::HashMap;
use marlin_io::File;
use marlin_serial::cooked::CookedBuffer;

use crate::Uuid;
use crate::cooked::{BULK_ALIGN, UnitView};
use crate::package::{PackageError, PackageHeader, PackageIndex, read_header};

/// Cooked unit bytes, aligned for in-place use (payloads to 16, bulk to a page).
pub type UnitBytes = CookedBuffer;

/// Alignment of unit buffers.
pub const UNIT_ALIGN: usize = BULK_ALIGN as usize;

#[derive(Debug)]
pub enum SourceError {
    /// No layer has the unit.
    NotFound,
    Io(marlin_io::Error),
    Package(PackageError),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("unit not found"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Package(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for SourceError {}

impl From<marlin_io::Error> for SourceError {
    fn from(e: marlin_io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<PackageError> for SourceError {
    fn from(e: PackageError) -> Self {
        Self::Package(e)
    }
}

/// How a unit is read.
pub enum Read<'a> {
    /// Read `len` bytes of `file` at `offset` (asynchronously, through an `IoQueue`), then hand
    /// them to [`UnitSource::finish`].
    File {
        file: &'a File,
        offset: u64,
        len: usize,
    },
    /// The bytes, already there.
    Ready(UnitBytes),
}

/// A provider of cooked units.
pub trait UnitSource: Send + Sync {
    fn contains(&self, unit: u64) -> bool;
    /// The unit holding object `uuid`, if this source has it.
    fn object_unit(&self, uuid: Uuid) -> Option<u64>;
    fn begin(&self, unit: u64) -> Result<Read<'_>, SourceError>;
    /// Turns the bytes read for a [`Read::File`] into the unit (decompression: may run on any
    /// thread).
    fn finish(&self, unit: u64, read: &[u8]) -> Result<UnitBytes, SourceError>;
}

/// A mounted package file.
pub struct Package {
    file: File,
    index: PackageIndex,
}

impl Package {
    pub fn open(path: impl AsRef<marlin_io::Path>) -> Result<Self, SourceError> {
        let file = File::open(path)?;
        let mut header = [0u8; PackageHeader::SIZE];
        file.read_exact_at(&mut header, 0)?;
        let header = read_header(&header)?;
        let size = file.size()?;
        if header
            .index_offset
            .checked_add(header.index_size)
            .is_none_or(|e| e > size)
        {
            return Err(PackageError::Truncated.into());
        }
        let mut index = alloc::vec![0u8; header.index_size as usize];
        file.read_exact_at(&mut index, header.index_offset)?;
        let index = PackageIndex::parse(header, &index)?;
        Ok(Self { file, index })
    }

    pub fn index(&self) -> &PackageIndex {
        &self.index
    }

    /// Reads and decodes `unit` now.
    pub fn read_unit(&self, unit: u64) -> Result<UnitBytes, SourceError> {
        let record = self.index.unit(unit).ok_or(SourceError::NotFound)?;
        let (_, range) = self.index.unit_read(record);
        let mut stored = alloc::vec![0u8; (range.end - range.start) as usize];
        self.file.read_exact_at(&mut stored, range.start)?;
        self.finish(unit, &stored)
    }

    /// Reads one bulk blob of `unit` (blob `blob` of the unit's bulk table), for streaming.
    pub fn read_blob(&self, unit: u64, blob: u32) -> Result<CookedBuffer, SourceError> {
        let record = self.index.unit(unit).ok_or(SourceError::NotFound)?;
        if blob >= record.blob_count {
            return Err(SourceError::NotFound);
        }
        let b = self.index.blobs[(record.first_blob + blob) as usize];
        let chunks = b.first_chunk as usize..(b.first_chunk + b.chunk_count) as usize;
        let range = self.index.file_range(chunks.clone());
        let mut stored = alloc::vec![0u8; (range.end - range.start) as usize];
        self.file.read_exact_at(&mut stored, range.start)?;
        let mut out = CookedBuffer::zeroed(b.size as usize, UNIT_ALIGN);
        self.index.decode(
            chunks,
            &stored,
            range.start,
            out.as_mut_slice(),
            b.unit_offset,
        )?;
        Ok(out)
    }
}

impl UnitSource for Package {
    fn contains(&self, unit: u64) -> bool {
        self.index.unit(unit).is_some()
    }

    fn object_unit(&self, uuid: Uuid) -> Option<u64> {
        self.index.object_unit(uuid)
    }

    fn begin(&self, unit: u64) -> Result<Read<'_>, SourceError> {
        let record = self.index.unit(unit).ok_or(SourceError::NotFound)?;
        let (_, range) = self.index.unit_read(record);
        Ok(Read::File {
            file: &self.file,
            offset: range.start,
            len: (range.end - range.start) as usize,
        })
    }

    fn finish(&self, unit: u64, read: &[u8]) -> Result<UnitBytes, SourceError> {
        let record = self.index.unit(unit).ok_or(SourceError::NotFound)?;
        let (chunks, range) = self.index.unit_read(record);
        let mut out = CookedBuffer::zeroed(record.size as usize, UNIT_ALIGN);
        self.index
            .decode(chunks, read, range.start, out.as_mut_slice(), 0)?;
        Ok(out)
    }
}

/// Units held in memory: tests, tools, and units streamed in by other means.
#[derive(Default)]
pub struct MemorySource {
    units: HashMap<u64, Vec<u8>>,
    objects: HashMap<Uuid, u64>,
}

impl MemorySource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a cooked unit, indexing its objects. Fails if the bytes are not a unit.
    pub fn insert(&mut self, bytes: Vec<u8>) -> Result<u64, crate::cooked::UnitError> {
        let view = UnitView::new(&bytes)?;
        let unit = view.header().unit;
        for i in 0..view.object_count() {
            self.objects.insert(view.object(i)?.uuid, unit);
        }
        self.units.insert(unit, bytes);
        Ok(unit)
    }
}

impl UnitSource for MemorySource {
    fn contains(&self, unit: u64) -> bool {
        self.units.contains_key(&unit)
    }

    fn object_unit(&self, uuid: Uuid) -> Option<u64> {
        self.objects.get(&uuid).copied()
    }

    fn begin(&self, unit: u64) -> Result<Read<'_>, SourceError> {
        let bytes = self.units.get(&unit).ok_or(SourceError::NotFound)?;
        Ok(Read::Ready(CookedBuffer::copy_from(bytes, UNIT_ALIGN)))
    }

    fn finish(&self, _unit: u64, _read: &[u8]) -> Result<UnitBytes, SourceError> {
        Err(SourceError::NotFound)
    }
}

struct Layer {
    name: String,
    source: Box<dyn UnitSource>,
}

/// Mounted sources, searched from the most recently mounted down.
#[derive(Default)]
pub struct Vfs {
    layers: Vec<Layer>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mounts `source` above everything mounted so far.
    pub fn mount(&mut self, name: impl Into<String>, source: impl UnitSource + 'static) {
        self.layers.push(Layer {
            name: name.into(),
            source: Box::new(source),
        });
    }

    /// Removes the topmost layer called `name`; whether there was one.
    pub fn unmount(&mut self, name: &str) -> bool {
        match self.layers.iter().rposition(|l| l.name == name) {
            Some(i) => {
                self.layers.remove(i);
                true
            }
            None => false,
        }
    }

    pub fn layer_names(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().rev().map(|l| l.name.as_str())
    }

    /// The topmost source with `unit`.
    pub fn source(&self, unit: u64) -> Option<&dyn UnitSource> {
        self.layers
            .iter()
            .rev()
            .find(|l| l.source.contains(unit))
            .map(|l| l.source.as_ref())
    }

    /// The unit of object `uuid`, as the topmost source that has the object says.
    pub fn object_unit(&self, uuid: Uuid) -> Option<u64> {
        self.layers
            .iter()
            .rev()
            .find_map(|l| l.source.object_unit(uuid))
    }
}
