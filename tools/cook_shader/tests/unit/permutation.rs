//! `Shader` permutations: one entry point with type arguments and static parameters, compiled
//! in a package's context into a library of that entry.

use marlin_asset::Uuid;
use marlin_asset::cooked::{Encoding, UnitView};
use marlin_asset_db::{Content, ENGINE_MOUNT, GAME_MOUNT, Mount};
use marlin_cook_core::{CookOptions, Cookers, Ddc, Platform, Profile, cook_all, unit_key};
use marlin_cook_shader::{ShaderCooker, ShaderPackageCooker};
use marlin_reflect::type_info;
use marlin_serial::cooked::CookedBuffer;
use marlin_shader::asset::{Shader, ShaderLibrary, ShaderPackage};
use marlin_shader::reflect::{ConstValue, Stage};

use crate::support::{Project, uuid};

const SPIRV_MAGIC: [u8; 4] = 0x0723_0203u32.to_le_bytes();

/// A generic entry point (not in the package's library) reading a static parameter.
const KIT: &str = "namespace Kit;\n\
    public interface IOp { static abstract uint Apply(uint x); }\n\
    public struct Doubler : IOp { public static uint Apply(uint x) => x * 2; }\n\
    public struct Tuning { public const uint Scale = 1; }\n\
    public struct Args { public Buffer<uint> Out; }\n\
    [Compute(8, 1, 1)]\n\
    public void Map<T>([DispatchThreadId] uint3 id, [Push] in Args a) where T : IOp\n\
    { a.Out[id.x] = T.Apply(id.x) * Tuning.Scale; }\n";

/// A game type for the kit's entry point.
const GAME: &str = "namespace Game;\n\
    public struct Tripler : Kit.IOp { public static uint Apply(uint x) => x * 3; }\n";

const KIT_ID: Uuid = uuid(1);
const GAME_ID: Uuid = uuid(2);

fn cookers() -> Cookers {
    let mut cookers = Cookers::new();
    cookers
        .register_named::<ShaderPackage>(ShaderPackageCooker::new())
        .unwrap();
    cookers.register::<Shader>(ShaderCooker::new()).unwrap();
    cookers
}

/// `Kit` (`kit/`) and `Game` (`game/`, using `Kit`).
fn packages(p: &Project) {
    p.write("kit/Kit.mshader", KIT);
    p.write(
        "kit/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{KIT_ID}\" !Name = \"Kit\" \
             Sources = [`source:kit/Kit.mshader`] }}\n"
        ),
    );
    p.write("game/Game.mshader", GAME);
    p.write(
        "game/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{GAME_ID}\" !Name = \"Game\" \
             Sources = [`source:game/Game.mshader`] Dependencies = [<kit/package:Kit #{KIT_ID}>] }}\n"
        ),
    );
}

/// `shaders.masset` with the given `Shader` objects' bodies (after `!Uuid` and `!Name`).
fn shaders(p: &Project, objects: &[(u64, &str, &str)]) {
    let mut text = String::from("!Version 1\n");
    for (id, name, body) in objects {
        text.push_str(&format!(
            "Shader {{ !Uuid = \"{}\" !Name = \"{name}\" {body} }}\n",
            uuid(*id)
        ));
    }
    p.write("shaders.masset", text);
}

fn in_kit(entry: &str, type_args: &str, consts: &str) -> String {
    format!(
        "Package = <kit/package:Kit #{KIT_ID}> Entry = \"{entry}\" TypeArgs = [{type_args}] \
         Consts = [{consts}]"
    )
}

/// Object `id`'s library in the unit `bytes`.
fn library(bytes: &[u8], id: u64) -> ShaderLibrary {
    let view = UnitView::new(bytes).unwrap();
    let (_, entry) = view.find(uuid(id)).expect("the shader's object");
    assert_eq!(entry.type_id, type_info::<ShaderLibrary>().id.0);
    assert_eq!(entry.encoding, Encoding::Serial);
    marlin_serial::from_bytes(view.payload(&entry)).unwrap()
}

