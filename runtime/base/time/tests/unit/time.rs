use std::time::Instant as StdInstant;

use marlin_time::{Duration, Instant, Stopwatch, sleep, sleep_until, ticks_per_second};

#[test]
fn instant_is_monotonic() {
    let mut last = Instant::now();
    for _ in 0..100_000 {
        let now = Instant::now();
        assert!(now >= last);
        assert!(now.checked_duration_since(last).is_some());
        last = now;
    }
}

#[test]
fn conversion_matches_std_clock() {
    // Bracket a sleep with both clocks; they measure the same interval to within scheduling noise.
    let (std_start, start) = (StdInstant::now(), Instant::now());
    sleep(Duration::from_millis(100));
    let ours = start.elapsed();
    let std = std_start.elapsed();
    let diff = ours.abs_diff(std);
    assert!(ours >= Duration::from_millis(99), "{ours:?}");
    assert!(
        diff < Duration::from_millis(5),
        "ours {ours:?}, std {std:?}"
    );
}

#[test]
fn add_sub_round_trip() {
    let tick = Duration::from_nanos(1_000_000_000u64.div_ceil(ticks_per_second()));
    let t = Instant::now();
    for d in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_nanos(99),
        Duration::from_micros(1234),
        Duration::from_millis(16),
        Duration::new(3, 999_999_999),
        // Large enough that a naive `ticks * 10^9` would overflow u64.
        Duration::from_secs(60 * 60 * 24 * 365),
    ] {
        let later = t + d;
        let measured = later.duration_since(t);
        // Durations round up to whole ticks, never down.
        assert!(measured >= d, "{d:?} -> {measured:?}");
        assert!(measured - d < tick, "{d:?} -> {measured:?}");
        // Both directions round the same way, so subtracting undoes adding exactly.
        assert_eq!(later - d, t);
    }
}

#[test]
fn large_tick_counts_convert_exactly() {
    // A year of ticks, past the point where ticks * 10^9 overflows u64.
    let tps = ticks_per_second();
    let year = 60 * 60 * 24 * 365;
    let a = Instant::from_ticks(0);
    let b = Instant::from_ticks(year * tps + tps / 2);
    assert_eq!(b.duration_since(a), Duration::new(year, 500_000_000));
    assert_eq!(b - a, Duration::new(year, 500_000_000));
}

#[test]
fn checked_ops_and_saturation() {
    let t = Instant::now();
    assert!(t.checked_add(Duration::MAX).is_none());
    assert!(t.checked_sub(Duration::MAX).is_none());
    assert!(
        Instant::from_ticks(u64::MAX)
            .checked_add(Duration::from_nanos(1))
            .is_none()
    );
    let later = t + Duration::from_millis(1);
    assert_eq!(t.duration_since(later), Duration::ZERO);
    assert_eq!(t.checked_duration_since(later), None);
    assert_eq!(t - later, Duration::ZERO);
    let mut u = t;
    u += Duration::from_secs(2);
    u -= Duration::from_secs(1);
    assert!(u > t);
}

#[test]
fn stopwatch_accumulates_only_while_running() {
    let mut sw = Stopwatch::new();
    assert!(!sw.is_running());
    assert_eq!(sw.elapsed(), Duration::ZERO);

    sw.start();
    sleep(Duration::from_millis(20));
    sw.stop();
    let first = sw.elapsed();
    assert!(first >= Duration::from_millis(19), "{first:?}");

    // Stopped: the reading does not move.
    sleep(Duration::from_millis(20));
    assert_eq!(sw.elapsed(), first);

    sw.start();
    sleep(Duration::from_millis(20));
    assert!(sw.elapsed() >= first + Duration::from_millis(19));

    let lap = sw.restart();
    assert!(lap >= first + Duration::from_millis(19));
    assert!(sw.is_running() && sw.elapsed() < lap);

    sw.reset();
    assert!(!sw.is_running());
    assert_eq!(sw.elapsed(), Duration::ZERO);

    let sw = Stopwatch::start_new();
    assert!(sw.is_running());
}

#[test]
fn sleep_until_is_precise() {
    // Frame-pacing shape: a run of short deadlines. Never early; late by little on average.
    let frame = Duration::from_millis(4);
    let mut deadline = Instant::now();
    let mut total_late = Duration::ZERO;
    const FRAMES: u32 = 25;
    for _ in 0..FRAMES {
        deadline += frame;
        sleep_until(deadline);
        let now = Instant::now();
        assert!(now >= deadline);
        total_late += now - deadline;
        deadline = now;
    }
    // Generous: CI machines get preempted, but spinning keeps the typical overshoot ~µs.
    assert!(
        total_late / FRAMES < Duration::from_millis(2),
        "{:?}",
        total_late / FRAMES
    );
}

#[test]
fn sleep_until_past_deadline_returns_immediately() {
    let past = Instant::now();
    sleep(Duration::from_millis(1));
    let start = StdInstant::now();
    sleep_until(past);
    assert!(start.elapsed() < Duration::from_millis(50));
}
