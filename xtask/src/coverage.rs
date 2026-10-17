//! `cargo xtask coverage`: source coverage of the workspace's `src/` from the automated test
//! tiers, through the toolchain's llvm-tools (docs/design/0014-testing.md §7.4).
//!
//! The tests are built into `target/coverage/build` with `-Cinstrument-coverage`, run by the
//! same runner as `cargo xtask test`, and the profiles are merged and summarized per crate.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::test::{self, Options, Tier};
use crate::{Result, cfg, platform};

/// Files left out of the report: tests, benches, tooling, the test helpers, std and
/// dependencies. Only workspace `src/` files are summarized in any case.
const IGNORE: &str = r"[\\/](tests|benches|xtask)[\\/]|[\\/]tools[\\/](gpu_)?test_support[\\/]|[\\/]\.cargo[\\/]|[\\/]\.rustup[\\/]|[\\/]rustc[\\/]|[\\/]lib[\\/]rustlib[\\/]";

/// A crate's line coverage may drop this many percentage points below the baseline before it is
/// flagged.
const DROP: f64 = 0.5;

pub fn run(root: &Path, args: &[String]) -> Result {
    let mut html = false;
    let mut save_baseline = false;
    let options = Options::parse(args, Tier::All, |arg, _| {
        match arg {
            "--html" => html = true,
            "--save-baseline" => save_baseline = true,
            _ => return Ok(false),
        }
        Ok(true)
    })?;
    if matches!(
        options.tier,
        Tier::Stress | Tier::Manual | Tier::Bench | Tier::Fuzz
    ) {
        return Err("coverage counts the quick, gpu and desktop tiers (or all of them)".into());
    }
    // `all` here means every automated tier the report counts, not stress.
    let tiers: &[Tier] = match options.tier {
        Tier::All => &[Tier::Quick, Tier::Gpu, Tier::Desktop],
        t => std::slice::from_ref(match t {
            Tier::Quick => &Tier::Quick,
            Tier::Gpu => &Tier::Gpu,
            _ => &Tier::Desktop,
        }),
    };

    let (host, sysroot) = rustc_host()?;
    let bin_dir = sysroot.join("lib").join("rustlib").join(&host).join("bin");
    let tool = |name: &str| bin_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let (profdata, llvm_cov) = (tool("llvm-profdata"), tool("llvm-cov"));
    if !profdata.exists() || !llvm_cov.exists() {
        return Err(format!(
            "{} not found: the toolchain needs the llvm-tools component \
             (listed in rust-toolchain.toml; `rustup component add llvm-tools`)",
            profdata.display()
        )
        .into());
    }

    let dir = root.join("target").join("coverage");
    let raw = dir.join("profraw");
    let _ = std::fs::remove_dir_all(&raw);
    std::fs::create_dir_all(&raw)?;

    let platform = platform::find(root, platform::host())?;
    let mut flags = cfg::flags(&platform, &host, None);
    flags.push("-Cinstrument-coverage".into());
    let env = [("CARGO_ENCODED_RUSTFLAGS", flags.join("\x1f"))];
    let binaries = test::build(root, &options.packages, &env, Some(&dir.join("build")))?;

    let profile = raw.join("%p-%m.profraw").to_string_lossy().into_owned();
    let mut failed = false;
    let mut ran = Vec::new();
    for &tier in tiers {
        let tier_options = Options {
            tier,
            packages: options.packages.clone(),
            seed: options.seed.clone(),
            bless: false,
            stall: options.stall,
            harness_args: options.harness_args.clone(),
        };
        let report = test::execute(
            root,
            &binaries,
            &tier_options,
            &[("LLVM_PROFILE_FILE", profile.clone())],
        )?;
        failed |= report.failed();
        ran.extend(
            binaries
                .iter()
                .filter(|b| tier.includes(b.class))
                .map(|b| b.exe.clone()),
        );
    }
    if ran.is_empty() {
        return Err(format!(
            "no test binaries in the {:?} tier (0014 §5 names them unit, gpu, golden, desktop, ...)",
            options.tier
        )
        .into());
    }

    // Merge. Lists go through files: there can be more paths than a Windows command line holds.
    let list = dir.join("profraw.txt");
    let mut files = String::new();
    for entry in std::fs::read_dir(&raw)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "profraw") {
            writeln!(files, "{}", path.display())?;
        }
    }
    if files.is_empty() {
        return Err("the tests wrote no coverage profiles".into());
    }
    std::fs::write(&list, files)?;
    let merged = dir.join("coverage.profdata");
    let status = Command::new(&profdata)
        .args(["merge", "-sparse", "-f"])
        .arg(&list)
        .arg("-o")
        .arg(&merged)
        .status()?;
    if !status.success() {
        return Err("llvm-profdata merge failed".into());
    }

    let objects = response_file(&dir.join("objects.rsp"), &merged, &ran)?;
    let output = Command::new(&llvm_cov)
        .args(["export", "-summary-only"])
        .arg(format!("@{}", objects.display()))
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "llvm-cov export failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let export: Value = serde_json::from_slice(&output.stdout)?;
    let crates = summarize(root, &export)?;

    let baseline_path = dir.join("baseline.json");
    let baseline: BTreeMap<String, (f64, f64)> = std::fs::read(&baseline_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| {
            v["crates"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, c)| {
                    (
                        k.clone(),
                        (
                            c["lines"].as_f64().unwrap_or(0.0),
                            c["regions"].as_f64().unwrap_or(0.0),
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    // With -p, crates the selected tests never reach are noise; without it they are a finding.
    print_table(
        &crates,
        &baseline,
        options.tier,
        &platform.name,
        options.packages.is_empty(),
    );

    let summary = json!({
        "platform": platform.name,
        "tier": format!("{:?}", options.tier),
        "crates": crates
            .iter()
            .filter(|c| c.lines.count > 0)
            .map(|c| (c.name.clone(), json!({ "lines": c.lines.percent(), "regions": c.regions.percent() })))
            .collect::<serde_json::Map<_, _>>(),
    });
    let text = serde_json::to_string_pretty(&summary)?;
    std::fs::write(dir.join("summary.json"), &text)?;
    if save_baseline {
        std::fs::write(&baseline_path, &text)?;
        println!("saved the baseline: target/coverage/baseline.json");
    }

    if html {
        let out = dir.join("html");
        let status = Command::new(&llvm_cov)
            .args(["show", "-format=html", "-show-line-counts-or-regions"])
            .arg(format!("-output-dir={}", out.display()))
            .arg(format!("@{}", objects.display()))
            .status()?;
        if !status.success() {
            return Err("llvm-cov show failed".into());
        }
        println!("HTML report: target/coverage/html/index.html");
    }
    if failed {
        return Err("some tests failed; the coverage above is of the run as it went".into());
    }
    Ok(())
}

/// The host triple and the toolchain's sysroot.
pub fn rustc_host() -> Result<(String, PathBuf)> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let version = Command::new(&rustc).arg("-vV").output()?;
    let host = String::from_utf8_lossy(&version.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .ok_or("rustc -vV printed no host")?;
    let sysroot = Command::new(&rustc).args(["--print", "sysroot"]).output()?;
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    Ok((host, sysroot))
}

/// An llvm-cov response file: the profile, the ignore pattern and every object.
fn response_file(path: &Path, profile: &Path, objects: &[PathBuf]) -> Result<PathBuf> {
    let quote = |p: &Path| format!("\"{}\"", p.display().to_string().replace('\\', "/"));
    let mut text = format!(
        "-instr-profile={}\n-ignore-filename-regex=\"{}\"\n",
        quote(profile),
        IGNORE.replace('\\', "\\\\")
    );
    for (i, object) in objects.iter().enumerate() {
        if i > 0 {
            text.push_str("-object\n");
        }
        writeln!(text, "{}", quote(object))?;
    }
    std::fs::write(path, text)?;
    Ok(path.to_path_buf())
}

#[derive(Default, Clone, Copy)]
struct Count {
    covered: u64,
    count: u64,
}

impl Count {
    fn add(&mut self, v: &Value) {
        self.covered += v["covered"].as_u64().unwrap_or(0);
        self.count += v["count"].as_u64().unwrap_or(0);
    }

    fn percent(self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            100.0 * self.covered as f64 / self.count as f64
        }
    }
}

struct CrateCoverage {
    name: String,
    group: &'static str,
    lines: Count,
    regions: Count,
}

/// The report's files grouped by workspace crate (files under a crate's `src/` only).
fn summarize(root: &Path, export: &Value) -> Result<Vec<CrateCoverage>> {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let root_s = norm(root);
    let mut crates: Vec<(String, CrateCoverage)> = test::package_names(root)?
        .into_iter()
        .filter(|(_, name)| {
            !matches!(
                name.as_str(),
                "xtask" | "marlin-test-support" | "marlin-gpu-test-support"
            )
        })
        .map(|(manifest, name)| {
            let dir = norm(manifest.parent().unwrap());
            let rel = dir
                .strip_prefix(&root_s)
                .unwrap_or(&dir)
                .trim_start_matches('/')
                .to_owned();
            let group = group(&rel);
            (
                format!("{dir}/src/"),
                CrateCoverage {
                    name,
                    group,
                    lines: Count::default(),
                    regions: Count::default(),
                },
            )
        })
        .collect();
    // Longest prefix first, so a nested crate wins over its parent directory.
    crates.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));

    for file in export["data"][0]["files"].as_array().into_iter().flatten() {
        let Some(name) = file["filename"].as_str() else {
            continue;
        };
        let name = norm(Path::new(name));
        if let Some((_, c)) = crates
            .iter_mut()
            .find(|(prefix, _)| name.starts_with(prefix.as_str()))
        {
            c.lines.add(&file["summary"]["lines"]);
            c.regions.add(&file["summary"]["regions"]);
        }
    }
    let mut crates: Vec<CrateCoverage> = crates.into_iter().map(|(_, c)| c).collect();
    crates.sort_by(|a, b| (a.group, &a.name).cmp(&(b.group, &b.name)));
    Ok(crates)
}

fn group(rel: &str) -> &'static str {
    [
        ("runtime/base/", "L0 base"),
        ("runtime/platform/", "L1 platform"),
        ("runtime/core/", "L2 core"),
        ("runtime/engine/", "L3 engine"),
        ("runtime/", "runtime"),
        ("editor/", "editor"),
        ("tools/", "tools"),
        ("apps/", "apps"),
    ]
    .into_iter()
    .find(|(prefix, _)| rel.starts_with(prefix))
    .map_or("other", |(_, g)| g)
}

