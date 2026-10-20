//! Startup: the staged layout, the config cooked into packages, the command line and the entry
//! level (0003 §4.6).

use marlin_asset::Uuid;
use marlin_asset::package::{ConfigFile, ConfigLayer};
use marlin_config::Source;
use marlin_framework::boot::{GAME_DEFAULT_LEVEL, R_VSYNC, WINDOW_WIDTH};
use marlin_framework::{
    BootError, apply_config_files, boot, default_level, level_ref, register_cvars, staged_packages,
};

const LEVEL: &str = "01992d00-0000-7000-8000-000000000001";

fn file(layer: ConfigLayer, name: &str, text: &str) -> ConfigFile {
    ConfigFile {
        layer,
        name: name.to_owned(),
        text: text.to_owned(),
    }
}

#[test]
fn levels_are_links_or_uuids() {
    let uuid: Uuid = LEVEL.parse().unwrap();
    assert_eq!(level_ref(LEVEL), Some(uuid));
    assert_eq!(
        level_ref(&format!(" </Game/levels/main:Main #{LEVEL}> ")),
        Some(uuid)
    );
    assert_eq!(
        level_ref(&format!("<levels/main:Main # {LEVEL} >")),
        Some(uuid)
    );
    assert_eq!(level_ref("</Game/levels/main:Main>"), None);
    assert_eq!(
        level_ref(&format!("</Game/levels/main:Main #{LEVEL}")),
        None
    );
    assert_eq!(level_ref("nope"), None);
}

#[test]
fn config_files_apply_by_layer_and_in_order() {
    register_cvars();
    let files = [
        file(
            ConfigLayer::Engine,
            "Engine/config/engine.toml",
            "[window]\nwidth = 800\n",
        ),
        file(
            ConfigLayer::Platform,
            "Engine/config/windows/engine.toml",
            "[r]\nvsync = false\n",
        ),
        file(
            ConfigLayer::Project,
            "Game/config/game.toml",
            &format!("[game]\ndefault_level = \"</Game/levels/main:Main #{LEVEL}>\"\n"),
        ),
        // A later file of the same layer wins; a cvar the build lacks is only reported.
        file(
            ConfigLayer::Project,
            "Game/config/windows/engine.toml",
            "window.width = 1024\nr.no_such_cvar = 1\n",
        ),
    ];
    let problems = apply_config_files(&files);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with("Game/config/windows/engine.toml: "),
        "{problems:?}"
    );
    assert_eq!(WINDOW_WIDTH.get(), 1024);
    assert_eq!(WINDOW_WIDTH.set_by(), Some(Source::Project));
    assert_eq!(
        (R_VSYNC.get(), R_VSYNC.set_by()),
        (false, Some(Source::Platform))
    );
    assert_eq!(default_level().unwrap(), LEVEL.parse().unwrap());

    // Applying again replaces the layers: what the files no longer set falls back.
    assert!(apply_config_files(&files[..1]).is_empty());
    assert_eq!(
        (WINDOW_WIDTH.get(), WINDOW_WIDTH.set_by()),
        (800, Some(Source::Engine))
    );
    assert!(R_VSYNC.get());
    assert!(matches!(default_level(), Err(BootError::NoLevel)));
    GAME_DEFAULT_LEVEL.set("nope");
    assert!(matches!(default_level(), Err(BootError::Level(_))));
    GAME_DEFAULT_LEVEL.set("");
}

#[test]
fn staged_games_find_their_packages_by_convention() {
    let dir = marlin_test_support::TempDir::new("staged");
    let root = dir.path().to_str().unwrap().to_owned();
    // No base package: not a staged game.
    match staged_packages(&root) {
        Err(BootError::NoContent(message)) => assert!(message.contains("base.mpak"), "{message}"),
        other => panic!("{:?}", other.map(|_| ())),
    }
    assert!(matches!(boot(&root, []), Err(BootError::NoContent(_))));

    let paks = dir.path().join("content/paks");
    std::fs::create_dir_all(&paks).unwrap();
    for name in [
        "base.mpak",
        "patch_002_p.mpak",
        "patch_001_p.mpak",
        "notes.txt",
        "dlc.mpak",
    ] {
        std::fs::write(paks.join(name), b"").unwrap();
    }
    let packages = staged_packages(&root).unwrap();
    let names: Vec<&str> = packages
        .iter()
        .map(|p| p.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(names, ["base.mpak", "patch_001_p.mpak", "patch_002_p.mpak"]);
    assert!(packages[0].starts_with(&root), "{packages:?}");

    // Mounting names the package that does not open.
    match boot(&root, []) {
        Err(BootError::Package { path, .. }) => assert!(path.ends_with("base.mpak"), "{path}"),
        other => panic!("{:?}", other.map(|_| ())),
    }
}
