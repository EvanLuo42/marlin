//! Cooking units: keys, assembly and the parallel driver.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use marlin_asset::Uuid;
use marlin_asset::cooked::{platform_id, unit_id, write};
use marlin_asset_db::{Content, ContentFile, ContentObject, MASSET_EXT, MountId};
use marlin_hash::xxh3_128;
use marlin_masset::{DocumentLinks, Severity, parse};

use crate::cooker::{BuildInputs, CookContext, Cookers, SourceObject};
use crate::ddc::Ddc;
use crate::manifest::{Manifest, ManifestUnit};
use crate::platform::{Platform, Profile};

/// Bump when the unit layout or key derivation changes: every key changes with it.
pub const COOK_FORMAT: u32 = 1;

#[derive(Clone, Debug)]
pub struct CookOptions {
    /// The target, from `platforms/<name>/platform.toml`.
    pub platform: &'static Platform,
    /// 0: one thread per logical CPU.
    pub threads: usize,
    pub profile: Profile,
}

impl Default for CookOptions {
    fn default() -> Self {
        Self {
            platform: Platform::host(),
            threads: 0,
            profile: Profile::Development,
        }
    }
}

/// Why a unit did not cook.
#[derive(Debug, marlin_error::Error)]
pub enum CookError {
    #[error("{path}: {message}")]
    Unit { path: String, message: String },
    #[error("{path}: {object}: {message}")]
    Object {
        path: String,
        object: String,
        message: String,
    },
    #[error("{path}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// One cooked (or cache-hit) unit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnitOutcome {
    /// Asset path (the file's path without `.masset`).
    pub path: String,
    pub unit: u64,
    pub key: u128,
    /// Found in the cache: not cooked again.
    pub cached: bool,
    pub size: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct CookReport {
    pub units: Vec<UnitOutcome>,
    pub errors: Vec<CookError>,
}

impl CookReport {
    pub fn cooked(&self) -> usize {
        self.units.iter().filter(|u| !u.cached).count()
    }

    pub fn cached(&self) -> usize {
        self.units.iter().filter(|u| u.cached).count()
    }
}

/// Feeds the cook key, field by field.
struct KeyWriter(Vec<u8>);

impl KeyWriter {
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(&(b.len() as u64).to_le_bytes());
        self.0.extend_from_slice(b);
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u128(&mut self, v: u128) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

/// The unit of the object with `uuid` as `mount` sees it, from the index.
fn unit_of(content: &Content, mount: MountId, uuid: Uuid) -> Option<u64> {
    let object = content.visible_object(mount, uuid)?;
    Some(unit_id(&content.path(content.object_file(object))))
}

/// The objects `object` is built from (0003 §4.1): the targets of its links whose type its
/// cooker declares as build dependencies, transitively, without `object` itself. They may be
/// in the mounts below `object`'s (0003 §4.5).
fn build_closure(
    content: &Content,
    cookers: &Cookers,
    object: ContentObject,
) -> Vec<ContentObject> {
    let mut seen = HashSet::from([object]);
    let mut queue = vec![object];
    let mut out = Vec::new();
    while let Some(o) = queue.pop() {
        for target in build_dependencies(content, cookers, o) {
            if seen.insert(target) {
                out.push(target);
                queue.push(target);
            }
        }
    }
    out
}

/// The direct build dependencies of `object` (see [`build_closure`]).
fn build_dependencies<'a>(
    content: &'a Content,
    cookers: &'a Cookers,
    object: ContentObject,
) -> impl Iterator<Item = ContentObject> + 'a {
    let db = content.db(object.mount);
    let from = db.object_info(object.object).type_name;
    db.links(object.object).filter_map(move |link| {
        let target = content.link_target(object, link)?;
        cookers
            .is_build_dependency(from, content.object_info(target).type_name)
            .then_some(target)
    })
}

/// Why a key could not be computed: an ordinary unit error, or a cycle of build dependencies
/// (reported once, by the unit the cook asked for, rather than wrapped by every unit on it).
enum KeyError {
    Unit(CookError),
    Cycle(Vec<ContentFile>),
}

/// Computes cook keys, each unit's once: a build dependency's key is part of every key of the
/// units built from it, so a cook that asks for many keys shares them.
struct Keys<'a> {
    content: &'a Content,
    cookers: &'a Cookers,
    platform: &'a Platform,
    profile: Profile,
    done: Mutex<HashMap<ContentFile, u128>>,
}

