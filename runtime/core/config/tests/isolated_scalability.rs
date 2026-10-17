//! Scalability groups and device profiles. One test: the scalability layer spans the registry.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the cvar registry is global.
//! Run alone with `cargo test -p marlin-config --test isolated_scalability`.

use std::ptr;

use marlin_config::{
    ApplyError, Bounds, Cause, CvarBool, CvarEnum, CvarI64, Error, Location, Source,
    clear_scalability, declare, load_device_profile, load_layer, load_scalability,
    scalability_groups, scalability_overrides,
};

declare! {
    static SHADOWS: CvarI64 = CvarI64::new("sg.shadows", 3, "Shadow quality.").range(0, 3);
    static EFFECTS: CvarEnum = CvarEnum::new("sg.effects", 2, &["low", "medium", "high"], "");
    static SHADOW_ON: CvarBool = CvarBool::new("r.shadow.enable", true, "");
    static SHADOW_RES: CvarI64 = CvarI64::new("r.shadow.resolution", 4096, "");
    static PARTICLES: CvarI64 = CvarI64::new("fx.particles", 10000, "");
}

const TABLES: &str = r#"
[sg.shadows.0]
r.shadow.enable = false

[sg.shadows.1]
r.shadow.resolution = 512

[sg.shadows.2]
r.shadow = { enable = true, resolution = 2048 }

[sg.effects.low]
fx.particles = 500

[sg.effects.1]
fx.particles = 2000
"#;

#[test]
fn groups_and_profiles() {
    // Level 3 has no table: nothing applies until the level changes.
    assert_eq!(load_scalability(TABLES), []);
    assert_eq!(
        scalability_groups()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["sg.effects", "sg.shadows"]
    );
    assert_eq!((SHADOW_RES.get(), SHADOW_RES.set_by()), (4096, None));

    // Changing the group applies its level; cvars the level does not name lose the group's
    // value.
    SHADOWS.set(2);
    assert_eq!((SHADOW_ON.get(), SHADOW_RES.get()), (true, 2048));
    assert_eq!(SHADOW_RES.set_by(), Some(Source::Scalability));
    SHADOWS.set(1);
    assert_eq!((SHADOW_ON.get(), SHADOW_RES.get()), (true, 512));
    assert_eq!(SHADOW_ON.set_by(), None);
    SHADOWS.set(0);
    assert_eq!((SHADOW_ON.get(), SHADOW_RES.get()), (false, 4096));
    // Enum groups take names or indices as levels.
    EFFECTS.set(0);
    assert_eq!(PARTICLES.get(), 500);
    EFFECTS.set_str("medium", Source::Console).unwrap();
    assert_eq!(PARTICLES.get(), 2000);

    // A cvar set above the scalability layer after the group applied is an override, and
    // stays when the group changes.
    SHADOWS.set(2);
    assert!(scalability_overrides(&SHADOWS).is_empty());
    SHADOW_RES.set_str("1024", Source::User).unwrap();
    let overrides = scalability_overrides(&SHADOWS);
    assert_eq!(overrides.len(), 1);
    assert!(ptr::eq(overrides[0], &*SHADOW_RES));
    SHADOWS.set(1);
    assert_eq!(SHADOW_RES.get(), 1024);
    assert_eq!(
        SHADOW_RES.layer_string(Source::Scalability).as_deref(),
        Some("512")
    );
    SHADOW_RES.unset(Source::User).unwrap();
    assert_eq!(SHADOW_RES.get(), 512);

    // Later tables merge into the earlier ones.
    assert_eq!(
        load_scalability("[sg.shadows.1]\nr.shadow.resolution = 768"),
        []
    );
    assert_eq!(SHADOW_RES.get(), 768);

    // A device profile picks levels at boot, from its own layer: the console still wins.
    SHADOWS.unset(Source::Code).unwrap();
    EFFECTS.unset(Source::Console).unwrap();
    let profiles =
        "[low-end]\nsg.shadows = 0\nsg.effects = \"low\"\n\n[high-end]\nsg.shadows = 2\n";
    assert_eq!(load_device_profile(profiles, "low-end"), []);
    assert_eq!(
        (SHADOWS.get(), SHADOWS.set_by()),
        (0, Some(Source::DeviceProfile))
    );
    assert_eq!((SHADOW_ON.get(), PARTICLES.get()), (false, 500));
    assert_eq!(load_device_profile(profiles, "high-end"), []);
    assert_eq!(
        (SHADOW_ON.get(), SHADOW_RES.get(), PARTICLES.get()),
        (true, 2048, 10000)
    );
    // The user's choice beats the profile.
    assert_eq!(load_layer(Source::User, "sg.shadows = 1"), []);
    assert_eq!(SHADOW_RES.get(), 768);

    // Errors: unknown cvars, levels outside the group's range, values that do not fit.
    let errors = load_scalability(
        "[sg.shadows.7]\nr.shadow.enable = true\n\n[sg.shadows.2]\nr.shadow.enable = 5\n\
         [nope.0]\nx = 1\n[sg.effects]\nultra = { fx.particles = 1 }",
    );
    let at = |line, key: &str, source| ApplyError {
        location: Location::Line(line),
        key: key.into(),
        source,
    };
    assert_eq!(
        errors,
        [
            at(
                1,
                "sg.shadows.7",
                Cause::Set(Error::OutOfRange(Bounds::Int(0, 3)))
            ),
            at(
                5,
                "r.shadow.enable",
                Cause::Set(Error::Invalid(marlin_config::Expected::Bool))
            ),
            at(7, "nope.0.x", Cause::Set(Error::Unknown)),
            at(
                9,
                "sg.effects.ultra",
                Cause::Set(Error::Invalid(marlin_config::Expected::Enum(&[
                    "low", "medium", "high"
                ])))
            ),
        ]
    );
    assert_eq!(SHADOW_RES.get(), 768, "valid tables are unchanged");

    // Dropping the tables clears the layer.
    clear_scalability();
    assert!(scalability_groups().is_empty());
    assert_eq!((SHADOW_RES.get(), SHADOW_RES.set_by()), (4096, None));
    SHADOWS.set(0);
    assert!(SHADOW_ON.get(), "no table, no effect");
}
