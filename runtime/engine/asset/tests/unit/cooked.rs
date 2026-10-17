//! The cooked unit reader on hand-built and damaged units.

use marlin_asset::Uuid;
use marlin_asset::cooked::{
    BulkEntry, BulkKind, Encoding, ImportEntry, ObjectEntry, UnitError, UnitHeader, UnitView,
    unit_id, write,
};
use marlin_math::Rng;

/// Two objects (the second with a bulk blob), one import.
fn unit() -> Vec<u8> {
    let objects = [
        ObjectEntry {
            uuid: Uuid::from_u128(1),
            type_id: 11,
            encoding: Encoding::Serial,
            payload_offset: 0,
            payload_size: 3,
            bulk_first: 0,
            bulk_count: 0,
        },
        ObjectEntry {
            uuid: Uuid::from_u128(2),
            type_id: 22,
            encoding: Encoding::Cooked,
            payload_offset: 16,
            payload_size: 4,
            bulk_first: 0,
            bulk_count: 1,
        },
    ];
    let tables = UnitHeader::SIZE + 2 * ObjectEntry::SIZE + ImportEntry::SIZE + BulkEntry::SIZE;
    let payload_offset = (tables as u64).next_multiple_of(16);
    let bulk_offset = (payload_offset + 20).next_multiple_of(4096);
    let mut out = Vec::new();
    write::header(
        &mut out,
        &UnitHeader {
            unit: unit_id("a/b"),
            platform: 7,
            object_count: 2,
            import_count: 1,
            bulk_count: 1,
            payload_offset,
            payload_size: 20,
            bulk_offset,
            bulk_size: 5,
        },
    );
    for o in &objects {
        write::object(&mut out, o);
    }
    write::import(
        &mut out,
        &ImportEntry {
            uuid: Uuid::from_u128(9),
            unit: 99,
        },
    );
    write::bulk(
        &mut out,
        &BulkEntry {
            offset: 0,
            size: 5,
            kind: BulkKind::Vertices,
        },
    );
    out.resize(payload_offset as usize, 0);
    out.extend_from_slice(b"abc");
    out.resize(payload_offset as usize + 16, 0);
    out.extend_from_slice(b"defg");
    out.resize(bulk_offset as usize, 0);
    out.extend_from_slice(b"BULK!");
    out
}

#[test]
fn reads_a_unit() {
    let bytes = unit();
    let view = UnitView::new(&bytes).unwrap();
    assert_eq!(view.header().unit, unit_id("a/b"));
    assert_eq!(view.object_count(), 2);
    let (i, second) = view.find(Uuid::from_u128(2)).unwrap();
    assert_eq!(i, 1);
    assert_eq!(second.encoding, Encoding::Cooked);
    assert_eq!(view.payload(&second), b"defg");
    let first = view.object(0).unwrap();
    assert_eq!(view.payload(&first), b"abc");
    assert!(view.find(Uuid::from_u128(3)).is_none());
    assert_eq!(view.import(0).unwrap().unit, 99);
    let blobs: Vec<BulkEntry> = view.object_bulk(&second).collect();
    assert_eq!(blobs.len(), 1);
    assert_eq!(view.bulk_bytes(&blobs[0]), b"BULK!");
    assert_eq!(view.object_bulk(&first).count(), 0);
}

#[test]
fn damage_is_an_error_never_a_panic() {
    let bytes = unit();
    assert_eq!(
        UnitView::new(&bytes[..10]).unwrap_err(),
        UnitError::Truncated
    );
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert_eq!(UnitView::new(&bad).unwrap_err(), UnitError::BadMagic);
    let mut bad = bytes.clone();
    bad[4] = 9;
    assert_eq!(UnitView::new(&bad).unwrap_err(), UnitError::Version(9));

    // Truncations and flipped bytes everywhere: errors or valid views, never out-of-bounds.
    for len in 0..bytes.len() {
        let _ = UnitView::new(&bytes[..len]).map(|v| {
            for i in 0..v.object_count() {
                let e = v.object(i).unwrap();
                v.payload(&e);
                v.object_bulk(&e).for_each(|b| {
                    v.bulk_bytes(&b);
                });
            }
        });
    }
    let mut rng = Rng::new(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        let state = rng.next_u64();
        let mut bad = bytes.clone();
        let at = (state % 512) as usize;
        bad[at] ^= (state >> 32) as u8 | 1;
        if let Ok(v) = UnitView::new(&bad) {
            for i in 0..v.object_count() {
                let e = v.object(i).unwrap();
                v.payload(&e);
                v.object_bulk(&e).for_each(|b| {
                    v.bulk_bytes(&b);
                });
            }
        }
    }
}
