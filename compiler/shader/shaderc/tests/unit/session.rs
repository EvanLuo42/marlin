use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use marlin_shader::reflect::Stage;
use marlin_shaderc::{
    CompileError, ConstValue, EntryRequest, FieldValue, PackageDesc, Profile, Session, SessionDesc,
    SourceError, SourcePath, SourceProvider, default_targets,
};
use marlin_test_support::{data_path, golden};

/// Packages held in memory, read from `tests/data/<package>/`; tests change them in place.
struct Memory {
    files: Mutex<BTreeMap<SourcePath, Arc<str>>>,
}

impl Memory {
    fn new() -> Memory {
        let mut files = BTreeMap::new();
        for package in ["Game"] {
            let dir = data_path(env!("CARGO_MANIFEST_DIR"), package);
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for name in names {
                let text = std::fs::read_to_string(data_path(
                    env!("CARGO_MANIFEST_DIR"),
                    &format!("{package}/{name}"),
                ))
                .unwrap()
                .replace("\r\n", "\n");
                files.insert(SourcePath::new(package, name.as_str()), text.into());
            }
        }
        Memory {
            files: Mutex::new(files),
        }
    }

    fn set(&self, package: &str, path: &str, text: &str) {
        self.files
            .lock()
            .unwrap()
            .insert(SourcePath::new(package, path), text.into());
    }
}

impl SourceProvider for Memory {
    fn list(&self, package: &str) -> Result<Vec<SourcePath>, SourceError> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.package() == package)
            .cloned()
            .collect())
    }

    fn read(&self, path: &SourcePath) -> Result<Arc<str>, SourceError> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| SourceError {
                message: format!("no file {path}"),
            })
    }
}

fn session<'a>(sources: &'a Memory, targets: &'a marlin_shaderc::Registry) -> Session<'a> {
    let mut s = Session::new(SessionDesc { sources, targets });
    s.add_package(&PackageDesc {
        name: "Game",
        dependencies: &["Marlin"],
    })
    .unwrap();
    s
}

fn request<'a>(entry: &'a str, consts: &'a [(&'a str, ConstValue)]) -> EntryRequest<'a> {
    EntryRequest {
        entry,
        type_args: &[],
        consts,
        target: "vulkan-1.4",
        profile: Profile::Development,
    }
}

fn errors(s: &Session<'_>, e: &CompileError) -> String {
    s.render(&e.diagnostics)
}

#[test]
fn an_entry_compiles_to_spirv_with_its_reflection() {
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    let out = s
        .compile(&request("Game.Mirror", &[]))
        .unwrap_or_else(|e| panic!("{}", errors(&s, &e)));
    assert_eq!(
        &out.code.bytes[..4],
        &0x0723_0203u32.to_le_bytes(),
        "SPIR-V"
    );
    assert!(out.diagnostics.is_empty());

    let r = &out.reflection;
    assert_eq!(r.entry.name, "Game.Mirror");
    assert_eq!(r.entry.stage, Stage::Compute);
    assert_eq!(r.entry.group_size, [64, 1, 1]);
    assert_eq!(r.stats.groupshared_bytes, 256);
    assert_eq!(r.stats.code_bytes as usize, out.code.bytes.len());
    assert!(r.stats.instructions > 0);
    // The core library's files, then the package's.
    let core: Vec<SourcePath> = marlin_shaderc_core::FILES
        .iter()
        .map(|(p, _)| SourcePath::new("Marlin", *p))
        .collect();
    assert_eq!(out.used_sources[..core.len()], core[..]);
    assert_eq!(
        out.used_sources[core.len()..],
        [
            SourcePath::new("Game", "Debug.mshader"),
            SourcePath::new("Game", "Graphics.mshader"),
            SourcePath::new("Game", "Kernel.mshader"),
            SourcePath::new("Game", "Main.mshader"),
            SourcePath::new("Game", "Meshes.mshader"),
            SourcePath::new("Game", "Rays.mshader"),
            SourcePath::new("Game", "Textures.mshader"),
            SourcePath::new("Game", "Visibility.mshader"),
            SourcePath::new("Game", "Wide.mshader"),
        ]
    );
    // The whole reflection, but for what changes with the core library's text.
    let mut shown = r.clone();
    for source in &mut shown.sources {
        source.hash = 0;
    }
    shown.stats.instructions = 0;
    shown.stats.code_bytes = 0;
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "mirror_reflection.txt"),
        &format!("{shown:#?}\n"),
    );
}

