use marlin_time::{Duration, FixedStep, FixedSteps, FrameClock, Instant};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Feeds `deltas` to `clock` from an arbitrary origin; returns the last frame.
fn run(clock: &mut FrameClock, start: Instant, deltas: &[u64]) -> marlin_time::FrameTime {
    let mut now = start;
    let mut frame = clock.tick_at(now);
    for &d in deltas {
        now += ms(d);
        frame = clock.tick_at(now);
    }
    frame
}

fn origin() -> Instant {
    Instant::from_ticks(1 << 40)
}

#[test]
fn first_tick_is_zero() {
    let mut clock = FrameClock::new();
    let frame = clock.tick_at(origin());
    assert_eq!(frame.frame, 0);
    assert_eq!(frame.raw_delta, Duration::ZERO);
    assert_eq!(frame.scaled_delta, Duration::ZERO);
    assert_eq!(frame.game_time, Duration::ZERO);
    assert_eq!(frame.real_time, Duration::ZERO);
    assert_eq!(frame.smoothed_delta, Duration::ZERO);
    assert_eq!(frame.fps(), 0.0);
    assert_eq!(clock.frame_count(), 1);
}

#[test]
fn deltas_and_totals() {
    let mut clock = FrameClock::default();
    let t = origin();
    clock.tick_at(t);
    let frame = clock.tick_at(t + ms(16));
    assert_eq!(frame.frame, 1);
    assert_eq!(frame.raw_delta, ms(16));
    assert_eq!(frame.delta, ms(16));
    assert_eq!(frame.scaled_delta, ms(16));
    let frame = clock.tick_at(t + ms(50));
    assert_eq!(frame.raw_delta, ms(34));
    assert_eq!(frame.game_time, ms(50));
    assert_eq!(frame.real_time, ms(50));
    assert_eq!(clock.game_time(), ms(50));
    assert_eq!(clock.frame_count(), 3);
}

#[test]
fn delta_is_clamped() {
    let mut clock = FrameClock::new();
    assert_eq!(clock.max_delta(), FrameClock::DEFAULT_MAX_DELTA);
    let frame = run(&mut clock, origin(), &[16, 5_000]);
    assert_eq!(frame.raw_delta, ms(5_000));
    assert_eq!(frame.delta, ms(250));
    assert_eq!(frame.scaled_delta, ms(250));
    assert_eq!(frame.game_time, ms(266));
    // Real time is not clamped.
    assert_eq!(frame.real_time, ms(5_016));

    clock.set_max_delta(ms(100));
    let frame = clock.tick_at(origin() + ms(5_016 + 150));
    assert_eq!(frame.delta, ms(100));
    clock.set_max_delta(Duration::MAX);
    let frame = clock.tick_at(origin() + ms(5_166 + 10_000));
    assert_eq!(frame.delta, ms(10_000));
}

#[test]
fn time_going_backwards_counts_as_zero() {
    let mut clock = FrameClock::new();
    let t = origin();
    clock.tick_at(t + ms(100));
    assert_eq!(clock.tick_at(t).raw_delta, Duration::ZERO);
    // Measured from the latest instant seen, not the stale one.
    assert_eq!(clock.tick_at(t + ms(110)).raw_delta, ms(10));
}

#[test]
fn time_scale_scales_game_time_only() {
    let mut clock = FrameClock::new();
    let t = origin();
    clock.tick_at(t);
    clock.set_time_scale(0.5);
    let frame = clock.tick_at(t + ms(16));
    assert_eq!(frame.delta, ms(16));
    assert_eq!(frame.scaled_delta, ms(8));
    clock.set_time_scale(2.0);
    let frame = clock.tick_at(t + ms(32));
    assert_eq!(frame.scaled_delta, ms(32));
    assert_eq!(frame.game_time, ms(40));
    assert_eq!(frame.real_time, ms(32));

    // Scale applies after the clamp.
    let frame = clock.tick_at(t + ms(32 + 1_000));
    assert_eq!(frame.scaled_delta, ms(500));

    clock.set_time_scale(0.0);
    let frame = clock.tick_at(t + ms(1_048));
    assert_eq!(frame.scaled_delta, Duration::ZERO);
    assert_eq!(frame.game_time, ms(540));
    assert_eq!(clock.time_scale(), 0.0);
}

