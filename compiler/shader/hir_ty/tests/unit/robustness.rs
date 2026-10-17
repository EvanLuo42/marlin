//! Type checking must not panic on any text: seeded mutations of the body fixtures, checked
//! under the binary's memory cap.

use std::sync::Arc;

use marlin_front_hir_def::FileText;
use marlin_lang_query::Durability;
use marlin_math::Rng;
use marlin_shaderc_hir_ty::FileTyDiagnostics;
use marlin_test_support::{scale, seed};

use crate::support::world;

#[test]
fn broken_bodies_type_check_without_panicking() {
    let seed = seed(0x7e5e_0001);
    let mut rng = Rng::new(seed);
    for name in [
        "game/Bodies.mshader",
        "game/BodyErrors.mshader",
        "game/Game.mshader",
    ] {
        let world = world(&[name]);
        let file = world.file(name);
        let source: Vec<char> = world.files[file.0 as usize].1.chars().collect();
        for _ in 0..scale(25) {
            let mut mutated = source.clone();
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(mutated.len() + 1);
                let end = (at + rng.below(25)).min(mutated.len());
                if rng.bool() {
                    mutated.drain(at..end);
                } else {
                    let copy: Vec<char> = mutated[at..end].to_vec();
                    let to = rng.below(mutated.len() + 1);
                    mutated.splice(to..to, copy);
                }
            }
            let text: String = mutated.into_iter().collect();
            world
                .db
                .set::<FileText>(file, Arc::from(text.as_str()), Durability::Low);
            let result = std::panic::catch_unwind(|| {
                world.db.snapshot().get::<FileTyDiagnostics>(&file).unwrap();
            });
            assert!(
                result.is_ok(),
                "seed {seed}: type checking panicked on {text:?}"
            );
        }
    }
}
