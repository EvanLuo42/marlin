//! The workspace's benchmark harness (docs/design/0014-testing.md §7.2), a dev-dependency for
//! `benches/<name>.rs` files declared with `[[bench]] harness = false`.
//!
//! ```ignore
//! use marlin_bench::Bench;
//!
//! fn decode() {
//!     let packed = pack(&data);
//!     Bench::new("lz4/decode")
//!         .throughput_bytes(data.len() as u64)
//!         .run(|| unpack(&packed));
//! }
//!
//! marlin_bench::main!(decode);
//! ```
//!
//! Each [`Bench`] warms up, picks how many calls make one sample, takes repeated samples and
//! prints the median, p10/p90 and throughput. [`main!`] collects every result of the file into
//! `target/bench/<package>/<bench>.json`, which `cargo xtask test --tier bench` compares against a
//! saved baseline.
//!
//! Command line (after `cargo bench -p <crate> --bench <name> --`): words are name filters (a
//! bench runs when its name contains any of them), `--quick` takes fewer, shorter samples, and
//! `--list` prints the names without running anything. Without `--bench`, which `cargo bench`
//! always passes, the file runs as a smoke test under `cargo test --benches`: every closure once,
//! no timing, no JSON.
//!
//! Timing uses [`marlin_time::Instant`], the engine's own monotonic clock (QPC on Windows, 100 ns
//! ticks). That keeps the numbers on the same clock as the profiler. marlin-time is L0 and
//! no_std, and this crate is only ever a dev-dependency, so no layering rule is involved; the one
//! limit is that marlin-pal and marlin-time cannot bench themselves with it without a dev-dependency
//! cycle.

mod json;
mod stats;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use core::hint::black_box;
pub use marlin_time::Duration;
use marlin_time::Instant;

pub use stats::Summary;

/// The amount of work one iteration does, for the throughput column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Throughput {
    Bytes(u64),
    /// A count of `unit` ("pix", "req", "entity").
    Items(u64, &'static str),
}

/// One benchmark. Configure with the builder methods, then call one of the `run` methods.
#[must_use = "a Bench does nothing until one of its run methods is called"]
pub struct Bench {
    name: String,
    throughput: Option<Throughput>,
    warmup: Duration,
    samples: usize,
    sample_time: Duration,
    max_time: Duration,
    metrics: Vec<(String, f64)>,
}

/// Fewest samples a normal run takes, whatever `max_time` says.
const MIN_SAMPLES: usize = 3;

