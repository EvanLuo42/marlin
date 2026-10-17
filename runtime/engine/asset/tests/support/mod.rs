//! Helpers shared by marlin-asset's test binaries.

use marlin_asset::Uuid;

/// The test object id `n` (see `marlin_test_support::test_uuid`).
pub fn uuid(n: u64) -> Uuid {
    Uuid::from_u128(marlin_test_support::test_uuid(n))
}
