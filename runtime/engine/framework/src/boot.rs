//! Startup: where content and config come from (0003 §4.6).
//!
//! A game runs from the directory `marlin-cook stage` lays out, and from nothing else: its
//! executable and, next to it, `content/paks/` holding `base.mpak` (the build) and patches
//! (`*_p.mpak`, mounted above it in file name order). [`boot`] mounts them, applies the config
//! files cooked into them (the engine's and the project's `config/`, each package's above the
//! last), then the command line (`+cvar value`, the only arguments a game takes), and finds the
//! entry level:
//!
//! | cvar                  | meaning                                                          |
//! |-----------------------|------------------------------------------------------------------|
//! | `game.default_level`  | the entry level: a link (`</Game/levels/main:Main #<uuid>>`, the |
//! |                       | UUID decides) or a UUID                                          |
//! | `r.vsync`, `window.width`, `window.height` | presentation                                |
//! | `game.device_profile` | the device profile applied at boot (default: the platform)       |
//! | `sg.*`, `r.*`         | quality groups and the cvars they set (`marlin_renderer::settings`) |
//!
//! A config file's problems (a cvar this build does not have, a bad value) are warnings: a
//! shipping build lacks the development cvars, and the same config serves both. The command
//! line's are errors. After mounting, the boot applies the quality presets the content holds
//! ([`crate::scalability::apply_presets`]).
//!
//! The editor and tests mount content themselves ([`mount`]): packages, and in development
//! builds the cook service above them (0001 §8). A game never connects to the cook service.
//!
//! There is no load-by-path: the entry level is named by UUID, and everything else is reached
//! through its links, except the few objects code asks for by name (the engine's shader package,
//! `Marlin.Engine`). A build holds the engine's content with the game's (0003 §4.5).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddr;

use marlin_asset::cooked::platform_id;
use marlin_asset::package::{BASE_PACKAGE, ConfigFile, ConfigLayer, PAKS_DIR, PATCH_SUFFIX};
use marlin_asset::{Package, Uuid, Vfs};
use marlin_config::{CvarBool, CvarI64, CvarString, Source};
use marlin_sync::Once;

pub static GAME_DEFAULT_LEVEL: CvarString = CvarString::new(
    "game.default_level",
    "",
    "The entry level: a link (</Game/levels/main:Main #<uuid>>) or a UUID.",
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
            &GAME_DEFAULT_LEVEL,
            &R_VSYNC,
            &WINDOW_WIDTH,
            &WINDOW_HEIGHT,
            &crate::scalability::GAME_DEVICE_PROFILE,
        ]);
    });
}

#[derive(Debug, marlin_error::Error)]
pub enum BootError {
    #[error("command line: {0}")]
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
    #[error("no content: {0}")]
    NoContent(String),
    #[error("the cook service is a development tool: shipping builds read packages only")]
    NoCookd,
    #[error("game.default_level is not a link or a UUID: {0:?}")]
    Level(String),
    #[error("no entry level: set game.default_level in the project's config/game.toml")]
    NoLevel,
    #[error("quality presets")]
    Presets(#[source] crate::scalability::PresetError),
}

/// What a game starts with: mounted content and the entry level.
pub struct Boot {
    pub vfs: Vfs,
    pub level: Uuid,
}

/// The packages of the staged game in `dir` (its executable's directory), in mount order:
/// `content/paks/base.mpak`, then the patches (`*_p.mpak`) by file name.
pub fn staged_packages(dir: &str) -> Result<Vec<String>, BootError> {
    let paks = if dir.is_empty() {
        String::from(PAKS_DIR)
    } else {
        format!("{}/{PAKS_DIR}", dir.trim_end_matches(['/', '\\']))
    };
    let base = format!("{paks}/{BASE_PACKAGE}");
    if !marlin_io::fs::exists(base.as_str()) {
        return Err(BootError::NoContent(format!(
            "{base} is missing: run the game from the directory `marlin-cook stage` makes"
        )));
    }
    let entries = marlin_io::fs::read_dir(paks.as_str())
        .map_err(|e| BootError::NoContent(format!("{paks}: {e}")))?;
    let mut patches: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| String::from(e.file_name()))
        .filter(|name| name.ends_with(PATCH_SUFFIX))
        .collect();
    patches.sort();
    let mut packages = Vec::with_capacity(1 + patches.len());
    packages.push(base);
    packages.extend(patches.into_iter().map(|p| format!("{paks}/{p}")));
    Ok(packages)
}

