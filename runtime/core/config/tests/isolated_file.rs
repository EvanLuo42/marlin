//! TOML config files, command-line overrides and archiving. One test: archives cover the registry.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the cvar registry is global.
//! Run alone with `cargo test -p marlin-config --test isolated_file`.

use marlin_config::{
    ApplyError, Bounds, Cause, CvarBool, CvarEnum, CvarF64, CvarI64, CvarString, Error, Expected,
    Location, Source, apply_command_line, apply_config, archive_string, clear_layer,
    finish_startup, load_layer, register_all,
};

static VSYNC: CvarBool = CvarBool::new("r.vsync", true, "Wait for vertical blank.").archive();
static FPS: CvarI64 = CvarI64::new("r.maxFps", 0, "Frame cap.")
    .range(0, 1000)
    .archive();
static SCALE: CvarF64 = CvarF64::new("r.scale", 1.0, "").range(0.25, 4.0).archive();
static NAME: CvarString =
    CvarString::new("player.name", "Player", "Display name.\nShown to others.").archive();
static QUALITY: CvarEnum = CvarEnum::new("r.quality", 1, &["low", "medium", "high"], "").archive();
static WINDOW: CvarI64 = CvarI64::new("r.width", 1280, "")
    .archive()
    .requires_restart();
/// Not archived.
static DEBUG: CvarBool = CvarBool::new("debug.draw", false, "");

fn err(location: Location, key: &str, source: Error) -> ApplyError {
    ApplyError {
        location,
        key: key.into(),
        source: Cause::Set(source),
    }
}

#[test]
fn files_and_command_line() {
    register_all(&[&VSYNC, &FPS, &SCALE, &NAME, &QUALITY, &WINDOW, &DEBUG]).unwrap();

    let config = r#"# Settings
player.name = "Ann \"The\" # One\\"   # trailing comment
debug.draw = true
unknown.key = 3

[r]
vsync = false
maxFps = 5000
scale = 2                 # an integer for a float
quality = "HIGH"
width = 1920

[R]                       # names ignore case
MAXFPS = 144
SCALE = "big"
QUALITY = [1]
"#;
    let errors = apply_config(config, Source::Project);
    assert_eq!(
        errors,
        [
            err(Location::Line(4), "unknown.key", Error::Unknown),
            err(
                Location::Line(8),
                "r.maxFps",
                Error::OutOfRange(Bounds::Int(0, 1000))
            ),
            err(
                Location::Line(15),
                "R.SCALE",
                Error::Invalid(Expected::Float)
            ),
            err(
                Location::Line(16),
                "R.QUALITY",
                Error::Invalid(Expected::Enum(&["low", "medium", "high"]))
            ),
        ]
    );
    assert_eq!(errors[0].to_string(), "line 4: `unknown.key`");
    assert_eq!(errors[0].source.to_string(), "unknown cvar");
    assert!(!VSYNC.get());
    assert_eq!(FPS.get(), 144);
    assert_eq!(SCALE.get(), 2.0);
    assert_eq!(NAME.get(), "Ann \"The\" # One\\");
    assert_eq!(QUALITY.get_name(), "high");
    assert!(DEBUG.get());
    assert_eq!(WINDOW.get(), 1920, "startup: takes effect at once");

    // Inline tables and enum indices; strings are not bools in TOML.
    let errors = apply_config("r = { quality = 0, vsync = \"on\" }", Source::Project);
    assert_eq!(
        errors,
        [err(
            Location::Line(1),
            "r.vsync",
            Error::Invalid(Expected::Bool)
        )]
    );
    assert_eq!(QUALITY.get_name(), "low");

    // Malformed TOML: the syntax error is reported, and what parses still applies.
    let errors = apply_config("r.maxFps = 60\nthis is not toml\n", Source::Project);
    assert!(!errors.is_empty());
    assert!(
        errors
            .iter()
            .all(|e| { e.location == Location::Line(2) && matches!(e.source, Cause::Syntax(_)) })
    );
    assert!(errors[0].source.to_string().starts_with("syntax error: "));
    assert_eq!(FPS.get(), 60);

    let args = [
        "game.exe",
        "+r.maxFps",
        "70",
        "positional",
        "+r.scale=2.5",
        "+player.name",
        "+not a cvar",
        "+r.nope",
        "1",
        "+",
        "x",
        "+r.vsync",
    ];
    let errors = apply_command_line(args);
    assert_eq!(
        errors,
        [
            err(Location::Arg(7), "r.nope", Error::Unknown),
            ApplyError {
                location: Location::Arg(9),
                key: String::new(),
                source: Cause::Syntax("expected `+name value`".into()),
            },
            ApplyError {
                location: Location::Arg(11),
                key: "r.vsync".into(),
                source: Cause::Syntax("missing value".into()),
            },
        ]
    );
    assert_eq!(FPS.get(), 70);
    assert_eq!(SCALE.get(), 2.5);
    // The value is the next argument verbatim.
    assert_eq!(NAME.get(), "+not a cvar");
    // Owned strings work too.
    let errors = apply_command_line(vec![String::from("+r.vsync"), String::from("true")]);
    assert!(errors.is_empty());
    assert!(VSYNC.get());

    finish_startup();
    WINDOW.set(2560);
    assert_eq!(WINDOW.get(), 1920);

    // Archive: the user layer, and what code or the console set for archived cvars (pending
    // values included); not the project file or the command line (r.maxFps, r.vsync), nor
    // unarchived cvars (debug.draw).
    NAME.set("Ann \"The\" # One\\\n\t ");
    QUALITY.set(2);
    SCALE.set(2.0);
    DEBUG.set(false);
    let archive = archive_string();
    assert_eq!(
        archive,
        r#"# User settings.
# Display name.
# Shown to others.
player.name = "Ann \"The\" # One\\\n\t "
r.quality = "high"
r.scale = 2.0
r.width = 2560
"#
    );

    // Round trip: without the session's layers, the user layer decides.
    clear_layer(Source::Console);
    clear_layer(Source::CommandLine);
    assert_eq!(
        NAME.get(),
        "Ann \"The\" # One\\",
        "back to the project file"
    );
    assert_eq!(FPS.get(), 60);
    assert_eq!(load_layer(Source::User, &archive), []);
    assert_eq!(NAME.get(), "Ann \"The\" # One\\\n\t ");
    assert_eq!((QUALITY.get_name(), SCALE.get()), ("high", 2.0));
    assert_eq!(WINDOW.pending_string().as_deref(), Some("2560"));
    assert_eq!(archive_string(), archive);
}
