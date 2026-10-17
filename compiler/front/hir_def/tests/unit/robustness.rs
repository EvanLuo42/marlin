//! Declarations, bodies, signatures, scopes and diagnostics must come out of any text without
//! panicking: the parser's snapshot sources (valid and broken), and seeded mutations of them.

use std::fs;
use std::path::Path;

use marlin_front_hir_def::{
    BodyQuery, ExprScopesQuery, FileDiagnostics, ItemId, ItemSignature, ItemTreeQuery,
    PackageDefMap, body_owners,
};
use marlin_math::Rng;
use marlin_test_support::{scale, seed};

use crate::fixture::Fixture;

fn analyze(text: &str) {
    let fixture = Fixture::new(&[("P", &[], &[("f.mshader", text)])]);
    let snapshot = fixture.db.snapshot();
    let file = fixture.file("f.mshader");
    snapshot
        .get::<PackageDefMap>(&fixture.package("P"))
        .unwrap();
    snapshot.get::<FileDiagnostics>(&file).unwrap();
    let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
    for (local, _) in tree.items.iter() {
        let item = ItemId { file, local };
        snapshot.get::<ItemSignature>(&item).unwrap();
        for owner in body_owners(&snapshot, item).unwrap() {
            snapshot.get::<BodyQuery>(&owner).unwrap();
            snapshot.get::<ExprScopesQuery>(&owner).unwrap();
        }
    }
}

fn corpus() -> Vec<String> {
    let parser_data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/data/parser");
    let mut sources = Vec::new();
    for sub in ["ok", "err"] {
        for entry in fs::read_dir(parser_data.join(sub)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "src") {
                sources.push(fs::read_to_string(path).unwrap());
            }
        }
    }
    let bodies = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/bodies.mshader");
    sources.push(fs::read_to_string(bodies).unwrap());
    sources
}

#[test]
fn any_text_analyzes_without_panicking() {
    let seed = seed(0x4d1d_0001);
    let mut rng = Rng::new(seed);
    for source in corpus() {
        analyze(&source);
        let chars: Vec<char> = source.chars().collect();
        for _ in 0..scale(15) {
            let mut mutated = chars.clone();
            for _ in 0..1 + rng.below(3) {
                let at = rng.below(mutated.len() + 1);
                let end = (at + rng.below(30)).min(mutated.len());
                if rng.bool() {
                    mutated.drain(at..end);
                } else {
                    let copy: Vec<char> = mutated[at..end].to_vec();
                    let to = rng.below(mutated.len() + 1);
                    mutated.splice(to..to, copy);
                }
            }
            let text: String = mutated.into_iter().collect();
            if std::panic::catch_unwind(|| analyze(&text)).is_err() {
                panic!("seed {seed}: analysis panicked on {text:?}");
            }
        }
    }
}
