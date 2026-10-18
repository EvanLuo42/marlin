//! Shader packages through the cook pipeline: sources in, libraries of compiled entry points
//! out, found by the package's name.

use marlin_asset::cooked::{Encoding, UnitView, unit_id};
use marlin_asset_db::{Content, ENGINE_MOUNT};
use marlin_cook_core::{
    CookOptions, Cookers, Ddc, LocalDdc, Platform, Profile, cook_all, named_objects, unit_key,
};
use marlin_cook_shader::ShaderPackageCooker;
use marlin_reflect::type_info;
use marlin_serial::cooked::CookedBuffer;
use marlin_shader::asset::{ShaderLibrary, ShaderPackage};
use marlin_shader::reflect::Stage;

use crate::support::{Project, uuid};

const SPIRV_MAGIC: [u8; 4] = 0x0723_0203u32.to_le_bytes();

const LIB: &str = "namespace Lib;\n\
    [Pixel] public float4 Red() { return float4(1, 0, 0, 1); }\n\
    [Compute(64, 1, 1)] public void Clear([DispatchThreadId] uint3 id, [Push] in ClearArgs a)\n\
    { a.Out[id.x] = Twice(a.Value); }\n\
    [Compute(1, 1, 1)] internal void Hidden() {}\n";

const ARGS: &str = "namespace Lib;\n\
    public struct ClearArgs { public Buffer<uint> Out; public uint Value; }\n\
    internal uint Twice(uint v) { return v * 2; }\n";

fn cookers() -> Cookers {
    let mut cookers = Cookers::new();
    cookers
        .register_named::<ShaderPackage>(ShaderPackageCooker::new())
        .unwrap();
    cookers
}

/// A package `name` at `<dir>/package.masset` with object id `id`, listing `sources`.
fn package(p: &Project, dir: &str, name: &str, id: u64, sources: &[&str]) {
    let sources: Vec<String> = sources.iter().map(|s| format!("`source:{s}`")).collect();
    p.write(
        &format!("{dir}/package.masset"),
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"{name}\" Sources = [{}] }}\n",
            uuid(id),
            sources.join(", ")
        ),
    );
}

/// The `Lib` package: two files, two public entry points and an internal one.
fn lib(p: &Project) {
    p.write("shaders/Lib.mshader", LIB);
    p.write("shaders/Args.mshader", ARGS);
    package(
        p,
        "shaders",
        "Lib",
        1,
        &["shaders/Lib.mshader", "shaders/Args.mshader"],
    );
}

/// Object `id`'s library in the unit `bytes`.
fn library(bytes: &[u8], id: u64) -> ShaderLibrary {
    let view = UnitView::new(bytes).unwrap();
    let (_, entry) = view.find(uuid(id)).expect("the package's object");
    assert_eq!(entry.type_id, type_info::<ShaderLibrary>().id.0);
    assert_eq!(entry.encoding, Encoding::Serial);
    marlin_serial::from_bytes(view.payload(&entry)).unwrap()
}

/// The unit cooked under `key`, page-aligned.
fn unit(ddc: &LocalDdc, key: u128) -> CookedBuffer {
    CookedBuffer::copy_from(&ddc.get(key).unwrap().unwrap(), 4096)
}

#[test]
fn a_package_cooks_into_a_library_of_its_public_entry_points() {
    let p = Project::new("library");
    lib(&p);
    let units = p.cook(&cookers()).unwrap();
    assert_eq!(units.len(), 1);
    assert!(units[0].warnings.is_empty(), "{:?}", units[0].warnings);
    let lib = library(&units[0].bytes, 1);
    assert_eq!(lib.package, "Lib");
    assert_eq!(lib.target, Platform::host().shader_target);
    let names: Vec<&str> = lib.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Lib.Clear", "Lib.Red"]);

    let clear = lib.entry("Lib.Clear").unwrap();
    assert_eq!(clear.code[..4], SPIRV_MAGIC);
    assert_eq!(clear.reflection.entry.name, "Lib.Clear");
    assert_eq!(clear.reflection.entry.stage, Stage::Compute);
    assert_eq!(clear.reflection.entry.group_size, [64, 1, 1]);
    assert!(clear.reflection.push.is_some(), "a push struct");
    // The package's files by asset path, as the compiler saw them.
    let files: Vec<&str> = clear
        .reflection
        .sources
        .iter()
        .filter(|s| s.package == "Lib")
        .map(|s| s.path.as_str())
        .collect();
    assert_eq!(files, ["shaders/Args.mshader", "shaders/Lib.mshader"]);
    assert_eq!(
        lib.entry("Lib.Red").unwrap().reflection.entry.stage,
        Stage::Pixel
    );
    assert!(lib.entry("Lib.Hidden").is_none());
}

