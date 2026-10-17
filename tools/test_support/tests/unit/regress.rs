use marlin_test_support::{TempDir, panic_message, regress};

/// A crate directory with `tests/data/regress/<target>/` holding `inputs`.
fn crate_with(inputs: &[(&str, &[u8])]) -> TempDir {
    let dir = TempDir::new("regress");
    for (name, bytes) in inputs {
        dir.write(format!("tests/data/regress/t/{name}"), bytes);
    }
    dir
}

#[test]
fn replays_every_input_in_name_order_and_skips_dot_files() {
    let dir = crate_with(&[("b", b"2"), ("a", b"1"), (".gitattributes", b"* binary")]);
    let mut seen = Vec::new();
    let n = regress::for_each(dir.path().to_str().unwrap(), "t", |bytes| {
        seen.push(bytes.to_vec())
    });
    assert_eq!(n, 2);
    assert_eq!(seen, [b"1".to_vec(), b"2".to_vec()]);
}

#[test]
fn a_target_without_inputs_replays_nothing() {
    let dir = TempDir::new("regress");
    assert_eq!(
        regress::for_each(dir.path().to_str().unwrap(), "none", |_| unreachable!()),
        0
    );
}

#[test]
fn every_input_runs_and_each_panic_is_named() {
    let dir = crate_with(&[("crash-1", b"x"), ("ok", b"y"), ("crash-2", b"x")]);
    let mut runs = 0;
    let message = panic_message(|| {
        regress::for_each(dir.path().to_str().unwrap(), "t", |bytes| {
            runs += 1;
            assert!(bytes != b"x", "bad input");
        });
    });
    assert_eq!(runs, 3);
    assert!(message.contains("2 of 3"), "{message}");
    assert!(message.contains("crash-1: bad input"), "{message}");
    assert!(message.contains("crash-2"), "{message}");
}
