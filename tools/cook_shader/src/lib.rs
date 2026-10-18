//! Shader compilation and permutations (`docs/design/0003-asset-pipeline.md` §4.4).
//!
//! [`ShaderPackageCooker`] cooks a [`ShaderPackage`] into a [`ShaderLibrary`]: it reads the
//! package's `.mshader` files through its `source:` links, compiles every entry point the
//! package declares ([`Session::entries`]) for the platform's `shader-target` with
//! `marlin-shaderc`, and stores each entry's code with its reflection. The package's name is the
//! object's `!Name`; the type is registered as named, so the runtime finds the library by it.
//!
//! [`ShaderCooker`] cooks a [`Shader`] permutation (an entry with type arguments or static
//! parameters) into a library of that one entry.
//!
//! Both build from packages: `ShaderPackage.Dependencies` and `Shader.Package` are build
//! dependencies (0003 §4.1), read through the cook context with their sources, so the session
//! of a compile holds the package and, before it, every package it depends on.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use marlin_asset::Uuid;
use marlin_asset::cooked::Encoding;
use marlin_cook_core::{CookContext, Cooked, Cooker, Platform, Profile, SourceObject};
use marlin_reflect::type_info;
use marlin_shader::asset::{LibraryEntry, Shader, ShaderLibrary, ShaderPackage};
use marlin_shader::reflect::ConstValue;
use marlin_shaderc::{
    CompileError, CompileOutput, EntryRequest, PackageDesc, Registry, Session, SessionDesc,
    SourceError, SourcePath, SourceProvider, default_targets,
};

/// The core library every package depends on (it comes with the compiler).
const CORE: &str = "Marlin";

/// The type a shader build depends on (see `Cooker::build_dependency_types`).
const BUILT_FROM: &[&str] = &["ShaderPackage"];

/// The compiler's targets and what a cook key needs of each.
struct Compiler {
    targets: Registry,
    /// The compiler's fingerprint for each target it has (0005 §8).
    fingerprints: HashMap<String, u128>,
}

impl Compiler {
    fn new() -> Self {
        let targets = default_targets();
        let empty = Files::default();
        let session = Session::new(SessionDesc {
            sources: &empty,
            targets: &targets,
        });
        let fingerprints = targets
            .names()
            .into_iter()
            .filter_map(|name| Some((name.to_owned(), session.fingerprint(name)?)))
            .collect();
        drop(session);
        Self {
            targets,
            fingerprints,
        }
    }

    /// The compiler (its sources and the target description) and the profile.
    fn key(&self, platform: &Platform, profile: Profile) -> u128 {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(platform.shader_target.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(profile.name().as_bytes());
        bytes.push(0);
        // An unknown target cooks to an error; its key only has to be stable.
        let compiler = self.fingerprints.get(platform.shader_target).copied();
        bytes.extend_from_slice(&compiler.unwrap_or(0).to_le_bytes());
        marlin_hash::xxh3_128(&bytes)
    }

    /// The platform's target, if the compiler has a backend for it.
    fn target<'p>(&self, ctx: &CookContext<'p>) -> Result<&'p str, String> {
        let target = ctx.platform().shader_target;
        if self.fingerprints.contains_key(target) {
            return Ok(target);
        }
        let known = self.targets.names().join("`, `");
        Err(format!(
            "the platform's shader target `{target}` has no compiler backend (there are `{known}`)"
        ))
    }
}

fn profile(ctx: &CookContext<'_>) -> marlin_shaderc::Profile {
    match ctx.profile() {
        Profile::Development => marlin_shaderc::Profile::Development,
        Profile::Shipping => marlin_shaderc::Profile::Shipping,
    }
}

fn library_payload(library: &ShaderLibrary) -> Result<Cooked, String> {
    let payload = marlin_serial::to_bytes(library).map_err(|e| format!("{e:?}"))?;
    Ok(Cooked {
        type_id: type_info::<ShaderLibrary>().id,
        encoding: Encoding::Serial,
        payload,
    })
}

/// Cooks [`ShaderPackage`]s; register it with `Cookers::register_named`.
pub struct ShaderPackageCooker {
    compiler: Compiler,
}

impl ShaderPackageCooker {
    pub fn new() -> Self {
        Self {
            compiler: Compiler::new(),
        }
    }
}

impl Default for ShaderPackageCooker {
    fn default() -> Self {
        Self::new()
    }
}

impl Cooker for ShaderPackageCooker {
    fn version(&self) -> u32 {
        1
    }

