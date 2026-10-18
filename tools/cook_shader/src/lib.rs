//! Shader compilation and permutations (`docs/design/0003-asset-pipeline.md` §4.4).
//!
//! [`ShaderPackageCooker`] cooks a [`ShaderPackage`] into a [`ShaderLibrary`]: it reads the
//! package's `.mshader` files through its `source:` links, compiles every entry point the
//! package declares ([`Session::entries`]) for the platform's `shader-target` with
//! `marlin-shaderc`, and stores each entry's code with its reflection. The package's name is the
//! object's `!Name`; the type is registered as named, so the runtime finds the library by it.

use std::collections::HashMap;
use std::sync::Arc;

use marlin_asset::cooked::Encoding;
use marlin_cook_core::{CookContext, Cooked, Cooker, Platform, Profile, SourceObject};
use marlin_reflect::type_info;
use marlin_shader::asset::{LibraryEntry, ShaderLibrary, ShaderPackage};
use marlin_shaderc::{
    CompileError, CompileOutput, EntryRequest, PackageDesc, Registry, Session, SessionDesc,
    SourceError, SourcePath, SourceProvider, default_targets,
};

/// The core library every package depends on (it comes with the compiler).
const CORE: &str = "Marlin";

/// Cooks [`ShaderPackage`]s; register it with `Cookers::register_named`.
pub struct ShaderPackageCooker {
    targets: Registry,
    /// The compiler's fingerprint for each target it has (0005 §8).
    fingerprints: HashMap<String, u128>,
}

impl ShaderPackageCooker {
    pub fn new() -> Self {
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

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let package = object
            .value
            .as_ref()
            .downcast::<ShaderPackage>()
            .ok_or("not a ShaderPackage")?;
        if !package.dependencies.is_empty() {
            return Err(
                "packages that depend on other packages do not cook yet: that needs \
                 build dependencies (0003 §4.4); only the core library can be used"
                    .into(),
            );
        }
        let name = object.name;
        let target = ctx.platform().shader_target;
        if !self.fingerprints.contains_key(target) {
            let known = self.targets.names().join("`, `");
            return Err(format!(
                "the platform's shader target `{target}` has no compiler backend (there are `{known}`)"
            ));
        }

        // The package's files, as the compiler sees them: `<package>/<asset path>`.
        let mut files = Files::default();
        for source in &package.sources {
            let bytes = ctx.read_source(&source.path)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("`{}` is not UTF-8", source.path))?
                // Line endings as `\n`, so a checkout's settings do not change the code.
                .replace("\r\n", "\n");
            files.add(SourcePath::new(name, source.path.as_str()), text);
        }
        if files.0.is_empty() {
            return Err("the package lists no sources".into());
        }
        let mut session = Session::new(SessionDesc {
            sources: &files,
            targets: &self.targets,
        });
        session
            .add_package(&PackageDesc {
                name,
                dependencies: &[CORE],
            })
            .map_err(|e| e.message)?;
        let entries = session.entries(name).map_err(|e| e.message)?;
        if entries.is_empty() {
            ctx.warn(
                "the package declares no entry points (public functions with a stage attribute)",
            );
        }
        let profile = match ctx.profile() {
            Profile::Development => marlin_shaderc::Profile::Development,
            Profile::Shipping => marlin_shaderc::Profile::Shipping,
        };
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        let outputs = compile_all(&session, &paths, target, profile);

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
        let payload = marlin_serial::to_bytes(&library).map_err(|e| format!("{e:?}"))?;
        Ok(Cooked {
            type_id: type_info::<ShaderLibrary>().id,
            encoding: Encoding::Serial,
            payload,
        })
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

/// A package's files, read by the cooker beforehand (the compiler never touches the disk).
#[derive(Default)]
struct Files(Vec<(SourcePath, Arc<str>)>);

impl Files {
    fn add(&mut self, path: SourcePath, text: String) {
        self.0.push((path, text.into()));
        self.0.sort_by(|a, b| a.0.path().cmp(b.0.path()));
    }
}

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
