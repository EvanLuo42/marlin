//! Fuzz regressions: inputs a fuzz target crashed on, kept under
//! `<crate>/tests/data/regress/<target>/` and replayed by a `unit` test, so a fixed crash stays
//! fixed in the quick tier (docs/design/0014-testing.md §7.3). `cargo xtask test --tier fuzz`
//! copies new crashes there.
//!
//! ```ignore
//! #[test]
//! fn damaged_messages_are_errors_never_panics() {
//!     regress::for_each(env!("CARGO_MANIFEST_DIR"), "text_message", |bytes| {
//!         // The body of fuzz/fuzz_targets/text_message.rs.
//!     });
//! }
//! ```

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

/// `tests/data/regress/<target>` of the crate. Pass `env!("CARGO_MANIFEST_DIR")`.
pub fn dir(manifest_dir: &str, target: &str) -> PathBuf {
    crate::data_path(manifest_dir, "regress").join(target)
}

/// Calls `f` with every input in [`dir`]`(manifest_dir, target)`, in file name order, and
/// returns how many there were (none when the directory does not exist yet). Every input runs
/// even when an earlier one panics; afterwards, panics naming each input that did. Dot files
/// (`.gitattributes`) are not inputs.
pub fn for_each(manifest_dir: &str, target: &str, mut f: impl FnMut(&[u8])) -> usize {
    let dir = dir(manifest_dir, target);
    let Ok(entries) = fs::read_dir(&dir) else {
        return 0;
    };
    let mut inputs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .is_some_and(|n| !n.to_string_lossy().starts_with('.'))
        })
        .collect();
    inputs.sort();
    let mut failed = Vec::new();
    for path in &inputs {
        let bytes = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| f(&bytes))) {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("<non-string panic>");
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            failed.push(format!("  {name}: {message}"));
        }
    }
    if !failed.is_empty() {
        panic!(
            "{} of {} regression inputs in {} panicked:\n{}",
            failed.len(),
            inputs.len(),
            dir.display(),
            failed.join("\n")
        );
    }
    inputs.len()
}
