//! Command-line asset cooker (`docs/design/0003-asset-pipeline.md`).
//!
//! - `marlin-cook cook <root> [--platform <name>] [--threads <n>] [--store <addr>]`: brings the index up to date,
//!   cooks every unit whose inputs changed (the rest come from the local cache in
//!   `<root>/.marlin/ddc`) and publishes the build manifest to
//!   `<root>/.marlin/build/<platform>.manifest`. Fails if any unit fails.
//! - `marlin-cook package <root> [--platform <name>] [--store <addr>] [--out <file>] [--head-codec <codec>]
//!   [--bulk-codec <codec>] [--level <1-9>]`: packs the last cooked build of the platform into a
//!   `.mpak` (default `<root>/.marlin/build/<platform>.mpak`). Codecs: none, lz4, zstd,
//!   gdeflate.
//! - `marlin-cook index <root>`: brings the asset index of `root` up to date and saves it.
//! - `marlin-cook validate <root>`: the same, then reports content problems (dangling and stale
//!   links, UUID collisions, parse errors, missing source files). Fails on errors.
//! - `marlin-cook validate <root> --pre-submit`: only what the version control submit would
//!   contain, plus links into files left out of it or deleted by it.
//! - `marlin-cook install-hook <root>`: runs the pre-submit validation before every commit.
//! - `marlin-cook check <path>...`: parses `.masset` files without an index, for a quick look
//!   at a few files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use marlin_asset_db::{AssetDb, Options};
use marlin_cook_core::{
    CookOptions, Manifest, PackageOptions, cook_all, publish_manifest, write_package,
};
use marlin_cooker::{engine_cookers, open_ddc};
use marlin_masset::{NodeKind, parse};

const USAGE: &str = "usage:\n  marlin-cook cook <root> [--platform <name>] [--threads <n>] [--store <addr>]\n  marlin-cook package <root> [--platform <name>] [--store <addr>] [--out <file>] [--head-codec <codec>] [--bulk-codec <codec>] [--level <1-9>]\n  marlin-cook index <root>\n  marlin-cook validate <root> [--pre-submit]\n  marlin-cook install-hook <root>\n  marlin-cook check <file-or-directory>...";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((command, rest)) if command == "cook" && !rest.is_empty() => cook(rest),
        Some((command, rest)) if command == "package" && !rest.is_empty() => package(rest),
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

fn index(root: &str, validate: Validate) -> ExitCode {
    let start = Instant::now();
    let (mut db, report) = match AssetDb::open(root, Options::default()) {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("{root}: {}", marlin_error_chain(&e));
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = db.save() {
        eprintln!("saving the index: {}", marlin_error_chain(&e));
        return ExitCode::FAILURE;
    }
    println!(
        "{} files, {} objects; {:?}: {} checked, {} read (+{} ~{} -{}) in {:.2?}",
        db.file_count(),
        db.object_count(),
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
        Validate::All => db.validate(),
        Validate::Submit => {
            match marlin_vcs::open(root).and_then(|vcs| db.validate_submit(vcs.as_ref())) {
                Ok(issues) => issues,
                Err(e) => {
                    eprintln!("{root}: {}", marlin_error_chain(&e));
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    let mut errors = 0;
    for issue in &issues {
        let kind = if issue.kind.is_error() {
            errors += 1;
            "error"
        } else {
            "fixable"
        };
        eprintln!("{kind}: {}", issue.describe(&db));
    }
    println!("{} issue(s), {errors} error(s)", issues.len());
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
fn install_hook(root: &str) -> ExitCode {
    let paths = std::env::current_exe().and_then(|exe| Ok((exe, std::fs::canonicalize(root)?)));
    let (exe, root_abs) = match paths {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("{root}: {e}");
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
            eprintln!("{root}: {}", marlin_error_chain(&e));
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
        match (flag.as_str(), rest.next()) {
            ("--platform", Some(p)) => options.platform = p.clone(),
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
    let (mut db, _) = match AssetDb::open(root, Options::default()) {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("{root}: {}", marlin_error_chain(&e));
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = db.save() {
        eprintln!("saving the index: {}", marlin_error_chain(&e));
        return ExitCode::FAILURE;
    }
    let ddc = match open_ddc(Path::new(root), store) {
        Ok(ddc) => ddc,
        Err(e) => {
            eprintln!("opening the cache: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cookers = engine_cookers();
    let (report, manifest) = cook_all(&db, &cookers, ddc.as_ref(), &options);
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
        options.platform,
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
    let key = match publish_manifest(Path::new(root), ddc.as_ref(), &manifest) {
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
    let root = Path::new(&args[0]);
    let mut platform = CookOptions::default().platform;
    let mut out: Option<PathBuf> = None;
    let mut options = PackageOptions::default();
    let mut store = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next();
        let ok = match (flag.as_str(), value) {
            ("--platform", Some(p)) => {
                platform = p.clone();
                true
            }
            ("--out", Some(p)) => {
                out = Some(PathBuf::from(p));
                true
            }
            ("--store", Some(a)) => a.parse().map(|a| store = Some(a)).is_ok(),
            ("--head-codec", Some(c)) => codec(c).map(|c| options.head_codec = c).is_some(),
            ("--bulk-codec", Some(c)) => codec(c).map(|c| options.bulk_codec = c).is_some(),
            ("--level", Some(n)) => n
                .parse()
                .map(|n| options.level = marlin_compress::Level::new(n))
                .is_ok(),
            _ => false,
        };
        if !ok {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let build = root.join(".marlin").join("build");
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
    let ddc = match open_ddc(root, store) {
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