#[test]
fn static_parameters_come_from_the_request() {
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    let consts = [
        ("Game.Rock.Factor", ConstValue::UInt(5)),
        ("Game.Rock.UseMoss", ConstValue::Bool(true)),
    ];
    let out = s.compile(&request("Game.Mirror", &consts)).unwrap();
    let statics: Vec<_> = out
        .reflection
        .static_params
        .iter()
        .map(|p| (p.name.as_str(), p.ty.as_str(), p.value.clone()))
        .collect();
    assert_eq!(
        statics,
        [
            ("Game.Rock.Factor", "uint", ConstValue::UInt(5)),
            ("Game.Rock.UseMoss", "bool", ConstValue::Bool(true)),
        ]
    );
    let exports = &out.reflection.exports;
    assert_eq!(exports[0].name, "Shape");
    assert_eq!(exports[0].ty, "Topology", "as the source writes it");
    assert_eq!(
        exports[0].value,
        ConstValue::Struct(vec![
            FieldValue {
                name: "Count".into(),
                value: ConstValue::UInt(6),
            },
            FieldValue {
                name: "Features".into(),
                value: ConstValue::UInt(3),
            },
        ])
    );
    assert_eq!(exports[1].value, ConstValue::Int(-4));
    // Another value is another permutation: other code.
    let plain = s.compile(&request("Game.Mirror", &[])).unwrap();
    assert_ne!(plain.code, out.code);
}

#[test]
fn bad_requests_are_diagnostics() {
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    let wrong_type = [("Game.Rock.Factor", ConstValue::Bool(true))];
    let out_of_range = [("Game.Rock.Factor", ConstValue::UInt(1 << 40))];
    let not_static = [("Game.Mirror", ConstValue::UInt(1))];
    let mut other_target = request("Game.Mirror", &[]);
    other_target.target = "metal-3";
    let cases = [
        ("an unknown target", other_target),
        ("an unknown entry", request("Game.Nothing", &[])),
        ("not an entry", request("Game.Flatten", &[])),
        (
            "a value of the wrong type",
            request("Game.Mirror", &wrong_type),
        ),
        (
            "a value out of range",
            request("Game.Mirror", &out_of_range),
        ),
        (
            "not a static parameter",
            request("Game.Mirror", &not_static),
        ),
        ("a capability the target lacks", request("Game.Wide", &[])),
        (
            "an implicit level in a compute entry",
            request("Game.ImplicitInCompute", &[]),
        ),
        (
            "Mesh.Dispatch not last",
            request("Game.DispatchNotLast", &[]),
        ),
        (
            "Mesh.Dispatch after a return",
            request("Game.DispatchAfterReturn", &[]),
        ),
        ("the wrong mesh index list", request("Game.WrongList", &[])),
        (
            "a mesh output before SetCounts",
            request("Game.WriteBeforeCounts", &[]),
        ),
        (
            "a format specifier in debug output",
            request("Game.FormattedPrint", &[]),
        ),
        ("a matrix in debug output", request("Game.MatrixPrint", &[])),
        (
            "a texture offset known at run time",
            request("Game.RuntimeOffset", &[]),
        ),
        (
            "a texture offset out of range",
            request("Game.FarOffset", &[]),
        ),
        (
            "a gather channel known at run time",
            request("Game.RuntimeChannel", &[]),
        ),
        (
            "a derivative in a compute entry",
            request("Game.SlopesInCompute", &[]),
        ),
        (
            "an atomic on a float texture",
            request("Game.FloatTexelAtomic", &[]),
        ),
    ];
    let mut text = String::new();
    for (what, req) in cases {
        let e = s.compile(&req).expect_err(what);
        text += &format!("== {what}\n{}\n", errors(&s, &e));
    }
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "bad_requests.txt"),
        &text,
    );
}

