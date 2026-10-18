//! The compiler's own source hash, for the cook fingerprint (0005 §8): xxh3-128 over every
//! `Cargo.toml`, Rust file and `.mshader` file (the core library) of the crates under
//! `compiler/`, outside their tests, by their paths relative to it, with line endings made
//! `\n`. Any change to the compiler's code or its core library changes it.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let compiler = manifest.join("../..");
    println!("cargo::rerun-if-changed={}", compiler.display());
    let mut files = Vec::new();
    walk(&compiler, &compiler, &mut files);
    files.sort();
    let mut all = Vec::new();
    for (rel, path) in &files {
        let text = fs::read(path).unwrap();
        let text: Vec<u8> = String::from_utf8_lossy(&text)
            .replace("\r\n", "\n")
            .into_bytes();
        all.extend(rel.as_bytes());
        all.push(0);
        all.extend((text.len() as u64).to_le_bytes());
        all.extend(text);
    }
    let hash = marlin_hash::xxh3_128(&all);
    println!("cargo::rustc-env=MARLIN_SHADERC_SOURCE_HASH={hash:032x}");
}

/// The crates' manifests and sources under `dir`, as (path relative to `root`, path).
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            // Tests and their data do not change what the compiler does.
            if !matches!(name.as_str(), "tests" | "target" | "benches") {
                walk(root, &path, out);
            }
        } else if name == "Cargo.toml" || name.ends_with(".rs") || name.ends_with(".mshader") {
            let rel = path.strip_prefix(root).unwrap();
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, path));
        }
    }
}
