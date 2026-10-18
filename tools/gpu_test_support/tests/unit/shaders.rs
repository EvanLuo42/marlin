use marlin_gpu_test_support::Shader;

/// Every entry of the shared package, with its stage.
const ENTRIES: &[(&str, &str)] = &[
    ("Fill", "Compute"),
    ("Count", "Compute"),
    ("QuadVs", "Vertex"),
    ("QuadPs", "Pixel"),
    ("SolidVs", "Vertex"),
    ("SolidPs", "Pixel"),
    ("SolidDual", "Pixel"),
    ("SolidCoord", "Pixel"),
    ("ColumnsVs", "Vertex"),
    ("ColumnsPs", "Pixel"),
    ("ColumnsMesh", "Mesh"),
    ("ColumnsAmplify", "Amplification"),
    ("ColumnsAmplified", "Mesh"),
    ("SampleOne", "Compute"),
    ("SampleStencil", "Compute"),
    ("SampleMultisample", "Compute"),
    ("SampleCube", "Compute"),
    ("VolumeWrite", "Compute"),
    ("VolumeRead", "Compute"),
    ("SignedWrite", "Compute"),
    ("SignedRead", "Compute"),
    ("GatherRed", "Compute"),
    ("SampleOffsets", "Compute"),
    ("CompareDepth", "Compute"),
    ("Queries", "Compute"),
    ("TraceRays", "Compute"),
    ("Prints", "Compute"),
    ("Slopes", "Pixel"),
    ("SlopeWidths", "Pixel"),
    ("Wide", "Compute"),
    ("VisClear", "Compute"),
    ("VisRaster", "Compute"),
    ("Atomic32", "Compute"),
];

/// Every entry of the engine's package, with its stage.
const ENGINE_ENTRIES: &[(&str, &str)] = &[
    ("Marlin.Engine.Lighting.DirectLight", "Compute"),
    ("Marlin.Engine.Visibility.VisMesh", "Mesh"),
    ("Marlin.Engine.Visibility.VisPs", "Pixel"),
];

/// Khronos' spirv-val on `bytes`, when `MARLIN_SPIRV_VAL` names it (0014 §6.1).
fn external_check(bytes: &[u8], what: &str) {
    let Ok(tool) = std::env::var("MARLIN_SPIRV_VAL") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("marlin-gpu-test-support-{}", std::process::id()));
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
fn every_shared_shader_compiles() {
    for &(entry, stage) in ENTRIES {
        let shader = Shader::new(entry);
        let reflection = shader.reflection();
        assert_eq!(reflection.entry.name, format!("Test.{entry}"));
        assert_eq!(format!("{:?}", reflection.entry.stage), stage, "{entry}");
        let module = shader.module();
        assert_eq!(module.entry, "main");
        external_check(module.code, entry);
    }
}

#[test]
fn every_engine_shader_compiles() {
    for &(entry, stage) in ENGINE_ENTRIES {
        let shader = Shader::new(entry);
        let reflection = shader.reflection();
        assert_eq!(reflection.entry.name, entry);
        assert_eq!(format!("{:?}", reflection.entry.stage), stage, "{entry}");
        assert!(
            reflection
                .sources
                .iter()
                .any(|s| s.package == marlin_gpu_test_support::ENGINE),
            "{entry} comes from content/engine/shaders"
        );
        external_check(shader.module().code, entry);
    }
}

#[test]
fn compiled_shaders_are_kept() {
    let a = Shader::new("Fill").module().code;
    let b = Shader::new("Test.Fill").module().code;
    assert_eq!(a, b);
    assert!(std::ptr::eq(a, Shader::new("Fill").module().code));
}

#[test]
#[should_panic(expected = "test shader `Test.Nothing` does not compile")]
fn an_unknown_entry_panics_with_the_diagnostics() {
    Shader::new("Nothing").module();
}

#[test]
fn zz_dump_tmp() {
    if let Ok(dir) = std::env::var("ZZ_DUMP") {
        for e in ["SignedWrite", "SignedRead"] {
            std::fs::write(format!("{dir}/{e}.spv"), Shader::new(e).module().code).unwrap();
        }
    }
}