#[test]
fn scaled_time_does_not_drift() {
    // A third is not exact in binary; per-frame rounding is to whole nanoseconds and the sum is
    // integer, so 30 000 frames of 3 ms at 1/3 is exactly 30 000 ms, not 29 999.99... .
    let mut clock = FrameClock::new();
    let t = origin();
    clock.tick_at(t);
    clock.set_time_scale(1.0 / 3.0);
    let mut now = t;
    let mut frame = clock.tick_at(now);
    for _ in 0..30_000 {
        now += ms(3);
        frame = clock.tick_at(now);
        assert_eq!(frame.scaled_delta, ms(1));
    }
    assert_eq!(frame.game_time, ms(30_000));
}

#[test]
#[should_panic(expected = "time scale")]
fn negative_time_scale_panics() {
    FrameClock::new().set_time_scale(-1.0);
}

#[test]
#[should_panic(expected = "time scale")]
fn nan_time_scale_panics() {
    FrameClock::new().set_time_scale(f64::NAN);
}

#[test]
fn pause_stops_game_time() {
    let mut clock = FrameClock::new();
    let t = origin();
    clock.tick_at(t);
    clock.set_time_scale(0.5);
    clock.tick_at(t + ms(20));
    clock.pause();
    assert!(clock.is_paused());
    let frame = clock.tick_at(t + ms(40));
    assert_eq!(frame.delta, ms(20));
    assert_eq!(frame.scaled_delta, Duration::ZERO);
    assert_eq!(frame.game_time, ms(10));
    assert_eq!(frame.real_time, ms(40));
    assert_eq!(frame.frame, 2);
    // Pausing keeps the scale.
    assert_eq!(clock.time_scale(), 0.5);
    clock.resume();
    assert!(!clock.is_paused());
    let frame = clock.tick_at(t + ms(60));
    assert_eq!(frame.scaled_delta, ms(10));
    assert_eq!(frame.game_time, ms(20));
}

#[test]
fn smoothing_is_a_moving_average() {
    let mut clock = FrameClock::new();
    assert_eq!(clock.smoothing(), FrameClock::DEFAULT_SMOOTHING);
    clock.set_smoothing(0.5);
    let t = origin();
    // Seeded with the first nonzero delta, so it does not ramp up from zero.
    let frame = run(&mut clock, t, &[20]);
    assert_eq!(frame.smoothed_delta, ms(20));
    assert_eq!(frame.fps(), 50.0);
    // Halfway toward each new delta.
    let frame = clock.tick_at(t + ms(20 + 40));
    assert_eq!(frame.smoothed_delta, ms(30));
    let frame = clock.tick_at(t + ms(60 + 10));
    assert_eq!(frame.smoothed_delta, ms(20));

    // Smoothing follows the clamped delta, so a hitch moves it by at most `max_delta`.
    let frame = clock.tick_at(t + ms(70 + 60_000));
    assert_eq!(frame.smoothed_delta, ms(135));

    // A factor of 1 is no smoothing.
    clock.set_smoothing(1.0);
    let frame = clock.tick_at(t + ms(60_070 + 7));
    assert_eq!(frame.smoothed_delta, ms(7));
}

#[test]
fn smoothing_converges_to_a_steady_rate() {
    let mut clock = FrameClock::new();
    let mut deltas = vec![100];
    deltas.extend([10; 200]);
    let frame = run(&mut clock, origin(), &deltas);
    assert!(frame.smoothed_delta.abs_diff(ms(10)) < Duration::from_micros(1));
    assert!((frame.fps() - 100.0).abs() < 0.01, "{}", frame.fps());
}

#[test]
#[should_panic(expected = "smoothing factor")]
fn zero_smoothing_panics() {
    FrameClock::new().set_smoothing(0.0);
}

#[test]
fn tick_reads_the_clock() {
    let mut clock = FrameClock::new();
    clock.tick();
    marlin_time::sleep(ms(2));
    let frame = clock.tick();
    assert!(frame.raw_delta >= ms(1), "{:?}", frame.raw_delta);
    assert_eq!(frame.frame, 1);
}

