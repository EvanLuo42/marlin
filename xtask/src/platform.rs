//! Platform manifests: `platforms/<name>/platform.toml`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use toml::de::{DeTable, DeValue};

use crate::Result;

/// Read with toml's serde-free `DeTable`: toml is shared with marlin-config, and any feature
/// enabled here (`std`, `serde`) would reach the no_std game binary through feature unification.
#[derive(Debug)]
pub struct Platform {
    pub name: String,
    pub family: Family,
    /// Rust target triples; the first is the default.
    pub targets: Vec<String>,
    /// Capability extensions; each becomes `--cfg marlin_ext="<name>"`.
    pub ext: Vec<String>,
    /// Custom target spec JSON, relative to the platform directory (consoles).
    pub target_spec: Option<PathBuf>,
    /// Build core and alloc from source with `-Zbuild-std`.
    pub build_std: bool,
    /// Minimum CPU per architecture (the triple's first component), e.g. `x86_64 = "x86-64-v3"`;
    /// becomes `-Ctarget-cpu`.
    pub target_cpu: BTreeMap<String, String>,
    pub dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Desktop,
    Console,
}

fn string(value: &DeValue, key: &str) -> std::result::Result<String, String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("`{key}` must be a string"))
}

fn strings(value: &DeValue, key: &str) -> std::result::Result<Vec<String>, String> {
    let array = value
        .as_array()
        .ok_or_else(|| format!("`{key}` must be an array of strings"))?;
    array.iter().map(|v| string(v.get_ref(), key)).collect()
}

impl Platform {
    /// Parses a manifest; unknown keys are errors.
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let table = DeTable::parse(text).map_err(|e| e.to_string())?;
        let (mut name, mut family) = (None, None);
        let mut platform = Platform {
            name: String::new(),
            family: Family::Desktop,
            targets: Vec::new(),
            ext: Vec::new(),
            target_spec: None,
            build_std: false,
            target_cpu: BTreeMap::new(),
            dir: PathBuf::new(),
        };
        for (key, value) in table.get_ref().iter() {
            let (key, value) = (key.get_ref().as_ref(), value.get_ref());
            match key {
                "name" => name = Some(string(value, key)?),
                "family" => {
                    family = Some(match string(value, key)?.as_str() {
                        "desktop" => Family::Desktop,
                        "console" => Family::Console,
                        other => return Err(format!("unknown family `{other}`")),
                    })
                }
                "targets" => platform.targets = strings(value, key)?,
                "ext" => platform.ext = strings(value, key)?,
                "target-spec" => platform.target_spec = Some(string(value, key)?.into()),
                "build-std" => {
                    platform.build_std = value
                        .as_bool()
                        .ok_or_else(|| format!("`{key}` must be a boolean"))?
                }
                "target-cpu" => {
                    let cpus = value
                        .as_table()
                        .ok_or_else(|| format!("`{key}` must be a table"))?;
                    for (arch, cpu) in cpus.iter() {
                        let arch = arch.get_ref().to_string();
                        let cpu = string(cpu.get_ref(), key)?;
                        platform.target_cpu.insert(arch, cpu);
                    }
                }
                other => return Err(format!("unknown field `{other}`")),
            }
        }
        platform.name = name.ok_or("missing field `name`")?;
        platform.family = family.ok_or("missing field `family`")?;
        Ok(platform)
    }
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
            Platform::parse(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
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
