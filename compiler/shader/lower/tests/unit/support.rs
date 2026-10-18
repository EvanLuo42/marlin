//! The stand-in core library plus user code in a fresh database, and running lowered code.

use std::fs;
use std::sync::Arc;

use marlin_front_hir_def::{
    FileId, FilePackage, FileText, ItemId, ItemTreeQuery, LanguageInput, PackageData, PackageId,
    PackageInput, item_path,
};
use marlin_lang_diag::{SourceMap, render_all};
use marlin_lang_query::{Db, Durability};
use marlin_shaderc_hir_ty::{FileTyDiagnostics, GenericArgTy, SHADER_LANGUAGE, Ty};
use marlin_shaderc_ir::{Interpreter, Module, Output, Resources, Val, print_module, verify_module};
use marlin_shaderc_lower::Lowering;
use marlin_test_support::data_path;

/// Compile-time values by name.
pub type Named = Vec<(String, Val)>;

pub struct World {
    pub db: Db,
    /// `(name, text)` per file, indexed by `FileId`.
    pub files: Vec<(String, Arc<str>)>,
    pub game: PackageId,
}

fn read(rel: &str) -> Arc<str> {
    fs::read_to_string(data_path(env!("CARGO_MANIFEST_DIR"), rel))
        .unwrap()
        .replace("\r\n", "\n")
        .into()
}

/// Package `Marlin` (core/Core.mshader) and package `Game` (`game_files`), which uses it. The
/// game files must type-check cleanly.
pub fn world(game_files: &[&str]) -> World {
    let db = Db::new();
    let (core, game) = (PackageId(0), PackageId(1));
    let mut files = Vec::new();
    let (mut core_ids, mut game_ids) = (Vec::new(), Vec::new());
    db.update(|u| {
        u.set::<LanguageInput>((), &SHADER_LANGUAGE, Durability::High);
        for (package, ids, names) in [
            (core, &mut core_ids, &["core/Core.mshader"][..]),
            (game, &mut game_ids, game_files),
        ] {
            for name in names {
                let file = FileId(files.len() as u32);
                let text = read(name);
                u.set::<FileText>(file, text.clone(), Durability::Low);
                u.set::<FilePackage>(file, package, Durability::Low);
                files.push((name.to_string(), text));
                ids.push(file);
            }
        }
        for (package, name, ids, deps) in [
            (core, "Marlin", &core_ids, vec![]),
            (game, "Game", &game_ids, vec![core]),
        ] {
            u.set::<PackageInput>(
                package,
                Arc::new(PackageData {
                    name: name.into(),
                    files: ids.clone().into(),
                    dependencies: deps.into(),
                }),
                Durability::Low,
            );
        }
    });
    let world = World { db, files, game };
    let diagnostics = world.diagnostics(game_files);
    assert!(
        diagnostics.is_empty(),
        "the test code has type errors:\n{diagnostics}"
    );
    world
}

impl World {
    pub fn file(&self, name: &str) -> FileId {
        FileId(self.files.iter().position(|(n, _)| n == name).unwrap() as u32)
    }

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