impl Bench {
    /// Names are unique within a bench file; `/` groups related cases (`lz4/text/9/compress`).
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            throughput: None,
            warmup: Duration::from_millis(200),
            samples: 20,
            sample_time: Duration::from_millis(20),
            max_time: Duration::from_secs(10),
            metrics: Vec::new(),
        }
    }

    /// Each iteration processes `n` bytes.
    pub fn throughput_bytes(mut self, n: u64) -> Self {
        self.throughput = Some(Throughput::Bytes(n));
        self
    }

    /// Each iteration processes `n` items of `unit`.
    pub fn throughput_items(mut self, n: u64, unit: &'static str) -> Self {
        self.throughput = Some(Throughput::Items(n, unit));
        self
    }

    /// How long to run before sampling (default 200 ms). [`run`](Self::run) always makes at least
    /// one call, to calibrate; [`run_custom`](Self::run_custom) skips warm-up at zero.
    pub fn warmup(mut self, warmup: Duration) -> Self {
        self.warmup = warmup;
        self
    }

    /// How many samples to take (default 20).
    pub fn samples(mut self, samples: usize) -> Self {
        self.samples = samples.max(1);
        self
    }

    /// The least time one sample of [`run`](Self::run) covers (default 20 ms); fast closures are
    /// called repeatedly within a sample.
    pub fn sample_time(mut self, sample_time: Duration) -> Self {
        self.sample_time = sample_time;
        self
    }

    /// Stops sampling once this much time has passed and at least 3 samples are in (default
    /// 10 s), so slow cases stay bounded.
    pub fn max_time(mut self, max_time: Duration) -> Self {
        self.max_time = max_time;
        self
    }

    /// A figure to report and store next to the timing: a compression ratio, a PSNR.
    pub fn metric(mut self, name: impl Into<String>, value: f64) -> Self {
        self.metrics.push((name.into(), value));
        self
    }

    /// Whether a `run` method would run, so expensive preparation can be skipped: false when the
    /// name filter excludes this bench, and under `--list`, where it prints the name instead.
    pub fn selected(&self) -> bool {
        match SESSION.lock().unwrap().as_ref() {
            Some(s) if !s.matches(&self.name) => false,
            Some(s) if s.list => {
                println!("{}", self.name);
                false
            }
            _ => true,
        }
    }

    /// Times `f`. Its result goes through [`black_box`], so the work cannot be optimized away.
    /// Returns `None` when the name filter excludes this bench.
    pub fn run<R>(self, mut f: impl FnMut() -> R) -> Option<Summary> {
        let mode = self.begin()?;
        if mode.smoke {
            black_box(f());
            return None;
        }
        let plan = mode.plan(&self);

        // Warm up and calibrate: the mean call time decides the calls per sample.
        let start = Instant::now();
        let mut calls = 0u64;
        loop {
            black_box(f());
            calls += 1;
            if start.elapsed() >= plan.warmup {
                break;
            }
        }
        let per_call = start.elapsed().as_secs_f64() / calls as f64;
        let iters = (plan.sample_time.as_secs_f64() / per_call.max(1e-9)).ceil() as u64;
        let iters = iters.max(1);

        let times = plan.collect(|| {
            let start = Instant::now();
            for _ in 0..iters {
                black_box(f());
            }
            nanos(start.elapsed()) / iters as f64
        });
        Some(self.finish(times, iters))
    }

    /// Times `f` alone on a fresh value from `setup` per call: one call per sample, with `setup`
    /// and dropping the value outside the timing.
    pub fn run_with_setup<S, R>(
        self,
        mut setup: impl FnMut() -> S,
        mut f: impl FnMut(S) -> R,
    ) -> Option<Summary> {
        let mode = self.begin()?;
        let mut call = || {
            let input = setup();
            let start = Instant::now();
            let out = black_box(f(black_box(input)));
            let time = start.elapsed();
            drop(out);
            nanos(time)
        };
        if mode.smoke {
            call();
            return None;
        }
        let plan = mode.plan(&self);

        let start = Instant::now();
        loop {
            call();
            if start.elapsed() >= plan.warmup {
                break;
            }
        }
        let times = plan.collect(call);
        Some(self.finish(times, 1))
    }

    /// For work that times itself (a fixed-length network run, a multi-gigabyte transfer): each
    /// call of `f` is one sample and returns how long its measured part took and how many
    /// iterations, in units of the throughput, it covered.
    pub fn run_custom(self, mut f: impl FnMut() -> (Duration, u64)) -> Option<Summary> {
        let mode = self.begin()?;
        if mode.smoke {
            f();
            return None;
        }
        let plan = mode.plan(&self);

        let start = Instant::now();
        while start.elapsed() < plan.warmup {
            f();
        }
        let times = plan.collect(|| {
            let (time, iters) = f();
            nanos(time) / iters.max(1) as f64
        });
        Some(self.finish(times, 1))
    }

    /// Whether to run at all, and how. Prints the name in `--list` mode.
    fn begin(&self) -> Option<Mode> {
        let session = SESSION.lock().unwrap();
        let mode = match session.as_ref() {
            Some(s) => {
                if !s.matches(&self.name) {
                    return None;
                }
                if s.list {
                    println!("{}", self.name);
                    return None;
                }
                Mode {
                    smoke: s.smoke,
                    quick: s.quick,
                }
            }
            // Called outside `main!`: just time it.
            None => Mode {
                smoke: false,
                quick: false,
            },
        };
        if mode.smoke {
            println!("smoke {}", self.name);
        }
        Some(mode)
    }

    fn finish(self, times: Vec<f64>, iters: u64) -> Summary {
        let summary = Summary::new(self.name, times, iters, self.throughput, self.metrics);
        println!("{summary}");
        if let Some(s) = SESSION.lock().unwrap().as_mut() {
            s.results.push(summary.clone());
        }
        summary
    }
}

fn nanos(d: Duration) -> f64 {
    d.as_nanos() as f64
}

#[derive(Clone, Copy)]
struct Mode {
    /// Not under `cargo bench`: call once, time nothing.
    smoke: bool,
    quick: bool,
}

/// A bench's settings after `--quick`.
struct Plan {
    warmup: Duration,
    samples: usize,
    sample_time: Duration,
    max_time: Duration,
    min_samples: usize,
}

