//! `cargo xtask test --tier bench`: builds and runs the `benches/` targets (marlin-bench
//! harness, release), collects their JSON results and compares them with a saved baseline
//! (docs/design/0014-testing.md §7.2).
//!
//! Results: `target/bench/<package>/<bench>.json`, written by the harness. Baselines are copies
//! under `target/bench/baselines/<tag>/`; they stay local (one machine for now).

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::test::{Options, Tier, package_names};
use crate::{Result, cargo};

/// The bench-only options of `cargo xtask test`.
#[derive(Default)]
pub struct Flags {
    save_baseline: Option<String>,
    baseline: Option<String>,
    /// Percent slower than the baseline's median that gets flagged (default 5).
    threshold: Option<f64>,
}

impl Flags {
    /// For [`Options::parse`]: takes `--save-baseline <tag>`, `--baseline <tag>` and
    /// `--threshold <percent>`.
    pub fn parse(&mut self, arg: &str, it: &mut std::slice::Iter<'_, String>) -> Result<bool> {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--save-baseline" => self.save_baseline = Some(tag(value()?)?),
            "--baseline" => self.baseline = Some(tag(value()?)?),
            "--threshold" => self.threshold = Some(value()?.parse()?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn given(&self) -> bool {
        self.save_baseline.is_some() || self.baseline.is_some() || self.threshold.is_some()
    }
}

/// A baseline tag names a directory: keep it to one plain component.
fn tag(tag: String) -> Result<String> {
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || tag.starts_with('.')
    {
        return Err(format!("baseline tag `{tag}`: use letters, digits, `-`, `_` and `.`").into());
    }
    Ok(tag)
}

/// `CARGO_TARGET_DIR` (relative to the workspace root) or `<root>/target`, the same rule the
/// harness follows.
fn bench_dir(root: &Path) -> PathBuf {
    root.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
        .join("bench")
}

struct BenchBinary {
    package: String,
    target: String,
    exe: PathBuf,
    manifest_dir: PathBuf,
}

impl BenchBinary {
    /// Where the harness writes: it names the file after the crate, `-` becoming `_`.
    fn results(&self, dir: &Path) -> PathBuf {
        dir.join(&self.package)
            .join(format!("{}.json", self.target.replace('-', "_")))
    }
}

pub fn run(root: &Path, options: &Options, flags: &Flags) -> Result {
    if options.tier != Tier::Bench {
        return Err("--baseline, --save-baseline and --threshold need --tier bench".into());
    }
    let dir = bench_dir(root);
    let baseline = match &flags.baseline {
        Some(tag) => {
            let path = dir.join("baselines").join(tag);
            if !path.is_dir() {
                return Err(
                    format!("no baseline `{tag}` (save one with --save-baseline {tag})").into(),
                );
            }
            Some(path)
        }
        None => None,
    };

    let binaries = build(root, &options.packages)?;
    if binaries.is_empty() {
        return Err("no bench targets in the selected packages (0014 §7.2: benches/*.rs)".into());
    }
    // A bench binary bounds each case itself (marlin-bench's max_time); this catches a hang.
    let limit = Duration::from_secs(options.stall.unwrap_or(1800));

    let mut results = Vec::new();
    let mut failed = Vec::new();
    for binary in &binaries {
        let label = format!("{}/{}", binary.package, binary.target);
        let path = binary.results(&dir);
        // Results only from this run: a filtered or failed run must not leave old ones behind.
        let _ = std::fs::remove_file(&path);
        let mut command = Command::new(&binary.exe);
        command
            .current_dir(&binary.manifest_dir)
            .env("CARGO_MANIFEST_DIR", &binary.manifest_dir)
            .arg("--bench")
            .args(&options.harness_args);
        let start = Instant::now();
        match wait(command, limit)? {
            Some(true) => {}
            Some(false) => failed.push(format!("{label}: failed")),
            None => failed.push(format!("{label}: still running after {limit:?}; killed")),
        }
        if path.is_file() {
            results.push((binary, path));
        }
        println!("({label}: {:.1} s)\n", start.elapsed().as_secs_f64());
    }

    println!("== summary (Bench tier)");
    println!(
        "{} bench binaries, {} wrote results",
        binaries.len(),
        results.len()
    );
    for (_, path) in &results {
        println!("  {}", shown(root, path));
    }
    for f in &failed {
        println!("  FAILED {f}");
    }

    if let Some(base) = &baseline {
        compare(base, &results, flags.threshold.unwrap_or(5.0))?;
    }
    if let Some(tag) = &flags.save_baseline {
        let to = dir.join("baselines").join(tag);
        for (binary, path) in &results {
            let dest = binary.results(&to);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::copy(path, &dest)?;
        }
        println!(
            "\nsaved {} result files as baseline `{tag}` ({})",
            results.len(),
            shown(root, &to)
        );
    }
    if !failed.is_empty() {
        return Err("benchmarks failed".into());
    }
    Ok(())
}

fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Runs `command` to completion, or kills it after `limit`. `None` if killed.
fn wait(mut command: Command, limit: Duration) -> Result<Option<bool>> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.success()));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Builds the `benches/` targets of `packages` (all when empty) in the bench profile.
fn build(root: &Path, packages: &[String]) -> Result<Vec<BenchBinary>> {
    let names = package_names(root)?;
    let mut command = cargo();
    command.current_dir(root).args([
        "bench",
        "--no-run",
        "--benches",
        "--message-format=json-render-diagnostics",
    ]);
    if packages.is_empty() {
        command.arg("--workspace");
    }
    for p in packages {
        command.args(["--package", p]);
    }
    command.env_remove("RUSTFLAGS").stdout(Stdio::piped());
    let output = command.spawn()?.wait_with_output()?;
    if !output.status.success() {
        return Err("building the benchmarks failed".into());
    }
    let mut binaries = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // `--benches` also builds libraries' unit tests; only bench targets count.
        if message["reason"] != "compiler-artifact"
            || message["target"]["kind"]
                .as_array()
                .is_none_or(|k| k.iter().all(|k| k != "bench"))
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
        binaries.push(BenchBinary {
            package: names
                .get(&manifest)
                .cloned()
                .unwrap_or_else(|| manifest.display().to_string()),
            target: message["target"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            exe: exe.into(),
            manifest_dir: manifest.parent().unwrap().to_path_buf(),
        });
    }
    binaries.sort_by(|a, b| (&a.package, &a.target).cmp(&(&b.package, &b.target)));
    Ok(binaries)
}