#[test]
fn edits_are_seen_after_invalidate() {
    let (sources, targets) = (Memory::new(), default_targets());
    let mut s = session(&sources, &targets);
    let before = s.compile(&request("Game.Mirror", &[])).unwrap();

    // A type error in a file of the package stops every entry that sees it.
    sources.set(
        "Game",
        "Broken.mshader",
        "namespace Game;\npublic uint Bad() => true;\n",
    );
    s.invalidate(&SourcePath::new("Game", "Broken.mshader"))
        .unwrap();
    let e = s.compile(&request("Game.Mirror", &[])).unwrap_err();
    let shown = errors(&s, &e);
    assert!(shown.contains("Game/Broken.mshader:2"), "{shown}");

    // Fixed: the same code as before, and the new file is a source of it.
    sources.set(
        "Game",
        "Broken.mshader",
        "namespace Game;\npublic uint Bad() => 1;\n",
    );
    s.invalidate(&SourcePath::new("Game", "Broken.mshader"))
        .unwrap();
    let after = s.compile(&request("Game.Mirror", &[])).unwrap();
    assert_eq!(after.code, before.code);
    assert!(
        after
            .used_sources
            .contains(&SourcePath::new("Game", "Broken.mshader"))
    );

    // A change to the entry's own file changes the code.
    let main = sources
        .read(&SourcePath::new("Game", "Main.mshader"))
        .unwrap()
        .replace("Shape.Count;", "Shape.Count + 1;");
    sources.set("Game", "Main.mshader", &main);
    s.invalidate(&SourcePath::new("Game", "Main.mshader"))
        .unwrap();
    let edited = s.compile(&request("Game.Mirror", &[])).unwrap();
    assert_ne!(edited.code, before.code);
}

#[test]
fn packages_come_after_their_dependencies() {
    let (sources, targets) = (Memory::new(), default_targets());
    let mut s = Session::new(SessionDesc {
        sources: &sources,
        targets: &targets,
    });
    let e = s
        .add_package(&PackageDesc {
            name: "Game",
            dependencies: &["Marlin", "Game.Shared"],
        })
        .unwrap_err();
    assert!(e.message.contains("add it first"), "{e}");
    let core = s
        .add_package(&PackageDesc {
            name: "Marlin",
            dependencies: &[],
        })
        .unwrap_err();
    assert!(core.message.contains("core library"), "{core}");
    s.add_package(&PackageDesc {
        name: "Game",
        dependencies: &["Marlin"],
    })
    .unwrap();
    let again = s
        .add_package(&PackageDesc {
            name: "Game",
            dependencies: &["Marlin"],
        })
        .unwrap_err();
    assert!(again.message.contains("already"), "{again}");
    let reload = s
        .invalidate(&SourcePath::new("Marlin", "Math.mshader"))
        .unwrap_err();
    assert!(reload.message.contains("core library"), "{reload}");
}

#[test]
fn the_fingerprint_names_the_compiler_and_the_target() {
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    let f = s.fingerprint("vulkan-1.4").unwrap();
    assert_eq!(
        session(&sources, &targets).fingerprint("vulkan-1.4"),
        Some(f)
    );
    assert_eq!(s.fingerprint("metal-3"), None);
}

