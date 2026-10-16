//! The global cfgs: `marlin_platform`, `marlin_ext` and `marlin_build`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::Result;
use crate::platform::{self, Family, Platform};

#[derive(Clone, Copy, Debug)]
pub enum Config {
    Debug,
    Development,
    Profile,
    Shipping,
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "debug" => Self::Debug,
            "development" => Self::Development,
            "profile" => Self::Profile,
            "shipping" => Self::Shipping,
            _ => return Err(format!("unknown config `{s}`").into()),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Development => "development",
            Self::Profile => "profile",
            Self::Shipping => "shipping",
        }
    }

    pub fn cargo_profile(self) -> &'static str {
        match self {
            Self::Debug => "dev",
            other => other.name(),
        }
    }
}

/// rustc flags selecting the platform, its extensions and (for xtask builds) the configuration.
pub fn flags(platform: &Platform, config: Option<Config>) -> Vec<String> {
    let mut cfgs = vec![format!("marlin_platform=\"{}\"", platform.name)];
    cfgs.extend(platform.ext.iter().map(|ext| format!("marlin_ext=\"{ext}\"")));
    cfgs.extend(config.map(|c| format!("marlin_build=\"{}\"", c.name())));
    cfgs.into_iter().flat_map(|cfg| ["--cfg".to_owned(), cfg]).collect()
}

const BEGIN: &str = "# BEGIN gen-cfg";
const END: &str = "# END gen-cfg";

/// Rewrites the generated block of `.cargo/config.toml` so plain `cargo build`/`check`/`test` and
/// rust-analyzer see the right cfgs on desktop hosts. Consoles always build through
/// `cargo xtask build`: Xbox shares its target triple with Windows.
pub fn gen_cfg(root: &Path) -> Result {
    let path = root.join(".cargo").join("config.toml");
    let text = fs::read_to_string(&path)?;
    let (Some(begin), Some(end)) = (text.find(BEGIN), text.find(END)) else {
        return Err(format!("{}: missing `{BEGIN}` / `{END}` markers", path.display()).into());
    };
    let block_start = begin + text[begin..].find('\n').map_or(text.len() - begin, |i| i + 1);

    let mut block = String::new();
    let mut owners: HashMap<&str, &str> = HashMap::new();
    let platforms = platform::load_all(root)?;
    for p in platforms.iter().filter(|p| p.family == Family::Desktop) {
        let flags: Vec<String> = flags(p, None).iter().map(|f| toml_string(f)).collect();
        let flags = flags.join(", ");
        for target in &p.targets {
            if let Some(other) = owners.insert(target, &p.name) {
                return Err(format!("target `{target}` is claimed by both {other} and {}", p.name).into());
            }
            // rustdoc (doctests) does not see rustflags.
            writeln!(block, "[target.{target}]")?;
            writeln!(block, "rustflags = [{flags}]")?;
            writeln!(block, "rustdocflags = [{flags}]")?;
        }
    }

    let updated = format!("{}{}{}", &text[..block_start], block, &text[end..]);
    if updated == text {
        println!(".cargo/config.toml is up to date");
    } else {
        fs::write(&path, updated)?;
        println!("updated .cargo/config.toml");
    }
    Ok(())
}

fn toml_string(s: &str) -> String {
    // Literal strings avoid escaping the quotes inside cfg values.
    if s.contains('"') { format!("'{s}'") } else { format!("\"{s}\"") }
}