/// One result file: whether it was a `--quick` run, and each bench's median ns, in run order.
fn medians(path: &Path) -> Result<(bool, Vec<(String, f64)>)> {
    let json: Value = serde_json::from_str(&std::fs::read_to_string(path)?)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let medians = json["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((r["name"].as_str()?.to_owned(), r["median_ns"].as_f64()?)))
        .collect();
    Ok((json["quick"].as_bool().unwrap_or(false), medians))
}

/// Prints every bench's change against the baseline and flags medians more than `threshold`
/// percent slower. Only reports: a slower bench does not fail the run.
fn compare(base: &Path, results: &[(&BenchBinary, PathBuf)], threshold: f64) -> Result {
    let color = std::io::stdout().is_terminal();
    let red = |s: &str| {
        if color {
            format!("\x1b[31m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    };
    println!(
        "\n== against baseline `{}` (flagging > {threshold}% slower)",
        base.file_name().unwrap_or_default().to_string_lossy()
    );
    let (mut slower, mut compared, mut missing) = (Vec::new(), 0, 0);
    let mut quick = false;
    for (binary, path) in results {
        let (now_quick, now) = medians(path)?;
        let base_path = binary.results(base);
        let (then_quick, then) = if base_path.is_file() {
            medians(&base_path)?
        } else {
            (false, Vec::new())
        };
        let then: HashMap<String, f64> = then.into_iter().collect();
        quick |= now_quick || then_quick;
        println!("  {}/{}", binary.package, binary.target);
        for (name, median) in now {
            let Some(&old) = then.get(&name) else {
                missing += 1;
                println!("    {name:<52} {:>11}  (not in baseline)", time(median));
                continue;
            };
            compared += 1;
            let change = (median / old - 1.0) * 100.0;
            let line = format!(
                "    {name:<52} {:>11} -> {:>11}  {change:>+7.1}%",
                time(old),
                time(median)
            );
            if change > threshold {
                println!("{}", red(&format!("{line}  SLOWER")));
                slower.push(format!(
                    "{}/{}: {name} {change:+.1}%",
                    binary.package, binary.target
                ));
            } else {
                println!("{line}");
            }
        }
    }
    println!(
        "{compared} compared, {} slower by more than {threshold}%, {missing} not in the baseline",
        slower.len()
    );
    for s in &slower {
        println!("  {}", red(&format!("SLOWER {s}")));
    }
    if quick {
        println!("note: a --quick run is on one side; its medians are noisy");
    }
    Ok(())
}

/// `1.234 ms`, as the harness prints.
fn time(ns: f64) -> String {
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
