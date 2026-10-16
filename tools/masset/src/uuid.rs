//! New object identities.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use marlin_asset::Uuid;

/// The last id handed out: (Unix milliseconds, random bits).
static LAST: Mutex<(u64, u128)> = Mutex::new((0, 0));

/// 128 bits from the OS-seeded hasher keys (std seeds `RandomState` from the system RNG once per
/// thread and steps it per instance).
fn random_bits() -> u128 {
    let a = RandomState::new().build_hasher().finish();
    let mut h = RandomState::new().build_hasher();
    h.write_u64(a);
    (u128::from(a) << 64) | u128::from(h.finish())
}

/// A new version 7 UUID. Ids from one process are strictly increasing: within a millisecond (or
/// if the clock steps back) the random part counts up from the previous id instead.
pub fn new_uuid() -> Uuid {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let (millis, random) = if now > last.0 {
        // 73 of the 74 random bits: the spare top bit is headroom for counting up within a
        // millisecond.
        (now, random_bits() & ((1 << 73) - 1))
    } else {
        (last.0, (last.1 + 1) & ((1 << 74) - 1))
    };
    *last = (millis, random);
    Uuid::v7(millis, random)
}
