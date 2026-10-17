//! Wall-clock time and calendar dates, for timestamps in logs, saves and file names. Measure
//! intervals with [`Instant`](crate::Instant) instead: the wall clock can jump.

use core::fmt;
use core::time::Duration;

use marlin_pal::time;

/// A point on the wall clock: seconds and nanoseconds since 1970-01-01 00:00:00 UTC (leap seconds
/// are not counted, as everywhere).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SystemTime {
    secs: i64,
    nanos: u32,
}

impl SystemTime {
    pub const UNIX_EPOCH: Self = Self { secs: 0, nanos: 0 };

    /// The current wall-clock time.
    pub fn now() -> Self {
        let (secs, nanos) = time::system_time();
        Self { secs, nanos }
    }

    /// `nanos` is reduced to whole seconds plus a remainder.
    pub const fn from_unix(secs: i64, nanos: u32) -> Self {
        Self {
            secs: secs + (nanos / 1_000_000_000) as i64,
            nanos: nanos % 1_000_000_000,
        }
    }

    /// Whole seconds since the Unix epoch (negative before it).
    pub const fn unix_seconds(self) -> i64 {
        self.secs
    }

    pub const fn subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// Time from `earlier` to `self`, or `None` if `earlier` is later.
    pub fn duration_since(self, earlier: SystemTime) -> Option<Duration> {
        let mut secs = self.secs.checked_sub(earlier.secs)?;
        let nanos = if self.nanos >= earlier.nanos {
            self.nanos - earlier.nanos
        } else {
            secs = secs.checked_sub(1)?;
            self.nanos + 1_000_000_000 - earlier.nanos
        };
        u64::try_from(secs)
            .ok()
            .map(|secs| Duration::new(secs, nanos))
    }

    pub fn checked_add(self, d: Duration) -> Option<Self> {
        let secs = self.secs.checked_add(i64::try_from(d.as_secs()).ok()?)?;
        let nanos = self.nanos + d.subsec_nanos();
        Some(Self::from_unix(secs, nanos))
    }

    pub fn checked_sub(self, d: Duration) -> Option<Self> {
        let mut secs = self.secs.checked_sub(i64::try_from(d.as_secs()).ok()?)?;
        let nanos = if self.nanos >= d.subsec_nanos() {
            self.nanos - d.subsec_nanos()
        } else {
            secs = secs.checked_sub(1)?;
            self.nanos + 1_000_000_000 - d.subsec_nanos()
        };
        Some(Self { secs, nanos })
    }

    /// The calendar date and time in UTC.
    pub fn to_utc(self) -> DateTime {
        DateTime::from_unix(self.secs, self.nanos, 0)
    }

    /// The calendar date and time in the system's time zone, with the offset in force at that
    /// instant (daylight saving included). Falls back to UTC if the OS cannot tell.
    pub fn to_local(self) -> DateTime {
        let offset = time::local_offset(self.secs).unwrap_or(0);
        DateTime::from_unix(self.secs + i64::from(offset), self.nanos, offset)
    }
}

/// A calendar date and time of day in some time zone, proleptic Gregorian.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DateTime {
    pub year: i32,
    /// 1-12.
    pub month: u8,
    /// 1-31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub nanosecond: u32,
    /// Offset from UTC in seconds east; 0 for UTC.
    pub offset: i32,
}

impl DateTime {
    /// `local_secs` is seconds since the epoch as seen in the zone (UTC plus `offset`).
    fn from_unix(local_secs: i64, nanos: u32, offset: i32) -> Self {
        let days = local_secs.div_euclid(86_400);
        let rem = local_secs.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Self {
            year: year as i32,
            month,
            day,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
            nanosecond: nanos,
            offset,
        }
    }

    /// The instant this date and time names, or `None` if a field is out of range.
    pub fn to_system_time(self) -> Option<SystemTime> {
        let valid = (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
            && self.nanosecond < 1_000_000_000;
        if !valid {
            return None;
        }
        let days = days_from_civil(i64::from(self.year), self.month, self.day);
        let local = days * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second);
        Some(SystemTime::from_unix(
            local - i64::from(self.offset),
            self.nanosecond,
        ))
    }

    /// Day of the week, 0 = Monday ... 6 = Sunday.
    pub fn weekday(self) -> u8 {
        let days = days_from_civil(i64::from(self.year), self.month, self.day);
        // 1970-01-01 was a Thursday.
        (days + 3).rem_euclid(7) as u8
    }

    /// `YYYYMMDD-HHMMSS`, for file names.
    pub fn compact(self) -> impl fmt::Display {
        struct Compact(DateTime);
        impl fmt::Display for Compact {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let d = self.0;
                write!(
                    f,
                    "{:04}{:02}{:02}-{:02}{:02}{:02}",
                    d.year, d.month, d.day, d.hour, d.minute, d.second
                )
            }
        }
        Compact(self)
    }
}

/// ISO 8601 with milliseconds: `2026-10-16T21:33:08.123Z`, or `+08:00` style for other zones.
impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            self.nanosecond / 1_000_000
        )?;
        if self.offset == 0 {
            f.write_str("Z")
        } else {
            let sign = if self.offset < 0 { '-' } else { '+' };
            let abs = self.offset.unsigned_abs();
            write!(f, "{sign}{:02}:{:02}", abs / 3600, abs / 60 % 60)
        }
    }
}

fn is_leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01.
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}
