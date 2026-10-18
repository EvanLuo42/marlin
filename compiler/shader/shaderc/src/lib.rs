//! The shader compiler's facade (`docs/design/0005-shader-compiler.md` §5, step 9c of §11).
//!
//! A [`Session`] lives long (a cook daemon keeps one, a build script makes one for all its
//! entries): it holds the incremental database, so a changed file only recomputes what depends
//! on it. The host describes packages ([`PackageDesc`]: a name and direct dependencies, read
//! from its own `ShaderPackage` manifests) and gives the files through a [`SourceProvider`];
//! the compiler never touches the file system. One [`EntryRequest`] (an entry point, its type
//! arguments and static parameters, a target, a profile) gives one entry point's code, its
//! reflection (`marlin_shader::reflect`) and diagnostics, or the diagnostics of why not.
//!
//! The pipeline per request: the front end's diagnostics for the packages the entry sees,
//! lowering (0006 §2), the target's capability check (0004 §12.3), the optimizer (0005 §4.4),
//! the backend and its validator, then the reflection. The session is `Sync`; requests may run
//! on many threads at once and share what the front end computed.

mod compile;
mod values;

use std::fmt;
use std::sync::Arc;

use marlin_front_hir_def::{
    FilePackage, FileText, LanguageInput, PackageData, PackageId, PackageInput,
};
use marlin_lang_diag::SourceMap;
use marlin_lang_query::{Db, Durability};
use marlin_lang_span::FileId;
use marlin_shaderc_hir_ty::SHADER_LANGUAGE;

pub use marlin_lang_diag::{Diagnostic, Severity, render_all};
pub use marlin_lang_span::SourcePath;
pub use marlin_shader::reflect::{ConstValue, FieldValue, ShaderReflection, Stage};
pub use marlin_shaderc_opt::Profile;
pub use marlin_shaderc_target::{Backend, Blob, Registry, Target};

/// The source text of packages, from the host (0005 §5.2): from disk for a build script, with
/// content hashes for the cook, with unsaved buffers for the editor.
pub trait SourceProvider: Send + Sync {
    /// The files of `package`, in a stable order.
    fn list(&self, package: &str) -> Result<Vec<SourcePath>, SourceError>;
    fn read(&self, path: &SourcePath) -> Result<Arc<str>, SourceError>;
}

/// A source could not be listed or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError {
    pub message: String,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SourceError {}

/// The targets every session can name: the backends of this build (`vulkan-1.4`).
pub fn default_targets() -> Registry {
    let mut r = Registry::new();
    r.add(Arc::new(marlin_shaderc_backend_spirv::SpirvBackend));
    r
}

pub struct SessionDesc<'a> {
    pub sources: &'a dyn SourceProvider,
    pub targets: &'a Registry,
}

/// A package: its name (`Marlin.Substrate`) and the packages it uses directly, which the
/// session must know already.
#[derive(Clone, Copy, Debug)]
pub struct PackageDesc<'a> {
    pub name: &'a str,
    pub dependencies: &'a [&'a str],
}

/// One entry point to compile.
#[derive(Clone, Debug)]
pub struct EntryRequest<'a> {
    /// The entry function's path (`Marlin.Material.Entries.ResolvePixel`).
    pub entry: &'a str,
    /// Paths of the types for its generic parameters (`Game.Rock`).
    pub type_args: &'a [&'a str],
    /// Static parameters set for this compile, by path (`Game.Rock.UseMoss`, 0004 §13.3).
    pub consts: &'a [(&'a str, ConstValue)],
    /// The target's name in the session's registry (`vulkan-1.4`).
    pub target: &'a str,
    pub profile: Profile,
}

/// A compiled entry point.
#[derive(Clone, Debug)]
pub struct CompileOutput {
    /// The single entry point's code, for the target's graphics API.
    pub code: Blob,
    pub reflection: ShaderReflection,
    /// Warnings.
    pub diagnostics: Vec<Diagnostic>,
    /// The files the result depends on (0005 §8), for the cook's manifest.
    pub used_sources: Vec<SourcePath>,
}

/// An entry point a package declares (see [`Session::entries`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryPoint {
    /// The full path, as [`EntryRequest::entry`] takes it (`Marlin.Engine.ForwardVs`).
    pub path: String,
    pub stage: Stage,
}

/// Why a request did not compile.
#[derive(Clone, Debug)]
pub struct CompileError {
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, d) in self.diagnostics.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "error[{}]: {}", d.code.0, d.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for CompileError {}

/// A package or file the session could not take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionError {
    pub message: String,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