/// Boots the staged game in `dir` (its executable's directory): mounts its packages, applies
/// their config and then `args` (`+cvar value`), finds `game.default_level` and applies the
/// quality presets. Config file problems are logged as warnings.
pub fn boot<'a>(dir: &str, args: impl IntoIterator<Item = &'a str>) -> Result<Boot, BootError> {
    register_cvars();
    let packages = staged_packages(dir)?;
    let settings = ContentSettings {
        packages,
        cookd: None,
        platform: String::from(marlin_rt::PLATFORM),
        level: Uuid::default(),
    };
    let (mut boot, config) = mount_with_config(&settings)?;
    for warning in apply_config_files(&config) {
        marlin_diag::warn!("config: {warning}");
    }
    let problems: Vec<String> = marlin_config::apply_command_line(args)
        .into_iter()
        .map(|e| format!("{e}"))
        .collect();
    if !problems.is_empty() {
        return Err(BootError::Config(problems.join("; ")));
    }
    boot.level = default_level()?;
    // The quality presets: scalability tables and the device profile (0008 §2.2).
    crate::scalability::apply_presets(&boot.vfs).map_err(BootError::Presets)?;
    Ok(boot)
}

/// The entry level `game.default_level` names.
pub fn default_level() -> Result<Uuid, BootError> {
    let text = GAME_DEFAULT_LEVEL.get();
    if text.trim().is_empty() {
        return Err(BootError::NoLevel);
    }
    level_ref(&text).ok_or(BootError::Level(text))
}

/// The level a config value names: a link (`</Game/levels/main:Main #<uuid>>`; only the UUID
/// counts) or a bare UUID.
pub fn level_ref(text: &str) -> Option<Uuid> {
    let text = text.trim();
    let uuid = match text.split_once('#') {
        Some((_, rest)) => rest.trim_end().strip_suffix('>')?,
        None => text,
    };
    uuid.trim().parse().ok()
}

/// Replaces the engine, platform and project config layers with `files` (in order, later files
/// over earlier ones in the same layer). Returns each file's problems, for the caller to report
/// (`Game/config/engine.toml: line 3: …`).
pub fn apply_config_files(files: &[ConfigFile]) -> Vec<String> {
    for source in [Source::Engine, Source::Platform, Source::Project] {
        marlin_config::clear_layer(source);
    }
    let mut problems = Vec::new();
    for file in files {
        let source = match file.layer {
            ConfigLayer::Engine => Source::Engine,
            ConfigLayer::Platform => Source::Platform,
            ConfigLayer::Project => Source::Project,
        };
        for e in marlin_config::apply_config(&file.text, source) {
            problems.push(format!("{}: {e}", file.name));
        }
    }
    problems
}

/// Where content comes from, as plain data: for the editor and tests, which mount it
/// themselves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContentSettings {
    /// Package paths, mounted in order: the build (engine and game content together), then
    /// patches above it.
    pub packages: Vec<String>,
    /// The cook service, mounted above the packages (development builds only).
    pub cookd: Option<SocketAddr>,
    /// What the packages must be cooked for, and the cook service cooks for.
    pub platform: String,
    pub level: Uuid,
}

/// Mounts `settings`' content: the packages, then the cook service above them.
pub fn mount(settings: &ContentSettings) -> Result<Boot, BootError> {
    mount_with_config(settings).map(|(boot, _)| boot)
}

/// [`mount`], and the config files the packages hold, in mount order.
fn mount_with_config(settings: &ContentSettings) -> Result<(Boot, Vec<ConfigFile>), BootError> {
    let mut vfs = Vfs::new();
    let mut config = Vec::new();
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
        config.extend(package.index().config.iter().cloned());
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
        return Err(BootError::NoContent(String::from(
            "no packages and no cook service",
        )));
    }
    Ok((
        Boot {
            vfs,
            level: settings.level,
        },
        config,
    ))
}
