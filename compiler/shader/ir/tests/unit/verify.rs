use marlin_shaderc_ir::{parse_module, verify_module};
use marlin_test_support::{data_path, golden};

fn read(name: &str) -> String {
    std::fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), name)).unwrap()
}

fn errors(text: &str) -> Vec<String> {
    let module = parse_module(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    verify_module(&module)
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn valid_modules_verify() {
    for name in ["text/ops.ir", "text/blur.ir"] {
        assert_eq!(errors(&read(name)), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn broken_modules_are_reported() {
    let text = read("verify/bad.ir");
    let mut out = String::new();
    for case in text.split("// ==== ").skip(1) {
        let (title, body) = case.split_once('\n').unwrap_or((case, ""));
        out.push_str(&format!("== {title}\n"));
        for e in errors(body) {
            out.push_str(&e);
            out.push('\n');
        }
    }
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "verify/bad.txt"),
        &out,
    );
}

#[test]
fn stale_use_lists_are_caught() {
    // Not a golden file: those change under `MARLIN_BLESS=1` while tests run.
    let mut m = parse_module(&read("text/ops.ir")).unwrap();
    let f = m.find_function("Game.Twice").unwrap();
    let func = &mut m.funcs[f];
    // Take the add out of its block but leave its operands' uses behind.
    let entry = func.layout[0];
    let add = func.first_inst(entry).unwrap();
    func.detach(add);
    let errors: Vec<String> = verify_module(&m).iter().map(ToString::to_string).collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("use list names an operand that is not in a block")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("does not match what defines it")),
        "{errors:?}"
    );
}
