//! Pre-submit validation against a real Git repository whose assets live in a subdirectory.

mod common;

use std::process::Command;

use common::{Project, U1, U2, U3, U4, object};
use marlin_asset_db::{AssetDb, IssueKind};

fn git(p: &Project, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(&p.root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo() -> Project {
    let p = Project::new("submit");
    git(&p, &["init", "-q", "-b", "main"]);
    git(&p, &["config", "user.name", "Marlin Test"]);
    git(&p, &["config", "user.email", "test@example.invalid"]);
    git(&p, &["config", "commit.gpgsign", "false"]);
    p.write(
        "assets/a.masset",
        &format!(
            "!Version 1\n{}",
            object("Thing", U1, "A", &format!("    B = <b:B #{U2}>\n"))
        ),
    );
    p.write(
        "assets/b.masset",
        &format!("!Version 1\n{}", object("Thing", U2, "B", "")),
    );
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "base"]);
    p
}

fn kinds(issues: &[marlin_asset_db::Issue]) -> Vec<&IssueKind> {
    issues.iter().map(|i| &i.kind).collect()
}

#[test]
fn links_to_files_left_out_of_the_submit_are_errors() {
    let p = repo();
    // c links to d and uses new.png; only c is staged.
    p.write(
        "assets/c.masset",
        &format!(
            "!Version 1\n{}",
            object(
                "Thing",
                U3,
                "C",
                &format!("    D = <d:D #{U4}>\n    B = <b:B #{U2}>\n    Src = `source:new.png`\n")
            )
        ),
    );
    p.write(
        "assets/d.masset",
        &format!("!Version 1\n{}", object("Thing", U4, "D", "")),
    );
    p.write("assets/new.png", "png");
    git(&p, &["add", "assets/c.masset"]);

    let root = p.path("assets");
    let mut db = AssetDb::build(&root, p.options()).unwrap();
    let vcs = marlin_vcs::open(&root).unwrap();
    let issues = db.validate_submit(vcs.as_ref()).unwrap();
    let lines: Vec<String> = issues.iter().map(|i| i.describe(&db)).collect();
    assert_eq!(issues.len(), 2, "{lines:#?}");
    assert!(kinds(&issues).iter().any(|k| matches!(k,
        IssueKind::UnsubmittedTarget { path, .. } if path == "d.masset")));
    assert!(kinds(&issues).iter().any(|k| matches!(k,
        IssueKind::UnsubmittedSource { path } if path == "new.png")));
    assert!(
        lines.iter().all(|l| l.starts_with("c.masset: C: ")),
        "{lines:#?}"
    );

    // Adding what it points at fixes it; b was tracked all along.
    git(&p, &["add", "assets/d.masset", "assets/new.png"]);
    assert!(db.validate_submit(vcs.as_ref()).unwrap().is_empty());
}

#[test]
fn deleting_a_file_that_is_still_linked_is_an_error() {
    let p = repo();
    git(&p, &["rm", "-q", "assets/b.masset"]);
    let root = p.path("assets");
    let mut db = AssetDb::build(&root, p.options()).unwrap();
    let vcs = marlin_vcs::open(&root).unwrap();
    let issues = db.validate_submit(vcs.as_ref()).unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(matches!(&issues[0].kind,
        IssueKind::DeletedTarget { path, .. } if path == "b.masset"));
    assert_eq!(
        issues[0].describe(&db),
        "a.masset: A: links into `b.masset`, which this submit deletes"
    );
}

#[test]
fn errors_count_only_in_submitted_files() {
    let p = repo();
    p.write("assets/broken.masset", "!Version 1\nThing {");
    p.write("assets/also_broken.masset", "!Version 1\nThing {");
    git(&p, &["add", "assets/broken.masset"]);
    let root = p.path("assets");
    let mut db = AssetDb::build(&root, p.options()).unwrap();
    let vcs = marlin_vcs::open(&root).unwrap();
    let issues = db.validate_submit(vcs.as_ref()).unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(db.file_info(issues[0].file).path, "broken.masset");
    assert!(matches!(issues[0].kind, IssueKind::ParseError { .. }));
}
