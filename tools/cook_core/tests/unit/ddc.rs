//! The local derived data cache.

use std::fs::{self, OpenOptions};
use std::io::Write;

use marlin_cook_core::{Ddc, LocalDdc};
use marlin_test_support::TempDir;

#[test]
fn stores_and_reopens() {
    let tmp = TempDir::new("ddc-reopen");
    let d = tmp.join("ddc");
    {
        let ddc = LocalDdc::open(&d).unwrap();
        assert!(ddc.get(1).unwrap().is_none());
        ddc.put(1, b"one").unwrap();
        ddc.put(2, &vec![7u8; 100_000]).unwrap();
        ddc.put(1, b"ignored: the key exists").unwrap();
        assert_eq!(ddc.get(1).unwrap().unwrap(), b"one");
        assert_eq!(ddc.len(), 2);
    }
    let ddc = LocalDdc::open(&d).unwrap();
    assert_eq!(ddc.len(), 2);
    assert_eq!(ddc.get(2).unwrap().unwrap().len(), 100_000);
    assert!(ddc.contains(1));
}

#[test]
fn a_torn_tail_is_dropped_on_open() {
    let tmp = TempDir::new("ddc-torn");
    let d = tmp.join("ddc");
    {
        let ddc = LocalDdc::open(&d).unwrap();
        ddc.put(1, b"whole").unwrap();
        ddc.put(2, b"also whole").unwrap();
    }
    // A crash in the middle of writing a third record.
    let pack = d.join("pack-0000.ddc");
    let mut file = OpenOptions::new().append(true).open(&pack).unwrap();
    file.write_all(b"DDC1partial-header").unwrap();
    drop(file);
    let before = fs::metadata(&pack).unwrap().len();
    let ddc = LocalDdc::open(&d).unwrap();
    assert_eq!(ddc.len(), 2);
    assert!(
        fs::metadata(&pack).unwrap().len() < before,
        "truncated to the last whole record"
    );
    ddc.put(3, b"after").unwrap();
    drop(ddc);
    let ddc = LocalDdc::open(&d).unwrap();
    assert_eq!(ddc.get(3).unwrap().unwrap(), b"after");
}

#[test]
fn damaged_data_is_a_miss() {
    let tmp = TempDir::new("ddc-damaged");
    let d = tmp.join("ddc");
    {
        let ddc = LocalDdc::open(&d).unwrap();
        ddc.put(9, b"precious data").unwrap();
    }
    let pack = d.join("pack-0000.ddc");
    let mut bytes = fs::read(&pack).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&pack, bytes).unwrap();
    let ddc = LocalDdc::open(&d).unwrap();
    assert!(ddc.get(9).unwrap().is_none(), "checksum mismatch");
    assert!(
        !ddc.contains(9),
        "and the entry is forgotten, so it gets cooked again"
    );
}

#[test]
fn concurrent_puts() {
    let tmp = TempDir::new("ddc-concurrent");
    let d = tmp.join("ddc");
    let ddc = LocalDdc::open(&d).unwrap();
    std::thread::scope(|s| {
        for t in 0..8u128 {
            let ddc = &ddc;
            s.spawn(move || {
                for i in 0..200u128 {
                    let key = t * 1000 + i;
                    ddc.put(key, &key.to_le_bytes().repeat((i % 7 + 1) as usize))
                        .unwrap();
                }
            });
        }
    });
    assert_eq!(ddc.len(), 1600);
    for t in 0..8u128 {
        for i in (0..200u128).step_by(37) {
            let key = t * 1000 + i;
            assert_eq!(
                ddc.get(key).unwrap().unwrap(),
                key.to_le_bytes().repeat((i % 7 + 1) as usize)
            );
        }
    }
    drop(ddc);
    assert_eq!(LocalDdc::open(&d).unwrap().len(), 1600);
}

/// Two caches over one directory, as the cook service and a command-line cook have: each its
/// own handles, so they coordinate only through the directory's lock file.
#[test]
fn two_openers_share_a_directory() {
    let tmp = TempDir::new("ddc-shared");
    let d = tmp.join("ddc");
    let value = |key: u128| key.to_le_bytes().repeat((key % 5 + 1) as usize);
    let (one, two) = (LocalDdc::open(&d).unwrap(), LocalDdc::open(&d).unwrap());
    std::thread::scope(|s| {
        for t in 0..4u128 {
            let ddc = if t % 2 == 0 { &one } else { &two };
            s.spawn(move || {
                for i in 0..150u128 {
                    // Half the keys are stored by both.
                    let key = if i % 2 == 0 { i } else { t * 1000 + i };
                    ddc.put(key, &value(key)).unwrap();
                }
            });
        }
    });
    // One's records are found by two once it appends: it catches up first.
    two.put(1_000_000, b"last").unwrap();
    assert_eq!(two.get(1001).unwrap().unwrap(), value(1001));
    assert_eq!(two.get(2001).unwrap().unwrap(), value(2001));
    drop((one, two));

    let ddc = LocalDdc::open(&d).unwrap();
    let mut expected = std::collections::HashSet::new();
    for t in 0..4u128 {
        for i in 0..150u128 {
            expected.insert(if i % 2 == 0 { i } else { t * 1000 + i });
        }
    }
    assert_eq!(ddc.len(), expected.len() + 1, "no record lost or clobbered");
    for &key in &expected {
        assert_eq!(ddc.get(key).unwrap().unwrap(), value(key), "key {key}");
    }
    let records: u64 = expected
        .iter()
        .map(|&k| 36 + value(k).len() as u64)
        .sum::<u64>()
        + 36
        + 4;
    assert_eq!(ddc.size(), records, "keys stored by both are stored once");
}