impl Mode {
    fn plan(self, bench: &Bench) -> Plan {
        if self.quick {
            Plan {
                warmup: bench.warmup.min(Duration::from_millis(50)),
                samples: bench.samples.min(5),
                sample_time: bench.sample_time.min(Duration::from_millis(5)),
                max_time: bench.max_time.min(Duration::from_secs(1)),
                min_samples: 1,
            }
        } else {
            Plan {
                warmup: bench.warmup,
                samples: bench.samples,
                sample_time: bench.sample_time,
                max_time: bench.max_time,
                min_samples: MIN_SAMPLES,
            }
        }
    }
}

impl Plan {
    /// Takes up to `samples` samples, stopping early once `max_time` has passed and at least
    /// `min_samples` are in.
    fn collect(&self, mut sample: impl FnMut() -> f64) -> Vec<f64> {
        let start = Instant::now();
        let mut times = Vec::with_capacity(self.samples);
        while times.len() < self.samples
            && (times.len() < self.min_samples || start.elapsed() < self.max_time)
        {
            times.push(sample());
        }
        times
    }
}

/// The bench file being run by [`main!`].
struct Session {
    package: &'static str,
    bench: &'static str,
    manifest_dir: PathBuf,
    filters: Vec<String>,
    smoke: bool,
    quick: bool,
    list: bool,
    results: Vec<Summary>,
}

impl Session {
    fn matches(&self, name: &str) -> bool {
        self.filters.is_empty() || self.filters.iter().any(|f| name.contains(f.as_str()))
    }
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Generates `fn main` for a `harness = false` bench file: parses the command line, calls each
/// function (each holding one or more [`Bench`]es) in order, and writes the JSON results.
#[macro_export]
macro_rules! main {
    ($($group:path),+ $(,)?) => {
        fn main() {
            $crate::__start(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_CRATE_NAME"),
                env!("CARGO_MANIFEST_DIR"),
            );
            $($group();)+
            $crate::__finish();
        }
    };
}

#[doc(hidden)]
pub fn __start(package: &'static str, bench: &'static str, manifest_dir: &'static str) {
    let mut filters = Vec::new();
    let (mut smoke, mut quick, mut list) = (true, false, false);
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--bench" => smoke = false,
            "--quick" => quick = true,
            "--list" => list = true,
            "--help" | "-h" => {
                println!(
                    "{package}/{bench} [--quick] [--list] [filter]...\n  \
                     filter: run benches whose name contains any filter"
                );
                std::process::exit(0);
            }
            // Flags cargo or libtest habits pass along (`--nocapture`, `--test-threads`).
            a if a.starts_with('-') => {}
            a => filters.push(a.to_owned()),
        }
    }
    // Under `cargo bench` (or xtask) this is the package's directory; the build-time path is the
    // fallback for running the executable directly.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(manifest_dir), PathBuf::from);
    if !list {
        println!(
            "== {package}/{bench}{}{}",
            if smoke {
                " (smoke test: --bench not given)"
            } else {
                ""
            },
            if quick { " (quick)" } else { "" }
        );
    }
    *SESSION.lock().unwrap() = Some(Session {
        package,
        bench,
        manifest_dir,
        filters,
        smoke,
        quick,
        list,
        results: Vec::new(),
    });
}

#[doc(hidden)]
pub fn __finish() {
    let Some(session) = SESSION.lock().unwrap().take() else {
        return;
    };
    if session.smoke || session.list {
        return;
    }
    if session.results.is_empty() {
        println!("no bench matched {:?}", session.filters);
        return;
    }
    let root = workspace_root(&session.manifest_dir);
    let dir = target_dir(&root).join("bench").join(session.package);
    let path = dir.join(format!("{}.json", session.bench));
    let text = json::results(
        session.package,
        session.bench,
        session.quick,
        &session.results,
    );
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, text)) {
        eprintln!("could not write {}: {e}", path.display());
        std::process::exit(1);
    }
    let shown = path.strip_prefix(&root).unwrap_or(&path);
    println!("results: {}", shown.display());
}

/// The nearest ancestor of `manifest_dir` whose Cargo.toml has a `[workspace]` table.
fn workspace_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|toml| toml.lines().any(|l| l.trim() == "[workspace]"))
        })
        .unwrap_or(manifest_dir)
        .to_path_buf()
}

/// `CARGO_TARGET_DIR` (relative to the workspace root) or `<root>/target`; xtask uses the same
/// rule to find the results.
fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => root.join(dir),
        None => root.join("target"),
    }
}
