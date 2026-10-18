//! Shader packages through the cook pipeline: sources in, libraries of compiled entry points
//! out, found by the package's name.

use marlin_asset::cooked::{Encoding, UnitView, unit_id};
use marlin_asset_db::AssetDb;
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
    let db = p.db();
    let (report, manifest) = cook_all(&db, &cookers(), &p.ddc("ddc"), &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(manifest.names, [("Lib".to_string(), uuid(1))]);
    assert_eq!(manifest.named("Lib"), Some(uuid(1)));
    assert_eq!(manifest.unit_of(uuid(1)), Some(unit_id("shaders/package")));

    // A second package of the same name: neither is in the table, and the build fails.
    p.write("other/Other.mshader", "namespace Other;\n");
    package(&p, "other", "Lib", 2, &["other/Other.mshader"]);
    let db = p.db();
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

#[test]
fn packages_with_dependencies_do_not_cook_yet() {
    let p = Project::new("dependencies");
    lib(&p);
    p.write("game/Game.mshader", "namespace Game;\n");
    p.write(
        "game/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{}\" !Name = \"Game\" \
             Sources = [`source:game/Game.mshader`] Dependencies = [<shaders/package:Lib #{}>] }}\n",
            uuid(2),
            uuid(1)
        ),
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("build dependencies"), "{}", errors[0]);
}

#[test]
fn the_key_follows_the_sources_the_profile_and_the_target() {
    let p = Project::new("keys");
    lib(&p);
    let cookers = cookers();
    let host = Platform::host();
    let key = |db: &AssetDb, platform: &Platform, profile| {
        let file = db.file("shaders/package.masset").unwrap();
        unit_key(db, &cookers, file, platform, profile).unwrap()
    };
    let db = p.db();
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
    assert_ne!(key(&p.db(), host, Profile::Development), dev);
}

#[test]
fn shipping_and_development_cook_the_same_entries() {
    let p = Project::new("profiles");
    lib(&p);
    let db = p.db();
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
    let db = p.db();
    let cookers = cookers();
    let (one, two) = (p.ddc("one"), p.ddc("two"));
    let (r1, _) = cook_all(&db, &cookers, &one, &CookOptions::default());
    let (r2, _) = cook_all(&db, &cookers, &two, &CookOptions::default());
    let key = r1.units[0].key;
    assert_eq!(r2.units[0].key, key);
    assert_eq!(one.get(key).unwrap(), two.get(key).unwrap());
}

/// The engine's own package (`content/engine/shaders/`) cooks, with the forward pass in it.
#[test]
fn the_engine_package_cooks() {
    let p = Project::new("engine");
    let content = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content/engine");
    for file in ["shaders/package.masset", "shaders/Forward.mshader"] {
        p.write(file, std::fs::read(content.join(file)).unwrap());
    }
    let db = p.db();
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
    ] {
        let e = lib.entry(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(e.reflection.entry.stage, stage);
        assert_eq!(e.code[..4], SPIRV_MAGIC);
    }
    // The forward pass's push constants: four 32-bit words (0002).
    let vs = lib.entry("Marlin.Engine.ForwardVs").unwrap();
    assert_eq!(vs.reflection.push.as_ref().unwrap().size, 16);
}
