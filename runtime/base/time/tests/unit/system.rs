//! Wall-clock time and calendar conversion.

use marlin_time::{DateTime, Duration, SystemTime};

#[test]
fn now_matches_std() {
    let std = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let ours = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    assert!(ours.abs_diff(std) < Duration::from_secs(1));
}

#[test]
fn calendar_round_trips() {
    let cases = [
        (0, (1970, 1, 1, 0, 0, 0)),
        (951_782_400, (2000, 2, 29, 0, 0, 0)),
        (1_791_589_988, (2026, 10, 9, 23, 53, 8)),
        (-1, (1969, 12, 31, 23, 59, 59)),
        (4_107_542_399, (2100, 2, 28, 23, 59, 59)),
        (253_402_300_799, (9999, 12, 31, 23, 59, 59)),
    ];
    for (secs, (year, month, day, hour, minute, second)) in cases {
        let t = SystemTime::from_unix(secs, 123_000_000);
        let d = t.to_utc();
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute, d.second),
            (year, month, day, hour, minute, second),
            "{secs}"
        );
        assert_eq!(d.to_system_time(), Some(t), "{secs}");
    }
    let d = SystemTime::from_unix(1_791_589_988, 123_456_789).to_utc();
    assert_eq!(d.to_string(), "2026-10-09T23:53:08.123Z");
    assert_eq!(d.compact().to_string(), "20261009-235308");
    // 2026-10-09 is a Friday.
    assert_eq!(d.weekday(), 4);
}

#[test]
fn offsets_and_invalid_dates() {
    let t = SystemTime::from_unix(1_791_589_988, 0);
    let east = DateTime {
        hour: 7,
        day: 10,
        offset: 8 * 3600,
        ..t.to_utc()
    };
    assert_eq!(east.to_system_time(), Some(t));
    assert_eq!(east.to_string(), "2026-10-10T07:53:08.000+08:00");
    let local = t.to_local();
    assert_eq!(local.to_system_time(), Some(t));

    let bad = DateTime {
        month: 2,
        day: 29,
        year: 2026,
        ..t.to_utc()
    };
    assert_eq!(bad.to_system_time(), None);
}

#[test]
fn arithmetic() {
    let t = SystemTime::from_unix(10, 900_000_000);
    let later = t.checked_add(Duration::from_millis(250)).unwrap();
    assert_eq!(
        (later.unix_seconds(), later.subsec_nanos()),
        (11, 150_000_000)
    );
    assert_eq!(later.duration_since(t), Some(Duration::from_millis(250)));
    assert_eq!(t.duration_since(later), None);
    assert_eq!(later.checked_sub(Duration::from_millis(250)), Some(t));
    assert!(marlin_time::cycles() > 0);
}
