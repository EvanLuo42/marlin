//! Startup: the boot configuration and where content comes from (0003 §9).
//!
//! A game starts from `boot.toml`, cvars in TOML (`[content] package = "game.mpak"` sets
//! `content.package`), overridable on the command line (`+content.cookd 127.0.0.1:47200`):
//!
//! | cvar              | meaning                                                          |
//! |-------------------|------------------------------------------------------------------|
//! | `content.package` | package(s) to mount in order, `;`-separated, relative to the boot  |
//! |                   | file (the build, then patches); `{platform}` stands for            |
//! |                   | `game.platform`                                                  |
//! | `content.cookd`   | cook service address; mounted above the packages (dev streaming)* |
//! | `game.level`      | UUID of the entry level                                          |
//! | `game.platform`   | the platform cookd cooks for; defaults to the build's platform*  |
//! | `r.vsync`, `window.width`, `window.height` | presentation                            |
//! | `r.stats`         | the stats overlay (F3 toggles it)*                               |
//! | `r.sharc.view`    | draw the world radiance cache in place of the lit scene*        |
//! | `game.device_profile` | the device profile applied at boot (default: the platform) |
//! | `sg.*`, `r.*`     | quality groups and the cvars they set ([`crate::quality`])       |
//!
//! Those marked * are development tools: not in shipping builds (`marlin_build = "shipping"`),
//! where these cvars do not exist and the cook service cannot be mounted (0001 §8).
//!
//! After mounting, the boot applies the quality presets the content holds
//! ([`crate::scalability::apply_presets`]).
//!
//! There is no load-by-path: the entry level is named by UUID, and everything else is reached
//! through its links, except the few objects code asks for by name (the engine's shader package,
//! `Marlin.Engine`). A build holds the engine's content with the game's (0003 §4.5): the base
//! package has both, and there is no separate engine package to mount.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddr;

use marlin_asset::cooked::platform_id;
use marlin_asset::{Package, Uuid, Vfs};
use marlin_config::{CvarBool, CvarI64, CvarString, Source};
use marlin_sync::Once;

pub static CONTENT_PACKAGE: CvarString = CvarString::new(
    "content.package",
    "",
    "Packages to mount, ';'-separated, relative to the boot file.",
);
#[cfg(not(marlin_build = "shipping"))]
pub static CONTENT_COOKD: CvarString = CvarString::new(
    "content.cookd",
    "",
    "Cook service address (host:port) to stream content from, above the packages.",
);
pub static GAME_LEVEL: CvarString = CvarString::new("game.level", "", "UUID of the entry level.");
#[cfg(not(marlin_build = "shipping"))]
pub static GAME_PLATFORM: CvarString = CvarString::new(
    "game.platform",
    marlin_rt::PLATFORM,
    "Platform the cook service cooks for; defaults to the one this build targets.",
);
pub static R_VSYNC: CvarBool = CvarBool::new("r.vsync", true, "Wait for vertical blank.");
#[cfg(not(marlin_build = "shipping"))]
pub static R_STATS: CvarBool = CvarBool::new(
    "r.stats",
    false,
    "Show the stats overlay: frame rate, CPU and GPU times, the costliest passes (F3 toggles).",
);
#[cfg(not(marlin_build = "shipping"))]
pub static R_SHARC_VIEW: CvarBool = CvarBool::new(
    "r.sharc.view",
    false,
    "Draw the world radiance cache (the radiance each surface's voxel holds) in place of the lit scene.",
);
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
            &GAME_LEVEL,
            &R_VSYNC,
            &WINDOW_WIDTH,
            &WINDOW_HEIGHT,
            &crate::scalability::GAME_DEVICE_PROFILE,
        ]);
        crate::quality::register_cvars();
        // The development tools'.
        #[cfg(not(marlin_build = "shipping"))]
        let _ =
            marlin_config::register_all(&[&CONTENT_COOKD, &GAME_PLATFORM, &R_STATS, &R_SHARC_VIEW]);
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
    #[error("{path} is cooked for another platform than {platform}")]
    WrongPlatform { path: String, platform: String },
    #[error("connecting to the cook service at {addr}")]
    Cookd {
        addr: String,
        #[source]
        source: marlin_asset::SourceError,
    },
    #[error("no content: set content.package")]
    NoContent,
    #[error("the cook service is a development tool: shipping builds read packages only")]
    NoCookd,
    #[error("game.level is not a UUID: {0:?}")]
    Level(String),
    #[error("quality presets")]
    Presets(#[source] crate::scalability::PresetError),
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
    let boot = mount(&ContentSettings::from_cvars(base_dir)?)?;
    // The quality presets: scalability tables and the device profile (0008 §2.2).
    crate::scalability::apply_presets(&boot.vfs).map_err(BootError::Presets)?;
    Ok(boot)
}

/// Where content comes from: what the cvars say, as plain data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContentSettings {
    /// Package paths, mounted in order: the build (engine and game content together), then
    /// patches above it.
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
        #[cfg(not(marlin_build = "shipping"))]
        let platform = GAME_PLATFORM.get();
        #[cfg(marlin_build = "shipping")]
        let platform = String::from(marlin_rt::PLATFORM);
        let packages = CONTENT_PACKAGE
            .get()
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|path| path.replace("{platform}", &platform))
            .map(|path| {
                let absolute = path.starts_with('/') || path.get(1..2) == Some(":");
                if absolute || base_dir.is_empty() {
                    path
                } else {
                    format!("{}/{path}", base_dir.trim_end_matches(['/', '\\']))
                }
            })
            .collect();
        #[cfg(not(marlin_build = "shipping"))]
        let cookd = CONTENT_COOKD.get();
        #[cfg(marlin_build = "shipping")]
        let cookd = String::new();
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
            platform,
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
        if package.index().header.platform != platform_id(&settings.platform) {
            return Err(BootError::WrongPlatform {
                path: path.clone(),
                platform: settings.platform.clone(),
            });
        }
        vfs.mount(path.clone(), package);
    }
    #[cfg(marlin_build = "shipping")]
    if settings.cookd.is_some() {
        return Err(BootError::NoCookd);
    }
    #[cfg(not(marlin_build = "shipping"))]
    if let Some(addr) = settings.cookd {
        let source = marlin_asset::DevSource::connect(
            addr,
            &settings.platform,
            core::time::Duration::from_secs(10),
        )
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