#[test]
fn fixed_step_counts_and_alpha() {
    let mut fixed = FixedStep::new(ms(10), 8);
    assert_eq!(fixed.step(), ms(10));
    assert_eq!(fixed.max_steps_per_frame(), 8);

    let s = fixed.advance(ms(4));
    assert_eq!(
        s,
        FixedSteps {
            steps: 0,
            alpha: 0.4,
            dropped: Duration::ZERO
        }
    );
    // 4 banked + 16 = 20: two steps, nothing left.
    let s = fixed.advance(ms(16));
    assert_eq!((s.steps, s.alpha), (2, 0.0));
    assert_eq!(fixed.accumulated(), Duration::ZERO);
    let s = fixed.advance(ms(25));
    assert_eq!((s.steps, s.alpha), (2, 0.5));
    assert_eq!(fixed.accumulated(), ms(5));
    let s = fixed.advance(ms(5));
    assert_eq!((s.steps, s.alpha), (1, 0.0));
    // Paused frames bank nothing.
    let s = fixed.advance(Duration::ZERO);
    assert_eq!((s.steps, s.alpha), (0, 0.0));

    fixed.advance(ms(7));
    fixed.reset();
    assert_eq!(fixed.accumulated(), Duration::ZERO);
}

#[test]
fn fixed_step_cap_drops_whole_steps() {
    let mut fixed = FixedStep::new(ms(10), 3);
    let s = fixed.advance(ms(57));
    assert_eq!(s.steps, 3);
    assert_eq!(s.dropped, ms(20));
    // The sub-step remainder is kept, so alpha stays continuous.
    assert_eq!(fixed.accumulated(), ms(7));
    assert!((s.alpha - 0.7).abs() < 1e-6);
    let s = fixed.advance(ms(3));
    assert_eq!((s.steps, s.dropped), (1, Duration::ZERO));
    assert_eq!(fixed.accumulated(), Duration::ZERO);

    // A huge delta cannot overflow anything.
    let s = fixed.advance(Duration::MAX);
    assert_eq!(s.steps, 3);
    assert!(s.alpha < 1.0);
    assert!(fixed.accumulated() < ms(10));
}

#[test]
fn fixed_step_is_exact_over_many_frames() {
    // 60 Hz steps (16 666 667 ns, not a whole number of ms) fed 1 ms frames for 60 s: integer
    // arithmetic gives exactly the floor of total / step, with the remainder carried.
    let step = Duration::from_nanos(16_666_667);
    let mut fixed = FixedStep::new(step, 4);
    let mut total = 0u64;
    for _ in 0..60_000 {
        let s = fixed.advance(ms(1));
        assert!((0.0..1.0).contains(&s.alpha));
        assert_eq!(s.dropped, Duration::ZERO);
        total += u64::from(s.steps);
    }
    let elapsed = 60_000_000_000u128;
    assert_eq!(u128::from(total), elapsed / step.as_nanos());
    assert_eq!(fixed.accumulated().as_nanos(), elapsed % step.as_nanos());
}

#[test]
fn alpha_stays_below_one() {
    // One nanosecond short of a step: the f32 ratio would round to 1.
    let mut fixed = FixedStep::new(Duration::from_secs(1), 1);
    let s = fixed.advance(Duration::from_nanos(999_999_999));
    assert_eq!(s.steps, 0);
    assert!(s.alpha < 1.0 && s.alpha > 0.99);
}

#[test]
fn clock_drives_fixed_step() {
    let mut clock = FrameClock::new();
    let mut fixed = FixedStep::new(ms(10), 5);
    let t = origin();
    let mut now = t;
    let mut steps = 0;
    fixed.advance(clock.tick_at(now).scaled_delta);
    clock.set_time_scale(0.5);
    for _ in 0..100 {
        now += ms(16);
        steps += fixed.advance(clock.tick_at(now).scaled_delta).steps;
    }
    // 1600 ms real at half speed is 800 ms of game time: 80 steps.
    assert_eq!(steps, 80);
    assert_eq!(clock.game_time(), ms(800));
}

#[test]
#[should_panic(expected = "fixed step must be nonzero")]
fn zero_step_panics() {
    let _ = FixedStep::new(Duration::ZERO, 1);
}
