//! Config layers: priority, which layer set a value, loading, reloading and removing layers,
//! platform and device-profile sections, writing a layer. One test: layers span the registry.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the cvar registry is global.
//! Run alone with `cargo test -p marlin-config --test isolated_layers`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_config::{
    ApplyError, Cause, CvarBool, CvarI64, CvarString, Error, Location, Source, apply_command_line,
    apply_config, clear_layer, declare, load_device_profile, load_layer, load_platform,
    load_section, on_change, write_layer,
};

declare! {
    static FPS: CvarI64 = CvarI64::new("l.fps", 0, "Frame cap.").range(0, 1000);
    static VSYNC: CvarBool = CvarBool::new("l.vsync", true, "");
    static TITLE: CvarString = CvarString::new("l.title", "Marlin", "");
    static AUDIO: CvarString = CvarString::new("l.audio", "default", "");
}

fn layer(source: Source) -> String {
    let mut out = String::new();
    write_layer(source, &mut out).unwrap();
    out
}

#[test]
fn layers() {
    assert_eq!(
        Source::LAYERS.map(|s| s.to_string()),
        [
            "engine",
            "platform",
            "scalability",
            "project",
            "device profile",
            "user",
            "command line",
            "console",
            "server"
        ]
    );

    // Each layer holds its own value; the highest one decides.
    assert_eq!(
        load_layer(Source::Engine, "l.fps = 30\nl.vsync = false"),
        []
    );
    assert_eq!((FPS.get(), FPS.set_by()), (30, Some(Source::Engine)));
    assert_eq!(load_layer(Source::User, "l.fps = 144"), []);
    assert_eq!(
        load_layer(Source::Project, "[l]\nfps = 60\ntitle = \"Game\""),
        []
    );
    assert_eq!((FPS.get(), FPS.set_by()), (144, Some(Source::User)));
    assert_eq!(apply_command_line(["+l.fps", "200"]), []);
    FPS.set(500);
    assert_eq!((FPS.get(), FPS.set_by()), (500, Some(Source::Code)));
    // Code and the console share a layer: the later write wins.
    FPS.set_str("400", Source::Console).unwrap();
    assert_eq!((FPS.get(), FPS.set_by()), (400, Some(Source::Console)));
    let held: Vec<_> = Source::LAYERS
        .iter()
        .filter_map(|&s| Some((s, FPS.layer_string(s)?)))
        .collect();
    assert_eq!(
        held,
        [
            (Source::Engine, "30".into()),
            (Source::Project, "60".into()),
            (Source::User, "144".into()),
            (Source::CommandLine, "200".into()),
            (Source::Console, "400".into()),
        ]
    );
    assert_eq!(FPS.layer_string(Source::Code).as_deref(), Some("400"));

    // Removing values falls back layer by layer.
    FPS.unset(Source::Console).unwrap();
    assert_eq!(FPS.get(), 200);
    clear_layer(Source::CommandLine);
    assert_eq!((FPS.get(), FPS.set_by()), (144, Some(Source::User)));

    // Reloading a layer replaces it: what the new text leaves out falls back, and each cvar
    // changes once.
    let changes = Arc::new(AtomicUsize::new(0));
    let id = on_change(&FPS, {
        let changes = changes.clone();
        move |_| {
            changes.fetch_add(1, Ordering::SeqCst);
        }
    });
    assert_eq!(
        load_layer(Source::User, "l.fps = 90\nl.title = \"Mine\""),
        []
    );
    assert_eq!((FPS.get(), TITLE.get()), (90, "Mine".into()));
    assert_eq!(load_layer(Source::User, "l.title = \"Mine\""), []);
    assert_eq!((FPS.get(), FPS.set_by()), (60, Some(Source::Project)));
    assert_eq!(changes.load(Ordering::SeqCst), 2);
    // `apply_config` adds to a layer instead.
    assert_eq!(apply_config("l.audio = \"wasapi\"", Source::User), []);
    assert_eq!(TITLE.get(), "Mine");
    assert!(marlin_config::remove_callback(id));
    // A layer that fails to validate keeps only what did.
    let errors = load_layer(Source::User, "l.fps = 5000\nl.vsync = true");
    assert_eq!(errors.len(), 1);
    assert_eq!(FPS.get(), 60);
    assert_eq!(VSYNC.set_by(), Some(Source::User));
    assert_eq!(TITLE.get(), "Game", "the reload dropped the user's title");

    // Writing a layer, and reading it back.
    assert_eq!(layer(Source::User), "# The user layer.\nl.vsync = true\n");
    assert_eq!(
        layer(Source::Project),
        "# The project layer.\n# Frame cap.\nl.fps = 60\nl.title = \"Game\"\n"
    );
    let project = layer(Source::Project);
    clear_layer(Source::Project);
    assert_eq!((FPS.get(), TITLE.get()), (30, "Marlin".into()));
    assert_eq!(load_layer(Source::Project, &project), []);
    assert_eq!((FPS.get(), TITLE.get()), (60, "Game".into()));

    // Platform sections: the one named, any case; none for this platform is fine.
    let platforms = "[windows]\nl.audio = \"wasapi\"\n\n[Linux]\nl.audio = \"pipewire\"\n";
    assert_eq!(apply_config("l.audio = \"alsa\"", Source::User), []);
    assert_eq!(load_platform(platforms, "linux"), []);
    assert_eq!(
        (AUDIO.layer_string(Source::Platform).as_deref(), AUDIO.get()),
        (Some("pipewire"), "alsa".into()),
        "the user layer is above"
    );
    clear_layer(Source::User);
    assert_eq!(AUDIO.get(), "pipewire");
    assert_eq!(load_platform(platforms, "ps5"), []);
    assert_eq!(AUDIO.get(), "default");

    // Device profiles: a missing one is an error, and leaves the layer empty.
    let profiles = r#"
[steamdeck]
l.fps = 40
l.title = "Deck"

[desktop]
l = { fps = 0 }
"#;
    assert_eq!(load_device_profile(profiles, "SteamDeck"), []);
    assert_eq!((FPS.get(), FPS.set_by()), (40, Some(Source::DeviceProfile)));
    assert_eq!(TITLE.get(), "Deck");
    assert_eq!(load_device_profile(profiles, "desktop"), []);
    assert_eq!((FPS.get(), TITLE.get()), (0, "Game".into()));
    assert_eq!(
        load_device_profile(profiles, "phone"),
        [ApplyError {
            location: Location::Document,
            key: "phone".into(),
            source: Cause::NoSection,
        }]
    );
    assert_eq!(FPS.get(), 60);
    assert_eq!(
        load_section(Source::DeviceProfile, "steamdeck = 1\n", "steamdeck"),
        [ApplyError {
            location: Location::Line(1),
            key: "steamdeck".into(),
            source: Cause::NoSection,
        }]
    );
    let errors = load_section(Source::DeviceProfile, profiles, "steamdeck");
    assert_eq!(errors, []);
    let errors = load_section(Source::DeviceProfile, "[x]\nl.nope = 1", "x");
    assert_eq!(errors[0].source, Cause::Set(Error::Unknown));
    assert_eq!(errors[0].to_string(), "line 2: `l.nope`");
}