    fn key(&self, platform: &Platform, profile: Profile) -> u128 {
        self.compiler.key(platform, profile)
    }

    fn build_dependency_types(&self) -> &[&str] {
        BUILT_FROM
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let package = object
            .value
            .as_ref()
            .downcast::<ShaderPackage>()
            .ok_or("not a ShaderPackage")?;
        let name = object.name;
        let target = self.compiler.target(ctx)?;
        let packages = Packages::read(object.uuid, name, package, ctx)?;
        let session = packages.session(&self.compiler.targets)?;
        let entries = session.entries(name).map_err(|e| e.message)?;
        if entries.is_empty() {
            ctx.warn(
                "the package declares no entry points (public functions with a stage attribute)",
            );
        }
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        let outputs = compile_all(&session, &paths, target, profile(ctx));

        let mut library = ShaderLibrary {
            package: name.to_owned(),
            target: target.to_owned(),
            entries: Vec::with_capacity(outputs.len()),
        };
        let mut errors = Vec::new();
        for (path, output) in paths.iter().zip(outputs) {
            match output {
                Ok(out) => {
                    if !out.diagnostics.is_empty() {
                        ctx.warn(session.render(&out.diagnostics));
                    }
                    library.entries.push(LibraryEntry {
                        name: (*path).to_owned(),
                        code: out.code.bytes,
                        reflection: out.reflection,
                    });
                }
                Err(e) => errors.push(format!("`{path}`:\n{}", session.render(&e.diagnostics))),
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        library_payload(&library)
    }
}

/// Cooks [`Shader`] permutations; register it with `Cookers::register`.
pub struct ShaderCooker {
    compiler: Compiler,
}

impl ShaderCooker {
    pub fn new() -> Self {
        Self {
            compiler: Compiler::new(),
        }
    }
}

impl Default for ShaderCooker {
    fn default() -> Self {
        Self::new()
    }
}

impl Cooker for ShaderCooker {
    fn version(&self) -> u32 {
        1
    }

    fn key(&self, platform: &Platform, profile: Profile) -> u128 {
        self.compiler.key(platform, profile)
    }

    fn build_dependency_types(&self) -> &[&str] {
        BUILT_FROM
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let shader = object
            .value
            .as_ref()
            .downcast::<Shader>()
            .ok_or("not a Shader")?;
        if shader.package.is_none() {
            return Err("`Package` is not set: link the package to compile in".into());
        }
        if shader.entry.is_empty() {
            return Err("`Entry` is not set: give the entry function's full path".into());
        }
        let target = self.compiler.target(ctx)?;
        let root = ctx.dependency(shader.package.target)?;
        let package = as_package(&root.value, &root.name, &root.type_name)?;
        let packages = Packages::read(root.uuid, &root.name, package, ctx)?;
        let session = packages.session(&self.compiler.targets)?;

        let type_args: Vec<&str> = shader.type_args.iter().map(String::as_str).collect();
        let consts: Vec<(&str, ConstValue)> = shader
            .consts
            .iter()
            .map(|(path, value)| (path.as_str(), value.clone()))
            .collect();
        let output = session
            .compile(&EntryRequest {
                entry: &shader.entry,
                type_args: &type_args,
                consts: &consts,
                target,
                profile: profile(ctx),
            })
            .map_err(|e| format!("`{}`:\n{}", shader.entry, session.render(&e.diagnostics)))?;
        if !output.diagnostics.is_empty() {
            ctx.warn(session.render(&output.diagnostics));
        }
        library_payload(&ShaderLibrary {
            package: root.name.clone(),
            target: target.to_owned(),
            entries: vec![LibraryEntry {
                name: shader.entry.clone(),
                code: output.code.bytes,
                reflection: output.reflection,
            }],
        })
    }
}

fn as_package<'v>(
    value: &'v marlin_reflect::Value,
    name: &str,
    type_name: &str,
) -> Result<&'v ShaderPackage, String> {
    value
        .as_ref()
        .downcast::<ShaderPackage>()
        .ok_or_else(|| format!("`{name}` is a `{type_name}`, not a `ShaderPackage`"))
}

/// The packages a compile sees: one and, before it, every package it depends on, with their
/// files.
#[derive(Default)]
struct Packages {
    files: Files,
    /// In dependency order: each with the packages it uses directly.
    order: Vec<(String, Vec<String>)>,
    done: HashSet<Uuid>,
    /// Name to package, so two packages with one name in a closure are an error.
    names: HashMap<String, Uuid>,
}

