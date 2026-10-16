//! Command-line asset cooker.
//!
//! So far only `marlin-cook check <path>...`: parses every `.masset` file under the paths and
//! reports syntax errors, UUIDs used by two objects, and links to objects none of the checked
//! files define. Cooking arrives with the rest of `docs/design/0003-asset-pipeline.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use marlin_cook_audio as _;
use marlin_cook_core as _;
use marlin_cook_level as _;
use marlin_cook_mesh as _;
use marlin_cook_script as _;
use marlin_cook_shader as _;
use marlin_cook_texture as _;
use marlin_masset::{NodeKind, parse};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.split_first() {
        Some((command, paths)) if command == "check" && !paths.is_empty() => check(paths),
        _ => {
            eprintln!("usage: marlin-cook check <file-or-directory>...");
            ExitCode::from(2)
        }
    }
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
