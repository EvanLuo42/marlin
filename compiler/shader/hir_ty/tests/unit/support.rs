//! The stand-in core library plus user packages, in a fresh database.

use std::fs;
use std::sync::Arc;

use marlin_front_hir_def::{
    FileId, FilePackage, FileText, LanguageInput, PackageData, PackageId, PackageInput,
};
use marlin_lang_diag::{SourceMap, render_all};
use marlin_lang_query::{Db, Durability};
use marlin_shaderc_hir_ty::{FileTyDiagnostics, SHADER_LANGUAGE};
use marlin_test_support::data_path;

pub struct World {
    pub db: Db,
    /// `(name, text)` per file, indexed by `FileId`.
    pub files: Vec<(String, Arc<str>)>,
    pub core: PackageId,
    pub game: PackageId,
}

fn read(rel: &str) -> Arc<str> {
    fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), rel))
        .unwrap()
        .replace("\r\n", "\n")
        .into()
}

/// Package `Marlin` (core/*) and package `Game` (game/*), which depends on it.
pub fn world(game_files: &[&str]) -> World {
    let db = Db::new();
    let core = PackageId(0);
    let game = PackageId(1);
    let mut files = Vec::new();
    let mut core_ids = Vec::new();
    let mut game_ids = Vec::new();
    db.update(|u| {
        u.set::<LanguageInput>((), &SHADER_LANGUAGE, Durability::High);
        for (package, ids, names, durability) in [
            (
                core,
                &mut core_ids,
                &["core/Core.mshader"][..],
                Durability::High,
            ),
            (game, &mut game_ids, game_files, Durability::Low),
        ] {
            for name in names {
                let file = FileId(files.len() as u32);
                let text = read(name);
                u.set::<FileText>(file, text.clone(), durability);
                u.set::<FilePackage>(file, package, durability);
                files.push((name.to_string(), text));
                ids.push(file);
            }
        }
        u.set::<PackageInput>(
            core,
            Arc::new(PackageData {
                name: "Marlin".into(),
                files: core_ids.clone().into(),
                dependencies: Arc::new([]),
            }),
            Durability::High,
        );
        u.set::<PackageInput>(
            game,
            Arc::new(PackageData {
                name: "Game".into(),
                files: game_ids.clone().into(),
                dependencies: Arc::new([core]),
            }),
            Durability::Low,
        );
    });
    World {
        db,
        files,
        core,
        game,
    }
}

impl World {
    pub fn file(&self, name: &str) -> FileId {
        FileId(self.files.iter().position(|(n, _)| n == name).unwrap() as u32)
    }

    /// The rendered diagnostics of `files`.
    pub fn diagnostics(&self, files: &[&str]) -> String {
        let snapshot = self.db.snapshot();
        let mut all = Vec::new();
        for name in files {
            all.extend(
                snapshot
                    .get::<FileTyDiagnostics>(&self.file(name))
                    .unwrap()
                    .iter()
                    .cloned(),
            );
        }
        render_all(&all, self)
    }
}

impl SourceMap for World {
    fn name(&self, file: FileId) -> String {
        self.files[file.0 as usize].0.clone()
    }
    fn text(&self, file: FileId) -> &str {
        &self.files[file.0 as usize].1
    }
}