#[test]
fn a_package_lists_its_public_entry_points() {
    let (sources, targets) = (Memory::new(), default_targets());
    sources.set(
        "Lib",
        "Lib.mshader",
        "namespace Lib.Passes;\n\
         [Pixel] public float4 Shade() { return float4(1, 0, 0, 1); }\n\
         [Compute(1, 1, 1)] public void Clear() {}\n\
         [Compute(1, 1, 1)] internal void Hidden() {}\n\
         [Compute(1, 1, 1)] public void Generic<T>() {}\n\
         public float Helper(float x) { return x; }\n",
    );
    sources.set(
        "Lib",
        "Inner.mshader",
        "namespace Lib.Passes.Inner;\n[Compute(1, 1, 1)] public void Nested() {}\n",
    );
    let mut s = Session::new(SessionDesc {
        sources: &sources,
        targets: &targets,
    });
    s.add_package(&PackageDesc {
        name: "Lib",
        dependencies: &["Marlin"],
    })
    .unwrap();
    let entries: Vec<(String, Stage)> = s
        .entries("Lib")
        .unwrap()
        .into_iter()
        .map(|e| (e.path, e.stage))
        .collect();
    assert_eq!(
        entries,
        [
            ("Lib.Passes.Clear".to_string(), Stage::Compute),
            ("Lib.Passes.Inner.Nested".to_string(), Stage::Compute),
            ("Lib.Passes.Shade".to_string(), Stage::Pixel),
        ]
    );
    for (entry, _) in &entries {
        s.compile(&request(entry, &[]))
            .unwrap_or_else(|e| panic!("{entry}: {}", errors(&s, &e)));
    }
    assert!(s.entries("Nope").is_err());
}

#[test]
fn sessions_are_shared_across_threads() {
    fn sync<T: Sync>(_: &T) {}
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    sync(&s);
    let one = s.compile(&request("Game.Mirror", &[])).unwrap().code;
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| s.compile(&request("Game.Mirror", &[])).unwrap().code))
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), one);
        }
    });
}

/// 64-bit atomics need their own capabilities, on buffers and on textures (2026-10-18).
#[test]
fn wide_atomics_need_their_capabilities() {
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    let caps = |entry: &str| {
        s.compile(&request(entry, &[]))
            .unwrap_or_else(|e| panic!("{entry}: {}", errors(&s, &e)))
            .reflection
            .capabilities
    };
    assert_eq!(
        caps("Game.Rasterize"),
        ["Int64", "Int64Atomics", "Int64TextureAtomics"]
    );
    assert_eq!(caps("Game.Counting"), ["Int64"]);
    assert!(caps("Game.Kernel").is_empty());
    assert!(caps("Game.Slopes").is_empty());
}

/// Khronos' spirv-val on `bytes`, when `MARLIN_SPIRV_VAL` names it (0014 §6.1).
fn external_check(bytes: &[u8], what: &str) {
    let Ok(tool) = std::env::var("MARLIN_SPIRV_VAL") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("marlin-shaderc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{what}.spv"));
    std::fs::write(&path, bytes).unwrap();
    let out = std::process::Command::new(tool)
        .args(["--target-env", "vulkan1.3", "--scalar-block-layout"])
        .arg(&path)
        .output()
        .expect("runs spirv-val");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{what}: spirv-val: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_core_library_compiles_for_vulkan() {
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    // Shipping: `Debug.Print` is not written for SPIR-V until the debug buffer exists.
    for entry in [
        "Game.Mirror",
        "Game.Kernel",
        "Game.SolidVs",
        "Game.SolidPs",
        "Game.SolidDual",
        "Game.SolidCoord",
        "Game.ColumnsVs",
        "Game.ColumnsPs",
        "Game.Instanced",
        "Game.InstancedPs",
        "Game.QuadVs",
        "Game.QuadPs",
        "Game.Levels",
        "Game.SampleOne",
        "Game.Stencil",
        "Game.Multisample",
        "Game.Cube",
        "Game.VolumeWrite",
        "Game.VolumeRead",
        "Game.TexelLoad",
        "Game.ForwardVs",
        "Game.ForwardPs",
        "Game.MeshMain",
        "Game.AmplifyMain",
        "Game.MeshAmplified",
        "Game.Trace",
        "Game.Shadowed",
        "Game.Slopes",
        "Game.Rasterize",
    ] {
        let mut req = request(entry, &[]);
        req.profile = Profile::Shipping;
        let out = s
            .compile(&req)
            .unwrap_or_else(|e| panic!("{entry}: {}", errors(&s, &e)));
        external_check(&out.code.bytes, entry);
    }
}

