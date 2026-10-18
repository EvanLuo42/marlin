use marlin_shaderc_backend_spirv::{disassemble, to_bytes, validate, write_module};
use marlin_shaderc_ir::{Module, parse_module, verify_module};
use marlin_test_support::{data_path, golden};

fn fixture(name: &str) -> Module {
    let path = data_path(env!("CARGO_MANIFEST_DIR"), &format!("compute/{name}.ir"));
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let m = parse_module(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(verify_module(&m).is_empty(), "{name}");
    m
}

/// Writes the fixture's entry point; the disassembly is the golden file. With
/// `MARLIN_SPIRV_OUT`, the module also goes to that directory (to look at with other tools).
fn check(name: &str) -> Vec<u32> {
    let m = fixture(name);
    let entry = m.entries[0].func;
    let words = write_module(&m, entry).unwrap_or_else(|e| panic!("{name}: {e}"));
    golden::assert_text(
        &data_path(
            env!("CARGO_MANIFEST_DIR"),
            &format!("compute/{name}.spvasm"),
        ),
        &disassemble(&words),
    );
    if let Ok(dir) = std::env::var("MARLIN_SPIRV_OUT") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.spv")),
            to_bytes(&words),
        )
        .unwrap();
    }
    words
}

#[test]
fn compute_entries_become_spirv() {
    for name in ["mirror", "move", "branches"] {
        let words = check(name);
        let problems = validate(&words);
        assert!(problems.is_empty(), "{name}: {problems:#?}");
    }
}

/// With `MARLIN_SPIRV_VAL` set to a `spirv-val` binary, what it says about `words` (opt-in, to
/// check the writer and the validator against Khronos' tools; the tests never need it).
fn external_check(words: &[u32], what: &str) {
    let Ok(tool) = std::env::var("MARLIN_SPIRV_VAL") else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("marlin-spirv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{what}.spv"));
    std::fs::write(&path, to_bytes(words)).unwrap();
    let out = std::process::Command::new(tool)
        .args(["--target-env", "vulkan1.3", "--scalar-block-layout"])
        .arg(&path)
        .output()
        .expect("runs spirv-val");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{what}: spirv-val: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        disassemble(words)
    );
}

#[test]
fn random_programs_become_spirv() {
    use marlin_shaderc_opt::{Options, Profile, pipeline};
    let base = marlin_test_support::seed(0x09_a001);
    let options = Options {
        profile: Profile::Shipping,
        verify_each: true,
    };
    for case in 0..marlin_test_support::scale(100) as u64 {
        let seed = base.wrapping_add(case);
        let (mut m, f) = marlin_shaderc_testgen::function(seed, 30);
        let entry = marlin_shaderc_testgen::compute_entry(&mut m, f);
        let _ = entry;
        pipeline(&options)
            .run(&mut m)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        let entry = m.entries[0].func;
        let words = write_module(&m, entry).unwrap_or_else(|e| {
            panic!("seed {seed}: {e}\n{}", marlin_shaderc_ir::print_module(&m))
        });
        let problems = validate(&words);
        assert!(
            problems.is_empty(),
            "seed {seed}: {problems:#?}
{}",
            disassemble(&words)
        );
        external_check(&words, &format!("random-{seed}"));
    }
}
