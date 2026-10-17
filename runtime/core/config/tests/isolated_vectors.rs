//! Vector and color cvars: parsing, TOML arrays, the command line, archives, torn-free reads.
//! Only `toml_and_command_line` loads and clears a layer, and only its own cvars live there.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the user layer it writes
//! back is global. Run alone with `cargo test -p marlin-config --test isolated_vectors`.

use std::thread;

use marlin_config::{
    ApplyError, Cause, CvarColor, CvarVec2, CvarVec3, CvarVec4, Error, Expected, Kind, Location,
    Source, apply_command_line, apply_config, clear_layer, declare, load_layer, write_layer,
};

declare! {
    static OFFSET: CvarVec2 = CvarVec2::new("v.offset", [0.0, 0.0], "Screen offset.");
    static GRAVITY: CvarVec3 = CvarVec3::new("v.gravity", [0.0, -9.81, 0.0], "");
    static RECT: CvarVec4 = CvarVec4::new("v.rect", [0.0, 0.0, 1.0, 1.0], "");
    static TINT: CvarColor = CvarColor::new("v.tint", [1.0, 1.0, 1.0, 1.0], "Tint.");
}

#[test]
fn parsing() {
    assert_eq!(
        [OFFSET.kind(), GRAVITY.kind(), RECT.kind(), TINT.kind()],
        [Kind::Vec2, Kind::Vec3, Kind::Vec4, Kind::Color]
    );
    assert_eq!(GRAVITY.get(), [0.0, -9.81, 0.0]);
    assert_eq!(GRAVITY.default_string(), "0 -9.81 0");
    for (text, value) in [
        ("1 2 3", [1.0, 2.0, 3.0]),
        ("1, 2,3", [1.0, 2.0, 3.0]),
        ("(0.5, -1, 1e2)", [0.5, -1.0, 100.0]),
        (" [4 5 6] ", [4.0, 5.0, 6.0]),
    ] {
        GRAVITY.set_str(text, Source::Console).unwrap();
        assert_eq!(GRAVITY.get(), value, "{text}");
    }
    assert_eq!(GRAVITY.value_string(), "4 5 6");
    for text in [
        "1 2", "1 2 3 4", "1,,3", "a b c", "1 nan 3", "1 inf 3", "", "(1 2 3",
    ] {
        assert_eq!(
            GRAVITY.set_str(text, Source::Console),
            Err(Error::Invalid(Expected::Vector(3))),
            "{text}"
        );
    }
    assert_eq!(
        Error::Invalid(Expected::Vector(3)).to_string(),
        "expected 3 finite numbers"
    );

    for (text, value) in [
        ("#ff8000", [1.0, 128.0 / 255.0, 0.0, 1.0]),
        ("#FF800040", [1.0, 128.0 / 255.0, 0.0, 64.0 / 255.0]),
        ("0.5 0.25 2", [0.5, 0.25, 2.0, 1.0]),
        ("0, 0, 0, 0", [0.0; 4]),
    ] {
        TINT.set_str(text, Source::Console).unwrap();
        assert_eq!(TINT.get(), value, "{text}");
    }
    for text in ["#ff80", "#ff80001", "#gg0000", "1 2", "red"] {
        assert_eq!(
            TINT.set_str(text, Source::Console),
            Err(Error::Invalid(Expected::Color)),
            "{text}"
        );
    }

    // Code ignores non-finite values.
    OFFSET.set([3.0, 4.0]);
    OFFSET.set([f32::NAN, 0.0]);
    assert_eq!(OFFSET.get(), [3.0, 4.0]);
    RECT.set([1.0, 2.0, 3.0, 4.0]);
    assert_eq!(RECT.get(), [1.0, 2.0, 3.0, 4.0]);
    RECT.reset(Source::Code).unwrap();
    assert!(RECT.is_default());
}

declare! {
    static F_POS: CvarVec3 = CvarVec3::new("f.pos", [0.0; 3], "Spawn point.");
    static F_TINT: CvarColor = CvarColor::new("f.tint", [0.0, 0.0, 0.0, 1.0], "");
    static F_UV: CvarVec2 = CvarVec2::new("f.uv", [0.0; 2], "");
}

#[test]
fn toml_and_command_line() {
    let errors = apply_config(
        "f.pos = [1, 2.5, -3]\nf.tint = \"#00ff00\"\nf.uv = [1, 2, 3]",
        Source::User,
    );
    let err = |line, expected| ApplyError {
        location: Location::Line(line),
        key: "f.uv".into(),
        source: Cause::Set(Error::Invalid(expected)),
    };
    assert_eq!(errors, [err(3, Expected::Vector(2))]);
    let errors = apply_config("[f]\nuv = [1, \"x\"]", Source::User);
    assert_eq!(errors, [err(2, Expected::Vector(2))]);
    assert_eq!(F_POS.get(), [1.0, 2.5, -3.0]);
    assert_eq!(F_TINT.get(), [0.0, 1.0, 0.0, 1.0]);
    assert_eq!(apply_config("f.tint = [0.5, 0.5, 0.5]", Source::User), []);
    assert_eq!(F_TINT.get(), [0.5, 0.5, 0.5, 1.0]);

    // Written as arrays, which read back.
    let mut out = String::new();
    write_layer(Source::User, &mut out).unwrap();
    assert_eq!(
        out,
        "# The user layer.\n# Spawn point.\nf.pos = [1.0, 2.5, -3.0]\nf.tint = [0.5, 0.5, 0.5, 1.0]\n"
    );
    clear_layer(Source::User);
    assert_eq!(F_POS.get(), [0.0; 3]);
    assert_eq!(load_layer(Source::User, &out), []);
    assert_eq!(F_POS.get(), [1.0, 2.5, -3.0]);

    // One argument each.
    let errors = apply_command_line(["+f.uv", "0.25 0.75", "+f.tint=#102030ff", "+f.pos", "1"]);
    assert_eq!(
        errors,
        [ApplyError {
            location: Location::Arg(3),
            key: "f.pos".into(),
            source: Cause::Set(Error::Invalid(Expected::Vector(3))),
        }]
    );
    assert_eq!(F_UV.get(), [0.25, 0.75]);
    assert_eq!(
        F_TINT.get(),
        [16.0 / 255.0, 32.0 / 255.0, 48.0 / 255.0, 1.0]
    );
}

declare! {
    static SHARED: CvarVec4 = CvarVec4::new("mt.vec", [0.0; 4], "");
}

#[test]
fn reads_are_never_torn() {
    const WRITES: u32 = 2000;
    thread::scope(|s| {
        for w in 0..2u32 {
            s.spawn(move || {
                for i in 0..WRITES {
                    // All four components equal: a torn read would mix two writes.
                    let v = (w * WRITES + i) as f32;
                    SHARED.set([v; 4]);
                }
            });
        }
        for _ in 0..4 {
            s.spawn(|| {
                for _ in 0..WRITES {
                    let v = SHARED.get();
                    assert!(v.iter().all(|&c| c == v[0]), "torn: {v:?}");
                }
            });
        }
    });
}