#[test]
fn the_library_is_found_by_the_package_name() {
    let p = Project::new("names");
    lib(&p);
    let db = p.content();
    let (report, manifest) = cook_all(&db, &cookers(), &p.ddc("ddc"), &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(manifest.names, [("Lib".to_string(), uuid(1))]);
    assert_eq!(manifest.named("Lib"), Some(uuid(1)));
    assert_eq!(
        manifest.unit_of(uuid(1)),
        Some(unit_id("/Game/shaders/package"))
    );

    // A second package of the same name: neither is in the table, and the build fails.
    p.write("other/Other.mshader", "namespace Other;\n");
    package(&p, "other", "Lib", 2, &["other/Other.mshader"]);
    let db = p.content();
    let (names, errors) = named_objects(&db, &cookers());
    assert!(names.is_empty(), "{names:?}");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].to_string().contains("names must be unique"),
        "{}",
        errors[0]
    );
    let (report, manifest) = cook_all(&db, &cookers(), &p.ddc("ddc"), &CookOptions::default());
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(manifest.names.is_empty());
}

#[test]
fn compile_errors_fail_the_unit_with_the_diagnostics() {
    let p = Project::new("errors");
    p.write(
        "shaders/Bad.mshader",
        "namespace Bad;\n[Pixel] public float4 Broken() { return Missing(); }\n",
    );
    package(&p, "shaders", "Bad", 1, &["shaders/Bad.mshader"]);
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("`Bad.Broken`"), "{}", errors[0]);
    assert!(errors[0].contains("Missing"), "{}", errors[0]);
    assert!(errors[0].contains("shaders/Bad.mshader"), "{}", errors[0]);
}

#[test]
fn only_the_listed_sources_are_in_the_package() {
    let p = Project::new("sources");
    p.write("shaders/Lib.mshader", LIB);
    // `Args.mshader` is next to it but not listed: what it declares does not exist.
    p.write("shaders/Args.mshader", ARGS);
    package(&p, "shaders", "Lib", 1, &["shaders/Lib.mshader"]);
    let errors = p.cook(&cookers()).unwrap_err();
    assert!(errors[0].contains("ClearArgs"), "{}", errors[0]);

    // A listed file that is missing is an error before anything compiles.
    package(
        &p,
        "shaders",
        "Lib",
        1,
        &["shaders/Lib.mshader", "shaders/Gone.mshader"],
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert!(errors[0].contains("Gone.mshader"), "{}", errors[0]);
}

const GAME: &str = "namespace Game;\n\
    [Compute(64, 1, 1)] public void Fill([DispatchThreadId] uint3 id, [Push] in Lib.ClearArgs a)\n\
    { a.Out[id.x] = a.Value + 1; }\n";

/// `Game` (`game/`, object 2), using `Lib`.
fn game(p: &Project) {
    p.write("game/Game.mshader", GAME);
    p.write(
        "game/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"Game\" \
             Sources = [`source:game/Game.mshader`] Dependencies = [<shaders/package:Lib #{}>] }}\n",
            uuid(2),
            uuid(1)
        ),
    );
}

#[test]
fn a_package_compiles_against_its_dependencies() {
    let p = Project::new("dependencies");
    lib(&p);
    game(&p);
    let units = p.cook(&cookers()).unwrap();
    let game = units
        .iter()
        .find(|u| u.path == "/Game/game/package")
        .unwrap();
    assert!(game.warnings.is_empty(), "{:?}", game.warnings);
    let lib = library(&game.bytes, 2);
    assert_eq!(lib.package, "Game");
    // Its own entry points only: Lib's are in Lib's library.
    let names: Vec<&str> = lib.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Game.Fill"]);
    let fill = lib.entry("Game.Fill").unwrap();
    assert_eq!(fill.code[..4], SPIRV_MAGIC);
    let packages: Vec<(&str, &str)> = fill
        .reflection
        .sources
        .iter()
        .filter(|s| s.package != "Marlin")
        .map(|s| (s.package.as_str(), s.path.as_str()))
        .collect();
    assert_eq!(
        packages,
        [
            ("Lib", "shaders/Args.mshader"),
            ("Lib", "shaders/Lib.mshader"),
            ("Game", "game/Game.mshader")
        ]
    );
    // Built from Lib, not loaded with it.
    let view = UnitView::new(&game.bytes).unwrap();
    assert_eq!(view.import_count(), 0);
}

