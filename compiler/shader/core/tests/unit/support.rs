//! The core library and test code in a fresh database, and running lowered code.

use std::sync::Arc;

use marlin_front_hir_def::{
    FileId, FilePackage, FileText, ItemId, ItemTreeQuery, LanguageInput, PackageData, PackageId,
    PackageInput, item_path,
};
use marlin_lang_diag::{SourceMap, render_all};
use marlin_lang_query::{Db, Durability};
use marlin_shaderc_hir_ty::{FileTyDiagnostics, SHADER_LANGUAGE};
use marlin_shaderc_ir::{Interpreter, Module, Val, print_module, verify_module};
use marlin_shaderc_lower::Lowering;

pub struct World {
    pub db: Db,
    /// `(name, text)` per file, by `FileId`.
    pub files: Vec<(String, Arc<str>)>,
    pub game: PackageId,
}

/// The core library, and package `Game` with one file of `text`.
pub fn world(text: &str) -> World {
    let db = Db::new();
    let (core, game) = (PackageId(0), PackageId(1));
    let mut files: Vec<(String, Arc<str>)> = marlin_shaderc_core::FILES
        .iter()
        .map(|(p, t)| (format!("Marlin/{p}"), Arc::from(t.replace("\r\n", "\n"))))
        .collect();
    let core_count = files.len();
    files.push(("Game/Test.mshader".into(), text.into()));
    db.update(|u| {
        u.set::<LanguageInput>((), &SHADER_LANGUAGE, Durability::High);
        for (i, (_, text)) in files.iter().enumerate() {
            let file = FileId(i as u32);
            u.set::<FileText>(file, text.clone(), Durability::Low);
            let package = if i < core_count { core } else { game };
            u.set::<FilePackage>(file, package, Durability::Low);
        }
        let ids = |r: std::ops::Range<usize>| -> Arc<[FileId]> {
            r.map(|i| FileId(i as u32)).collect::<Vec<_>>().into()
        };
        u.set::<PackageInput>(
            core,
            Arc::new(PackageData {
                name: "Marlin".into(),
                files: ids(0..core_count),
                dependencies: Arc::new([]),
            }),
            Durability::Low,
        );
        u.set::<PackageInput>(
            game,
            Arc::new(PackageData {
                name: "Game".into(),
                files: ids(core_count..core_count + 1),
                dependencies: Arc::new([core]),
            }),
            Durability::Low,
        );
    });
    World { db, files, game }
}

impl World {
    /// Every diagnostic of every file, rendered.
    pub fn diagnostics(&self) -> String {
        let snapshot = self.db.snapshot();
        let mut all = Vec::new();
        for i in 0..self.files.len() {
            all.extend(
                snapshot
                    .get::<FileTyDiagnostics>(&FileId(i as u32))
                    .unwrap()
                    .iter()
                    .cloned(),
            );
        }
        render_all(&all, self)
    }

    /// Lowers entry `path`, which must fail; returns the error as shown.
    pub fn lowering_error(&self, path: &str) -> String {
        let snapshot = self.db.snapshot();
        let mut l = Lowering::new(&snapshot, self.game).unwrap();
        let err = l
            .entry(self.item(path), &[])
            .and_then(|_| l.finish().map(|_| ()))
            .expect_err("lowering fails");
        match err.to_diagnostic(&snapshot) {
            Ok(Some(d)) => render_all(&[d], self),
            _ => err.to_string(),
        }
    }

    fn item(&self, path: &str) -> ItemId {
        let snapshot = self.db.snapshot();
        for i in 0..self.files.len() {
            let file = FileId(i as u32);
            let tree = snapshot.get::<ItemTreeQuery>(&file).unwrap();
            for (local, _) in tree.items.iter() {
                let item = ItemId { file, local };
                if item_path(&snapshot, item).unwrap() == path {
                    return item;
                }
            }
        }
        panic!("no item {path}")
    }

