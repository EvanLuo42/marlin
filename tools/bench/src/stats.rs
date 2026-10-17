use std::fmt;

use crate::Throughput;

/// One bench's result. Times are nanoseconds per iteration.
#[derive(Clone, Debug)]
pub struct Summary {
    pub name: String,
    pub samples: usize,
    /// Calls of the closure per sample.
    pub iters_per_sample: u64,
    pub median_ns: f64,
    pub p10_ns: f64,
    pub p90_ns: f64,
    pub min_ns: f64,
    pub max_ns: f64,
    pub throughput: Option<Throughput>,
    pub metrics: Vec<(String, f64)>,
}

impl Summary {
    pub(crate) fn new(
        name: String,
        mut times: Vec<f64>,
        iters_per_sample: u64,
        throughput: Option<Throughput>,
        metrics: Vec<(String, f64)>,
    ) -> Self {
        times.sort_by(f64::total_cmp);
        Self {
            name,
            samples: times.len(),
            iters_per_sample,
            median_ns: percentile(&times, 0.5),
            p10_ns: percentile(&times, 0.1),
            p90_ns: percentile(&times, 0.9),
            min_ns: times.first().copied().unwrap_or(f64::NAN),
            max_ns: times.last().copied().unwrap_or(f64::NAN),
            throughput,
            metrics,
        }
    }

    /// Throughput units per second at the median time.
    pub fn per_second(&self) -> Option<f64> {
        let per_iter = match self.throughput? {
            Throughput::Bytes(n) | Throughput::Items(n, _) => n,
        };
        Some(per_iter as f64 / (self.median_ns * 1e-9))
    }
}

/// Linear interpolation between the closest ranks of sorted `times`.
fn percentile(times: &[f64], p: f64) -> f64 {
    match times.len() {
        0 => f64::NAN,
        1 => times[0],
        n => {
            let rank = p * (n - 1) as f64;
            let (low, high) = (rank.floor() as usize, rank.ceil() as usize);
            times[low] + (times[high] - times[low]) * (rank - low as f64)
        }
    }
}

/// `1.23 ms`, with ASCII `us` so every console shows it.
pub(crate) fn time(ns: f64) -> String {
    let (value, unit) = if ns < 1e3 {
        (ns, "ns")
    } else if ns < 1e6 {
        (ns / 1e3, "us")
    } else if ns < 1e9 {
        (ns / 1e6, "ms")
    } else {
        (ns / 1e9, "s")
    };
    format!("{value:.3} {unit}")
}

/// Decimal prefixes, as the old per-crate benches printed MB/s.
fn rate(per_second: f64, unit: &str) -> String {
    let (value, prefix) = if per_second >= 1e9 {
        (per_second / 1e9, "G")
    } else if per_second >= 1e6 {
        (per_second / 1e6, "M")
    } else if per_second >= 1e3 {
        (per_second / 1e3, "k")
    } else {
        (per_second, "")
    };
    format!("{value:.2} {prefix}{unit}/s")
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<52} {:>11}  [{:>11} .. {:>11}]",
            self.name,
            time(self.median_ns),
            time(self.p10_ns),
            time(self.p90_ns)
        )?;
        if let (Some(t), Some(per_second)) = (self.throughput, self.per_second()) {
            let unit = match t {
                Throughput::Bytes(_) => "B",
                Throughput::Items(_, unit) => unit,
            };
            write!(f, "  {:>14}", rate(per_second, unit))?;
        }
        write!(f, "  {}x{}", self.samples, self.iters_per_sample)?;
        for (name, value) in &self.metrics {
            if value.fract() == 0.0 && value.abs() < 1e15 {
                write!(f, "  {name}={value}")?;
            } else {
                write!(f, "  {name}={value:.3}")?;
            }
        }
        Ok(())
    }
}