#[test]
fn a_shader_cooks_into_a_library_of_its_one_entry() {
    let p = Project::new("permutation");
    packages(&p);
    shaders(
        &p,
        &[
            (10, "Double", &in_kit("Kit.Map", "\"Kit.Doubler\"", "")),
            (
                11,
                "DoubleScaled",
                &in_kit(
                    "Kit.Map",
                    "\"Kit.Doubler\"",
                    "(\"Kit.Tuning.Scale\", UInt(5))",
                ),
            ),
        ],
    );
    let units = p.cook(&cookers()).unwrap();
    let unit = units.iter().find(|u| u.path == "/Game/shaders").unwrap();
    assert!(unit.warnings.is_empty(), "{:?}", unit.warnings);

    let plain = library(&unit.bytes, 10);
    assert_eq!(plain.package, "Kit");
    assert_eq!(plain.target, Platform::host().shader_target);
    assert_eq!(plain.entries.len(), 1);
    let entry = plain.entry("Kit.Map").unwrap();
    assert_eq!(entry.code[..4], SPIRV_MAGIC);
    assert_eq!(entry.reflection.entry.stage, Stage::Compute);
    assert_eq!(entry.reflection.entry.group_size, [8, 1, 1]);

    let scaled = library(&unit.bytes, 11);
    let statics: Vec<(&str, &ConstValue)> = scaled.entries[0]
        .reflection
        .static_params
        .iter()
        .map(|s| (s.name.as_str(), &s.value))
        .collect();
    assert_eq!(statics, [("Kit.Tuning.Scale", &ConstValue::UInt(5))]);
    assert_ne!(scaled.entries[0].code, entry.code, "another permutation");

    // Built from the package, not loaded with it: loading a permutation loads nothing else.
    let view = UnitView::new(&unit.bytes).unwrap();
    assert_eq!(view.import_count(), 0);

    // The package's own library has no generic entries; it is still a valid library.
    let kit = units
        .iter()
        .find(|u| u.path == "/Game/kit/package")
        .unwrap();
    let kit = library(&kit.bytes, 1);
    assert!(kit.entries.is_empty());
}

