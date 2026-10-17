//! Startup: the boot configuration and where content comes from (0003 §9).
//!
//! A game starts from `boot.toml`, cvars in TOML (`[content] package = "game.mpak"` sets
//! `content.package`), overridable on the command line (`+content.cookd 127.0.0.1:47200`):
//!
//! | cvar              | meaning                                                          |
//! |-------------------|------------------------------------------------------------------|
//! | `content.package` | package(s) to mount, `;`-separated, relative to the boot file    |
//! | `content.cookd`   | cook service address; mounted above the packages (dev streaming)  |
//! | `game.level`      | UUID of the entry level                                          |
//! | `game.platform`   | the platform cookd cooks for                                     |
//! | `r.vsync`, `window.width`, `window.height` | presentation                            |
//!
//! There is no load-by-path: the entry level is named by UUID, and everything else is reached
//! through its links.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddr;
use core::time::Duration;

use marlin_asset::{DevSource, Package, Uuid, Vfs};
use marlin_config::{CvarBool, CvarI64, CvarString, Source};
use marlin_sync::Once;

pub static CONTENT_PACKAGE: CvarString = CvarString::new(
    "content.package",
    "",
    "Packages to mount, ';'-separated, relative to the boot file.",
);
pub static CONTENT_COOKD: CvarString = CvarString::new(
    "content.cookd",
    "",
    "Cook service address (host:port) to stream content from, above the packages.",
);
pub static GAME_LEVEL: CvarString = CvarString::new("game.level", "", "UUID of the entry level.");
pub static GAME_PLATFORM: CvarString = CvarString::new(
    "game.platform",
    "windows",
    "Platform the cook service cooks for.",
);
pub static R_VSYNC: CvarBool = CvarBool::new("r.vsync", true, "Wait for vertical blank.");
pub static WINDOW_WIDTH: CvarI64 =
    CvarI64::new("window.width", 1280, "Window width.").range(64, 16384);
pub static WINDOW_HEIGHT: CvarI64 =
    CvarI64::new("window.height", 720, "Window height.").range(64, 16384);

static REGISTER: Once = Once::new();

/// Registers the framework's cvars (once per process).
pub fn register_cvars() {
    REGISTER.call_once(|| {
        let _ = marlin_config::register_all(&[
            &CONTENT_PACKAGE,
            &CONTENT_COOKD,
            &GAME_LEVEL,
            &GAME_PLATFORM,
            &R_VSYNC,
            &WINDOW_WIDTH,
            &WINDOW_HEIGHT,
        ]);
    });
}

#[derive(Debug, marlin_error::Error)]
pub enum BootError {
    #[error("{0}")]
    Config(String),
    #[error("mounting {path}")]
    Package {
        path: String,
        #[source]
        source: marlin_asset::SourceError,
    },
    #[error("connecting to the cook service at {addr}")]
    Cookd {
        addr: String,
        #[source]
        source: marlin_asset::SourceError,
    },
    #[error("no content: set content.package or content.cookd")]
    NoContent,
    #[error("game.level is not a UUID: {0:?}")]
    Level(String),
}

/// What a game starts with: mounted content and the entry level.
pub struct Boot {
    pub vfs: Vfs,
    pub level: Uuid,
}

/// Applies `config` (the boot file's text) and `args` (command-line overrides), then mounts
/// the content. Package paths are relative to `base_dir`, the boot file's directory.
pub fn boot<'a>(
    config: &str,
    base_dir: &str,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<Boot, BootError> {
    register_cvars();
    let mut problems: Vec<String> = marlin_config::apply_config(config, Source::Project)
        .into_iter()
        .map(|e| format!("boot file: {e}"))
        .collect();
    problems.extend(
        marlin_config::apply_command_line(args)
            .into_iter()
            .map(|e| format!("command line: {e}")),
    );
    if !problems.is_empty() {
        return Err(BootError::Config(problems.join("; ")));
    }
    mount(&ContentSettings::from_cvars(base_dir)?)
}

/// Where content comes from: what the cvars say, as plain data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContentSettings {
    /// Package paths, mounted in order.
    pub packages: Vec<String>,
    /// The cook service, mounted above the packages.
    pub cookd: Option<SocketAddr>,
    pub platform: String,
    pub level: Uuid,
}

impl ContentSettings {
    /// From the `content.*` and `game.*` cvars; package paths resolved against `base_dir`.
    pub fn from_cvars(base_dir: &str) -> Result<Self, BootError> {
        let level = GAME_LEVEL.get();
        let level: Uuid = level
            .trim()
            .parse()
            .map_err(|_| BootError::Level(level.clone()))?;
        let packages = CONTENT_PACKAGE
            .get()
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|path| {
                let absolute = path.starts_with('/') || path.get(1..2) == Some(":");
                if absolute || base_dir.is_empty() {
                    String::from(path)
                } else {
                    format!("{}/{path}", base_dir.trim_end_matches(['/', '\\']))
                }
            })
            .collect();
        let cookd = CONTENT_COOKD.get();
        let cookd = match cookd.trim() {
            "" => None,
            addr => Some(addr.parse().map_err(|_| BootError::Cookd {
                addr: String::from(addr),
                source: marlin_asset::SourceError::Remote("not an address".into()),
            })?),
        };
        Ok(Self {
            packages,
            cookd,
            platform: GAME_PLATFORM.get(),
            level,
        })
    }
}

/// Mounts `settings`' content: the packages, then the cook service above them.
pub fn mount(settings: &ContentSettings) -> Result<Boot, BootError> {
    let mut vfs = Vfs::new();
    for path in &settings.packages {
        let package = Package::open(path.as_str()).map_err(|source| BootError::Package {
            path: path.clone(),
            source,
        })?;
        vfs.mount(path.clone(), package);
    }
    if let Some(addr) = settings.cookd {
        let source = DevSource::connect(addr, &settings.platform, Duration::from_secs(10))
            .map_err(|source| BootError::Cookd {
                addr: format!("{addr}"),
                source,
            })?;
        vfs.mount("cookd", source);
    }
    if vfs.layer_names().next().is_none() {
        return Err(BootError::NoContent);
    }
    Ok(Boot {
        vfs,
        level: settings.level,
    })
}
