//! `cargo xtask test`: runs the workspace's test binaries by tier (docs/design/0014-testing.md §4).
//!
//! Tests are built once with `cargo test --no-run`; xtask then runs each selected binary itself,
//! one at a time, with libtest's JSON output. That gives a stall watchdog (a hung GPU test is
//! reported by name instead of stalling the run), a count of skipped tests, and one summary.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{Result, cargo};

/// Which binaries a test target belongs to, from its name alone (0014 §5).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    Quick,
    Gpu,
    Desktop,
    Stress,
    /// Not yet in the 0014 layout: runs only with `--tier all` (and `manual`).
    Unclassified,
}

pub fn classify(target: &str) -> Class {
    match target {
        "unit" => Class::Quick,
        "gpu" | "golden" => Class::Gpu,
        "desktop" => Class::Desktop,
        "stress" => Class::Stress,
        t if t.starts_with("isolated_gpu_") => Class::Gpu,
        t if t.starts_with("isolated_desktop_") => Class::Desktop,
        t if t.starts_with("isolated_") => Class::Quick,
        _ => Class::Unclassified,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Quick,
    Gpu,
    Desktop,
    Stress,
    Manual,
    /// `benches/`, run by bench.rs rather than as test binaries.
    Bench,
    /// The fuzz targets in fuzz/, not test binaries: see `fuzz.rs`.
    Fuzz,
    All,
}

impl Tier {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "quick" => Self::Quick,
            "gpu" => Self::Gpu,
            "desktop" => Self::Desktop,
            "stress" => Self::Stress,
            "manual" => Self::Manual,
            "all" => Self::All,
            "bench" => Self::Bench,
            "fuzz" => Self::Fuzz,
            _ => {
                return Err(format!(
                    "unknown tier `{s}` (quick, gpu, desktop, stress, manual, bench, fuzz, all)"
                )
                .into());
            }
        })
    }

    pub fn includes(self, class: Class) -> bool {
        match self {
            Self::Quick => class == Class::Quick,
            Self::Gpu => class == Class::Gpu,
            Self::Desktop => class == Class::Desktop,
            Self::Stress => class == Class::Stress,
            Self::Manual | Self::All => true,
            Self::Bench | Self::Fuzz => false,
        }
    }

    pub fn runs_doctests(self) -> bool {
        matches!(self, Self::Quick | Self::All)
    }

    /// Seconds without any test finishing before the watchdog calls a binary stuck.
    fn default_stall(self) -> u64 {
        match self {
            Self::Stress | Self::Manual => 1800,
            _ => 300,
        }
    }
}

/// A built integration-test binary.
pub struct TestBinary {
    pub package: String,
    pub target: String,
    pub class: Class,
    pub exe: PathBuf,
    pub manifest_dir: PathBuf,
}

impl TestBinary {
    fn label(&self) -> String {
        format!("{}/{}", self.package, self.target)
    }
}

/// Options shared by `test` and `coverage`.
pub struct Options {
    pub tier: Tier,
    pub packages: Vec<String>,
    pub seed: Option<String>,
    pub bless: bool,
    pub stall: Option<u64>,
    /// Passed to every test binary (name filters, `--test-threads`, ...).
    pub harness_args: Vec<String>,
}

