//! Command-line asset cooker (`docs/design/0003-asset-pipeline.md`).
//!
//! `<project>` is a project directory or its `.mproject` file (0003 §4.5); its content is the
//! engine's (`/Engine`) and its own (`/Game`), and one build holds both.
//!
//! - `marlin-cook cook <project> [--platform <name>] [--threads <n>] [--store <addr>] [--shipping]`:
//!   brings the indexes up to date, cooks every unit whose inputs changed (the rest come from the
//!   local cache in `<project>/.marlin/ddc`) and publishes the build manifest to
//!   `<project>/.marlin/build/<platform>.manifest`. Fails if any unit fails. `--shipping` cooks
//!   for a shipping build (shaders without `Debug.*` and debug information). The build holds the
//!   engine's and the project's config files for the platform (0003 §4.6).
//! - `marlin-cook package <project> [--platform <name>] [--store <addr>] [--out <file>] [--head-codec <codec>]
//!   [--bulk-codec <codec>] [--level <1-9>]`: packs the last cooked build of the platform into a
//!   `.mpak` (default `<project>/.marlin/build/<platform>.mpak`). Codecs: none, lz4, zstd,
//!   gdeflate.
//! - `marlin-cook stage <project> [--platform <name>] [--game <exe>]`: lays out the game as it ships
//!   (0003 §4.6) in `<project>/.marlin/staged/<platform>/<Name>/`: the game executable, renamed
//!   `<Name>`, and the last package of the platform as `content/paks/base.mpak`. The executable is
//!   `--game`, or the `marlin-game` next to this tool.
//! - `marlin-cook index <project>`: brings the asset index of every mount up to date and saves it.
//! - `marlin-cook validate <project>`: the same, then reports content problems (dangling and stale
//!   links, UUID collisions, parse errors, missing source files, shader package names given
//!   twice), links across mounts included. Fails on errors.
//! - `marlin-cook validate <project> --pre-submit`: only what the version control submit would
//!   contain, plus links into files left out of it or deleted by it; for every mount under
//!   version control (an installed engine is not).
//! - `marlin-cook install-hook <project>`: runs the pre-submit validation before every commit.
//! - `marlin-cook check <path>...`: parses `.masset` files without an index, for a quick look
//!   at a few files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use marlin_asset_db::{ConfigScope, Content, GAME_MOUNT, Options, Project, UpdateReport};
use marlin_cook_core::{
    CookOptions, Manifest, PackageOptions, Platform, Profile, cook_all, named_objects,
    publish_manifest, write_package,
};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_masset::{NodeKind, parse};

const USAGE: &str = "usage:\n  marlin-cook cook <project> [--platform <name>] [--threads <n>] [--store <addr>] [--shipping]\n  marlin-cook package <project> [--platform <name>] [--store <addr>] [--out <file>] [--head-codec <codec>] [--bulk-codec <codec>] [--level <1-9>]\n  marlin-cook index <project>\n  marlin-cook validate <project> [--pre-submit]\n  marlin-cook install-hook <project>\n  marlin-cook check <file-or-directory>...";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((command, rest)) if command == "cook" && !rest.is_empty() => cook(rest),
        Some((command, rest)) if command == "package" && !rest.is_empty() => package(rest),
        Some((command, rest)) if command == "stage" && !rest.is_empty() => stage(rest),
        Some((command, paths)) if command == "check" && !paths.is_empty() => check(paths),
        Some((command, [root])) if command == "index" => index(root, Validate::No),
        Some((command, [root])) if command == "validate" => index(root, Validate::All),
        Some((command, [root, flag])) if command == "validate" && flag == "--pre-submit" => {
            index(root, Validate::Submit)
        }
        Some((command, [root])) if command == "install-hook" => install_hook(root),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Validate {
    No,
    All,
    /// Only what the version control submit set touches.
    Submit,
}

/// The project at `path` and its content, indexes brought up to date and saved.
fn open(path: &str) -> Result<(Project, Content, UpdateReport), ExitCode> {
    let project = Project::open(path).map_err(|e| {
        eprintln!("{}", marlin_error_chain(&e));
        ExitCode::FAILURE
    })?;
    let (mut content, report) = project.open_content(&Options::default()).map_err(|e| {
        eprintln!("{}", marlin_error_chain(&e));
        ExitCode::FAILURE
    })?;
    if let Err(e) = content.save() {
        eprintln!("saving the index: {}", marlin_error_chain(&e));
        return Err(ExitCode::FAILURE);
    }
    Ok((project, content, report))
}

