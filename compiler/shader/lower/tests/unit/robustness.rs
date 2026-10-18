//! Lowering must not panic on code the type checker accepts, and what it lowers must verify:
//! seeded mutations of the fixtures (those that still type-check), every function lowered,
//! under the binary's memory cap.

use std::sync::Arc;

use marlin_front_hir_def::{FileText, ItemKind, ItemTreeQuery};
use marlin_lang_query::Durability;
use marlin_math::Rng;
use marlin_shaderc_hir_ty::FileTyDiagnostics;
use marlin_shaderc_ir::verify_module;
use marlin_shaderc_lower::Lowering;
use marlin_test_support::{scale, seed};

use crate::support::world;

#[test]
fn lowering_never_panics_and_always_verifies() {
    let seed = seed(0x10_0001);
    let mut rng = Rng::new(seed);
    let name = "game/Basics.mshader";
    let w = world(&[name]);
    let file = w.file(name);
    let source: Vec<char> = w.files[file.0 as usize].1.chars().collect();
    let mut lowered = 0;
    for _ in 0..scale(120) {
        let mut mutated = source.clone();
        for _ in 0..1 + rng.below(2) {
            let at = rng.below(mutated.len() + 1);
            let end = (at + rng.below(12)).min(mutated.len());
            if rng.bool() {
                mutated.drain(at..end);
            } else {
                let copy: Vec<char> = mutated[at..end].to_vec();
                let to = rng.below(mutated.len() + 1);
                mutated.splice(to..to, copy);
            }
        }
        let text: String = mutated.into_iter().collect();
        w.db.set::<FileText>(file, Arc::from(text.as_str()), Durability::Low);
        let snapshot = w.db.snapshot();
        if !snapshot.get::<FileTyDiagnostics>(&file).unwrap().is_empty() {
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
            for (local, data) in tree.items.iter() {
                if data.kind != ItemKind::Function || !data.generic_params.is_empty() {
                    continue;
                }
                let item = marlin_front_hir_def::ItemId { file, local };
                let mut l = Lowering::new(&snapshot, w.game).unwrap();
                if l.function(item).is_err() {
                    continue;
                }
                if let Ok(module) = l.finish() {
                    let errors = verify_module(&module);
                    assert!(errors.is_empty(), "{errors:?}");
                    lowered += 1;
                }
            }
        }));
        assert!(
            result.is_ok(),
            "seed {seed}: lowering panicked or did not verify on {text:?}"
        );
    }
    assert!(
        lowered > 0,
        "seed {seed}: no mutation type-checked; the test tests nothing"
    );
}
