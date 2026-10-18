//! What shaders print in Development builds (`Debug.Print`, `Debug.Assert`; 0004 §12.5): on
//! Vulkan through the validation layer's debug printf, logged as `shader: ...`.

use marlin_gpu_test_support as support;

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use marlin_diag::{DebugOutput, Record, Sink, register_sink};
use marlin_rhi::GraphicsApi;
use support::{compute_pipeline, devices, run, skip};

/// Every logged line that mentions `marlin-`, kept for the test to find.
struct Lines(Mutex<Vec<String>>);

impl Sink for Lines {
    fn log(&self, record: &Record<'_>) {
        let line = record.args().to_string();
        if line.contains("marlin-")
            && let Ok(mut lines) = self.0.lock()
        {
            lines.push(line);
        }
    }
}

fn lines() -> &'static Lines {
    static LINES: OnceLock<&'static Lines> = OnceLock::new();
    LINES.get_or_init(|| {
        let lines: &'static Lines = Box::leak(Box::new(Lines(Mutex::new(Vec::new()))));
        // Registering a sink replaces the default output: keep it for the other tests.
        register_sink(&DebugOutput).unwrap();
        register_sink(lines).unwrap();
        lines
    })
}

/// Waits (a little) for a logged line containing `want`; the layer reports after the GPU ran.
fn logged(want: &str) -> Option<String> {
    let start = Instant::now();
    loop {
        if let Some(l) = lines().0.lock().unwrap().iter().find(|l| l.contains(want)) {
            return Some(l.clone());
        }
        if start.elapsed() > Duration::from_secs(5) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn shaders_print_and_report_failed_assertions() {
    lines();
    for device in devices() {
        if marlin_rhi::GRAPHICS_API != GraphicsApi::Vulkan {
            skip!("shader output goes through Vulkan's debug printf");
        }
        let pipeline = compute_pipeline(device, "Prints");
        run(device, |list| {
            list.set_compute_pipeline(&pipeline);
            list.push_constants(0, &[5, (-3i32) as u32, 2.5f32.to_bits()]);
            list.dispatch(1, 1, 1);
        });
        let print = logged("marlin-print").expect("the shader's print was logged");
        assert!(print.starts_with("shader: marlin-print"), "{print}");
        assert!(
            print.contains("marlin-print 5 -3 2.50") && print.ends_with(" 1 100%"),
            "{print}"
        );
        let failed = logged("marlin-assert").expect("the failed assertion was logged");
        assert!(
            failed == "shader: assertion failed: id.x != 1: marlin-assert lane 1",
            "{failed}"
        );
        assert_eq!(device.validation_errors(), 0);
    }
}
