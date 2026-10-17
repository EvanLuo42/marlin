use core::fmt::Write;
use core::time::Duration;

use marlin_pal::{debug, thread, time};

#[test]
fn output_buffer_handles_short_and_oversized_writes() {
    let mut out = debug::OutputBuffer::new();
    writeln!(out, "marlin-pal debug test: {} ünïcödé", 42).unwrap();
    // Larger than the buffer: written straight through.
    let long = "x".repeat(2000);
    writeln!(out, "{long}").unwrap();
    // Flushed on drop.
}

#[test]
fn debugger_query_does_not_crash() {
    let _ = debug::is_debugger_present();
}

#[test]
fn ticks_are_monotonic() {
    assert!(time::ticks_per_second() > 0);
    let mut last = time::ticks();
    for _ in 0..10_000 {
        let now = time::ticks();
        assert!(now >= last);
        last = now;
    }
}

#[test]
fn sleep_waits_at_least_the_duration() {
    let duration = Duration::from_millis(20);
    let start = time::ticks();
    thread::sleep(duration);
    let elapsed = time::ticks() - start;
    let elapsed = Duration::from_secs_f64(elapsed as f64 / time::ticks_per_second() as f64);
    assert!(elapsed >= duration, "slept {elapsed:?}");
}

#[test]
fn terminal_colors_query_does_not_fail() {
    // Under the test harness stderr is usually a pipe; only check that the call is harmless and
    // stable.
    let first = marlin_pal::debug::enable_terminal_colors();
    assert_eq!(marlin_pal::debug::enable_terminal_colors(), first);
}