impl From<SourceError> for SessionError {
    fn from(e: SourceError) -> Self {
        SessionError { message: e.message }
    }
}

pub(crate) struct Package {
    pub name: String,
    pub id: PackageId,
    /// Direct dependencies, as indices into the session's packages.
    pub deps: Vec<usize>,
    pub files: Vec<FileId>,
}

pub(crate) struct File {
    pub path: SourcePath,
    pub text: Arc<str>,
    /// xxh3-128 of the text.
    pub hash: u128,
}

/// A long-lived, incremental compiler session.
pub struct Session<'a> {
    sources: &'a dyn SourceProvider,
    pub(crate) targets: &'a Registry,
    pub(crate) db: Db,
    pub(crate) packages: Vec<Package>,
    /// By `FileId`.
    pub(crate) files: Vec<File>,
}

impl<'a> Session<'a> {
    /// A session with the core library (`Marlin`) in it; packages that use it name it as a
    /// dependency.
    pub fn new(desc: SessionDesc<'a>) -> Session<'a> {
        let db = Db::new();
        db.set::<LanguageInput>((), &SHADER_LANGUAGE, Durability::High);
        let mut s = Session {
            sources: desc.sources,
            targets: desc.targets,
            db,
            packages: Vec::new(),
            files: Vec::new(),
        };
        let core = marlin_shaderc_core::PACKAGE;
        // Line endings as `\n`, so the files hash the same on every checkout.
        let texts = marlin_shaderc_core::FILES
            .iter()
            .map(|(path, text)| {
                (
                    SourcePath::new(core, *path),
                    text.replace("\r\n", "\n").into(),
                )
            })
            .collect();
        s.insert_package(core, Vec::new(), texts, Durability::High);
        s
    }

    /// Adds a package and reads its files; every package comes after its dependencies. The
    /// core library (`Marlin`) is the compiler's own and in every session already.
    pub fn add_package(&mut self, desc: &PackageDesc<'_>) -> Result<(), SessionError> {
        if desc.name == marlin_shaderc_core::PACKAGE {
            return Err(SessionError {
                message: format!(
                    "`{}` is the core library, which comes with the compiler",
                    desc.name
                ),
            });
        }
        if self.package(desc.name).is_some() {
            return Err(SessionError {
                message: format!("the package `{}` is already in the session", desc.name),
            });
        }
        let mut deps = Vec::new();
        for d in desc.dependencies {
            let k = self.package(d).ok_or_else(|| SessionError {
                message: format!(
                    "`{}` depends on `{d}`, which the session does not have (add it first)",
                    desc.name
                ),
            })?;
            deps.push(k);
        }
        let texts = self.read_package(desc.name)?;
        self.insert_package(desc.name, deps, texts, Durability::Low);
        Ok(())
    }

    fn insert_package(
        &mut self,
        name: &str,
        deps: Vec<usize>,
        texts: Vec<(SourcePath, Arc<str>)>,
        durability: Durability,
    ) {
        let index = self.packages.len();
        let id = PackageId(index as u32);
        let mut files = Vec::new();
        let mut sets = Vec::new();
        for (path, text) in texts {
            let file = FileId(self.files.len() as u32);
            self.files.push(File {
                hash: marlin_hash::xxh3_128(text.as_bytes()),
                path,
                text: text.clone(),
            });
            files.push(file);
            sets.push((file, text));
        }
        self.db.update(|u| {
            for (file, text) in sets {
                u.set::<FileText>(file, text, durability);
                u.set::<FilePackage>(file, id, durability);
            }
        });
        self.packages.push(Package {
            name: name.to_string(),
            id,
            deps,
            files,
        });
        self.set_package_input(index);
    }

    /// Reads `path`'s package again from the provider after a change: changed files, new
    /// ones and removed ones. What depends on them is recomputed when next asked for.
    pub fn invalidate(&mut self, path: &SourcePath) -> Result<(), SessionError> {
        let index = self.package(path.package()).ok_or_else(|| SessionError {
            message: format!("the session has no package `{}`", path.package()),
        })?;
        if path.package() == marlin_shaderc_core::PACKAGE {
            return Err(SessionError {
                message: "the core library comes with the compiler and does not change".into(),
            });
        }
        let texts = self.read_package(path.package())?;
        let id = self.packages[index].id;
        let mut files = Vec::new();
        // (file, text, new to the session)
        let mut sets = Vec::new();
        for (p, text) in texts {
            match self.files.iter().position(|f| f.path == p) {
                Some(k) => {
                    let f = &mut self.files[k];
                    if f.text != text {
                        f.hash = marlin_hash::xxh3_128(text.as_bytes());
                        f.text = text.clone();
                        sets.push((FileId(k as u32), text, false));
                    }
                    files.push(FileId(k as u32));
                }
                None => {
                    let file = FileId(self.files.len() as u32);
                    self.files.push(File {
                        hash: marlin_hash::xxh3_128(text.as_bytes()),
                        path: p,
                        text: text.clone(),
                    });
                    sets.push((file, text, true));
                    files.push(file);
                }
            }
        }
        self.db.update(|u| {
            for (file, text, new) in sets {
                u.set::<FileText>(file, text, Durability::Low);
                if new {
                    u.set::<FilePackage>(file, id, Durability::Low);
                }
            }
        });
        if files != self.packages[index].files {
            self.packages[index].files = files;
            self.set_package_input(index);
        }
        Ok(())
    }

    /// Compiles one entry point.
    pub fn compile(&self, request: &EntryRequest<'_>) -> Result<CompileOutput, CompileError> {
        compile::compile(self, request)
    }

    /// The entry points of `package` that compile without type arguments: its public,
    /// non-generic functions with a stage attribute, sorted by full path. A cook compiles all
    /// of them into the package's library (0003 §4.4); static parameters take their defaults.
    pub fn entries(&self, package: &str) -> Result<Vec<EntryPoint>, SessionError> {
        let index = self.package(package).ok_or_else(|| SessionError {
            message: format!("the session has no package `{package}`"),
        })?;
        Ok(compile::entries(self, index))
    }

    /// Compiles several entry points, in order (a host that wants threads gives each thread
    /// its own requests: the session is `Sync`).
    pub fn compile_batch(
        &self,
        requests: &[EntryRequest<'_>],
    ) -> Vec<Result<CompileOutput, CompileError>> {
        requests.iter().map(|r| self.compile(r)).collect()
    }

    /// What a cache key needs of the compiler for `target` (0005 §8): the compiler's own
    /// source hash and the target description's name and version. `None` for an unknown
    /// target.
    pub fn fingerprint(&self, target: &str) -> Option<u128> {
        let (t, _) = self.targets.get(target)?;
        Some(fingerprint(t))
    }

    /// Diagnostics rendered as rustc-style text, with this session's files.
    pub fn render(&self, diagnostics: &[Diagnostic]) -> String {
        render_all(diagnostics, self)
    }

    /// The files of `package` and their text, from the provider; every path names the
    /// package.
    fn read_package(&self, package: &str) -> Result<Vec<(SourcePath, Arc<str>)>, SessionError> {
        let mut out = Vec::new();
        for path in self.sources.list(package)? {
            if path.package() != package {
                return Err(SessionError {
                    message: format!("the provider lists `{path}` as a file of `{package}`"),
                });
            }
            let text = self.sources.read(&path)?;
            out.push((path, text));
        }
        Ok(out)
    }

    pub(crate) fn package(&self, name: &str) -> Option<usize> {
        self.packages.iter().position(|p| p.name == name)
    }

    fn set_package_input(&mut self, index: usize) {
        let p = &self.packages[index];
        let data = Arc::new(PackageData {
            name: p.name.as_str().into(),
            files: p.files.clone().into(),
            dependencies: p
                .deps
                .iter()
                .map(|&d| self.packages[d].id)
                .collect::<Vec<_>>()
                .into(),
        });
        self.db.set::<PackageInput>(p.id, data, Durability::Low);
    }

    /// `package` and every package it sees through its dependencies, in session order.
    pub(crate) fn closure(&self, package: usize) -> Vec<usize> {
        let mut out = vec![package];
        let mut at = 0;
        while at < out.len() {
            for &d in &self.packages[out[at]].deps {
                if !out.contains(&d) {
                    out.push(d);
                }
            }
            at += 1;
        }
        out.sort_unstable();
        out
    }
}

/// The compiler's source hash (computed when it was built) with the target's name and version.
fn fingerprint(t: &Target) -> u128 {
    let mut bytes = Vec::new();
    bytes.extend(env!("MARLIN_SHADERC_SOURCE_HASH").as_bytes());
    bytes.push(0);
    bytes.extend(t.name.as_bytes());
    bytes.push(0);
    bytes.extend(t.version.to_le_bytes());
    marlin_hash::xxh3_128(&bytes)
}

impl SourceMap for Session<'_> {
    fn name(&self, file: FileId) -> String {
        self.files
            .get(file.0 as usize)
            .map_or_else(|| format!("file {}", file.0), |f| f.path.to_string())
    }

    fn text(&self, file: FileId) -> &str {
        self.files.get(file.0 as usize).map_or("", |f| &f.text)
    }
}