#[test]
fn dependencies_are_transitive_and_in_the_key() {
    let p = Project::new("transitive");
    lib(&p);
    game(&p);
    p.write(
        "top/Top.mshader",
        "namespace Top;\n[Pixel] public float4 Shade() { return float4(0, 1, 0, 1); }\n",
    );
    p.write(
        "top/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"Top\" \
             Sources = [`source:top/Top.mshader`] Dependencies = [<game/package:Game #{}>] }}\n",
            uuid(3),
            uuid(2)
        ),
    );
    let cookers = cookers();
    let key = |p: &Project, path: &str| {
        let db = p.content();
        let file = db.file(&format!("/Game/{path}")).unwrap();
        unit_key(&db, &cookers, file, Platform::host(), Profile::Development).unwrap()
    };
    let units = p.cook(&cookers).unwrap();
    let top = units
        .iter()
        .find(|u| u.path == "/Game/top/package")
        .unwrap();
    let shade = library(&top.bytes, 3);
    let packages: Vec<&str> = shade.entries[0]
        .reflection
        .sources
        .iter()
        .map(|s| s.package.as_str())
        .filter(|&p| p != "Marlin")
        .collect();
    assert_eq!(packages, ["Lib", "Lib", "Game", "Top"], "Lib through Game");

    let (top, game) = (
        key(&p, "top/package.masset"),
        key(&p, "game/package.masset"),
    );
    p.write("shaders/Args.mshader", format!("{ARGS}// edited\n"));
    assert_ne!(key(&p, "game/package.masset"), game);
    assert_ne!(key(&p, "top/package.masset"), top, "through Game");
}

#[test]
fn packages_that_depend_on_each_other_are_errors() {
    // In one file: the cooker finds the cycle.
    let p = Project::new("package-cycle");
    p.write("a/A.mshader", "namespace A;\n");
    p.write("a/B.mshader", "namespace B;\n");
    p.write(
        "a/package.masset",
        format!(
            "!Version 1\n\
             ShaderPackage {{ !Uuid = \"{}\" !Name = \"A\" Sources = [`source:a/A.mshader`] \
             Dependencies = [<:B #{}>] }}\n\
             ShaderPackage {{ !Uuid = \"{}\" !Name = \"B\" Sources = [`source:a/B.mshader`] \
             Dependencies = [<:A #{}>] }}\n",
            uuid(1),
            uuid(2),
            uuid(2),
            uuid(1)
        ),
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("depend on each other: A → B → A"),
        "{}",
        errors[0]
    );

    // Across units: the key finds it.
    let p = Project::new("unit-cycle");
    lib(&p);
    game(&p);
    p.write(
        "shaders/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"Lib\" \
             Sources = [`source:shaders/Lib.mshader`, `source:shaders/Args.mshader`] \
             Dependencies = [<game/package:Game #{}>] }}\n",
            uuid(1),
            uuid(2)
        ),
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().all(|e| e.contains("form a cycle")),
        "{errors:?}"
    );
}

#[test]
fn a_missing_dependency_is_an_error() {
    let p = Project::new("not-a-package");
    lib(&p);
    game(&p);
    // Game depends on an object nothing defines.
    p.write(
        "game/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"Game\" \
             Sources = [`source:game/Game.mshader`] Dependencies = [<shaders/package:Lib #{}>] }}\n",
            uuid(2),
            uuid(9)
        ),
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("no indexed file defines"),
        "{}",
        errors[0]
    );
}

#[test]
fn the_key_follows_the_sources_the_profile_and_the_target() {
    let p = Project::new("keys");
    lib(&p);
    let cookers = cookers();
    let host = Platform::host();
    let key = |db: &Content, platform: &Platform, profile| {
        let file = db.file("/Game/shaders/package.masset").unwrap();
        unit_key(db, &cookers, file, platform, profile).unwrap()
    };
    let db = p.content();
    let dev = key(&db, host, Profile::Development);
    assert_eq!(key(&db, host, Profile::Development), dev);
    assert_ne!(key(&db, host, Profile::Shipping), dev);
    let metal: &'static Platform = Box::leak(Box::new(Platform {
        shader_target: "metal-3",
        ..*host
    }));
    assert_ne!(key(&db, metal, Profile::Development), dev);

    // A target the compiler has no backend for is a cook error, not a panic.
    let (report, _) = cook_all(
        &db,
        &cookers,
        &p.ddc("ddc"),
        &CookOptions {
            platform: metal,
            ..CookOptions::default()
        },
    );
    assert_eq!(report.errors.len(), 1);
    assert!(
        report.errors[0].to_string().contains("metal-3"),
        "{}",
        report.errors[0]
    );

    // Editing a listed source changes the key.
    p.write("shaders/Args.mshader", format!("{ARGS}// edited\n"));
    assert_ne!(key(&p.content(), host, Profile::Development), dev);
}

