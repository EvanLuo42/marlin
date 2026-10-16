//! Cooking units: keys, assembly and the parallel driver.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use marlin_asset::Uuid;
use marlin_asset::cooked::{
    BULK_ALIGN, BulkEntry, ImportEntry, ObjectEntry, PAYLOAD_ALIGN, UnitHeader, platform_id,
    unit_id, write,
};
use marlin_asset_db::{AssetDb, FileId, MASSET_EXT};
use marlin_hash::xxh3_128;
use marlin_masset::{DocumentLinks, Severity, parse};

use crate::cooker::{CookContext, Cookers, SourceObject};
use crate::ddc::Ddc;
use crate::manifest::{Manifest, ManifestUnit};

/// Bump when the unit layout or key derivation changes: every key changes with it.
pub const COOK_FORMAT: u32 = 1;

#[derive(Clone, Debug)]
pub struct CookOptions {
    /// Target platform name, as in `platforms/<name>/platform.toml`.
    pub platform: String,
    /// 0: one thread per logical CPU.
    pub threads: usize,
}

impl Default for CookOptions {
    fn default() -> Self {
        Self {
            platform: "windows".into(),
            threads: 0,
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

/// The unit of the object with `uuid`, from the index.
fn unit_of(db: &AssetDb, uuid: Uuid) -> Option<u64> {
    let object = db.object(uuid)?;
    Some(unit_id(&db.asset_path(db.object_info(object).file)))
}

/// The cook key of `file`: a hash over every input of its cooked unit.
///
/// Inputs: the format version and platform; the unit id; the source text; per object its type
/// and the type's fingerprint (structure and cooker version); every source file it links to, by
/// content; and the unit of every object it links to (which the import table records). A link's
/// target content is not an input: links are resolved at load time.
pub fn unit_key(
    db: &AssetDb,
    cookers: &Cookers,
    file: FileId,
    platform: &str,
) -> Result<u128, CookError> {
    let info = db.file_info(file);
    let asset_path = db.asset_path(file);
    let fail = |message: String| CookError::Unit {
        path: info.path.clone(),
        message,
    };
    if let Some(error) = info.parse_error {
        return Err(fail(format!("does not parse: {error}")));
    }
    let mut key = KeyWriter(Vec::with_capacity(256));
    key.bytes(b"marlin-cook");
    key.u64(u64::from(COOK_FORMAT));
    key.bytes(platform.as_bytes());
    key.u64(unit_id(&asset_path));
    key.u128(info.hash);
    for object in db.objects_in(file) {
        let o = db.object_info(object);
        let registered = cookers
            .get(o.type_name)
            .ok_or_else(|| fail(format!("{}: no cooker for type `{}`", o.name, o.type_name)))?;
        key.bytes(o.type_name.as_bytes());
        key.u64(registered.fingerprint);
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
        for link in db.links(object) {
            if let Some(target) = db.link_target(object, link) {
                let target_uuid = db.object_info(target).uuid;
                key.u128(target_uuid.as_u128());
                key.u64(unit_of(db, target_uuid).unwrap_or(0));
            }
        }
    }
    Ok(xxh3_128(&key.0))
}

struct CookedObject {
    uuid: Uuid,
    entry_type: u64,
    encoding: marlin_asset::cooked::Encoding,
    payload: Vec<u8>,
    bulk: Vec<(marlin_asset::cooked::BulkKind, Vec<u8>)>,
}

fn pad(out: &mut Vec<u8>, align: u64) {
    let len = (out.len() as u64).next_multiple_of(align);
    out.resize(len as usize, 0);
}

/// Lays out a unit (see `marlin_asset::cooked`).
fn assemble(
    unit: u64,
    platform: &str,
    mut objects: Vec<CookedObject>,
    imports: BTreeMap<Uuid, u64>,
) -> Vec<u8> {
    objects.sort_by_key(|o| o.uuid);
    let tables = UnitHeader::SIZE
        + objects.len() * ObjectEntry::SIZE
        + imports.len() * ImportEntry::SIZE
        + objects.iter().map(|o| o.bulk.len()).sum::<usize>() * BulkEntry::SIZE;
    let payload_offset = (tables as u64).next_multiple_of(PAYLOAD_ALIGN);

    // Payload and bulk regions first, to know the offsets.
    let mut payload = Vec::new();
    let mut bulk = Vec::new();
    let mut entries = Vec::with_capacity(objects.len());
    let mut bulk_entries = Vec::new();
    for o in &objects {
        pad(&mut payload, PAYLOAD_ALIGN);
        let payload_start = payload.len() as u64;
        payload.extend_from_slice(&o.payload);
        let bulk_first = bulk_entries.len() as u32;
        for (kind, data) in &o.bulk {
            pad(&mut bulk, BULK_ALIGN);
            bulk_entries.push(BulkEntry {
                offset: bulk.len() as u64,
                size: data.len() as u64,
                kind: *kind,
            });
            bulk.extend_from_slice(data);
        }
        entries.push(ObjectEntry {
            uuid: o.uuid,
            type_id: o.entry_type,
            encoding: o.encoding,
            payload_offset: payload_start,
            payload_size: o.payload.len() as u64,
            bulk_first,
            bulk_count: o.bulk.len() as u32,
        });
    }
    let bulk_offset = (payload_offset + payload.len() as u64).next_multiple_of(BULK_ALIGN);

    let mut out = Vec::with_capacity(bulk_offset as usize + bulk.len());
    write::header(
        &mut out,
        &UnitHeader {
            unit,
            platform: platform_id(platform),
            object_count: entries.len() as u32,
            import_count: imports.len() as u32,
            bulk_count: bulk_entries.len() as u32,
            payload_offset,
            payload_size: payload.len() as u64,
            bulk_offset: if bulk.is_empty() { 0 } else { bulk_offset },
            bulk_size: bulk.len() as u64,
        },
    );
    for e in &entries {
        write::object(&mut out, e);
    }
    for (&uuid, &unit) in &imports {
        write::import(&mut out, &ImportEntry { uuid, unit });
    }
    for e in &bulk_entries {
        write::bulk(&mut out, e);
    }
    pad(&mut out, PAYLOAD_ALIGN);
    out.extend_from_slice(&payload);
    if !bulk.is_empty() {
        out.resize(bulk_offset as usize, 0);
        out.extend_from_slice(&bulk);
    }
    out
}

/// Cooks one unit (no cache involved).
pub fn cook_unit(
    db: &AssetDb,
    cookers: &Cookers,
    file: FileId,
    platform: &str,
) -> Result<(Vec<u8>, Vec<String>), CookError> {
    let info = db.file_info(file);
    let asset_path = db.asset_path(file);
    let path = info.path.clone();
    let full = db.root().join(&path);
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
        path: &asset_path,
        fallback: db,
    };
    let unit = unit_id(&asset_path);
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
        for link in &read.links {
            if local.contains(&link.target) {
                continue;
            }
            let target = unit_of(db, link.target).ok_or_else(|| {
                object_error(format!(
                    "links to {}, which no indexed file defines",
                    link.target
                ))
            })?;
            imports.insert(link.target, target);
        }
        let sources: HashSet<String> = db
            .object(object.uuid)
            .map(|o| db.source_links(o).map(|s| s.path.to_owned()).collect())
            .unwrap_or_default();
        let mut ctx = CookContext {
            platform,
            root: db.root(),
            sources: &sources,
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

/// Every `.masset` file of the index.
pub fn source_units(db: &AssetDb) -> Vec<FileId> {
    let mut files: Vec<FileId> = db
        .files()
        .filter(|&f| db.file_info(f).path.ends_with(&format!(".{MASSET_EXT}")))
        .collect();
    files.sort_by_cached_key(|&f| db.file_info(f).path);
    files
}

/// Cooks `units` that are not in `ddc` yet, in parallel, and stores the results.
pub fn cook(
    db: &AssetDb,
    cookers: &Cookers,
    ddc: &dyn Ddc,
    options: &CookOptions,
    units: &[FileId],
) -> CookReport {
    let threads = match options.threads {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    }
    .clamp(1, units.len().max(1));
    let chunk = units.len().div_ceil(threads).max(1);
    let platform = options.platform.as_str();
    let results: Vec<Result<UnitOutcome, CookError>> = std::thread::scope(|s| {
        let handles: Vec<_> = units
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|&file| cook_one(db, cookers, ddc, platform, file))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
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

fn cook_one(
    db: &AssetDb,
    cookers: &Cookers,
    ddc: &dyn Ddc,
    platform: &str,
    file: FileId,
) -> Result<UnitOutcome, CookError> {
    let path = db.asset_path(file);
    let key = unit_key(db, cookers, file, platform)?;
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
    let (bytes, warnings) = cook_unit(db, cookers, file, platform)?;
    ddc.put(key, &bytes).map_err(|source| CookError::Io {
        path: db.file_info(file).path,
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

/// Cooks every unit of the project and returns the build manifest of the successful ones.
pub fn cook_all(
    db: &AssetDb,
    cookers: &Cookers,
    ddc: &dyn Ddc,
    options: &CookOptions,
) -> (CookReport, Manifest) {
    let report = cook(db, cookers, ddc, options, &source_units(db));
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
    let mut objects: Vec<(Uuid, u64)> = db
        .objects()
        .map(|o| {
            let info = db.object_info(o);
            (info.uuid, unit_id(&db.asset_path(info.file)))
        })
        .filter(|(_, unit)| cooked.contains(unit))
        .collect();
    objects.sort_unstable();
    objects.dedup_by_key(|(uuid, _)| *uuid);
    let manifest = Manifest {
        platform: options.platform.clone(),
        units,
        objects,
    };
    (report, manifest)
}

/// Writes `manifest` to `<root>/.marlin/build/<platform>.manifest` and into `ddc` (under its own
/// key, so a whole build is addressed by one key).
pub fn publish_manifest(root: &Path, ddc: &dyn Ddc, manifest: &Manifest) -> std::io::Result<u128> {
    let bytes = manifest.to_bytes();
    let key = manifest.key();
    ddc.put(key, &bytes)?;
    let dir = root.join(".marlin").join("build");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{}.manifest", manifest.platform)), &bytes)?;
    Ok(key)
}
