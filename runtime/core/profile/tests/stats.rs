//! Per-zone statistics over the frame history, and the GPU timing hook.
#![cfg(not(marlin_build = "shipping"))]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use marlin_profile::{
    GpuTimings, GpuZone, clear, frame_index, frame_mark, profile_scope, set_gpu_timings,
    with_history,
};

fn spin(d: Duration) {
    let start = Instant::now();
    while start.elapsed() < d {
        std::hint::spin_loop();
    }
}

fn step(ms: u64) {
    profile_scope!("step");
    spin(Duration::from_millis(ms));
}

/// Hands out the GPU zones queued by the test.
struct FakeGpu(Mutex<Vec<GpuZone>>);

impl GpuTimings for FakeGpu {
    fn collect(&self, out: &mut dyn FnMut(GpuZone)) {
        for zone in self.0.lock().unwrap().drain(..) {
            out(zone);
        }
    }

    fn queue_name(&self, queue: u32) -> &str {
        ["Graphics", "Compute"][queue as usize]
    }
}

static GPU: FakeGpu = FakeGpu(Mutex::new(Vec::new()));

#[test]
fn stats() {
    clear();

    // Frame 0: one 2 ms step. Frame 1: 2 + 6 ms. Frame 2: none. Frame 3: 4 ms.
    let frames: [&[u64]; 4] = [&[2], &[2, 6], &[], &[4]];
    for steps in frames {
        for &ms in steps {
            step(ms);
        }
        frame_mark();
    }
    with_history(|h| {
        let stats = h.zone_stats();
        assert_eq!(stats.len(), 1);
        let s = &stats[0];
        assert_eq!((s.name, s.calls, s.frames), ("step", 4, 3));
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        // Lower bounds are exact (we spin at least that long); upper bounds allow for preemption.
        assert!(ms(s.per_call.min) >= 2.0 && ms(s.per_call.min) < 4.0);
        assert!(ms(s.per_call.max) >= 6.0);
        assert!(ms(s.per_call.avg) >= 3.5);
        assert!(s.per_call.min <= s.per_call.avg && s.per_call.avg <= s.per_call.max);
        assert!(ms(s.per_frame.min) >= 2.0 && ms(s.per_frame.min) < 4.0);
        assert!(ms(s.per_frame.max) >= 8.0);
        // (2 + 8 + 4) / 3 frames in which it occurred.
        assert!(ms(s.per_frame.avg) >= 4.66);
        assert!(s.per_frame.min <= s.per_frame.avg && s.per_frame.avg <= s.per_frame.max);

        let total: Duration = h
            .frames()
            .flat_map(|f| f.zones())
            .map(|z| z.duration())
            .sum();
        // Each duration is floored to whole nanoseconds; the average is taken in ticks.
        let avg = total.as_nanos() as i128 / 4;
        assert!((avg - s.per_call.avg.as_nanos() as i128).abs() < 1000);
    });

    // GPU timings arrive late, tagged with the frame that submitted them.
    set_gpu_timings(Some(&GPU));
    let submitted = frame_index();
    frame_mark();
    let base = with_history(|h| h.last().unwrap().start_ticks());
    GPU.0.lock().unwrap().extend([
        GpuZone {
            frame: submitted,
            name: "shadows",
            queue: 0,
            start: base,
            end: base + 100,
        },
        GpuZone {
            frame: submitted,
            name: "cull",
            queue: 1,
            start: base + 10,
            end: base + 50,
        },
        // Not a frame in the history: dropped.
        GpuZone {
            frame: submitted + 1000,
            name: "unknown",
            queue: 0,
            start: 0,
            end: 1,
        },
    ]);
    frame_mark();
    with_history(|h| {
        let frame = h.frame(submitted).unwrap();
        let names: Vec<_> = frame.gpu_zones().iter().map(|z| z.name).collect();
        assert_eq!(names, ["shadows", "cull"]);
        assert!(h.last().unwrap().gpu_zones().is_empty());
        let stats = h.gpu_zone_stats();
        assert_eq!(stats.len(), 2);
        assert!(stats.iter().all(|s| s.calls == 1 && s.frames == 1));

        let mut json = Vec::new();
        h.write_chrome_trace(&mut json).unwrap();
        let trace: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let events = trace["traceEvents"].as_array().unwrap();
        let gpu: Vec<_> = events
            .iter()
            .filter(|e| e["pid"] == 2 && e["ph"] == "X")
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(gpu, ["shadows", "cull"]);
        let queues: Vec<_> = events
            .iter()
            .filter(|e| e["pid"] == 2 && e["name"] == "thread_name")
            .map(|e| e["args"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(queues, ["Graphics", "Compute"]);
    });
    set_gpu_timings(None);
}