#[test]
fn the_package_is_the_compile_s_context() {
    // A game type instantiates the kit's entry point: the session holds Game and, before it,
    // Kit.
    let p = Project::new("context");
    packages(&p);
    shaders(
        &p,
        &[(
            10,
            "Triple",
            &format!(
                "Package = <game/package:Game #{GAME_ID}> Entry = \"Kit.Map\" \
                 TypeArgs = [\"Game.Tripler\"]"
            ),
        )],
    );
    let units = p.cook(&cookers()).unwrap();
    let unit = units.iter().find(|u| u.path == "/Game/shaders").unwrap();
    let lib = library(&unit.bytes, 10);
    assert_eq!(lib.package, "Game");
    let packages: Vec<&str> = lib.entries[0]
        .reflection
        .sources
        .iter()
        .map(|s| s.package.as_str())
        .filter(|&p| p != "Marlin")
        .collect();
    assert_eq!(packages, ["Kit", "Game"]);

    // Without Game, Kit cannot see the type.
    shaders(
        &p,
        &[(10, "Triple", &in_kit("Kit.Map", "\"Game.Tripler\"", ""))],
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Game.Tripler"), "{}", errors[0]);
}

#[test]
fn the_key_follows_the_package_and_its_dependencies() {
    let p = Project::new("permutation-key");
    packages(&p);
    shaders(
        &p,
        &[(
            10,
            "Triple",
            &format!(
                "Package = <game/package:Game #{GAME_ID}> Entry = \"Kit.Map\" \
                 TypeArgs = [\"Game.Tripler\"]"
            ),
        )],
    );
    let cookers = cookers();
    let key = |p: &Project| {
        let db = p.content();
        let file = db.file("/Game/shaders.masset").unwrap();
        unit_key(&db, &cookers, file, Platform::host(), Profile::Development).unwrap()
    };
    let before = key(&p);
    assert_ne!(
        unit_key(
            &p.content(),
            &cookers,
            p.content().file("/Game/shaders.masset").unwrap(),
            Platform::host(),
            Profile::Shipping
        )
        .unwrap(),
        before,
        "the profile"
    );
    p.write("kit/Kit.mshader", format!("{KIT}// edited\n"));
    assert_ne!(key(&p), before, "Kit, through Game");
}

#[test]
fn bad_permutations_are_errors() {
    let p = Project::new("bad-permutations");
    packages(&p);
    for (body, expected) in [
        ("Entry = \"Kit.Map\"".to_string(), "`Package` is not set"),
        (
            format!("Package = <kit/package:Kit #{KIT_ID}>"),
            "`Entry` is not set",
        ),
        (in_kit("Kit.Missing", "", ""), "Kit.Missing"),
        (
            in_kit(
                "Kit.Map",
                "\"Kit.Doubler\"",
                "(\"Kit.Tuning.Scale\", Bool(true))",
            ),
            "Kit.Tuning.Scale",
        ),
    ] {
        shaders(&p, &[(10, "Bad", &body)]);
        let errors = p.cook(&cookers()).unwrap_err();
        assert_eq!(errors.len(), 1, "{body}: {errors:?}");
        assert!(errors[0].contains(expected), "{body}: {}", errors[0]);
    }
}

/// A game's permutation of an engine entry point (0003 §4.5): the engine's package is a build
/// dependency in the mount below, read from there, sources and all, and its library still
/// cooks in the engine's mount.
#[test]
fn a_game_shader_builds_from_an_engine_package() {
    let engine = Project::new("permutation-engine");
    engine.write(
        "kit/Kit.mshader",
        KIT.replace("namespace Kit;", "namespace Marlin.Kit;"),
    );
    engine.write(
        "kit/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{KIT_ID}\" !Name = \"Marlin.Kit\" \
             Sources = [`source:kit/Kit.mshader`] }}\n"
        ),
    );
    let game = Project::new("permutation-game");
    game.write(
        "game/Game.mshader",
        GAME.replace("Kit.IOp", "Marlin.Kit.IOp"),
    );
    game.write(
        "game/package.masset",
        format!(
            "!Version 1\nShaderPackage {{ !Uuid = \"{GAME_ID}\" !Name = \"Game\" \
             Sources = [`source:game/Game.mshader`] \
             Dependencies = [</Engine/kit/package:Marlin.Kit #{KIT_ID}>] }}\n"
        ),
    );
    shaders(
        &game,
        &[
            (
                10,
                "Double",
                &format!(
                    "Package = </Engine/kit/package:Marlin.Kit #{KIT_ID}> \
                     Entry = \"Marlin.Kit.Map\" TypeArgs = [\"Marlin.Kit.Doubler\"]"
                ),
            ),
            (
                11,
                "Triple",
                &format!(
                    "Package = <game/package:Game #{GAME_ID}> Entry = \"Marlin.Kit.Map\" \
                     TypeArgs = [\"Game.Tripler\"]"
                ),
            ),
        ],
    );
    let content = Content::new(vec![
        Mount {
            name: ENGINE_MOUNT.into(),
            db: engine.db(),
        },
        Mount {
            name: GAME_MOUNT.into(),
            db: game.db(),
        },
    ]);
    let ddc = game.ddc("ddc");
    let (report, manifest) = cook_all(&content, &cookers(), &ddc, &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(manifest.named("Marlin.Kit"), Some(KIT_ID));
    let unit = report
        .units
        .iter()
        .find(|u| u.path == "/Game/shaders")
        .unwrap();
    let bytes = CookedBuffer::copy_from(&ddc.get(unit.key).unwrap().unwrap(), 4096);
    for (id, package) in [(10, "Marlin.Kit"), (11, "Game")] {
        let lib = library(&bytes, id);
        assert_eq!(lib.package, package);
        let entry = lib.entry("Marlin.Kit.Map").unwrap();
        assert_eq!(entry.code[..4], SPIRV_MAGIC);
        // The engine's file is an input, by the path within its package.
        assert!(
            entry
                .reflection
                .sources
                .iter()
                .any(|s| s.package == "Marlin.Kit" && s.path == "kit/Kit.mshader"),
            "{:?}",
            entry.reflection.sources
        );
    }

    // Editing the engine's source changes the game's permutations.
    let before = unit.key;
    engine.write(
        "kit/Kit.mshader",
        format!(
            "{}// edited\n",
            KIT.replace("namespace Kit;", "namespace Marlin.Kit;")
        ),
    );
    let content = Content::new(vec![
        Mount {
            name: ENGINE_MOUNT.into(),
            db: engine.db(),
        },
        Mount {
            name: GAME_MOUNT.into(),
            db: game.db(),
        },
    ]);
    let file = content.file("/Game/shaders.masset").unwrap();
    let after = unit_key(
        &content,
        &cookers(),
        file,
        Platform::host(),
        Profile::Development,
    );
    assert_ne!(after.unwrap(), before);
}