    /// The declaration whose full path is `path` (`Game.Sum`), in any file.
    pub fn item(&self, path: &str) -> ItemId {
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

    /// Lowers the functions at `paths` (and what they call) into one verified module.
    pub fn lower(&self, paths: &[&str]) -> Module {
        let snapshot = self.db.snapshot();
        let mut l = Lowering::new(&snapshot, self.game).unwrap();
        for p in paths {
            let item = self.item(p);
            if let Err(e) = l.function(item) {
                panic!("{p}: {}", self.render_error(&snapshot, &e));
            }
        }
        let module = match l.finish() {
            Ok(m) => m,
            Err(e) => panic!("{}", self.render_error(&snapshot, &e)),
        };
        let errors = verify_module(&module);
        assert!(
            errors.is_empty(),
            "lowered IR does not verify: {errors:#?}\n{}",
            print_module(&module)
        );
        module
    }

    /// The `[Export]` constants and the static parameters used of the entry point at `path`,
    /// as `(name, value)`.
    pub fn evaluated(&self, path: &str, type_args: &[&str]) -> (Named, Named) {
        let snapshot = self.db.snapshot();
        let mut l = Lowering::new(&snapshot, self.game).unwrap();
        let args: Vec<GenericArgTy> = type_args
            .iter()
            .map(|t| GenericArgTy::Type(Ty::adt(self.item(t))))
            .collect();
        let lowered = l.entry(self.item(path), &args).and_then(|_| l.run());
        if let Err(e) = lowered {
            panic!("{path}: {}", self.render_error(&snapshot, &e));
        }
        let pairs = |v: Vec<marlin_shaderc_lower::Evaluated>| -> Vec<(String, Val)> {
            v.into_iter().map(|e| (e.name, e.value)).collect()
        };
        (pairs(l.exports()), pairs(l.static_params().unwrap()))
    }

    /// Lowers the entry point at `path` with type arguments (paths of types) and static
    /// parameters into a verified module, or the rendered error.
    pub fn try_entry(
        &self,
        path: &str,
        type_args: &[&str],
        statics: &[(&str, Val)],
    ) -> Result<Module, String> {
        let snapshot = self.db.snapshot();
        let mut l = Lowering::new(&snapshot, self.game).unwrap();
        for (p, v) in statics {
            l.set_static(self.item(p), v.clone())
                .map_err(|e| self.render_error(&snapshot, &e))?;
        }
        let args: Vec<GenericArgTy> = type_args
            .iter()
            .map(|t| GenericArgTy::Type(Ty::adt(self.item(t))))
            .collect();
        l.entry(self.item(path), &args)
            .map_err(|e| self.render_error(&snapshot, &e))?;
        let module = l.finish().map_err(|e| self.render_error(&snapshot, &e))?;
        let errors = verify_module(&module);
        assert!(
            errors.is_empty(),
            "lowered IR does not verify: {errors:#?}\n{}",
            print_module(&module)
        );
        Ok(module)
    }

    /// Lowers an entry point and runs `groups` thread groups of it over `resources`.
    pub fn dispatch(
        &self,
        path: &str,
        type_args: &[&str],
        statics: &[(&str, Val)],
        groups: [u32; 3],
        resources: &mut Resources,
    ) -> Output {
        let module = self
            .try_entry(path, type_args, statics)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let entry = module.entries[0].func;
        // The optimized module must compute the same, on a copy of the resources.
        let optimized = optimize(&module, path);
        let mut copy = resources.clone();
        let opt_out = Interpreter::new(&optimized)
            .dispatch(optimized.entries[0].func, groups, &mut copy)
            .unwrap_or_else(|e| panic!("{path}, optimized: {e}\n{}", print_module(&optimized)));
        let out = Interpreter::new(&module)
            .dispatch(entry, groups, resources)
            .unwrap_or_else(|e| panic!("{path}: {e}\n{}", print_module(&module)));
        assert_eq!(
            copy.buffers,
            resources.buffers,
            "{path}: the optimized entry writes something else\n{}",
            print_module(&optimized)
        );
        assert_eq!(opt_out.prints, out.prints, "{path}");
        out
    }

    fn render_error(
        &self,
        cx: &marlin_lang_query::Cx<'_>,
        e: &marlin_shaderc_lower::LowerError,
    ) -> String {
        match e.to_diagnostic(cx).unwrap() {
            Some(d) => render_all(&[d], self),
            None => e.to_string(),
        }
    }

    /// Lowers `path` and calls it with `args`.
    pub fn run(&self, path: &str, args: &[Val]) -> Vec<Val> {
        let module = self.lower(&[path]);
        let f = module.find_function(path).unwrap();
        let result = Interpreter::new(&module)
            .call(f, args)
            .unwrap_or_else(|e| panic!("{path}: {e}\n{}", print_module(&module)));
        // The optimized module must compute the same.
        let optimized = optimize(&module, path);
        let g = optimized.find_function(path).unwrap();
        let again = Interpreter::new(&optimized)
            .call(g, args)
            .unwrap_or_else(|e| panic!("{path}, optimized: {e}\n{}", print_module(&optimized)));
        assert_eq!(
            again,
            result,
            "{path}: the optimized function computes something else\n{}",
            print_module(&optimized)
        );
        result
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

/// `module` through the whole optimization pipeline, verified after every pass.
pub fn optimize(module: &Module, what: &str) -> Module {
    let mut m = module.clone();
    let options = marlin_shaderc_opt::Options {
        profile: marlin_shaderc_opt::Profile::Development,
        verify_each: true,
    };
    if let Err(e) = marlin_shaderc_opt::pipeline(&options).run(&mut m) {
        panic!("{what}: {e}\nbefore:\n{}", print_module(module));
    }
    m
}

impl World {
    /// What the optimizer's checks find wrong with the entry point at `path`, rendered as
    /// diagnostics (empty when the pipeline accepts it).
    pub fn problems(&self, path: &str) -> String {
        let module = self
            .try_entry(path, &[], &[])
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut m = module.clone();
        let options = marlin_shaderc_opt::Options {
            profile: marlin_shaderc_opt::Profile::Development,
            verify_each: true,
        };
        let problems = match marlin_shaderc_opt::pipeline(&options).run(&mut m) {
            Ok(_) => return String::new(),
            Err(marlin_shaderc_opt::PassError::Rejected { problems, .. }) => problems,
            Err(e) => panic!("{path}: {e}"),
        };
        let span = |at: Option<u32>| {
            at.map(|i| {
                let l = module.locs[i as usize];
                marlin_lang_diag::Span::new(
                    FileId(l.file),
                    marlin_lang_span::TextRange::new(l.start.into(), l.end.into()),
                )
            })
        };
        let diagnostics: Vec<_> = problems
            .iter()
            .map(|p| {
                let mut d = marlin_lang_diag::Diagnostic::error(
                    marlin_lang_diag::Code(p.code),
                    p.message.clone(),
                );
                if let Some(s) = span(p.at) {
                    d = d.at(s, "");
                }
                if let Some((why, at)) = &p.because
                    && let Some(s) = span(*at)
                {
                    d = d.label(s, why.clone());
                }
                d
            })
            .collect();
        render_all(&diagnostics, self)
    }
}
