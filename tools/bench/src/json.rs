//! The results file. Written by hand: the format is small and this crate stays dependency-free.
//!
//! ```json
//! { "package": "marlin-compress", "bench": "codecs", "quick": false, "results": [
//!   { "name": "lz4/text/1/compress", "samples": 20, "iters_per_sample": 31,
//!     "median_ns": 640123.5, "p10_ns": ..., "p90_ns": ..., "min_ns": ..., "max_ns": ...,
//!     "throughput": { "unit": "B", "per_iter": 1048576, "per_second": 1638069452.1 },
//!     "metrics": { "ratio": 2.31 } } ] }
//! ```

use std::fmt::Write;

use crate::{Summary, Throughput};

pub fn results(package: &str, bench: &str, quick: bool, results: &[Summary]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    let _ = writeln!(out, "  \"package\": {},", string(package));
    let _ = writeln!(out, "  \"bench\": {},", string(bench));
    let _ = writeln!(out, "  \"quick\": {quick},");
    out.push_str("  \"results\": [");
    for (i, r) in results.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        let _ = write!(
            out,
            "    {{\"name\": {}, \"samples\": {}, \"iters_per_sample\": {}, \"median_ns\": {}, \
             \"p10_ns\": {}, \"p90_ns\": {}, \"min_ns\": {}, \"max_ns\": {}",
            string(&r.name),
            r.samples,
            r.iters_per_sample,
            number(r.median_ns),
            number(r.p10_ns),
            number(r.p90_ns),
            number(r.min_ns),
            number(r.max_ns),
        );
        if let Some(t) = r.throughput {
            let (unit, per_iter) = match t {
                Throughput::Bytes(n) => ("B", n),
                Throughput::Items(n, unit) => (unit, n),
            };
            let _ = write!(
                out,
                ", \"throughput\": {{\"unit\": {}, \"per_iter\": {per_iter}, \"per_second\": {}}}",
                string(unit),
                number(r.per_second().unwrap_or(f64::NAN)),
            );
        }
        if !r.metrics.is_empty() {
            out.push_str(", \"metrics\": {");
            for (j, (name, value)) in r.metrics.iter().enumerate() {
                let sep = if j == 0 { "" } else { ", " };
                let _ = write!(out, "{sep}{}: {}", string(name), number(*value));
            }
            out.push('}');
        }
        out.push('}');
    }
    out.push_str("\n  ]\n}\n");
    out
}

fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON has no NaN or infinity.
fn number(x: f64) -> String {
    if x.is_finite() {
        format!("{x}")
    } else {
        "null".into()
    }
}
