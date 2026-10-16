//! Config files, command-line overrides and archiving. One test: archives cover the registry.

use marlin_config::{
    ApplyError, CvarBool, CvarEnum, CvarF64, CvarI64, CvarString, Error, Expected, Location,
    Source, apply_command_line, apply_config, archive_string, finish_startup, register_all,
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
        source,
    }
}

#[test]
fn files_and_command_line() {
    register_all(&[&VSYNC, &FPS, &SCALE, &NAME, &QUALITY, &WINDOW, &DEBUG]).unwrap();

    let config = "\u{feff}# Settings
; also a comment
// and this

r.vsync = off            # trailing comment
R.MAXFPS=144
r.scale = 1.5;comment needs whitespace, so this is part of the value
player.name = \"Ann \\\"The\\\" # One\\\\\"   ; comment after a string
r.quality = HIGH
debug.draw = 1
r.unknown = 3
just some words
= 5
r.maxFps = 5000
player.name = \"unterminated
player.name = \"ok\" trailing
r.width = 1920
";
    let errors = apply_config(config, Source::ConfigFile);
    assert_eq!(
        errors,
        [
            err(
                Location::Line(7),
                "r.scale",
                Error::Invalid(Expected::Float)
            ),
            err(Location::Line(11), "r.unknown", Error::Unknown),
            err(
                Location::Line(12),
                "just",
                Error::Syntax("expected `name = value`")
            ),
            err(
                Location::Line(13),
                "",
                Error::Syntax("expected `name = value`")
            ),
            err(
                Location::Line(14),
                "r.maxFps",
                Error::OutOfRange(marlin_config::Bounds::Int(0, 1000))
            ),
            err(
                Location::Line(15),
                "player.name",
                Error::Syntax("unterminated string")
            ),
            err(
                Location::Line(16),
                "player.name",
                Error::Syntax("text after the closing quote")
            ),
        ]
    );
    assert_eq!(errors[1].to_string(), "line 11: `r.unknown`");
    assert!(!VSYNC.get());
    assert_eq!(FPS.get(), 144);
    assert_eq!(SCALE.get(), 1.0);
    assert_eq!(NAME.get(), "Ann \"The\" # One\\");
    assert_eq!(QUALITY.get_name(), "high");
    assert!(DEBUG.get());
    assert_eq!(WINDOW.get(), 1920, "startup: takes effect at once");

    let args = [
        "game.exe",
        "+r.maxFps",
        "60",
        "positional",
        "+r.scale=2",
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
            err(
                Location::Arg(9),
                "",
                Error::Syntax("expected `+name value`")
            ),
            err(Location::Arg(11), "r.vsync", Error::Syntax("missing value")),
        ]
    );
    assert_eq!(FPS.get(), 60);
    assert_eq!(SCALE.get(), 2.0);
    // The value is the next argument verbatim.
    assert_eq!(NAME.get(), "+not a cvar");
    // Owned strings work too.
    let errors = apply_command_line(vec![String::from("+r.vsync"), String::from("true")]);
    assert!(errors.is_empty());
    assert!(VSYNC.get());

    finish_startup();
    WINDOW.set(2560);
    assert_eq!(WINDOW.get(), 1920);

    // Archive: only archived cvars that differ from the default, pending values included.
    NAME.set("Ann \"The\" # One\\\n\t ");
    VSYNC.set(true);
    let archive = archive_string();
    assert_eq!(
        archive,
        "# Archived cvars that differ from their defaults.
# Display name.
# Shown to others.
player.name = \"Ann \\\"The\\\" # One\\\\\\n\\t \"
# Frame cap.
r.maxFps = 60
r.quality = high
r.scale = 2
r.width = 2560
"
    );

    // Round trip.
    for cvar in [&*NAME, &FPS, &QUALITY, &SCALE] {
        cvar.reset(Source::Code).unwrap();
    }
    assert!(apply_config(&archive, Source::ConfigFile).is_empty());
    assert_eq!(NAME.get(), "Ann \"The\" # One\\\n\t ");
    assert_eq!(
        (FPS.get(), QUALITY.get_name(), SCALE.get()),
        (60, "high", 2.0)
    );
    assert_eq!(archive_string(), archive);
}
