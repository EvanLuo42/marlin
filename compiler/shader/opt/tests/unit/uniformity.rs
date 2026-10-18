use marlin_shaderc_ir::{Block, Function, Value, parse_module, print_module};
use marlin_shaderc_opt::uniformity::{Level, Uniformity, analyze};
use marlin_test_support::{data_path, golden};

/// Names as the text format prints them (the parser keeps source names as hints).
fn value_name(f: &Function, v: Value) -> String {
    f.values[v]
        .name
        .as_deref()
        .map_or_else(|| format!("{v:?}"), |n| format!("%{n}"))
}

fn block_name(f: &Function, b: Block) -> String {
    format!(
        "block{}",
        f.layout.iter().position(|&x| x == b).unwrap_or(usize::MAX)
    )
}

fn show(f: &Function, u: &Uniformity) -> String {
    let mut values: Vec<String> = f
        .values
        .iter()
        .filter(|(v, data)| u.is_divergent(*v) && data.name.is_some())
        .map(|(v, _)| value_name(f, v))
        .collect();
    values.sort();
    let mut blocks: Vec<String> = u
        .divergent_blocks
        .keys()
        .map(|&b| block_name(f, b))
        .collect();
    blocks.sort();
    format!(
        "  values: {}\n  blocks: {}\n",
        values.join(" "),
        blocks.join(" ")
    )
}

#[test]
fn values_and_blocks_that_differ_between_lanes() {
    let path = data_path(env!("CARGO_MANIFEST_DIR"), "uniformity.ir");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    let m = parse_module(&text).unwrap();
    assert!(
        marlin_shaderc_ir::verify_module(&m).is_empty(),
        "{}",
        print_module(&m)
    );
    let mut out = String::new();
    for (_, f) in m.funcs.iter() {
        for (level, name) in [(Level::Wave, "wave"), (Level::Group, "group")] {
            out.push_str(&format!("@{} across a {name}\n", f.name));
            out.push_str(&show(f, &analyze(&m, f, level)));
        }
    }
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "uniformity.txt"),
        &out,
    );
}
