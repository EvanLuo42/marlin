//! The Git backend against real throwaway repositories.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use marlin_vcs::{Change, ChangeKind, Error, VcsKind, open};

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "marlin-vcs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let repo = Self { root };
        repo.git(&["init", "-q", "-b", "main"]);
        // Local settings only: tests must not depend on the user's global configuration.
        repo.git(&["config", "user.name", "Marlin Test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo
    }

    fn git(&self, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) {
        let out = self.git(args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn commit(&self, message: &str) {
        self.ok(&["add", "-A"]);
        self.ok(&["commit", "-q", "-m", message]);
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn find<'a>(changes: &'a [Change], path: &str, staged: bool) -> Option<&'a ChangeKind> {
    changes
        .iter()
        .find(|c| c.path == path && c.staged == staged)
        .map(|c| &c.kind)
}

#[test]
fn opens_from_any_subdirectory() {
    let repo = Repo::new();
    repo.write("assets/level/a.masset", "x");
    let vcs = open(repo.root.join("assets/level")).unwrap();
    assert_eq!(vcs.kind(), VcsKind::Git);
    assert_eq!(
        fs::canonicalize(vcs.root()).unwrap(),
        fs::canonicalize(&repo.root).unwrap()
    );
    let vcs2 = open(repo.root.join("assets/level/a.masset")).unwrap();
    assert_eq!(vcs2.root(), vcs.root());
}

#[test]
fn reports_every_kind_of_change() {
    let repo = Repo::new();
    let vcs = open(&repo.root).unwrap();
    assert_eq!(vcs.revision().unwrap(), "", "no commit yet");

    repo.write("keep.masset", "1");
    repo.write("edit.masset", "1");
    repo.write("gone.masset", "1");
    repo.write(
        "move me.masset",
        "same contents for rename detection\n".repeat(20).as_str(),
    );
    repo.write("dir/ünï code.png", "1");
    repo.write(".gitignore", "*.tmp\nbuild/\n");
    repo.commit("first");
    let first = vcs.revision().unwrap();
    assert_eq!(first.len(), 40);

    repo.write("edit.masset", "2");
    fs::remove_file(repo.root.join("gone.masset")).unwrap();
    fs::create_dir_all(repo.root.join("moved")).unwrap();
    fs::rename(
        repo.root.join("move me.masset"),
        repo.root.join("moved/here.masset"),
    )
    .unwrap();
    repo.write("new.masset", "1");
    repo.write("dir/ünï code.png", "2");
    repo.write("scratch.tmp", "ignored");
    repo.write("build/out.bin", "ignored");
    repo.ok(&["add", "edit.masset", "new.masset", "moved/here.masset"]);
    repo.ok(&["rm", "-q", "--cached", "move me.masset"]);
    // Edited again after staging: staged and unstaged at once.
    repo.write("edit.masset", "3");

    let changes = vcs.status(None).unwrap();
    assert_eq!(
        find(&changes, "edit.masset", true),
        Some(&ChangeKind::Modified)
    );
    assert_eq!(
        find(&changes, "edit.masset", false),
        Some(&ChangeKind::Modified)
    );
    assert_eq!(find(&changes, "new.masset", true), Some(&ChangeKind::Added));
    assert_eq!(
        find(&changes, "gone.masset", false),
        Some(&ChangeKind::Deleted)
    );
    assert_eq!(
        find(&changes, "moved/here.masset", true),
        Some(&ChangeKind::Renamed {
            from: "move me.masset".into()
        })
    );
    assert_eq!(
        find(&changes, "dir/ünï code.png", false),
        Some(&ChangeKind::Modified)
    );
    assert!(
        changes
            .iter()
            .all(|c| !c.path.ends_with(".tmp") && !c.path.starts_with("build/"))
    );
    assert!(find(&changes, "keep.masset", false).is_none());

    let submit = vcs.submit_set().unwrap();
    let mut paths: Vec<&str> = submit.iter().map(|c| c.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["edit.masset", "moved/here.masset", "new.masset"]);

    let only = vcs.status(Some(&["dir"])).unwrap();
    assert_eq!(only.len(), 1, "{only:?}");

    repo.write("untracked/a.masset", "1");
    let changes = vcs.status(None).unwrap();
    assert_eq!(
        find(&changes, "untracked/a.masset", false),
        Some(&ChangeKind::Untracked)
    );

    repo.commit("second");
    assert_ne!(vcs.revision().unwrap(), first);
    assert!(vcs.status(None).unwrap().is_empty());
}

#[test]
fn conflicts_are_reported() {
    let repo = Repo::new();
    repo.write("a.masset", "base\n");
    repo.commit("base");
    repo.ok(&["checkout", "-q", "-b", "other"]);
    repo.write("a.masset", "other\n");
    repo.commit("other");
    repo.ok(&["checkout", "-q", "main"]);
    repo.write("a.masset", "main\n");
    repo.commit("main");
    let merge = repo.git(&["merge", "-q", "other"]);
    assert!(!merge.status.success());
    let vcs = open(&repo.root).unwrap();
    let changes = vcs.status(None).unwrap();
    assert_eq!(
        find(&changes, "a.masset", false),
        Some(&ChangeKind::Conflicted)
    );
}

#[test]
fn tracked_and_ignored_in_batches() {
    let repo = Repo::new();
    repo.write(".gitignore", "*.tmp\n");
    repo.write("a.masset", "1");
    repo.write("b c.masset", "1");
    repo.commit("first");
    repo.write("new.masset", "1");
    repo.write("x.tmp", "1");
    let vcs = open(&repo.root).unwrap();
    assert_eq!(
        vcs.tracked(&["a.masset", "b c.masset", "new.masset", "missing.masset"])
            .unwrap(),
        [true, true, false, false]
    );
    assert_eq!(
        vcs.ignored(&["x.tmp", "a.masset", "y.tmp"]).unwrap(),
        [true, false, true]
    );
    let many: Vec<String> = (0..5000).map(|i| format!("dir/file{i}.masset")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert!(vcs.tracked(&refs).unwrap().iter().all(|t| !t));
}

#[test]
fn the_pre_submit_hook_can_block_a_commit() {
    let repo = Repo::new();
    repo.write("a.masset", "1");
    repo.commit("first");
    let vcs = open(&repo.root).unwrap();
    vcs.install_pre_submit_hook("test ! -e BLOCK").unwrap();

    repo.write("b.masset", "1");
    repo.write("BLOCK", "");
    repo.ok(&["add", "b.masset"]);
    let blocked = repo.git(&["commit", "-q", "-m", "blocked"]);
    assert!(!blocked.status.success(), "the hook rejected the commit");

    fs::remove_file(repo.root.join("BLOCK")).unwrap();
    repo.ok(&["commit", "-q", "-m", "allowed"]);
}

#[test]
fn locks_need_an_lfs_server() {
    let repo = Repo::new();
    repo.write("big.png", "x");
    repo.commit("first");
    let vcs = open(&repo.root).unwrap();
    // Locking is a server operation: without LFS it is Unsupported, without a remote it fails.
    match vcs.lock(&["big.png"]) {
        Err(Error::Unsupported(_)) | Err(Error::CommandFailed { .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn outside_any_repository() {
    let dir: &Path = &std::env::temp_dir();
    // The temp directory itself is normally not in a repository.
    if dir.ancestors().any(|d| d.join(".git").exists()) {
        return;
    }
    assert!(matches!(open(dir), Err(Error::NotFound(_))));
    let p4 = std::env::temp_dir().join(format!("marlin-vcs-p4-{}", std::process::id()));
    fs::create_dir_all(&p4).unwrap();
    fs::write(p4.join(".p4config"), "P4PORT=ssl:perforce:1666\n").unwrap();
    assert!(matches!(open(&p4), Err(Error::Unsupported(_))));
    fs::remove_dir_all(&p4).unwrap();
}