impl Packages {
    /// `package` (object `uuid`, named `name`) and its dependencies, transitively.
    fn read(
        uuid: Uuid,
        name: &str,
        package: &ShaderPackage,
        ctx: &mut CookContext<'_>,
    ) -> Result<Packages, String> {
        let mut packages = Packages::default();
        packages.visit(uuid, name, package, ctx, &mut Vec::new())?;
        packages.files.0.sort_by(|a, b| a.0.path().cmp(b.0.path()));
        Ok(packages)
    }

    /// Adds `package` after its dependencies; `path` is the packages that wait for it.
    fn visit(
        &mut self,
        uuid: Uuid,
        name: &str,
        package: &ShaderPackage,
        ctx: &mut CookContext<'_>,
        path: &mut Vec<(Uuid, String)>,
    ) -> Result<(), String> {
        if self.done.contains(&uuid) {
            return Ok(());
        }
        if let Some(at) = path.iter().position(|(u, _)| *u == uuid) {
            let mut cycle: Vec<&str> = path[at..].iter().map(|(_, n)| n.as_str()).collect();
            cycle.push(name);
            return Err(format!(
                "the packages depend on each other: {}",
                cycle.join(" → ")
            ));
        }
        path.push((uuid, name.to_owned()));
        let mut uses = vec![CORE.to_owned()];
        for link in &package.dependencies {
            if link.is_none() {
                return Err(format!("`{name}` lists an empty link in `Dependencies`"));
            }
            let dependency = ctx.dependency(link.target)?;
            let inner = as_package(&dependency.value, &dependency.name, &dependency.type_name)?;
            self.visit(dependency.uuid, &dependency.name, inner, ctx, path)?;
            if !uses.contains(&dependency.name) {
                uses.push(dependency.name.clone());
            }
        }
        path.pop();

        if let Some(&other) = self.names.get(name)
            && other != uuid
        {
            return Err(format!(
                "two packages named `{name}` ({other} and {uuid}) are in one build"
            ));
        }
        if package.sources.is_empty() {
            return Err(format!("the package `{name}` lists no sources"));
        }
        // The package's files, as the compiler sees them: `<package>/<asset path>`.
        for source in &package.sources {
            let bytes = ctx.read_source(&source.path)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("`{}` is not UTF-8", source.path))?
                // Line endings as `\n`, so a checkout's settings do not change the code.
                .replace("\r\n", "\n");
            self.files
                .0
                .push((SourcePath::new(name, source.path.as_str()), text.into()));
        }
        self.names.insert(name.to_owned(), uuid);
        self.done.insert(uuid);
        self.order.push((name.to_owned(), uses));
        Ok(())
    }

    /// A session holding the packages.
    fn session<'s>(&'s self, targets: &'s Registry) -> Result<Session<'s>, String> {
        let mut session = Session::new(SessionDesc {
            sources: &self.files,
            targets,
        });
        for (name, uses) in &self.order {
            let uses: Vec<&str> = uses.iter().map(String::as_str).collect();
            session
                .add_package(&PackageDesc {
                    name,
                    dependencies: &uses,
                })
                .map_err(|e| e.message)?;
        }
        Ok(session)
    }
}

/// Compiles `entries` on a few threads (the session is shared); results in `entries`' order.
fn compile_all(
    session: &Session<'_>,
    entries: &[&str],
    target: &str,
    profile: marlin_shaderc::Profile,
) -> Vec<Result<CompileOutput, CompileError>> {
    let request = |entry| EntryRequest {
        entry,
        type_args: &[],
        consts: &[],
        target,
        profile,
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(entries.len())
        .max(1);
    let chunk = entries.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = entries
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|&e| session.compile(&request(e)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    })
}

/// The packages' files, read by the cooker beforehand (the compiler never touches the disk).
#[derive(Default)]
struct Files(Vec<(SourcePath, Arc<str>)>);

impl SourceProvider for Files {
    fn list(&self, package: &str) -> Result<Vec<SourcePath>, SourceError> {
        Ok(self
            .0
            .iter()
            .filter(|(p, _)| p.package() == package)
            .map(|(p, _)| p.clone())
            .collect())
    }

    fn read(&self, path: &SourcePath) -> Result<Arc<str>, SourceError> {
        self.0
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, text)| text.clone())
            .ok_or_else(|| SourceError {
                message: format!("no file {path}"),
            })
    }
}
