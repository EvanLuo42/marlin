//! Platform manifests: `platforms/<name>/platform.toml`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Result;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Platform {
    pub name: String,
    pub family: Family,
    /// Rust target triples; the first is the default.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Capability extensions; each becomes `--cfg marlin_ext="<name>"`.
    #[serde(default)]
    pub ext: Vec<String>,
    /// Custom target spec JSON, relative to the platform directory (consoles).
    pub target_spec: Option<PathBuf>,
    /// Build core and alloc from source with `-Zbuild-std`.
    #[serde(default)]
    pub build_std: bool,
    /// Minimum CPU per architecture (the triple's first component), e.g. `x86_64 = "x86-64-v3"`;
    /// becomes `-Ctarget-cpu`.
    #[serde(default)]
    pub target_cpu: BTreeMap<String, String>,
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Desktop,
    Console,
}

/// Every platform with a manifest under `platforms/`, sorted by name.
pub fn load_all(root: &Path) -> Result<Vec<Platform>> {
    let mut platforms = Vec::new();
    for entry in fs::read_dir(root.join("platforms"))? {
        let dir = entry?.path();
        let manifest = dir.join("platform.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = fs::read_to_string(&manifest)?;
        let mut platform: Platform =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        if dir.file_name() != Some(OsStr::new(&platform.name)) {
            return Err(format!(
                "{}: name `{}` does not match its directory",
                manifest.display(),
                platform.name
            )
            .into());
        }
        if platform.targets.is_empty() && platform.target_spec.is_none() {
            return Err(format!("{}: needs `targets` or `target-spec`", manifest.display()).into());
        }
        platform.dir = dir;
        platforms.push(platform);
    }
    platforms.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(platforms)
}

pub fn find(root: &Path, name: &str) -> Result<Platform> {
    load_all(root)?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| {
            format!(
                "unknown platform `{name}`: no platforms/{name}/platform.toml \
             (console backends must be cloned first, see platforms/README.md)"
            )
            .into()
        })
}

/// The desktop platform xtask itself runs on.
pub fn host() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

pub fn list(root: &Path) -> Result {
    for p in load_all(root)? {
        let family = match p.family {
            Family::Desktop => "desktop",
            Family::Console => "console",
        };
        let ext = if p.ext.is_empty() {
            "-".to_owned()
        } else {
            p.ext.join(", ")
        };
        println!(
            "{:<10} {:<8} targets: {}  ext: {}",
            p.name,
            family,
            p.targets.join(", "),
            ext
        );
    }
    Ok(())
}

impl Platform {
    /// The minimum CPU for `target`, a triple or a target-spec path.
    pub fn target_cpu(&self, target: &str) -> Option<&str> {
        let name = Path::new(target).file_stem()?.to_str()?;
        let arch = name.split('-').next()?;
        self.target_cpu.get(arch).map(String::as_str)
    }
}