    /// `path` (a function of the test file) lowered, or the rendered error.
    pub fn lower(&self, path: &str) -> Result<Module, String> {
        let snapshot = self.db.snapshot();
        let mut l = Lowering::new(&snapshot, self.game).unwrap();
        let render = |e: &marlin_shaderc_lower::LowerError| match e.to_diagnostic(&snapshot) {
            Ok(Some(d)) => render_all(&[d], self),
            _ => e.to_string(),
        };
        l.function(self.item(path)).map_err(|e| render(&e))?;
        let module = l.finish().map_err(|e| render(&e))?;
        let errors = verify_module(&module);
        assert!(
            errors.is_empty(),
            "lowered IR does not verify: {errors:#?}\n{}",
            print_module(&module)
        );
        Ok(module)
    }

    /// Calls `path` with `args` after optimization only: code with derivatives runs once the
    /// autodiff pass has made them arithmetic.
    pub fn run_optimized(&self, path: &str, args: &[Val]) -> Vec<Val> {
        let mut module = self.lower(path).unwrap_or_else(|e| panic!("{e}"));
        let options = marlin_shaderc_opt::Options {
            profile: marlin_shaderc_opt::Profile::Development,
            verify_each: true,
        };
        marlin_shaderc_opt::pipeline(&options)
            .run(&mut module)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let f = module.find_function(path).unwrap();
        Interpreter::new(&module).call(f, args).unwrap_or_else(|e| {
            panic!(
                "{path}, optimized: {e}
{}",
                print_module(&module)
            )
        })
    }

    /// Calls `path` with `args`, before and after optimization; both must agree.
    pub fn run(&self, path: &str, args: &[Val]) -> Vec<Val> {
        let module = self.lower(path).unwrap_or_else(|e| panic!("{e}"));
        let f = module.find_function(path).unwrap();
        let before = Interpreter::new(&module)
            .call(f, args)
            .unwrap_or_else(|e| panic!("{path}: {e}\n{}", print_module(&module)));
        let mut optimized = module.clone();
        let options = marlin_shaderc_opt::Options {
            profile: marlin_shaderc_opt::Profile::Development,
            verify_each: true,
        };
        marlin_shaderc_opt::pipeline(&options)
            .run(&mut optimized)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let g = optimized.find_function(path).unwrap();
        let after = Interpreter::new(&optimized)
            .call(g, args)
            .unwrap_or_else(|e| panic!("{path}, optimized: {e}"));
        assert_eq!(
            after, before,
            "{path}: the optimized code computes something else"
        );
        before
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

impl World {
    /// The entry point at `path` lowered and run for `groups` groups over `resources`, before
    /// and after optimization (both must write the same); the debug output.
    pub fn dispatch(
        &self,
        path: &str,
        groups: [u32; 3],
        resources: &mut marlin_shaderc_ir::Resources,
    ) -> Vec<String> {
        let snapshot = self.db.snapshot();
        let mut l = Lowering::new(&snapshot, self.game).unwrap();
        let render = |e: &marlin_shaderc_lower::LowerError| match e.to_diagnostic(&snapshot) {
            Ok(Some(d)) => render_all(&[d], self),
            _ => e.to_string(),
        };
        let entry = l
            .entry(self.item(path), &[])
            .unwrap_or_else(|e| panic!("{}", render(&e)));
        let module = l.finish().unwrap_or_else(|e| panic!("{}", render(&e)));
        let errors = verify_module(&module);
        assert!(errors.is_empty(), "{errors:#?}\n{}", print_module(&module));
        let mut optimized = module.clone();
        let options = marlin_shaderc_opt::Options {
            profile: marlin_shaderc_opt::Profile::Development,
            verify_each: true,
        };
        marlin_shaderc_opt::pipeline(&options)
            .run(&mut optimized)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut copy = resources.clone();
        let after = Interpreter::new(&optimized)
            .dispatch(optimized.entries[0].func, groups, &mut copy)
            .unwrap_or_else(|e| panic!("{path}, optimized: {e}"));
        let before = Interpreter::new(&module)
            .dispatch(entry, groups, resources)
            .unwrap_or_else(|e| panic!("{path}: {e}\n{}", print_module(&module)));
        assert_eq!(
            copy.buffers, resources.buffers,
            "{path}: the optimized entry writes otherwise"
        );
        assert_eq!(after.prints, before.prints);
        assert_eq!(before.out_of_bounds, 0);
        before.prints.into_iter().map(|p| p.text).collect()
    }
}