fn index(path: &str, validate: Validate) -> ExitCode {
    let start = Instant::now();
    let (project, mut content, report) = match open(path) {
        Ok(opened) => opened,
        Err(code) => return code,
    };
    println!(
        "{}: {} files, {} objects; {:?}: {} checked, {} read (+{} ~{} -{}) in {:.2?}",
        project.name,
        content.file_count(),
        content.object_count(),
        report.mode,
        report.checked,
        report.read,
        report.added,
        report.changed,
        report.removed,
        start.elapsed()
    );
    let issues = match validate {
        Validate::No => return ExitCode::SUCCESS,
        Validate::All => content.validate(),
        Validate::Submit => {
            let mut issues = Vec::new();
            for mount in content.mount_ids() {
                let root = content.root(mount).to_path_buf();
                let vcs = match marlin_vcs::open(&root) {
                    Ok(vcs) => vcs,
                    // An installed engine is not under version control: nothing is submitted
                    // there. The project's own content always is.
                    Err(_) if content.mount(mount).name != GAME_MOUNT => continue,
                    Err(e) => {
                        eprintln!("{}: {}", root.display(), marlin_error_chain(&e));
                        return ExitCode::FAILURE;
                    }
                };
                match content.validate_submit(mount, vcs.as_ref()) {
                    Ok(found) => issues.extend(found),
                    Err(e) => {
                        eprintln!("{}: {}", root.display(), marlin_error_chain(&e));
                        return ExitCode::FAILURE;
                    }
                }
            }
            issues
        }
    };
    let mut errors = 0;
    // Names code looks objects up by (shader packages) must be unique across the build.
    let (_, clashes) = named_objects(&content, &engine_cookers());
    for clash in &clashes {
        errors += 1;
        eprintln!("error: {clash}");
    }
    for issue in &issues {
        let kind = if issue.issue.kind.is_error() {
            errors += 1;
            "error"
        } else {
            "fixable"
        };
        eprintln!("{kind}: {}", content.describe(issue));
    }
    println!(
        "{} issue(s), {errors} error(s)",
        issues.len() + clashes.len()
    );
    if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `error: cause: root cause`.
fn marlin_error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(e) = source {
        text.push_str(": ");
        text.push_str(&e.to_string());
        source = e.source();
    }
    text
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            collect(&entry.path(), out)?;
        }
    } else if path.extension().is_some_and(|e| e == "masset") {
        out.push(path.to_owned());
    }
    Ok(())
}

fn links(kind: &NodeKind, out: &mut Vec<marlin_asset::Uuid>) {
    match kind {
        NodeKind::Link(link) => out.extend(link.uuid),
        NodeKind::Call(_, items) | NodeKind::Tuple(items) | NodeKind::List(items) => {
            items.iter().for_each(|n| links(&n.kind, out));
        }
        NodeKind::Record(_, attrs) => attrs.iter().for_each(|a| links(&a.value.kind, out)),
        _ => {}
    }
}

fn check(paths: &[String]) -> ExitCode {
    let mut files = Vec::new();
    for path in paths {
        if let Err(e) = collect(Path::new(path), &mut files) {
            eprintln!("{path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    let mut errors = 0usize;
    let mut defined = HashMap::new();
    let mut referenced = Vec::new();
    for file in &files {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("{}: {e}", file.display());
                errors += 1;
                continue;
            }
        };
        let doc = match parse(&text) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("{}:{e}", file.display());
                errors += 1;
                continue;
            }
        };
        for object in &doc.objects {
            let location = format!("{}:{}", file.display(), object.span.location(&text));
            if let Some(first) = defined.insert(object.uuid, location.clone()) {
                eprintln!("{location}: UUID {} is also used at {first}", object.uuid);
                errors += 1;
            }
            let mut targets = Vec::new();
            for attr in &object.attrs {
                links(&attr.value.kind, &mut targets);
            }
            referenced.extend(targets.into_iter().map(|t| (t, location.clone())));
        }
    }
    for (target, location) in referenced {
        if !defined.contains_key(&target) {
            eprintln!("{location}: link to {target}, which no checked file defines");
            errors += 1;
        }
    }
    println!(
        "checked {} files, {} objects: {errors} problem(s)",
        files.len(),
        defined.len()
    );
    if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Makes every commit in `root`'s workspace run `validate --pre-submit` first.
