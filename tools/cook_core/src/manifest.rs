//! The build manifest: which cooked unit (by cache key) makes up each unit of a build, and which
//! unit holds each object. With it a build is one key: the runtime (and the cook service) find
//! everything else from there.
//!
//! ```text
//! "MMAN" version:u32 platform:(len:u32 utf8)
//! units:u32   [unit:u64 key:u128 path:(len:u32 utf8)]   sorted by unit
//! objects:u32 [uuid:u128 unit:u64]                       sorted by uuid
//! ```

use marlin_asset::Uuid;
use marlin_asset::wire::{Reader, WireError, Writer};
use marlin_hash::xxh3_128;

const MAGIC: &[u8; 4] = b"MMAN";
const VERSION: u32 = 1;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ManifestUnit {
    pub unit: u64,
    pub key: u128,
    pub path: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Manifest {
    pub platform: String,
    pub units: Vec<ManifestUnit>,
    pub objects: Vec<(Uuid, u64)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, marlin_error::Error)]
#[error("not a build manifest, or a damaged one")]
pub struct ManifestError;

impl Manifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(MAGIC).u32(VERSION).str(&self.platform);
        w.u32(self.units.len() as u32);
        for u in &self.units {
            w.u64(u.unit).u128(u.key).str(&u.path);
        }
        w.u32(self.objects.len() as u32);
        for &(uuid, unit) in &self.objects {
            w.uuid(uuid).u64(unit);
        }
        w.finish()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        Self::read(&mut Reader::new(bytes), bytes.len()).map_err(|_| ManifestError)
    }

    fn read(r: &mut Reader<'_>, len: usize) -> Result<Self, WireError> {
        if r.u32()? != u32::from_le_bytes(*MAGIC) || r.u32()? != VERSION {
            return Err(WireError::Malformed);
        }
        let platform = r.str()?;
        let count = r.u32()? as usize;
        let mut units = Vec::with_capacity(count.min(len / 28));
        for _ in 0..count {
            units.push(ManifestUnit {
                unit: r.u64()?,
                key: r.u128()?,
                path: r.str()?,
            });
        }
        let count = r.u32()? as usize;
        let mut objects = Vec::with_capacity(count.min(len / 24));
        for _ in 0..count {
            objects.push((r.uuid()?, r.u64()?));
        }
        if !r.rest().is_empty() {
            return Err(WireError::Malformed);
        }
        Ok(Self {
            platform,
            units,
            objects,
        })
    }

    /// The manifest's own cache key: a hash of its bytes.
    pub fn key(&self) -> u128 {
        xxh3_128(&self.to_bytes())
    }

    /// The cache key of `unit`.
    pub fn unit_key(&self, unit: u64) -> Option<u128> {
        self.units
            .binary_search_by_key(&unit, |u| u.unit)
            .ok()
            .map(|i| self.units[i].key)
    }

    /// The unit holding object `uuid`.
    pub fn unit_of(&self, uuid: Uuid) -> Option<u64> {
        self.objects
            .binary_search_by_key(&uuid, |(u, _)| *u)
            .ok()
            .map(|i| self.objects[i].1)
    }
}
