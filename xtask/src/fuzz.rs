//! `cargo xtask test --tier fuzz`: the long-run fuzz layer (docs/design/0014-testing.md §7.3).
//!
//! The targets in `fuzz/fuzz_targets/` are libFuzzer targets (`libfuzzer-sys`, the layout
//! cargo-fuzz uses). xtask builds them itself with the flags cargo-fuzz would pass, plus the
//! platform's gen-cfg flags, so cargo-fuzz need not be installed, and handles what Windows
//! needs:
//!
//! - MSVC's linker finds libFuzzer's `main` in the libfuzzer-sys rlib only when told to
//!   (`/include:main`).
//! - The AddressSanitizer runtime is MSVC's `clang_rt.asan_dynamic-<arch>.dll`; it is found
//!   through vswhere and put on the fuzzer's `PATH`.
//! - `cl.exe` builds libFuzzer under the target dir; it fails with C1083 when that path nears
//!   260 characters, so keep the checkout's path short.
//!
//! Every run is bounded: `--time` seconds per target (libFuzzer's `-max_total_time`, and a
//! watchdog that kills the process tree after a grace period), `MEMORY_MB` of memory
//! (`-rss_limit_mb` and `-malloc_limit_mb`) and `INPUT_TIMEOUT` seconds per input. A crashing,
//! out-of-memory or hanging input is minimized, copied to
//! `<crate>/tests/data/regress/<target>/`, and the test that replays it is printed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::test::{self, Options};
use crate::{Result, cargo, cfg, coverage, platform};

/// Memory cap of every fuzz process.
const MEMORY_MB: u64 = 2048;
/// `--time` when not given.
const DEFAULT_TIME: u64 = 60;
/// An input that runs longer than this is a hang.
const INPUT_TIMEOUT: u64 = 10;
/// Time allowed for minimizing one crashing input.
const MINIMIZE_TIME: u64 = 60;
/// How long past its time limit a fuzzer may run (loading and merging the corpus) before the
/// watchdog ends it.
const GRACE: Duration = Duration::from_secs(120);

/// The `--tier fuzz` options.
#[derive(Default)]
pub struct Args {
    targets: Vec<String>,
    time: Option<u64>,
}

