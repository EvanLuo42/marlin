//! Tree snapshots: every `tests/data/parser/{ok,err}/*.src` file is parsed with the superset
//! dialect and its dump (tree, then errors) compared with the golden `.tree` file beside it.
//! `ok` files must parse without errors, `err` files with at least one. `MARLIN_BLESS=1`
//! rewrites the `.tree` files.

use std::fs;
use std::path::PathBuf;

use marlin_front_syntax::{Dialect, parse};
use marlin_test_support::{data_path, golden};

fn sources(sub: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(data_path(env!("CARGO_MANIFEST_DIR"), sub))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "src"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no test files in tests/data/{sub}");
    files
}

fn check_dir(sub: &str, expect_errors: bool) {
    for file in sources(sub) {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&file).unwrap().replace("\r\n", "\n");
        let parse = parse(&text, &Dialect::SUPERSET);
        assert_eq!(
            parse.syntax_node().text(),
            text,
            "{name}: the tree lost text"
        );
        assert_eq!(
            !parse.errors().is_empty(),
            expect_errors,
            "{name}: expected {}errors, got {:#?}",
            if expect_errors { "" } else { "no " },
            parse.errors()
        );
        golden::assert_text(&file.with_extension("tree"), &parse.debug_dump());
    }
}

#[test]
fn valid_sources_parse_cleanly_into_their_snapshot_trees() {
    check_dir("parser/ok", false);
}

#[test]
fn broken_sources_report_errors_and_recover_into_their_snapshot_trees() {
    check_dir("parser/err", true);
}
