//! Command-line asset cooker. Cooking arrives with the rest of
//! `docs/design/0003-asset-pipeline.md`; so far:
//!
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
use marlin_cook_audio as _;
use marlin_cook_core as _;
use marlin_cook_level as _;
use marlin_cook_mesh as _;
use marlin_cook_script as _;
use marlin_cook_shader as _;
use marlin_cook_texture as _;
use marlin_masset::{NodeKind, parse};

const USAGE: &str = "usage:\n  marlin-cook index <root>\n  marlin-cook validate <root>\n  marlin-cook check <file-or-directory>...";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
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