fn install_hook(path: &str) -> ExitCode {
    let project = match Project::open(path) {
        Ok(project) => project,
        Err(e) => {
            eprintln!("{}", marlin_error_chain(&e));
            return ExitCode::FAILURE;
        }
    };
    let root = &project.dir;
    let paths = std::env::current_exe().map(|exe| (exe, root.clone()));
    let (exe, root_abs) = match paths {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("{}: {e}", root.display());
            return ExitCode::FAILURE;
        }
    };
    // Hooks run under sh, which takes forward slashes everywhere (Git for Windows included).
    let unix = |p: &Path| {
        let s = p.display().to_string();
        s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/")
    };
    let command = format!(
        "\"{}\" validate \"{}\" --pre-submit",
        unix(&exe),
        unix(&root_abs)
    );
    match marlin_vcs::open(root).and_then(|vcs| vcs.install_pre_submit_hook(&command)) {
        Ok(()) => {
            println!("installed: {command}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {}", root.display(), marlin_error_chain(&e));
            ExitCode::FAILURE
        }
    }
}

fn cook(args: &[String]) -> ExitCode {
    let root = &args[0];
    let mut options = CookOptions::default();
    let mut store = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        if flag == "--shipping" {
            options.profile = Profile::Shipping;
            continue;
        }
        match (flag.as_str(), rest.next()) {
            ("--platform", Some(p)) => match Platform::find(p) {
                Ok(p) => options.platform = p,
                Err(e) => {
                    eprintln!("--platform: {e}");
                    return ExitCode::from(2);
                }
            },
            ("--store", Some(a)) => match a.parse() {
                Ok(a) => store = Some(a),
                Err(_) => {
                    eprintln!("--store: not an address: {a}");
                    return ExitCode::from(2);
                }
            },
            ("--threads", Some(n)) => match n.parse() {
                Ok(n) => options.threads = n,
                Err(_) => {
                    eprintln!("--threads: not a number: {n}");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let start = Instant::now();
    let (project, content, _) = match open(root) {
        Ok(opened) => opened,
        Err(code) => return code,
    };
    let ddc = match open_ddc(&project.ddc_dir(), store) {
        Ok(ddc) => ddc,
        Err(e) => {
            eprintln!("opening the cache: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cookers = engine_cookers();
    let (report, mut manifest) = cook_all(&content, &cookers, ddc.as_ref(), &options);
    manifest.config = match project.config_files(options.platform.name, ConfigScope::Build) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {}", marlin_error_chain(&e));
            return ExitCode::FAILURE;
        }
    };
    for unit in &report.units {
        for warning in &unit.warnings {
            eprintln!("warning: {}: {warning}", unit.path);
        }
    }
    for error in &report.errors {
        eprintln!("error: {}", marlin_error_chain(error));
    }
    let summary = format!(
        "{}: {} units ({} cooked, {} cached), {} failed",
        options.platform.name,
        report.units.len(),
        report.cooked(),
        report.cached(),
        report.errors.len(),
    );
    if !report.errors.is_empty() {
        // A partial build must not replace the last good manifest `package` builds from.
        println!("{summary}; manifest not published; {:.2?}", start.elapsed());
        return ExitCode::FAILURE;
    }
    let key = match publish_manifest(&project.build_dir(), ddc.as_ref(), &manifest) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("publishing the manifest: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{summary}; build {key:032x}; {:.2?}", start.elapsed());
    ExitCode::SUCCESS
}

fn codec(name: &str) -> Option<marlin_compress::Codec> {
    marlin_compress::Codec::ALL
        .into_iter()
        .find(|c| c.name() == name)
}

fn package(args: &[String]) -> ExitCode {
    // Packing reads the published build and the cache: no index needed.
    let project = match Project::open(&args[0]) {
        Ok(project) => project,
        Err(e) => {
            eprintln!("{}", marlin_error_chain(&e));
            return ExitCode::FAILURE;
        }
    };
    let mut platform = Platform::host();
    let mut out: Option<PathBuf> = None;
    // Codecs default to the platform's, which is known only after every flag.
    let (mut head_codec, mut bulk_codec, mut level) = (None, None, None);
    let mut store = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next();
        let ok = match (flag.as_str(), value) {
            ("--platform", Some(p)) => match Platform::find(p) {
                Ok(p) => {
                    platform = p;
                    true
                }
                Err(e) => {
                    eprintln!("--platform: {e}");
                    return ExitCode::from(2);
                }
            },
            ("--out", Some(p)) => {
                out = Some(PathBuf::from(p));
                true
            }
            ("--store", Some(a)) => a.parse().map(|a| store = Some(a)).is_ok(),
            ("--head-codec", Some(c)) => codec(c).map(|c| head_codec = Some(c)).is_some(),
            ("--bulk-codec", Some(c)) => codec(c).map(|c| bulk_codec = Some(c)).is_some(),
            ("--level", Some(n)) => n
                .parse()
                .map(|n| level = Some(marlin_compress::Level::new(n)))
                .is_ok(),
            _ => false,
        };
        if !ok {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let mut options = PackageOptions::for_platform(platform);
    options.head_codec = head_codec.unwrap_or(options.head_codec);
    options.bulk_codec = bulk_codec.unwrap_or(options.bulk_codec);
    options.level = level.unwrap_or(options.level);
    let platform = platform.name;
    let build = project.build_dir();
    let manifest_path = build.join(format!("{platform}.manifest"));
    let manifest = match std::fs::read(&manifest_path).map(|b| Manifest::from_bytes(&b)) {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => {
            eprintln!("{}: {e}", manifest_path.display());
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!(
                "{}: {e} (run `marlin-cook cook` first)",
                manifest_path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let ddc = match open_ddc(&project.ddc_dir(), store) {
        Ok(ddc) => ddc,
        Err(e) => {
            eprintln!("opening the cache: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = out.unwrap_or_else(|| build.join(format!("{platform}.mpak")));
    let start = Instant::now();
    match write_package(&manifest, ddc.as_ref(), &options, &out) {
        Ok(r) => {
            println!(
                "{}: {} units, {} chunks, {:.1} MiB raw -> {:.1} MiB ({:.0}%); {:.2?}",
                out.display(),
                r.units,
                r.chunks,
                r.raw_bytes as f64 / (1 << 20) as f64,
                r.file_bytes as f64 / (1 << 20) as f64,
                r.file_bytes as f64 * 100.0 / r.raw_bytes.max(1) as f64,
                start.elapsed()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", marlin_error_chain(&e));
            ExitCode::FAILURE
        }
    }
}

fn stage(args: &[String]) -> ExitCode {
    let project = match Project::open(&args[0]) {
        Ok(project) => project,
        Err(e) => {
            eprintln!("{}", marlin_error_chain(&e));
            return ExitCode::FAILURE;
        }
    };
    let mut platform = Platform::host();
    let mut game: Option<PathBuf> = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        match (flag.as_str(), rest.next()) {
            ("--platform", Some(p)) => match Platform::find(p) {
                Ok(p) => platform = p,
                Err(e) => {
                    eprintln!("--platform: {e}");
                    return ExitCode::from(2);
                }
            },
            ("--game", Some(p)) => game = Some(PathBuf::from(p)),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let package = project.build_dir().join(format!("{}.mpak", platform.name));
    if !package.is_file() {
        eprintln!(
            "{}: no package (run `marlin-cook cook` and `marlin-cook package` first)",
            package.display()
        );
        return ExitCode::FAILURE;
    }
    // The game executable every Marlin game shares, built for the platform: next to this tool
    // by default (cargo's target directory, or an installed engine's `bin/`).
    let game = match game {
        Some(game) => game,
        None => match std::env::current_exe() {
            Ok(exe) => exe.with_file_name(format!("marlin-game{}", std::env::consts::EXE_SUFFIX)),
            Err(e) => {
                eprintln!("finding this tool: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    if !game.is_file() {
        eprintln!(
            "{}: no game executable (build marlin-game, or name one with --game)",
            game.display()
        );
        return ExitCode::FAILURE;
    }
    let root = project.staged_dir(platform.name).join(&project.name);
    let mut exe_name = project.name.clone();
    if let Some(ext) = game.extension() {
        exe_name.push('.');
        exe_name.push_str(&ext.to_string_lossy());
    }
    let paks = root.join(marlin_asset::package::PAKS_DIR);
    // A fresh directory: nothing from an earlier stage (an old executable name, stale patches).
    let staged = (|| {
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&paks)?;
        std::fs::copy(&game, root.join(&exe_name))?;
        std::fs::copy(&package, paks.join(marlin_asset::package::BASE_PACKAGE))?;
        Ok::<_, std::io::Error>(())
    })();
    match staged {
        Ok(()) => {
            println!("{}", root.join(&exe_name).display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", root.display());
            ExitCode::FAILURE
        }
    }
}