#[test]
fn shipping_and_development_cook_the_same_entries() {
    let p = Project::new("profiles");
    lib(&p);
    let db = p.content();
    let cookers = cookers();
    let ddc = p.ddc("ddc");
    let mut libraries = Vec::new();
    for profile in [Profile::Development, Profile::Shipping] {
        let options = CookOptions {
            profile,
            ..CookOptions::default()
        };
        let (report, _) = cook_all(&db, &cookers, &ddc, &options);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        libraries.push(library(&unit(&ddc, report.units[0].key), 1));
    }
    let names = |l: &ShaderLibrary| l.entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&libraries[0]), names(&libraries[1]));
}

#[test]
fn cooking_is_deterministic() {
    let p = Project::new("determinism");
    lib(&p);
    let db = p.content();
    let cookers = cookers();
    let (one, two) = (p.ddc("one"), p.ddc("two"));
    let (r1, _) = cook_all(&db, &cookers, &one, &CookOptions::default());
    let (r2, _) = cook_all(&db, &cookers, &two, &CookOptions::default());
    let key = r1.units[0].key;
    assert_eq!(r2.units[0].key, key);
    assert_eq!(one.get(key).unwrap(), two.get(key).unwrap());
}

/// The engine's own package (`content/engine/shaders/`) cooks, with the forward pass, the post
/// chain, the GPU Scene's scatter and the visibility pipeline in it.
#[test]
fn the_engine_package_cooks() {
    let p = Project::new("engine");
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/engine");
    // The packages (the engine's and the Substrate library it uses) and their sources.
    let mut dirs = vec![
        std::path::PathBuf::from("shaders"),
        std::path::PathBuf::from("substrate"),
    ];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(content.join(&dir)).unwrap() {
            let e = e.unwrap();
            let rel = dir.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                dirs.push(rel);
            } else {
                let file = rel.to_str().unwrap().replace('\\', "/");
                p.write(&file, std::fs::read(e.path()).unwrap());
            }
        }
    }
    // As the engine's mount: only it may name packages `Marlin.…`.
    let db = p.content_as(ENGINE_MOUNT);
    let ddc = p.ddc("ddc");
    let (report, manifest) = cook_all(&db, &cookers(), &ddc, &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let id = manifest
        .named("Marlin.Engine")
        .expect("the engine package by name");
    let bytes = unit(&ddc, report.units[0].key);
    let view = UnitView::new(&bytes).unwrap();
    let (_, entry) = view.find(id).unwrap();
    let lib: ShaderLibrary = marlin_serial::from_bytes(view.payload(&entry)).unwrap();
    for (name, stage) in [
        ("Marlin.Engine.ForwardPs", Stage::Pixel),
        ("Marlin.Engine.ForwardVs", Stage::Vertex),
        ("Marlin.Engine.Post.TonemapPs", Stage::Pixel),
        ("Marlin.Engine.Scene.Scatter", Stage::Compute),
        ("Marlin.Engine.Visibility.VisMesh", Stage::Mesh),
        ("Marlin.Engine.Visibility.VisPs", Stage::Pixel),
    ] {
        let e = lib.entry(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(e.reflection.entry.stage, stage);
        assert_eq!(e.code[..4], SPIRV_MAGIC);
    }
    // The forward pass's push constants: eight 32-bit words (0002).
    let vs = lib.entry("Marlin.Engine.ForwardVs").unwrap();
    assert_eq!(vs.reflection.push.as_ref().unwrap().size, 32);
}

/// `Marlin` and `Marlin.…` are the engine's package names, and the engine uses no others: a
/// game's names can never clash with the engine's (0003 §4.5).
#[test]
fn marlin_names_belong_to_the_engine_mount() {
    let p = Project::new("reserved");
    p.write("shaders/Mine.mshader", "namespace Marlin.Mine;\n");
    package(&p, "shaders", "Marlin.Mine", 1, &["shaders/Mine.mshader"]);
    let errors = p.cook(&cookers()).unwrap_err();
    assert!(errors[0].contains("belong to the engine"), "{}", errors[0]);

    // In the engine's mount the same package cooks, and an unprefixed one does not.
    let ddc = p.ddc("engine");
    let (report, _) = cook_all(
        &p.content_as(ENGINE_MOUNT),
        &cookers(),
        &ddc,
        &CookOptions::default(),
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    package(&p, "shaders", "Mine", 1, &["shaders/Mine.mshader"]);
    let (report, _) = cook_all(
        &p.content_as(ENGINE_MOUNT),
        &cookers(),
        &ddc,
        &CookOptions::default(),
    );
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].to_string().contains("named `Marlin.…`"),
        "{}",
        report.errors[0]
    );
}
