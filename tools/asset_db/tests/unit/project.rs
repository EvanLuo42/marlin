//! Project files and where the engine is found.

use marlin_asset_db::{ENGINE_MOUNT, GAME_MOUNT, Options, Project, ProjectError};
use marlin_test_support::TempDir;

fn engine_at(dir: &TempDir, rel: &str) {
    dir.write(format!("{rel}/content/engine/core.masset"), "!Version 1\n");
}

fn options() -> Options {
    Options {
        journal: false,
        ..Options::default()
    }
}

#[test]
fn the_engine_is_named_or_found_above() {
    let base = TempDir::new("asset-db-project");
    engine_at(&base, "engine");
    base.write(
        "engine/samples/demo/Demo.mproject",
        "[project]\nname = \"Demo\"\n",
    );
    base.write("engine/samples/demo/content/a.masset", "!Version 1\n");
    base.write(
        "games/mine/Mine.mproject",
        "[project]\nname = \"Mine\"\nengine = \"../../engine\"\n",
    );
    base.write("games/mine/content/b.masset", "!Version 1\n");
    let demo = Project::open(base.join("engine/samples/demo")).unwrap();
    let engine = demo.engine.clone();
    assert!(engine.ends_with("engine"), "{}", engine.display());

    // In the engine's tree: found above, by the directory or by the file.
    assert_eq!(demo.name, "Demo");
    assert_eq!(
        Project::open(base.join("engine/samples/demo/Demo.mproject")).unwrap(),
        demo
    );
    // On its own: named in the file.
    let mine = Project::open(base.join("games/mine")).unwrap();
    assert_eq!(mine.engine, engine);
    let names: Vec<&str> = mine.mounts().iter().map(|m| m.0).collect();
    assert_eq!(names, [ENGINE_MOUNT, GAME_MOUNT]);

    // Both mounts indexed, each under the project's own `.marlin/index/<mount>`: nothing is
    // written into the engine, which may be installed read-only.
    let mut content = mine.build_content(&options()).unwrap();
    assert_eq!(content.file_count(), 2);
    content.save().unwrap();
    assert!(mine.data_dir().join("index/Engine").is_dir());
    assert!(mine.data_dir().join("index/Game").is_dir());
    assert!(!base.join("engine/content/engine/.marlin").exists());
    let (reopened, _) = mine.open_content(&options()).unwrap();
    assert_eq!(reopened.file_count(), 2);

    // A tool can bring its own engine.
    engine_at(&base, "other");
    let other = Project::open_with(base.join("games/mine"), Some(&base.join("other"))).unwrap();
    assert!(
        other.engine.ends_with("other"),
        "{}",
        other.engine.display()
    );
}

#[test]
fn bad_project_files_are_reported() {
    let base = TempDir::new("asset-db-project-bad");
    engine_at(&base, "engine");
    let open = |rel: &str, text: &str| {
        base.write(format!("{rel}/P.mproject"), text);
        Project::open(base.join(rel))
    };
    let invalid = |r: Result<Project, ProjectError>, what: &str| match r {
        Err(ProjectError::Invalid { message, .. }) => {
            assert!(message.contains(what), "{message}")
        }
        other => panic!("{other:?}"),
    };
    invalid(open("engine/a", "[project]\n"), "project.name");
    invalid(
        open("engine/b", "[project]\nname = \"B\"\ncolor = 1\n"),
        "project.color",
    );
    invalid(open("engine/c", "[other]\n"), "other");
    invalid(
        open(
            "engine/d",
            "[project]\nname = \"D\"\nengine = \"nowhere\"\n",
        ),
        "nowhere",
    );
    assert!(matches!(
        Project::open(base.join("engine")),
        Err(ProjectError::NoProjectFile { .. })
    ));
    base.write("engine/e/One.mproject", "[project]\nname = \"One\"\n");
    base.write("engine/e/Two.mproject", "[project]\nname = \"Two\"\n");
    assert!(matches!(
        Project::open(base.join("engine/e")),
        Err(ProjectError::Ambiguous { .. })
    ));
    // A project without its content directory is reported when its content is opened.
    let p = Project::open(base.join("engine/e/One.mproject")).unwrap();
    assert!(p.build_content(&options()).is_err());
}
