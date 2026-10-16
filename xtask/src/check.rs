//! `cargo xtask check-layers`: enforces the dependency rules of docs/design/0001-repo-layout.md.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{Result, cargo};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
}

/// Ordered bottom to top: a crate may depend only on its own layer or the ones below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Base,
    Platform,
    Core,
    Engine,
    Umbrella,
    Tools,
    Editor,
    Apps,
}

impl Layer {
    fn of(relative_dir: &Path) -> Option<Self> {
        let mut parts = relative_dir.components().map(|c| c.as_os_str().to_str().unwrap_or(""));
        Some(match (parts.next()?, parts.next()) {
            ("runtime", Some("base")) => Self::Base,
            ("runtime", Some("platform")) => Self::Platform,
            ("runtime", Some("core")) => Self::Core,
            ("runtime", Some("engine")) => Self::Engine,
            ("runtime", Some("marlin")) => Self::Umbrella,
            ("tools", _) => Self::Tools,
            ("editor", _) => Self::Editor,
            ("apps", _) => Self::Apps,
            _ => return None,
        })
    }

    fn is_runtime(self) -> bool {
        self <= Self::Umbrella
    }
}

pub fn layers(root: &Path) -> Result {
    let output = cargo()
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&output.stderr)).into());
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)?;

    let mut layers = HashMap::new();
    for package in &metadata.packages {
        let dir = package.manifest_path.parent().unwrap();
        if let Some(layer) = dir.strip_prefix(&metadata.workspace_root).ok().and_then(Layer::of) {
            layers.insert(package.name.as_str(), layer);
        }
    }

    let mut errors = Vec::new();
    for package in &metadata.packages {
        let Some(&layer) = layers.get(package.name.as_str()) else { continue };

        // Tests may use anything; only normal and build dependencies count.
        for dependency in package.dependencies.iter().filter(|d| d.kind.as_deref() != Some("dev")) {
            let Some(&dependency_layer) = layers.get(dependency.name.as_str()) else { continue };
            if dependency.name == "marlin-pal" && !matches!(layer, Layer::Base | Layer::Platform) {
                errors.push(format!(
                    "{}: only runtime/base and runtime/platform crates may depend on marlin-pal",
                    package.name
                ));
            } else if dependency_layer > layer {
                errors.push(format!(
                    "{} ({layer:?}) must not depend on {} ({dependency_layer:?})",
                    package.name, dependency.name
                ));
            }
        }

        if layer.is_runtime() {
            for lib in package.targets.iter().filter(|t| t.kind.iter().any(|k| k == "lib")) {
                if !fs::read_to_string(&lib.src_path)?.contains("no_std") {
                    errors.push(format!("{}: runtime crates must be no_std", package.name));
                }
            }
        }
    }

    if errors.is_empty() {
        println!("check-layers: {} crates OK", layers.len());
        return Ok(());
    }
    for error in &errors {
        eprintln!("  {error}");
    }
    Err(format!("{} layering violation(s)", errors.len()).into())
}
