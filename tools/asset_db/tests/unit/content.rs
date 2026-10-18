//! Content of several mounts: links across them, and what validation sees.

use marlin_asset_db::{Content, ENGINE_MOUNT, GAME_MOUNT, IssueKind, Mount, MountId};
use marlin_masset::Links;

use crate::support::{Project, U1, U2, U3, U4, U5, object};

/// engine/core: Core (U1), Up (U2, links up into the game's A); game/a: A (U3, links Core by
/// qualified path), Stale (U4, links Core under an old path), Hand (U5, no UUID); game/twin:
/// Twin (U2 again, the engine's Up has it).
fn mounts(engine: &Project, game: &Project) -> Content {
    engine.write(
        "core.masset",
        format!(
            "!Version 1\n{}{}",
            object("Thing", U1, "Core", ""),
            object("Thing", U2, "Up", &format!("    To = </Game/a:A #{U3}>\n")),
        ),
    );
    game.write(
        "a.masset",
        format!(
            "!Version 1\n{}{}{}",
            object(
                "Thing",
                U3,
                "A",
                &format!("    To = </Engine/core:Core #{U1}>\n")
            ),
            object(
                "Thing",
                U4,
                "Stale",
                &format!("    To = </Engine/old:Core #{U1}>\n")
            ),
            object("Thing", U5, "Hand", "    To = </Engine/core:Core>\n"),
        ),
    );
    game.write(
        "twin.masset",
        format!("!Version 1\n{}", object("Thing", U2, "Twin", "")),
    );
    Content::new(vec![
        Mount {
            name: ENGINE_MOUNT.into(),
            db: engine.db(),
        },
        Mount {
            name: GAME_MOUNT.into(),
            db: game.db(),
        },
    ])
}

#[test]
fn links_resolve_downward_only() {
    let (engine, game) = (Project::new("mounts-engine"), Project::new("mounts-game"));
    let content = mounts(&engine, &game);
    let (e, g) = (MountId(0), MountId(1));
    assert_eq!(content.find_mount(GAME_MOUNT), Some(g));
    let a = content.object(U3).unwrap();
    assert_eq!(content.path(content.object_file(a)), "/Game/a");
    let core = content.object(U1).unwrap();
    assert_eq!(core.mount, e);
    // The game sees the engine, by UUID and by path; links from it write the path qualified.
    assert_eq!(content.visible_object(g, U1), Some(core));
    assert_eq!(content.links(g).resolve("/Engine/core", "Core"), Some(U1));
    assert_eq!(
        content.links(g).describe(U1),
        Some(("/Engine/core".into(), "Core".into()))
    );
    assert_eq!(
        content.links(g).describe(U3),
        Some(("a".into(), "A".into()))
    );
    // The engine does not see the game.
    assert_eq!(content.visible_object(e, U3), None);
    assert_eq!(content.links(e).resolve("/Game/a", "A"), None);
    assert_eq!(
        content.file("/Engine/core.masset").map(|f| f.mount),
        Some(e)
    );
}

#[test]
fn validation_resolves_across_mounts() {
    let (engine, game) = (
        Project::new("validate-engine"),
        Project::new("validate-game"),
    );
    let mut content = mounts(&engine, &game);
    let issues = content.validate();
    let lines: Vec<String> = issues.iter().map(|i| content.describe(i)).collect();
    assert_eq!(issues.len(), 4, "{lines:#?}");
    let has = |mount: usize, f: &dyn Fn(&IssueKind) -> bool| {
        issues
            .iter()
            .any(|i| i.mount == MountId(mount) && f(&i.issue.kind))
    };
    // Up points into the game: dangling from the engine's side.
    assert!(
        has(
            0,
            &|k| matches!(k, IssueKind::DanglingLink { name, .. } if name == "A")
        ),
        "{lines:#?}"
    );
    assert!(
        has(1, &|k| matches!(
            k,
            IssueKind::StaleLink { actual, .. } if actual.0 == "/Engine/core"
        )),
        "{lines:#?}"
    );
    assert!(
        has(1, &|k| matches!(
            k,
            IssueKind::MissingUuid { resolved, .. } if *resolved == U1
        )),
        "{lines:#?}"
    );
    assert!(
        has(1, &|k| matches!(
            k,
            IssueKind::DuplicateUuidBelow { uuid, first }
                if *uuid == U2 && first == "/Engine/core.masset:Up"
        )),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with("/Engine/") || l.starts_with("/Game/")),
        "{lines:#?}"
    );
}
