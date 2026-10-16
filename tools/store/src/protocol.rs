//! The store protocol: message kinds and their payloads, over [`marlin_asset::wire`] frames.
//!
//! | request    | payload                          | responses                         |
//! |------------|----------------------------------|-----------------------------------|
//! | `GET`      | key:u128                         | `OK` data, `MISSING`              |
//! | `HAS`      | key:u128                         | `OK` (present), `MISSING`         |
//! | `PUT`      | key:u128 data…                   | `OK`                              |
//! | `BATCH`    | n:u32, n × (kind:u32 bytes)      | `OK` n × (kind:u32 bytes)         |
//! | `REF_GET`  | name                             | `OK` key:u128, `MISSING`          |
//! | `REF_SET`  | name key:u128 [expected:u128]    | `OK`, `CONFLICT` [current:u128]   |
//! | `STATS`    |                                  | `OK` entries:u64 bytes:u64 refs:u64 |
//!
//! Any request may also get `ERROR` message. Keys name derived data by content (cook keys):
//! an entry never changes once stored. Refs are the only mutable state: named pointers to keys,
//! such as the latest build manifest of a branch; `REF_SET` with an expected key is a
//! compare-and-swap.

use marlin_asset::wire::{Reader, WireError, Writer};

pub const GET: u16 = 1;
pub const HAS: u16 = 2;
pub const PUT: u16 = 3;
pub const BATCH: u16 = 4;
pub const REF_GET: u16 = 5;
pub const REF_SET: u16 = 6;
pub const STATS: u16 = 7;

pub const OK: u16 = 0x100;
pub const MISSING: u16 = 0x101;
pub const ERROR: u16 = 0x102;
pub const CONFLICT: u16 = 0x103;

/// Default port.
pub const PORT: u16 = 47_100;

pub fn key(key: u128) -> Vec<u8> {
    let mut w = Writer::new();
    w.u128(key);
    w.finish()
}

pub fn put(key: u128, data: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u128(key).raw(data);
    w.finish()
}

pub fn name(name: &str) -> Vec<u8> {
    let mut w = Writer::new();
    w.str(name);
    w.finish()
}

pub fn ref_set(name: &str, key: u128, expected: Option<u128>) -> Vec<u8> {
    let mut w = Writer::new();
    w.str(name).u128(key);
    if let Some(e) = expected {
        w.u128(e);
    }
    w.finish()
}

/// `(kind, payload)` pairs, as `BATCH` requests and responses carry them.
pub fn batch(items: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut w = Writer::new();
    w.u32(items.len() as u32);
    for (kind, payload) in items {
        w.u32(u32::from(*kind)).bytes(payload);
    }
    w.finish()
}

pub fn read_batch(payload: &[u8]) -> Result<Vec<(u16, Vec<u8>)>, WireError> {
    let mut r = Reader::new(payload);
    let n = r.u32()? as usize;
    // Each item takes at least 8 bytes: do not trust `n` for the allocation.
    let mut items = Vec::with_capacity(n.min(r.remaining() / 8));
    for _ in 0..n {
        let kind = u16::try_from(r.u32()?).map_err(|_| WireError::Malformed)?;
        items.push((kind, r.bytes()?.to_vec()));
    }
    r.end()?;
    Ok(items)
}