#[test]
fn stage_io_takes_locations_in_declaration_order() {
    use marlin_shader::reflect::StageBinding;
    let (sources, targets) = (Memory::new(), default_targets());
    let s = session(&sources, &targets);
    let io = |entry: &str| {
        let r = s
            .compile(&request(entry, &[]))
            .unwrap_or_else(|e| panic!("{entry}: {}", errors(&s, &e)))
            .reflection;
        let show = |vars: &[marlin_shader::reflect::StageVariable]| -> Vec<(String, StageBinding)> {
            vars.iter()
                .map(|v| (v.name.clone(), v.binding.clone()))
                .collect()
        };
        (show(&r.inputs), show(&r.outputs))
    };
    let sv = |n: &str| StageBinding::SystemValue(n.into());
    // The vertex outputs and the pixel inputs of one struct agree.
    let (_, vs_out) = io("Game.Instanced");
    let (ps_in, ps_out) = io("Game.InstancedPs");
    let want = vec![
        ("Pos".to_string(), sv("Position")),
        ("Instance".to_string(), StageBinding::Location(0)),
        ("Uv".to_string(), StageBinding::Location(1)),
    ];
    assert_eq!(vs_out, want);
    assert_eq!(ps_in, want);
    assert_eq!(
        ps_out,
        [(
            "target".to_string(),
            StageBinding::Target {
                index: 0,
                source: 0
            }
        )]
    );
    let (_, dual) = io("Game.SolidDual");
    assert_eq!(
        dual,
        [
            (
                "Color0".to_string(),
                StageBinding::Target {
                    index: 0,
                    source: 0
                }
            ),
            (
                "Color1".to_string(),
                StageBinding::Target {
                    index: 0,
                    source: 1
                }
            ),
        ]
    );
}

#[test]
fn type_arguments_bring_their_packages_into_the_compile() {
    let (sources, targets) = (Memory::new(), default_targets());
    sources.set(
        "Kit",
        "Kit.mshader",
        "namespace Kit;\n\
         public interface IOp { static abstract uint Apply(uint x); }\n\
         public struct Args { public Buffer<uint> Out; }\n\
         [Compute(8, 1, 1)]\n\
         public void Map<T>([DispatchThreadId] uint3 id, [Push] in Args a) where T : IOp\n\
         { a.Out[id.x] = T.Apply(id.x); }\n",
    );
    let tripler = "namespace Mod;\n\
         public struct Tripler : Kit.IOp { public static uint Apply(uint x) => x * 3; }\n";
    sources.set("Mod", "Mod.mshader", tripler);
    let mut s = Session::new(SessionDesc {
        sources: &sources,
        targets: &targets,
    });
    for (name, dependencies) in [("Kit", &["Marlin"][..]), ("Mod", &["Marlin", "Kit"][..])] {
        s.add_package(&PackageDesc { name, dependencies }).unwrap();
    }
    let request = EntryRequest {
        type_args: &["Mod.Tripler"],
        ..request("Kit.Map", &[])
    };
    let out = s
        .compile(&request)
        .unwrap_or_else(|e| panic!("{}", errors(&s, &e)));
    // Kit does not depend on Mod, but the type argument's package is an input.
    let packages: Vec<&str> = out
        .used_sources
        .iter()
        .map(|p| p.package())
        .filter(|&p| p != "Marlin")
        .collect();
    assert_eq!(packages, ["Kit", "Mod"]);
    assert!(
        out.reflection.sources.iter().any(|s| s.package == "Mod"),
        "the reflection lists it"
    );

    // And its front-end errors are the compile's.
    sources.set(
        "Mod",
        "Mod.mshader",
        &format!("{tripler}public struct Bad {{ Nope X; }}\n"),
    );
    s.invalidate(&SourcePath::new("Mod", "Mod.mshader"))
        .unwrap();
    let e = s.compile(&request).unwrap_err();
    assert!(errors(&s, &e).contains("Nope"), "{}", errors(&s, &e));
}