fn print_table(
    crates: &[CrateCoverage],
    baseline: &BTreeMap<String, (f64, f64)>,
    tier: Tier,
    platform: &str,
    show_unmeasured: bool,
) {
    println!("\n== coverage ({platform}, {tier:?} tier): lines / regions of src/");
    let mut group = "";
    let mut totals: BTreeMap<&str, (Count, Count)> = BTreeMap::new();
    for c in crates
        .iter()
        .filter(|c| show_unmeasured || c.lines.count > 0)
    {
        if c.group != group {
            group = c.group;
            println!("{group}");
        }
        let t = totals.entry(c.group).or_default();
        t.0.covered += c.lines.covered;
        t.0.count += c.lines.count;
        t.1.covered += c.regions.covered;
        t.1.count += c.regions.count;
        if c.lines.count == 0 {
            println!("  {:<28} —  no test runs this crate's code", c.name);
            continue;
        }
        let trend = match baseline.get(&c.name) {
            Some(&(lines, _)) if c.lines.percent() < lines - DROP => {
                format!(
                    "  ▼ {:.1} below the baseline ({lines:.1}%)",
                    lines - c.lines.percent()
                )
            }
            _ => String::new(),
        };
        println!(
            "  {:<28} {:>5.1}%  {:>5.1}%{trend}",
            c.name,
            c.lines.percent(),
            c.regions.percent()
        );
    }
    println!("by layer:");
    for (group, (lines, regions)) in totals.into_iter().filter(|(_, (l, _))| l.count > 0) {
        println!(
            "  {group:<28} {:>5.1}%  {:>5.1}%",
            lines.percent(),
            regions.percent()
        );
    }
}
