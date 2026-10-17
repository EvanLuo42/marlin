use marlin_shaderc_ir::{parse_module, print_module};
use marlin_test_support::{data_path, golden};

fn read(name: &str) -> String {
    std::fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), name)).unwrap()
}

/// Parses `name`, checks the printed form against `name.txt`, and that printing is a fixed
/// point of parsing.
fn round_trip(name: &str) {
    let module = parse_module(&read(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let printed = print_module(&module);
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), &format!("{name}.txt")),
        &printed,
    );
    let again = parse_module(&printed).unwrap_or_else(|e| panic!("{name} printed: {e}\n{printed}"));
    assert_eq!(
        print_module(&again),
        printed,
        "printing {name} is not a fixed point"
    );
}

#[test]
fn every_operation_round_trips() {
    round_trip("text/ops.ir");
}

#[test]
fn nested_instructions_expand_in_order() {
    round_trip("text/blur.ir");
}

#[test]
fn errors_say_where() {
    let cases = [
        (
            "func @f() {\nblock0:\n    %x = add %y, %y\n}\n",
            "3:14: `%y` is not defined above",
        ),
        (
            "func @f() {\nblock0:\n    br nowhere\n}\n",
            "3:8: no block `nowhere`",
        ),
        (
            "struct @S { A: f32 }\nfunc @f(%s: @S) {\nblock0:\n    %x = extract.field %s, .B\n}\n",
            "4:29: struct `S` has no field `B`",
        ),
        (
            "func @f() {\nblock0:\n    %x = frobnicate\n}\n",
            "3:10: unknown operation `frobnicate`",
        ),
        (
            "func @f(%a: i32) {\nblock0:\n    %x = load %a\n}\n",
            "3:10: expected a pointer",
        ),
        (
            "func @f() {\nblock0:\n    %x, %y = add i32 1, i32 2\n}\n",
            "3:5: this instruction has 1 result(s), 2 named",
        ),
        ("global @g : f32\n", "1:13: a global's type is a pointer"),
        (
            "func @f() {\nblock0:\n    return\n",
            "4:1: unterminated function body",
        ),
    ];
    for (text, expected) in cases {
        let error = parse_module(text).map(|_| ()).unwrap_err();
        assert_eq!(error.to_string(), expected, "for:\n{text}");
    }
}