impl<'a> Keys<'a> {
    fn new(
        content: &'a Content,
        cookers: &'a Cookers,
        platform: &'a Platform,
        profile: Profile,
    ) -> Self {
        Self {
            content,
            cookers,
            platform,
            profile,
            done: Mutex::new(HashMap::new()),
        }
    }

    fn key(&self, file: ContentFile) -> Result<u128, CookError> {
        self.key_on(file, &mut Vec::new()).map_err(|e| match e {
            KeyError::Unit(e) => e,
            KeyError::Cycle(files) => CookError::Unit {
                path: self.content.file_path(file),
                message: format!(
                    "its build dependencies form a cycle: {}",
                    files
                        .iter()
                        .map(|&f| self.content.file_path(f))
                        .collect::<Vec<_>>()
                        .join(" → ")
                ),
            },
        })
    }

    /// `file`'s key, with `stack` the units whose keys wait for it.
    fn key_on(&self, file: ContentFile, stack: &mut Vec<ContentFile>) -> Result<u128, KeyError> {
        if let Some(&key) = self.lock().get(&file) {
            return Ok(key);
        }
        if let Some(at) = stack.iter().position(|&f| f == file) {
            let mut cycle = stack[at..].to_vec();
            cycle.push(file);
            return Err(KeyError::Cycle(cycle));
        }
        stack.push(file);
        let key = self.compute(file, stack);
        stack.pop();
        let key = key?;
        self.lock().insert(file, key);
        Ok(key)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ContentFile, u128>> {
        // The map only caches finished keys: a panic elsewhere leaves it valid.
        self.done.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn compute(&self, file: ContentFile, stack: &mut Vec<ContentFile>) -> Result<u128, KeyError> {
        let (content, cookers) = (self.content, self.cookers);
        let db = content.db(file.mount);
        let info = db.file_info(file.file);
        let fail = |message: String| {
            KeyError::Unit(CookError::Unit {
                path: content.file_path(file),
                message,
            })
        };
        if let Some(error) = info.parse_error {
            return Err(fail(format!("does not parse: {error}")));
        }
        let mut key = KeyWriter(Vec::with_capacity(256));
        key.bytes(b"marlin-cook");
        key.u64(u64::from(COOK_FORMAT));
        key.bytes(self.platform.name.as_bytes());
        // What cookers read from the description; package codecs are not unit inputs.
        for family in self.platform.texture_compression {
            key.bytes(family.name().as_bytes());
        }
        key.bytes(self.platform.texture_layout.name().as_bytes());
        // The qualified path: the mount is part of the unit.
        key.u64(unit_id(&content.path(file)));
        key.u128(info.hash);
        for object in db.objects_in(file.file) {
            let o = db.object_info(object);
            let registered = cookers
                .get(o.type_name)
                .ok_or_else(|| fail(format!("{}: no cooker for type `{}`", o.name, o.type_name)))?;
            key.bytes(o.type_name.as_bytes());
            key.u64(registered.fingerprint);
            key.u128(registered.cooker.key(self.platform, self.profile));
            for source in db.source_links(object) {
                key.bytes(source.path.as_bytes());
                match db.file(source.path) {
                    Some(f) => key.u128(db.file_info(f).hash),
                    None => {
                        return Err(fail(format!(
                            "{}: source file `{}` does not exist",
                            o.name, source.path
                        )));
                    }
                }
            }
            let from = ContentObject {
                mount: file.mount,
                object,
            };
            for link in db.links(object) {
                let Some(target) = content.link_target(from, link) else {
                    continue;
                };
                let t = content.object_info(target);
                let target_file = content.object_file(target);
                key.u128(t.uuid.as_u128());
                if cookers.is_build_dependency(o.type_name, t.type_name) {
                    // Built from: all of the target's unit's inputs. One in this unit is in
                    // this key already.
                    key.bytes(b"build");
                    if target_file != file {
                        let dependency = self.key_on(target_file, stack).map_err(|e| match e {
                            KeyError::Unit(e) => {
                                fail(format!("{}: its build dependency {e}", o.name))
                            }
                            cycle => cycle,
                        })?;
                        key.u128(dependency);
                    }
                } else {
                    key.u64(unit_id(&content.path(target_file)));
                }
            }
        }
        Ok(xxh3_128(&key.0))
    }
}

/// The cook key of `file`: a hash over every input of its cooked unit.
///
/// Inputs: the format version; the platform's name and the texture description cookers read;
/// the unit id (of the qualified path, so the mount is in it); the source text; per object its
/// type, the type's fingerprint (structure and cooker version) and its cooker's own key for the
/// platform and profile; every source file it links to, by content; the unit of every object it
/// references (which the import table records); and the key of every unit holding a build
/// dependency (0003 §4.1), which covers that unit's inputs and, recursively, its own build
/// dependencies, in whichever mount. A runtime reference's target content is not an input:
/// links are resolved at load time. Build dependencies that form a cycle between units are an
/// error. Nothing machine-specific is an input: the same engine content has the same keys in
/// every project.
pub fn unit_key(
    content: &Content,
    cookers: &Cookers,
    file: ContentFile,
    platform: &Platform,
    profile: Profile,
) -> Result<u128, CookError> {
    Keys::new(content, cookers, platform, profile).key(file)
}

struct CookedObject {
    uuid: Uuid,
    entry_type: u64,
    encoding: marlin_asset::cooked::Encoding,
    payload: Vec<u8>,
    bulk: Vec<marlin_asset::cooked::write::UnitBulk>,
}

/// Lays out a unit (see `marlin_asset::cooked`).
fn assemble(
    unit: u64,
    platform: &Platform,
    objects: Vec<CookedObject>,
    imports: BTreeMap<Uuid, u64>,
) -> Vec<u8> {
    let objects = objects
        .into_iter()
        .map(|o| write::UnitObject {
            uuid: o.uuid,
            type_id: o.entry_type,
            encoding: o.encoding,
            payload: o.payload,
            bulk: o.bulk,
        })
        .collect();
    let imports: Vec<(Uuid, u64)> = imports.into_iter().collect();
    write::unit(unit, platform_id(platform.name), objects, &imports)
}

/// Cooks one unit (no cache involved).
pub fn cook_unit(
    content: &Content,
    cookers: &Cookers,
    file: ContentFile,
    platform: &Platform,
    profile: Profile,
) -> Result<(Vec<u8>, Vec<String>), CookError> {
    let mount = file.mount;
    let db = content.db(mount);
    let info = db.file_info(file.file);
    // Links inside the mount are written relative to it.
    let own_path = db.asset_path(file.file);
    let path = content.file_path(file);
    let full = db.root().join(&info.path);
    let text = std::fs::read_to_string(&full).map_err(|source| CookError::Io {
        path: path.clone(),
        source,
    })?;
    if xxh3_128(text.as_bytes()) != info.hash {
        return Err(CookError::Unit {
            path,
            message: "changed since it was indexed; update the index first".into(),
        });
    }
    let doc = parse(&text).map_err(|e| CookError::Unit {
        path: path.clone(),
        message: e.to_string(),
    })?;
    let links = DocumentLinks {
        document: &doc,
        path: &own_path,
        fallback: &content.links(mount),
    };
    let unit = unit_id(&content.path(file));
    let mut warnings = Vec::new();
    let mut objects = Vec::new();
    let mut imports = BTreeMap::new();
    let local: HashSet<Uuid> = doc.objects.iter().map(|o| o.uuid).collect();
    for object in &doc.objects {
        let object_error = |message: String| CookError::Object {
            path: path.clone(),
            object: object.name.clone(),
            message,
        };
        let registered = cookers
            .get(&object.type_name)
            .ok_or_else(|| object_error(format!("no cooker for type `{}`", object.type_name)))?;
        let read = cookers
            .schema()
            .read_object(object, &links)
            .map_err(|d| object_error(d.display(&text)))?;
        if let Some(error) = read
            .diagnostics
            .iter()
            .find(|d| d.severity == Severity::Error)
        {
            return Err(object_error(error.display(&text)));
        }
        warnings.extend(
            read.diagnostics
                .iter()
                .map(|d| format!("{}: {}", object.name, d.display(&text))),
        );
        let indexed = db
            .object(object.uuid)
            .map(|object| ContentObject { mount, object });
        // Built from, not loaded with: not imports.
        let built_from: HashSet<Uuid> = indexed
            .map(|o| {
                build_dependencies(content, cookers, o)
                    .map(|t| content.object_info(t).uuid)
                    .collect()
            })
            .unwrap_or_default();
        let closure = indexed
            .map(|o| build_closure(content, cookers, o))
            .unwrap_or_default();
        for link in &read.links {
            if local.contains(&link.target) || built_from.contains(&link.target) {
                continue;
            }
            let target = unit_of(content, mount, link.target).ok_or_else(|| {
                object_error(format!(
                    "links to {}, which no indexed file defines",
                    link.target
                ))
            })?;
            imports.insert(link.target, target);
        }
        // The object's and its build dependencies', by qualified path, with the files and the
        // hashes the keys used: `read_source` checks the bytes it reads against them.
        let sources: HashMap<String, (PathBuf, u128)> = indexed
            .into_iter()
            .chain(closure.iter().copied())
            .flat_map(|o| {
                let m = content.mount(o.mount);
                m.db.source_links(o.object).filter_map(move |s| {
                    let f = m.db.file(s.path)?;
                    Some((
                        format!("/{}/{}", m.name, s.path),
                        (m.db.root().join(s.path), m.db.file_info(f).hash),
                    ))
                })
            })
            .collect();
        let mut ctx = CookContext {
            platform,
            profile,
            mount: &content.mount(mount).name,
            sources: &sources,
            build: BuildInputs {
                content,
                cookers,
                // With the object itself, which a cycle in this unit leads back to.
                allowed: indexed
                    .into_iter()
                    .chain(closure.iter().copied())
                    .map(|o| (content.object_info(o).uuid, o))
                    .collect(),
                read: HashMap::new(),
            },
            bulk: Vec::new(),
            warnings: Vec::new(),
        };
        let source = SourceObject {
            uuid: object.uuid,
            name: &object.name,
            type_name: &object.type_name,
            value: &read.value,
        };
        let cooked = registered
            .cooker
            .cook(&source, &mut ctx)
            .map_err(object_error)?;
        warnings.extend(
            ctx.warnings
                .into_iter()
                .map(|w| format!("{}: {w}", object.name)),
        );
        objects.push(CookedObject {
            uuid: object.uuid,
            entry_type: cooked.type_id.0,
            encoding: cooked.encoding,
            payload: cooked.payload,
            bulk: ctx.bulk,
        });
    }
    Ok((assemble(unit, platform, objects, imports), warnings))
}

/// Every `.masset` file of every mount, by qualified path.
pub fn source_units(content: &Content) -> Vec<ContentFile> {
    let ext = format!(".{MASSET_EXT}");
    let mut files: Vec<ContentFile> = content
        .files()
        .filter(|&f| content.db(f.mount).file_info(f.file).path.ends_with(&ext))
        .collect();
    files.sort_by_cached_key(|&f| content.path(f));
    files
}

/// Cooks `units` that are not in `ddc` yet, in parallel, and stores the results.
pub fn cook(
    content: &Content,
    cookers: &Cookers,
    ddc: &dyn Ddc,
    options: &CookOptions,
    units: &[ContentFile],
) -> CookReport {
    let threads = match options.threads {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    }
    .clamp(1, units.len().max(1));
    let chunk = units.len().div_ceil(threads).max(1);
    let keys = &Keys::new(content, cookers, options.platform, options.profile);
    let results: Vec<Result<UnitOutcome, CookError>> = std::thread::scope(|s| {
        let handles: Vec<_> = units
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|&file| cook_one(keys, ddc, file))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            // A cooker that panicked is a bug: re-raise it rather than drop the chunk's units.
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    let mut report = CookReport::default();
    for result in results {
        match result {
            Ok(outcome) => report.units.push(outcome),
            Err(e) => report.errors.push(e),
        }
    }
    report
}

fn cook_one(keys: &Keys<'_>, ddc: &dyn Ddc, file: ContentFile) -> Result<UnitOutcome, CookError> {
    let (content, cookers, platform, profile) =
        (keys.content, keys.cookers, keys.platform, keys.profile);
    let path = content.path(file);
    let key = keys.key(file)?;
    let unit = unit_id(&path);
    if ddc.contains(key) {
        return Ok(UnitOutcome {
            path,
            unit,
            key,
            cached: true,
            size: 0,
            warnings: Vec::new(),
        });
    }
    let (bytes, warnings) = cook_unit(content, cookers, file, platform, profile)?;
    ddc.put(key, &bytes).map_err(|source| CookError::Io {
        path: content.file_path(file),
        source,
    })?;
    Ok(UnitOutcome {
        path,
        unit,
        key,
        cached: false,
        size: bytes.len(),
        warnings,
    })
}

/// Cooks every unit of every mount and returns the build manifest of the successful ones: one
/// build holds the engine's content and the project's (0003 §4.5).
pub fn cook_all(
    content: &Content,
    cookers: &Cookers,
    ddc: &dyn Ddc,
    options: &CookOptions,
) -> (CookReport, Manifest) {
    let mut report = cook(content, cookers, ddc, options, &source_units(content));
    let mut units: Vec<ManifestUnit> = report
        .units
        .iter()
        .map(|u| ManifestUnit {
            unit: u.unit,
            key: u.key,
            path: u.path.clone(),
        })
        .collect();
    units.sort_by_key(|u| u.unit);
    let cooked: HashSet<u64> = units.iter().map(|u| u.unit).collect();
    // Bottom mount first and a stable sort: a UUID two mounts use stays with the lower one, as
    // links resolve it.
    let mut objects: Vec<(Uuid, u64)> = content
        .objects()
        .map(|o| {
            let uuid = content.object_info(o).uuid;
            (uuid, unit_id(&content.path(content.object_file(o))))
        })
        .filter(|(_, unit)| cooked.contains(unit))
        .collect();
    objects.sort_by_key(|o| o.0);
    objects.dedup_by_key(|(uuid, _)| *uuid);
    let (mut names, errors) = named_objects(content, cookers);
    report.errors.extend(errors);
    names.retain(|(_, uuid)| objects.binary_search_by_key(uuid, |o| o.0).is_ok());
    let manifest = Manifest {
        platform: options.platform.name.to_owned(),
        units,
        objects,
        names,
    };
    (report, manifest)
}

/// Writes `manifest` to `<build>/<platform>.manifest` (a project's `Project::build_dir`) and into
/// `ddc` (under its own key, so a whole build is addressed by one key).
pub fn publish_manifest(build: &Path, ddc: &dyn Ddc, manifest: &Manifest) -> std::io::Result<u128> {
    let bytes = manifest.to_bytes();
    let key = manifest.key();
    ddc.put(key, &bytes)?;
    std::fs::create_dir_all(build)?;
    std::fs::write(
        build.join(format!("{}.manifest", manifest.platform)),
        &bytes,
    )?;
    Ok(key)
}

/// One unit's cooked bytes: from `ddc` when its key is there, else cooked now and stored.
/// Returns the key, the bytes, whether they were cached, and the cook's warnings.
pub fn fetch_unit(
    content: &Content,
    cookers: &Cookers,
    ddc: &dyn Ddc,
    platform: &Platform,
    profile: Profile,
    file: ContentFile,
) -> Result<(u128, Vec<u8>, bool, Vec<String>), CookError> {
    let key = unit_key(content, cookers, file, platform, profile)?;
    let io = |source| CookError::Io {
        path: content.file_path(file),
        source,
    };
    if let Some(bytes) = ddc.get(key).map_err(io)? {
        return Ok((key, bytes, true, Vec::new()));
    }
    let (bytes, warnings) = cook_unit(content, cookers, file, platform, profile)?;
    ddc.put(key, &bytes).map_err(io)?;
    Ok((key, bytes, false, warnings))
}

/// The build's name table (0003 §4.4): every object of a type registered as named, by `!Name`,
/// sorted by name. Names given twice (or whose hashes collide, which the runtime's tables could
/// not tell apart) are errors, and none of the objects involved is in the table. The table spans
/// every mount: a game's names and the engine's share it.
pub fn named_objects(
    content: &Content,
    cookers: &Cookers,
) -> (Vec<(String, Uuid)>, Vec<CookError>) {
    let mut named: Vec<(String, Uuid, String)> = content
        .objects()
        .filter_map(|o| {
            let info = content.object_info(o);
            cookers.is_named(info.type_name).then(|| {
                let path = content.file_path(content.object_file(o));
                (info.name.to_owned(), info.uuid, path)
            })
        })
        .collect();
    named.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let mut by_hash: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, (name, _, _)) in named.iter().enumerate() {
        by_hash
            .entry(marlin_asset::name_hash(name))
            .or_default()
            .push(i);
    }
    let mut errors = Vec::new();
    let mut clashing = HashSet::new();
    let mut groups: Vec<&Vec<usize>> = by_hash.values().filter(|g| g.len() > 1).collect();
    groups.sort();
    for group in groups {
        let first = &named[group[0]];
        for &i in &group[1..] {
            let (name, _, path) = &named[i];
            let message = if *name == first.0 {
                format!(
                    "`{name}` also names an object in {}; names must be unique",
                    first.2
                )
            } else {
                format!(
                    "the name `{name}` hashes like `{}` (in {}); rename one of them",
                    first.0, first.2
                )
            };
            errors.push(CookError::Object {
                path: path.clone(),
                object: name.clone(),
                message,
            });
        }
        clashing.extend(group.iter().copied());
    }
    let names = named
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !clashing.contains(i))
        .map(|(_, (name, uuid, _))| (name, uuid))
        .collect();
    (names, errors)
}
