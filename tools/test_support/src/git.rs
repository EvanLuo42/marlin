use std::path::Path;
use std::process::{Command, Output};

/// Runs `git -C dir args` and returns its output, whatever its exit status.
pub fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
}

/// Runs `git -C dir args` and panics with its stderr if it fails.
pub fn git(dir: &Path, args: &[&str]) {
    let out = git_output(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Makes `dir` a repository on branch `main`, configured locally so tests do not depend on the
/// user's global Git configuration (identity, signing, line endings).
pub fn git_init(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Marlin Test"]);
    git(dir, &["config", "user.email", "test@example.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "core.autocrlf", "false"]);
}
