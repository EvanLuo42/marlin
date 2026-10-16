//! Project-wide content checks.

mod common;

use common::{Project, U1, U2, U3, U4, U5, object};
use marlin_asset_db::{AssetDb, IssueKind};

#[test]
fn reports_every_kind_of_problem() {
    let p = Project::new("validate");
    p.write(
        "a.masset",
        &format!(
            "!Version 1\n{}\n{}",
            object(
                "Thing",
                U1,
                "First",
                // Dangling by UUID; stale path (B lives in b, not old); hand-written without a
                // UUID; a missing source file; a present one.
                &format!(
                    "    Gone = <x:Nothing #{U5}>\n    Moved = <old:B #{U3}>\n    Hand = <b:B>\n    Missing = `source:nope.png`\n    Here = `source:here.png`\n    Self = <:Second #{U2}>\n"
                ),
            ),
            object("Thing", U2, "Second", "    Lost = <b:Nobody>\n"),
        ),
    );
    p.write(
        "b.masset",
        &format!("!Version 1\n{}", object("Thing", U3, "B", "")),
    );
    // Same UUID as First.
    p.write(
        "c.masset",
        &format!("!Version 1\n{}", object("Thing", U1, "Copy", "")),
    );
    p.write("d.masset", "!Version 1\nThing { !Name = \"NoUuid\" }");
    p.write("here.png", "x");
    p.write(
        "e.masset",
        &format!(
            "!Version 1\n{}",
            object("Thing", U4, "Fine", &format!("    Ok = <b:B #{U3}>\n"))
        ),
    );

    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    let issues = db.validate();
    let lines: Vec<String> = issues.iter().map(|i| i.describe(&db)).collect();
    let kinds: Vec<&IssueKind> = issues.iter().map(|i| &i.kind).collect();
    assert_eq!(issues.len(), 7, "{lines:#?}");

    let has = |f: &dyn Fn(&IssueKind) -> bool| kinds.iter().any(|k| f(k));
    assert!(
        has(&|k| matches!(k, IssueKind::DuplicateUuid { .. })),
        "{lines:#?}"
    );
    assert!(has(
        &|k| matches!(k, IssueKind::ParseError { message } if message.contains("!Uuid"))
    ));
    assert!(has(
        &|k| matches!(k, IssueKind::DanglingLink { target: Some(_), name, .. } if name == "Nothing")
    ));
    assert!(has(
        &|k| matches!(k, IssueKind::DanglingLink { target: None, name, .. } if name == "Nobody")
    ));
    assert!(has(
        &|k| matches!(k, IssueKind::StaleLink { written, actual, .. }
        if written.0 == "old" && actual.0 == "b")
    ));
    assert!(has(
        &|k| matches!(k, IssueKind::MissingUuid { resolved, .. } if resolved.to_string() == U3)
    ));
    assert!(has(
        &|k| matches!(k, IssueKind::MissingSourceFile { path } if path == "nope.png")
    ));

    let errors = issues.iter().filter(|i| i.kind.is_error()).count();
    assert_eq!(
        errors, 5,
        "stale and UUID-less links are fixable, not errors"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("c.masset: Copy: UUID") && l.contains("a.masset:First")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "a.masset: First: link says <old:B> but its target is now <b:B>"),
        "{lines:#?}"
    );
}

#[test]
fn a_clean_project_has_no_issues() {
    let p = Project::new("clean");
    p.write(
        "a.masset",
        &format!(
            "!Version 1\n{}",
            object(
                "Thing",
                U1,
                "A",
                &format!("    B = <b:B #{U2}>\n    Src = `source:a.png`\n")
            )
        ),
    );
    p.write(
        "b.masset",
        &format!("!Version 1\n{}", object("Thing", U2, "B", "")),
    );
    p.write("a.png", "x");
    let mut db = AssetDb::build(&p.root, p.options()).unwrap();
    assert!(db.validate().is_empty());
}