impl Options {
    /// Parses `args`; `extra` handles command-specific flags, returning whether it took one
    /// (and advancing the iterator past its value).
    pub fn parse(
        args: &[String],
        default_tier: Tier,
        mut extra: impl FnMut(&str, &mut std::slice::Iter<'_, String>) -> Result<bool>,
    ) -> Result<Self> {
        let mut options = Self {
            tier: default_tier,
            packages: Vec::new(),
            seed: None,
            bless: false,
            stall: None,
            harness_args: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match arg.as_str() {
                "--tier" => options.tier = Tier::parse(&value("--tier")?)?,
                "-p" | "--package" => options.packages.push(value("--package")?),
                "--seed" => options.seed = Some(value("--seed")?),
                "--bless" => options.bless = true,
                "--stall" => options.stall = Some(value("--stall")?.parse()?),
                "--" => {
                    options.harness_args.extend(it.by_ref().cloned());
                }
                other => {
                    if !extra(other, &mut it)? {
                        return Err(format!("unknown option `{other}`").into());
                    }
                }
            }
        }
        Ok(options)
    }
}

pub fn run(root: &Path, args: &[String]) -> Result {
    let mut bench = crate::bench::Flags::default();
    let mut fuzz = crate::fuzz::Args::default();
    let options = Options::parse(args, Tier::Quick, |arg, it| {
        Ok(bench.parse(arg, it)? || fuzz.take(arg, it)?)
    })?;
    if options.tier == Tier::Fuzz {
        if bench.given() {
            return Err(
                "--save-baseline, --baseline and --threshold are options of --tier bench".into(),
            );
        }
        return crate::fuzz::run(root, &options, &fuzz);
    }
    fuzz.reject()?;
    if options.tier == Tier::Bench || bench.given() {
        return crate::bench::run(root, &options, &bench);
    }
    let binaries = build(root, &options.packages, &[], None)?;
    let report = execute(root, &binaries, &options, &[])?;
    let doc_ok = if options.tier.runs_doctests() {
        println!("\n== doctests");
        let mut command = cargo();
        command.current_dir(root).args(["test", "--doc"]);
        package_args(&mut command, &options.packages);
        command.status()?.success()
    } else {
        true
    };
    report.print(&binaries, &options);
    if report.failed() || !doc_ok {
        return Err("tests failed".into());
    }
    Ok(())
}

fn package_args(command: &mut Command, packages: &[String]) {
    if packages.is_empty() {
        command.arg("--workspace");
    }
    for p in packages {
        command.args(["--package", p]);
    }
}

/// Builds every integration-test binary of `packages` (all when empty) and lists them.
pub fn build(
    root: &Path,
    packages: &[String],
    env: &[(&str, String)],
    target_dir: Option<&Path>,
) -> Result<Vec<TestBinary>> {
    let names = package_names(root)?;
    let mut command = cargo();
    command.current_dir(root).args([
        "test",
        "--no-run",
        "--tests",
        "--message-format=json-render-diagnostics",
    ]);
    package_args(&mut command, packages);
    if let Some(dir) = target_dir {
        command.arg("--target-dir").arg(dir);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command.env_remove("RUSTFLAGS").stdout(Stdio::piped());
    let output = command.spawn()?.wait_with_output()?;
    if !output.status.success() {
        return Err("building the tests failed".into());
    }
    let mut binaries = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact"
            || !message["profile"]["test"].as_bool().unwrap_or(false)
        {
            continue;
        }
        let kinds = &message["target"]["kind"];
        if kinds
            .as_array()
            .is_none_or(|k| k.iter().all(|k| k != "test"))
        {
            continue;
        }
        let (Some(exe), Some(manifest)) = (
            message["executable"].as_str(),
            message["manifest_path"].as_str(),
        ) else {
            continue;
        };
        let manifest = PathBuf::from(manifest);
        let target = message["target"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        binaries.push(TestBinary {
            package: names
                .get(&manifest)
                .cloned()
                .unwrap_or_else(|| manifest.display().to_string()),
            class: classify(&target),
            target,
            exe: exe.into(),
            manifest_dir: manifest.parent().unwrap().to_path_buf(),
        });
    }
    binaries
        .sort_by(|a, b| (a.class, &a.package, &a.target).cmp(&(b.class, &b.package, &b.target)));
    Ok(binaries)
}

/// Every workspace package: manifest path → name.
pub fn package_names(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let output = cargo()
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()?;
    if !output.status.success() {
        return Err("cargo metadata failed".into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    Ok(metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some((
                PathBuf::from(p["manifest_path"].as_str()?),
                p["name"].as_str()?.to_owned(),
            ))
        })
        .collect())
}

#[derive(Default)]
struct BinaryResult {
    passed: usize,
    ignored: usize,
    /// Failed tests and what they printed.
    failed: Vec<(String, String)>,
    /// Tests that had started but not finished when the watchdog fired.
    stalled: Vec<String>,
    /// The binary exited abnormally without reporting a failed test (an abort, a crash).
    crashed: Option<String>,
    time: Duration,
}

#[derive(Default)]
pub struct Report {
    results: Vec<(String, BinaryResult)>,
    skips: Vec<String>,
    /// A stalled process could not be ended; the run stopped there.
    unkillable: Option<String>,
}

impl Report {
    pub fn failed(&self) -> bool {
        self.unkillable.is_some()
            || self
                .results
                .iter()
                .any(|(_, r)| !r.failed.is_empty() || !r.stalled.is_empty() || r.crashed.is_some())
    }

    pub fn print(&self, binaries: &[TestBinary], options: &Options) {
        let (mut passed, mut ignored, mut failed) = (0, 0, 0);
        for (_, r) in &self.results {
            passed += r.passed;
            ignored += r.ignored;
            failed += r.failed.len() + r.stalled.len();
        }
        println!("\n== summary ({:?} tier)", options.tier);
        println!(
            "{} binaries: {passed} passed, {failed} failed, {ignored} ignored, {} skipped",
            self.results.len(),
            self.skips.len()
        );
        for (label, r) in &self.results {
            for (name, _) in &r.failed {
                println!("  FAILED  {label}  {name}");
            }
            for name in &r.stalled {
                println!("  STALLED {label}  {name}");
            }
            if let Some(why) = &r.crashed {
                println!("  CRASHED {label}  {why}");
            }
        }
        if !self.skips.is_empty() {
            println!("skipped:");
            for s in &self.skips {
                println!("  {s}");
            }
        }
        if let Some(label) = &self.unkillable {
            println!(
                "\n{label} stalled and could not be ended. A GPU driver deadlock leaves such a \
                 process in a kernel wait that only a reboot clears; the run stopped there."
            );
        }
        if options.tier != Tier::All && options.tier != Tier::Manual {
            let unclassified: BTreeSet<&str> = binaries
                .iter()
                .filter(|b| b.class == Class::Unclassified)
                .map(|b| b.package.as_str())
                .collect();
            if !unclassified.is_empty() {
                let count = binaries
                    .iter()
                    .filter(|b| b.class == Class::Unclassified)
                    .count();
                println!(
                    "\nnote: {count} test targets in {} packages are not yet in the 0014 layout and \
                     run only with --tier all: {}",
                    unclassified.len(),
                    unclassified.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
        }
    }
}

/// Runs the binaries `options.tier` selects, one at a time, with `env` added.
pub fn execute(
    root: &Path,
    binaries: &[TestBinary],
    options: &Options,
    env: &[(&str, String)],
) -> Result<Report> {
    let log_dir = crate::target_dir(root).join("xtask");
    std::fs::create_dir_all(&log_dir)?;
    let skip_log = log_dir.join("skips.log");
    let _ = std::fs::remove_file(&skip_log);
    let stall = Duration::from_secs(options.stall.unwrap_or(options.tier.default_stall()));

    let mut report = Report::default();
    for binary in binaries.iter().filter(|b| options.tier.includes(b.class)) {
        let label = binary.label();
        let mut command = Command::new(&binary.exe);
        command
            .current_dir(&binary.manifest_dir)
            .env("CARGO_MANIFEST_DIR", &binary.manifest_dir)
            .env("MARLIN_SKIP_LOG", &skip_log)
            .args([
                "-Z",
                "unstable-options",
                "--format",
                "json",
                "--report-time",
            ]);
        if options.tier == Tier::Manual {
            command.arg("--ignored");
        }
        if options.tier == Tier::Stress && std::env::var_os("MARLIN_TEST_SCALE").is_none() {
            command.env("MARLIN_TEST_SCALE", "10");
        }
        if let Some(seed) = &options.seed {
            command.env("MARLIN_TEST_SEED", seed);
        }
        if options.bless {
            command.env("MARLIN_BLESS", "1");
        }
        for (key, value) in env {
            command.env(key, value);
        }
        command.args(&options.harness_args).stdout(Stdio::piped());

        let start = Instant::now();
        let mut child = command.spawn()?;
        let (result, ended) = watch(&mut child, stall);
        let mut result = result;
        result.time = start.elapsed();
        print_result(&label, &result);
        let stalled = !result.stalled.is_empty();
        report.results.push((label.clone(), result));
        if stalled && !ended {
            report.unkillable = Some(label);
            break;
        }
    }
    if let Ok(log) = std::fs::read_to_string(&skip_log) {
        report.skips = log.lines().map(str::to_owned).collect();
    }
    Ok(report)
}

/// Follows `child`'s JSON events until it exits, or until `stall` passes with no test finishing;
/// then names the tests still running and kills it. Returns whether the process ended.
fn watch(child: &mut Child, stall: Duration) -> (BinaryResult, bool) {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut result = BinaryResult::default();
    let mut running = BTreeSet::new();
    let mut suite_failed = false;
    loop {
        match rx.recv_timeout(stall) {
            Ok(line) => {
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    println!("{line}");
                    continue;
                };
                let name = event["name"].as_str().unwrap_or_default().to_owned();
                match (event["type"].as_str(), event["event"].as_str()) {
                    (Some("test"), Some("started")) => {
                        running.insert(name);
                    }
                    (Some("test"), Some("ok")) => {
                        running.remove(&name);
                        result.passed += 1;
                    }
                    (Some("test"), Some("ignored")) => {
                        running.remove(&name);
                        result.ignored += 1;
                    }
                    (Some("test"), Some("failed")) => {
                        running.remove(&name);
                        let out = event["stdout"].as_str().unwrap_or_default().to_owned();
                        result.failed.push((name, out));
                    }
                    (Some("suite"), Some("failed")) => suite_failed = true,
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                result.stalled = running.iter().cloned().collect();
                if result.stalled.is_empty() {
                    result.stalled.push("<no test running>".into());
                }
                crate::process::kill_tree(child);
                // A process stuck in a driver's kernel wait may never exit.
                let deadline = Instant::now() + Duration::from_secs(15);
                while Instant::now() < deadline {
                    if let Ok(Some(_)) = child.try_wait() {
                        return (result, true);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                return (result, false);
            }
        }
    }
    match child.wait() {
        Ok(status) if !status.success() && result.failed.is_empty() => {
            result.crashed = Some(if running.is_empty() {
                format!("exited with {status}")
            } else {
                format!(
                    "exited with {status} while running {}",
                    running.into_iter().collect::<Vec<_>>().join(", ")
                )
            });
        }
        Ok(_) => {}
        Err(e) => result.crashed = Some(e.to_string()),
    }
    if suite_failed && result.failed.is_empty() && result.crashed.is_none() {
        result.crashed = Some("the suite failed without a failed test".into());
    }
    (result, true)
}

fn print_result(label: &str, r: &BinaryResult) {
    let ok = r.failed.is_empty() && r.stalled.is_empty() && r.crashed.is_none();
    println!(
        "{} {label}  ({} passed, {} failed, {} ignored, {:.1} s)",
        if ok { "ok    " } else { "FAILED" },
        r.passed,
        r.failed.len(),
        r.ignored,
        r.time.as_secs_f64()
    );
    for (name, out) in &r.failed {
        println!("---- {name} ----\n{}", out.trim_end());
    }
    if !r.stalled.is_empty() {
        println!(
            "---- stalled: no test finished within the watchdog's limit; still running: {}",
            r.stalled.join(", ")
        );
    }
    if let Some(why) = &r.crashed {
        println!("---- {why}");
    }
}
