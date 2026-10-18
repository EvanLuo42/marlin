use marlin_test_support::{data_path, golden};

use crate::support::world;

#[test]
fn barriers_some_threads_may_miss_are_errors() {
    let w = world(&["game/Barriers.mshader"]);
    let mut text = String::new();
    for path in [
        "Game.Divergent",
        "Game.AfterReturn",
        "Game.InHelper",
        "Game.Together",
    ] {
        text.push_str(&format!("== {path}\n{}\n", w.problems(path)));
    }
    golden::assert_text(
        &data_path(env!("CARGO_MANIFEST_DIR"), "barriers.txt"),
        &text,
    );
    assert!(w.problems("Game.Together").is_empty());
}
