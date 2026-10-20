//! `marlin-cook` on a small project: cook, cache, fail, package, stage.

use std::io::Cursor;
use std::process::Command;

use marlin_asset::Uuid;
use marlin_cook_core::Platform;
use marlin_test_support::{TempDir, test_uuid};

/// An empty project (its engine is the one these tools belong to: this repository's).
fn project(name: &str) -> TempDir {
    let root = TempDir::new(name);
    root.write(
        "Test.mproject",
        "[project]
name = \"Test\"
",
    );
    root
}

fn run(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_marlin-cook"))
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn cooks_then_hits_the_cache_then_reports_failures() {
    let root = project("cook-cli");
    let image = image::RgbaImage::from_fn(64, 64, |x, y| {
        image::Rgba([x as u8 * 4, y as u8 * 4, 0, 255])
    });
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    root.write("content/tex/ramp.png", png.into_inner());
    root.write("config/game.toml", "[game]\ndefault_level = \"x\"\n");
    root.write("config/editor.toml", "[editor]\nstartup_level = \"y\"\n");
    root.write(
        "content/tex/ramp.masset",
        format!(
            "!Version 1\nTexture {{ !Uuid = \"{}\" !Name = \"Ramp\" Source = `source:tex/ramp.png` }}\n",
            Uuid::from_u128(test_uuid(1))
        ),
    );
    let r = root.to_str().unwrap();
    // The default target is the platform the tools were built for.
    let build = root.join(".marlin/build");
    let manifest_path = build.join(format!("{}.manifest", Platform::host().name));

    let (ok, text) = run(&["cook", r]);
    assert!(ok, "{text}");
    assert!(
        text.contains("13 units (13 cooked, 0 cached), 0 failed"),
        "{text}"
    );
    assert!(manifest_path.is_file());

    let (ok, text) = run(&["cook", r, "--threads", "2"]);
    assert!(ok, "{text}");
    assert!(text.contains("(0 cooked, 13 cached)"), "{text}");

    let (ok, text) = run(&["package", r, "--bulk-codec", "gdeflate"]);
    assert!(ok, "{text}");
    assert!(text.contains("13 units"), "{text}");
    let package_path = build.join(format!("{}.mpak", Platform::host().name));
    let package = marlin_asset::Package::open(package_path.to_str().unwrap()).unwrap();
    // The engine's content (its shader package, the Substrate library, the default material's
    // resolve and bake permutations, the UI font, the pairing patterns, the scalability table and
    // the two device profiles, and the input bindings: the spectator's, the host's and the
    // editor's) is in the build with the project's.
    assert_eq!(package.index().units.len(), 13);
    // The project's config is in the build (the editor's is not).
    let config: Vec<&str> = package
        .index()
        .config
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert!(config.contains(&"Game/config/game.toml"), "{config:?}");
    assert!(
        !config.iter().any(|n| n.ends_with("editor.toml")),
        "{config:?}"
    );
    let (ok, text) = run(&["package", r, "--bulk-codec", "brotli"]);
    assert!(!ok && text.contains("usage"), "{text}");

    // Staged as it ships: the executable renamed after the project, the package as the base.
    root.write("bin/game.exe", "not really a game");
    let staged = root.join(format!(".marlin/staged/{}/Test", Platform::host().name));
    root.write(staged.join("content/paks/old_p.mpak"), "stale");
    let game = root.join("bin/game.exe");
    let (ok, text) = run(&["stage", r, "--game", game.to_str().unwrap()]);
    assert!(ok, "{text}");
    assert_eq!(
        std::fs::read(staged.join("Test.exe")).unwrap(),
        b"not really a game"
    );
    assert_eq!(
        std::fs::read(staged.join("content/paks/base.mpak")).unwrap(),
        std::fs::read(&package_path).unwrap()
    );
    let paks: Vec<_> = std::fs::read_dir(staged.join("content/paks"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(paks, ["base.mpak"], "a stage starts afresh");
    let (ok, text) = run(&["stage", r, "--game", "missing.exe"]);
    assert!(!ok && text.contains("no game executable"), "{text}");

    let manifest = std::fs::read(&manifest_path).unwrap();
    root.write(
        "content/tex/broken.masset",
        format!(
            "!Version 1\nTexture {{ !Uuid = \"{}\" !Name = \"Broken\" Source = `source:tex/missing.png` }}\n",
            Uuid::from_u128(test_uuid(2))
        ),
    );
    let (ok, text) = run(&["cook", r]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("error: /Game/tex/broken") && text.contains("missing.png"),
        "{text}"
    );
    assert!(text.contains("1 failed; manifest not published"), "{text}");
    assert_eq!(
        std::fs::read(&manifest_path).unwrap(),
        manifest,
        "a failed cook keeps the last good manifest"
    );

    let (ok, text) = run(&["cook", r, "--platform"]);
    assert!(!ok && text.contains("usage"), "{text}");
    // Platforms come from platforms/*/platform.toml: a typo is not a new target.
    for command in ["cook", "package"] {
        let (ok, text) = run(&[command, r, "--platform", "Windows"]);
        assert!(!ok && text.contains("unknown platform `Windows`"), "{text}");
    }
}

#[test]
fn shader_packages_cook_for_both_profiles_and_names_are_validated() {
    let root = project("cook-cli-shaders");
    let package = |dir: &str, id: u64| {
        root.write(
            format!("content/{dir}/Lib.mshader"),
            "namespace Lib;\n[Compute(1, 1, 1)] public void Clear() {}\n",
        );
        root.write(
            format!("content/{dir}/package.masset"),
            format!(
                "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"Lib\" Sources = [`source:{dir}/Lib.mshader`] }}\n",
                Uuid::from_u128(test_uuid(id))
            ),
        );
    };
    package("shaders", 1);
    let r = root.to_str().unwrap();

    let (ok, text) = run(&["cook", r]);
    assert!(ok, "{text}");
    assert!(text.contains("(13 cooked, 0 cached)"), "{text}");
    // A shipping build is another key for shaders; the font, the pairing patterns, the presets and
    // the input bindings cook the same for every profile.
    let (ok, text) = run(&["cook", r, "--shipping"]);
    assert!(ok, "{text}");
    assert!(text.contains("(5 cooked, 8 cached)"), "{text}");
    let (ok, text) = run(&["cook", r, "--shipping", "--threads", "1"]);
    assert!(ok, "{text}");
    assert!(text.contains("(0 cooked, 13 cached)"), "{text}");

    let (ok, text) = run(&["validate", r]);
    assert!(ok, "{text}");
    // A second package called `Lib`: validation names the clash.
    package("more", 2);
    let (ok, text) = run(&["validate", r]);
    assert!(!ok, "{text}");
    assert!(text.contains("`Lib` also names"), "{text}");
}
