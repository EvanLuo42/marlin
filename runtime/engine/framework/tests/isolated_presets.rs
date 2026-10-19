//! Quality presets as assets (0008 §2.2, R4f): scalability tables and a device profile found by
//! name in mounted content, applied into the cvar layers, read into the render settings.
//!
//! A process of its own (quick tier, docs/design/0014-testing.md §5): the cvar registry is global.
//! The engine's real table is cooked and applied by the GPU end-to-end test.

use marlin_asset::cooked::write::{UnitObject, unit};
use marlin_asset::cooked::{Encoding, platform_id, unit_id};
use marlin_asset::{MemorySource, Uuid, Vfs};
use marlin_config::Source;
use marlin_framework::quality::{
    R_RESTIR_CANDIDATES, R_RESTIR_CHECKERBOARD, R_UPSCALE_MODE, RenderSettings, SG_LIGHTING,
    SG_QUALITY,
};
use marlin_framework::register_cvars;
use marlin_framework::scalability::{
    CvarValue, DEVICE_PROFILES, DeviceProfile, ENGINE_SCALABILITY, GAME_SCALABILITY, PresetError,
    Scalability, ScalabilityLevel, apply_presets,
};
use marlin_reflect::{Reflect, type_info};
use marlin_render::upscale::UpscaleMode;

fn values(pairs: &[(&str, &str)]) -> Vec<CvarValue> {
    pairs
        .iter()
        .map(|&(name, value)| CvarValue {
            name: name.into(),
            value: value.into(),
        })
        .collect()
}

fn level(group: &str, level: &str, pairs: &[(&str, &str)]) -> ScalabilityLevel {
    ScalabilityLevel {
        group: group.into(),
        level: level.into(),
        values: values(pairs),
    }
}

/// Content holding `objects`, each under its name.
fn content(objects: Vec<(&str, u128, Encoded)>) -> Vfs {
    let mut source = MemorySource::new();
    for (name, id, (type_id, payload)) in objects {
        let uuid = Uuid::from_u128(id);
        let bytes = unit(
            unit_id(name),
            platform_id("windows"),
            vec![UnitObject {
                uuid,
                type_id,
                encoding: Encoding::Serial,
                payload,
                bulk: Vec::new(),
            }],
            &[],
        );
        source.insert(bytes).unwrap();
        source.name(name, uuid);
    }
    let mut vfs = Vfs::new();
    vfs.mount("content", source);
    vfs
}

type Encoded = (u64, Vec<u8>);

fn encode<T: Reflect>(value: &T) -> Encoded {
    (
        type_info::<T>().id.0,
        marlin_serial::to_bytes(value).unwrap(),
    )
}

#[test]
fn presets_apply_from_content() {
    register_cvars();
    let engine = Scalability {
        levels: vec![
            level(
                "sg.quality",
                "low",
                &[("sg.lighting", "1"), ("sg.resolution", "2")],
            ),
            level(
                "sg.quality",
                "medium",
                &[("sg.lighting", "2"), ("sg.resolution", "3")],
            ),
            level(
                "sg.lighting",
                "1",
                &[
                    ("r.restir.candidates", "8"),
                    ("r.restir.checkerboard", "true"),
                ],
            ),
            level(
                "sg.lighting",
                "2",
                &[
                    ("r.restir.candidates", "16"),
                    ("r.restir.checkerboard", "true"),
                ],
            ),
            level("sg.resolution", "2", &[("r.upscale.mode", "quality")]),
            level("sg.resolution", "3", &[("r.upscale.mode", "native")]),
        ],
    };
    // The game's table merges over the engine's: its L2 takes the checkerboard off.
    let game = Scalability {
        levels: vec![level(
            "sg.lighting",
            "2",
            &[("r.restir.checkerboard", "false")],
        )],
    };
    let profile = DeviceProfile {
        values: values(&[("sg.quality", "low")]),
    };
    let profile_name = format!("{DEVICE_PROFILES}.{}", marlin_rt::PLATFORM);
    let vfs = content(vec![
        (ENGINE_SCALABILITY, 1, encode(&engine)),
        (GAME_SCALABILITY, 2, encode(&game)),
        (&profile_name, 3, encode(&profile)),
    ]);
    apply_presets(&vfs).unwrap();

    // The profile picked low: L1, quality upscaling.
    assert_eq!(
        (SG_QUALITY.get_name(), SG_QUALITY.set_by()),
        ("low", Some(Source::DeviceProfile))
    );
    assert_eq!(SG_LIGHTING.get(), 1);
    let s = RenderSettings::from_cvars();
    assert_eq!((s.restir_candidates, s.restir_checkerboard), (8, true));
    assert_eq!(s.upscale, Some(UpscaleMode::Quality));

    // The user picks medium: L2 as the game changed it.
    SG_QUALITY.set_str("medium", Source::User).unwrap();
    let s = RenderSettings::from_cvars();
    assert_eq!((s.restir_candidates, s.restir_checkerboard), (16, false));
    assert_eq!(s.upscale, Some(UpscaleMode::Native));

    // The console beats every preset; a cvar no level sets keeps its default.
    R_RESTIR_CANDIDATES.set_str("4", Source::Console).unwrap();
    assert_eq!(RenderSettings::from_cvars().restir_candidates, 4);
    assert_eq!(R_RESTIR_CHECKERBOARD.set_by(), Some(Source::Scalability));
    R_UPSCALE_MODE.set_str("off", Source::Console).unwrap();
    assert_eq!(RenderSettings::from_cvars().upscale, None);

    // A value the cvar cannot take is an error naming the table and the entry.
    let bad = Scalability {
        levels: vec![level(
            "sg.lighting",
            "3",
            &[("r.restir.candidates", "many")],
        )],
    };
    let vfs = content(vec![(ENGINE_SCALABILITY, 4, encode(&bad))]);
    match apply_presets(&vfs) {
        Err(PresetError::Values { name, errors }) => {
            assert_eq!(name, ENGINE_SCALABILITY);
            assert!(errors.contains("sg.lighting.3"), "{errors}");
            assert!(errors.contains("r.restir.candidates"), "{errors}");
        }
        other => panic!("{other:?}"),
    }
    // Content without presets (an old build) applies none.
    apply_presets(&Vfs::new()).unwrap();
}