impl Args {
    /// Takes `--target <name>` (repeatable) and `--time <secs>`, for `Options::parse`.
    pub fn take(&mut self, arg: &str, it: &mut std::slice::Iter<'_, String>) -> Result<bool> {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg {
            "--target" => self.targets.push(value("--target")?),
            "--time" => self.time = Some(value("--time")?.parse()?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Fails if fuzz options were given to another tier.
    pub fn reject(&self) -> Result {
        if !self.targets.is_empty() || self.time.is_some() {
            return Err("--target and --time are options of --tier fuzz".into());
        }
        Ok(())
    }
}

/// What a fuzz run ended with.
enum Outcome {
    /// Ran its time without finding anything.
    Clean,
    /// Saved an input that crashed, ran out of memory or hung (`crash`, `oom`, `timeout`, ...).
    Found {
        kind: String,
        regress: PathBuf,
        minimized: bool,
    },
    /// Failed without saving an input.
    Failed(String),
}

struct TargetResult {
    target: String,
    outcome: Outcome,
    stats: String,
    time: Duration,
}

pub fn run(root: &Path, options: &Options, args: &Args) -> Result {
    if options.bless || options.stall.is_some() {
        return Err("--bless and --stall do not apply to --tier fuzz".into());
    }
    let fuzz_dir = root.join("fuzz");
    let all = targets(&fuzz_dir)?;
    let crates = crate_dirs(root)?;
    for t in &args.targets {
        if !all.contains(t) {
            return Err(format!("no fuzz target `{t}` (fuzz targets: {})", all.join(", ")).into());
        }
    }
    let mut selected = Vec::new();
    for t in all {
        let (package, dir) = owner(&crates, &t)?;
        let wanted = (args.targets.is_empty() || args.targets.contains(&t))
            && (options.packages.is_empty() || options.packages.iter().any(|p| p == package));
        if wanted {
            selected.push((t, dir.clone()));
        }
    }
    if selected.is_empty() {
        return Err("no fuzz target matches the options".into());
    }
    let time = args.time.unwrap_or(DEFAULT_TIME);

    let (host, _) = coverage::rustc_host()?;
    let out = crate::target_dir(root).join("fuzz");
    build(root, &out, &host, selected.iter().map(|(t, _)| t.as_str()))?;
    let asan_dir = if host.contains("-msvc") {
        Some(msvc_asan_dir(&host)?)
    } else {
        None
    };
    let exe_dir = out.join(&host).join("release");

    let mut results = Vec::new();
    for (target, crate_dir) in &selected {
        println!("\n== fuzz {target} ({time} s, {MEMORY_MB} MB)");
        let fuzzer = Fuzzer {
            exe: exe_dir.join(format!("{target}{}", std::env::consts::EXE_SUFFIX)),
            asan_dir: asan_dir.clone(),
            seed: options.seed.clone(),
            extra: options.harness_args.clone(),
        };
        let start = Instant::now();
        let (outcome, stats) = fuzz_one(root, &out, &fuzzer, target, crate_dir, time)?;
        let result = TargetResult {
            target: target.clone(),
            outcome,
            stats,
            time: start.elapsed(),
        };
        print_result(root, &result);
        results.push(result);
    }

    println!("\n== summary (fuzz tier, {time} s per target)");
    let mut found = Vec::new();
    for r in &results {
        let status = match &r.outcome {
            Outcome::Clean => "ok    ",
            Outcome::Found { .. } => "FOUND ",
            Outcome::Failed(_) => "FAILED",
        };
        println!(
            "{status} {:<20} {:>6.1} s  {}",
            r.target,
            r.time.as_secs_f64(),
            r.stats
        );
        if !matches!(r.outcome, Outcome::Clean) {
            found.push(r);
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    for r in &found {
        if let Outcome::Found { regress, .. } = &r.outcome {
            let (package, _) = owner(&crates, &r.target)?;
            println!("\n{}", replay_hint(root, package, &r.target, regress));
        }
    }
    Err(format!("{} fuzz targets found a problem", found.len()).into())
}

/// The targets: `fuzz/fuzz_targets/*.rs`, sorted.
fn targets(fuzz_dir: &Path) -> Result<Vec<String>> {
    let dir = fuzz_dir.join("fuzz_targets");
    let mut names = Vec::new();
    for entry in
        std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", rel(fuzz_dir, &dir).display()))?
    {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "rs") {
            names.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Workspace crates by their target-name prefix (`marlin-audio-device` → `audio_device`):
/// (prefix, package, crate dir).
fn crate_dirs(root: &Path) -> Result<Vec<(String, String, PathBuf)>> {
    Ok(test::package_names(root)?
        .into_iter()
        .filter_map(|(manifest, name)| {
            let prefix = name.strip_prefix("marlin-")?.replace('-', "_");
            Some((prefix, name, manifest.parent()?.to_path_buf()))
        })
        .collect())
}

/// The crate a target `<crate>_<what>` belongs to: the longest matching prefix.
fn owner<'a>(
    crates: &'a [(String, String, PathBuf)],
    target: &str,
) -> Result<(&'a str, &'a PathBuf)> {
    crates
        .iter()
        .filter(|(prefix, _, _)| {
            target
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('_'))
        })
        .max_by_key(|(prefix, _, _)| prefix.len())
        .map(|(_, package, dir)| (package.as_str(), dir))
        .ok_or_else(|| {
            format!(
                "fuzz target `{target}` names no workspace crate: targets are `<crate>_<what>`, \
                 `<crate>` being the package name without `marlin-`"
            )
            .into()
        })
}

/// Builds `targets` with libFuzzer's instrumentation and AddressSanitizer into `out`.
fn build<'a>(
    root: &Path,
    out: &Path,
    host: &str,
    targets: impl Iterator<Item = &'a str>,
) -> Result {
    let platform = platform::find(root, platform::host())?;
    let mut flags = cfg::flags(&platform, host, None);
    flags.extend(
        [
            "-Cpasses=sancov-module",
            "-Cllvm-args=-sanitizer-coverage-level=4",
            "-Cllvm-args=-sanitizer-coverage-inline-8bit-counters",
            "-Cllvm-args=-sanitizer-coverage-pc-table",
            "-Cllvm-args=-sanitizer-coverage-trace-compares",
            "--cfg=fuzzing",
            "-Zsanitizer=address",
            "-Cdebug-assertions",
            "-Coverflow-checks",
        ]
        .map(String::from),
    );
    if host.contains("-msvc") {
        flags.push("-Clink-arg=/include:main".into());
    }
    let mut command = cargo();
    command
        .current_dir(root)
        .args(["build", "--release", "--manifest-path"])
        .arg(root.join("fuzz").join("Cargo.toml"))
        .args(["--target", host, "--target-dir"])
        .arg(out)
        .env("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))
        .env_remove("RUSTFLAGS");
    for t in targets {
        command.args(["--bin", t]);
    }
    if !command.status()?.success() {
        return Err("building the fuzz targets failed".into());
    }
    Ok(())
}

/// The directory of MSVC's AddressSanitizer runtime DLL for `host`, through vswhere.
fn msvc_asan_dir(host: &str) -> Result<PathBuf> {
    let (arch, tools) = match host.split('-').next() {
        Some("x86_64") => ("x86_64", r"Hostx64\x64"),
        Some("aarch64") => ("aarch64", r"Hostarm64\arm64"),
        _ => return Err(format!("no MSVC AddressSanitizer runtime is known for {host}").into()),
    };
    let dll = format!("clang_rt.asan_dynamic-{arch}.dll");
    let program_files =
        std::env::var_os("ProgramFiles(x86)").ok_or("ProgramFiles(x86) is not set")?;
    let vswhere = Path::new(&program_files)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    let output = Command::new(&vswhere)
        .args(["-latest", "-products", "*", "-find"])
        .arg(format!(r"VC\Tools\MSVC\**\bin\{tools}\{dll}"))
        .output()
        .map_err(|e| format!("{}: {e}", vswhere.display()))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // The newest toolset: versions sort as strings within one installation.
        .max()
        .and_then(|l| Path::new(l).parent().map(Path::to_path_buf))
        .ok_or_else(|| {
            format!(
                "{dll} not found: AddressSanitizer needs the MSVC build tools' \
                 \"C++ AddressSanitizer\" component"
            )
            .into()
        })
}

struct Fuzzer {
    exe: PathBuf,
    asan_dir: Option<PathBuf>,
    seed: Option<String>,
    /// From `--`: more libFuzzer flags.
    extra: Vec<String>,
}

impl Fuzzer {
    /// A command for the fuzzer with the memory and per-input limits, the ASan runtime on
    /// `PATH`, and stdout and stderr appended to `log`.
    fn command(&self, log: &Path) -> Result<Command> {
        let mut command = Command::new(&self.exe);
        command.args([
            format!("-rss_limit_mb={MEMORY_MB}"),
            format!("-malloc_limit_mb={MEMORY_MB}"),
            format!("-timeout={INPUT_TIMEOUT}"),
        ]);
        if let Some(dir) = &self.asan_dir {
            let old = std::env::var_os("PATH").unwrap_or_default();
            let path = std::env::join_paths(
                std::iter::once(dir.clone()).chain(std::env::split_paths(&old)),
            )
            .map_err(|e| format!("PATH: {e}"))?;
            command.env("PATH", path);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        command
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file);
        Ok(command)
    }
}

/// Runs one target for `time` seconds; on a find, minimizes it and copies it to the crate's
/// regress directory. Returns the outcome and a line of statistics.
fn fuzz_one(
    root: &Path,
    out: &Path,
    fuzzer: &Fuzzer,
    target: &str,
    crate_dir: &Path,
    time: u64,
) -> Result<(Outcome, String)> {
    // The fuzzer adds what it finds to the first corpus directory: keep that out of the
    // repository, and the seeds in fuzz/corpus/ read-only.
    let work = out.join("corpus").join(target);
    let seeds = root.join("fuzz").join("corpus").join(target);
    let artifacts = out.join("artifacts").join(target);
    let logs = out.join("logs");
    let _ = std::fs::remove_dir_all(&artifacts);
    for dir in [&work, &artifacts, &logs] {
        std::fs::create_dir_all(dir)?;
    }
    let log = logs.join(format!("{target}.log"));
    let _ = std::fs::remove_file(&log);

    let mut command = fuzzer.command(&log)?;
    command
        .arg(format!("-max_total_time={time}"))
        .arg(format!("-artifact_prefix={}", dir_prefix(&artifacts)))
        .arg("-print_final_stats=1");
    if let Some(seed) = &fuzzer.seed {
        command.arg(format!("-seed={seed}"));
    }
    command.args(&fuzzer.extra).arg(&work);
    if seeds.is_dir() {
        command.arg(&seeds);
    }
    let status = run_bounded(command, Duration::from_secs(time) + GRACE)?;
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let stats = stats(&text);

    let Some(found) = first_artifact(&artifacts)? else {
        return Ok(match status {
            Some(s) if s.success() => (Outcome::Clean, stats),
            Some(s) => (
                Outcome::Failed(format!(
                    "exited with {s} without saving an input; log: {}\n{}",
                    rel(root, &log).display(),
                    tail(&text, 40)
                )),
                stats,
            ),
            None => (
                Outcome::Failed(format!(
                    "ran past its time limit and was killed; log: {}",
                    rel(root, &log).display()
                )),
                stats,
            ),
        });
    };
    let name = found.file_name().unwrap().to_string_lossy().into_owned();
    let kind = name.split('-').next().unwrap_or("crash").to_owned();
    println!(
        "{target}: found {}\n{}",
        rel(root, &found).display(),
        tail(&text, 40)
    );

    // Minimize: libFuzzer reruns the input in child processes, shrinking it while it still fails.
    let minimized = artifacts.join(format!("minimized-{name}"));
    let minimize_log = logs.join(format!("{target}.minimize.log"));
    let _ = std::fs::remove_file(&minimize_log);
    let mut command = fuzzer.command(&minimize_log)?;
    command
        .arg("-minimize_crash=1")
        .arg(format!("-max_total_time={MINIMIZE_TIME}"))
        .arg(format!("-exact_artifact_path={}", minimized.display()))
        .arg(&found);
    run_bounded(command, Duration::from_secs(MINIMIZE_TIME) + GRACE)?;
    let is_minimized = minimized.is_file();
    let input = std::fs::read(if is_minimized { &minimized } else { &found })?;

    let regress_dir = crate_dir
        .join("tests")
        .join("data")
        .join("regress")
        .join(target);
    std::fs::create_dir_all(&regress_dir)?;
    let attributes = regress_dir.join(".gitattributes");
    if !attributes.exists() {
        std::fs::write(
            &attributes,
            "# Fuzz inputs are bytes: never touch line endings.\n* binary\n",
        )?;
    }
    let regress = regress_dir.join(&name);
    std::fs::write(&regress, input)?;
    Ok((
        Outcome::Found {
            kind,
            regress,
            minimized: is_minimized,
        },
        stats,
    ))
}

/// `dir` with a trailing separator: libFuzzer prepends the prefix to artifact names as is.
fn dir_prefix(dir: &Path) -> String {
    let mut s = dir.display().to_string();
    s.push(std::path::MAIN_SEPARATOR);
    s
}

/// Runs `command` until it exits or `limit` passes; then kills it with its children (libFuzzer
/// starts child processes to minimize). Returns its status, or `None` when it was killed.
fn run_bounded(mut command: Command, limit: Duration) -> Result<Option<std::process::ExitStatus>> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            crate::process::kill_tree(&mut child);
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// The saved input of a find, if any (`slow-unit-*` files are not finds).
fn first_artifact(dir: &Path) -> Result<Option<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if ["crash-", "oom-", "timeout-", "leak-"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            found.push(path);
        }
    }
    found.sort();
    Ok(found.into_iter().next())
}

/// Runs, coverage and corpus size from libFuzzer's output.
fn stats(log: &str) -> String {
    let stat = |key: &str| {
        log.lines()
            .find_map(|l| l.strip_prefix(&format!("stat::{key}:")))
            .map(str::trim)
            .unwrap_or("?")
            .to_owned()
    };
    // The last progress line: `#123 NEW cov: 45 ft: 67 corp: 8/90b ...`.
    let progress = log
        .lines()
        .rev()
        .find(|l| l.starts_with('#') && l.contains(" cov: "))
        .unwrap_or("");
    let field = |name: &str| {
        progress
            .split_whitespace()
            .skip_while(|w| *w != name)
            .nth(1)
            .unwrap_or("?")
            .to_owned()
    };
    format!(
        "{} runs, cov {}, corpus {}, peak {} MB",
        stat("number_of_executed_units"),
        field("cov:"),
        field("corp:"),
        stat("peak_rss_mb")
    )
}

fn print_result(root: &Path, r: &TargetResult) {
    match &r.outcome {
        Outcome::Clean => println!("ok     {}  ({})", r.target, r.stats),
        Outcome::Found {
            kind,
            regress,
            minimized,
        } => println!(
            "FOUND  {}  {kind}: {}input saved to {}",
            r.target,
            if *minimized { "minimized " } else { "" },
            rel(root, regress).display()
        ),
        Outcome::Failed(why) => println!("FAILED {}  {why}", r.target),
    }
}

/// What to add to the crate so its quick tier replays the saved inputs.
fn replay_hint(root: &Path, package: &str, target: &str, regress: &Path) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{target}: the input is in {}. Once the bug is fixed, {package}'s unit tests replay \
         that directory with (marlin-test-support as a dev-dependency; add it once per target):",
        rel(root, regress.parent().unwrap()).display()
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "    #[test]");
    let _ = writeln!(
        s,
        "    fn {target}_regressions_are_errors_never_panics() {{"
    );
    let _ = writeln!(
        s,
        "        marlin_test_support::regress::for_each(env!(\"CARGO_MANIFEST_DIR\"), \"{target}\", |data| {{"
    );
    let _ = writeln!(
        s,
        "            // The body of `run` in fuzz/fuzz_targets/{target}.rs."
    );
    let _ = writeln!(s, "        }});");
    let _ = write!(s, "    }}");
    s
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// `path` relative to `base` when it is inside it, for messages.
fn rel<'a>(base: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(base).unwrap_or(path)
}
