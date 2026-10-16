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

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], ManifestError> {
        let end = self.at.checked_add(n).ok_or(ManifestError)?;
        let s = self.b.get(self.at..end).ok_or(ManifestError)?;
        self.at = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, ManifestError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| ManifestError)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, ManifestError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| ManifestError)?,
        ))
    }

    fn u128(&mut self) -> Result<u128, ManifestError> {
        Ok(u128::from_le_bytes(
            self.take(16)?.try_into().map_err(|_| ManifestError)?,
        ))
    }

    fn string(&mut self) -> Result<String, ManifestError> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| ManifestError)
    }
}

impl Manifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        let string = |out: &mut Vec<u8>, s: &str| {
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        };
        string(&mut out, &self.platform);
        out.extend_from_slice(&(self.units.len() as u32).to_le_bytes());
        for u in &self.units {
            out.extend_from_slice(&u.unit.to_le_bytes());
            out.extend_from_slice(&u.key.to_le_bytes());
            string(&mut out, &u.path);
        }
        out.extend_from_slice(&(self.objects.len() as u32).to_le_bytes());
        for (uuid, unit) in &self.objects {
            out.extend_from_slice(&uuid.as_u128().to_le_bytes());
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        let mut r = Reader { b: bytes, at: 0 };
        if r.take(4)? != MAGIC || r.u32()? != VERSION {
            return Err(ManifestError);
        }
        let platform = r.string()?;
        let count = r.u32()? as usize;
        let mut units = Vec::with_capacity(count.min(bytes.len() / 28));
        for _ in 0..count {
            units.push(ManifestUnit {
                unit: r.u64()?,
                key: r.u128()?,
                path: r.string()?,
            });
        }
        let count = r.u32()? as usize;
        let mut objects = Vec::with_capacity(count.min(bytes.len() / 24));
        for _ in 0..count {
            objects.push((Uuid::from_u128(r.u128()?), r.u64()?));
        }
        if r.at != bytes.len() {
            return Err(ManifestError);
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
