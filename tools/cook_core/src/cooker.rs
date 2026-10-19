//! Cookers: what turns a source object into runtime data.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use marlin_asset::Uuid;
use marlin_asset::cooked::write::UnitBulk;
use marlin_asset::cooked::{BulkKind, Encoding};
use marlin_asset_db::{Content, ContentObject};
use marlin_hash::{xxh3_64, xxh3_128};
use marlin_masset::{DocumentLinks, Schema, SchemaError, Severity, parse};
use marlin_reflect::{Reflect, TypeId, TypeInfo, TypeKind, Value, type_info};

use crate::platform::{Platform, Profile};

/// A source object, bound to its reflected type.
pub struct SourceObject<'a> {
    pub uuid: Uuid,
    pub name: &'a str,
    pub type_name: &'a str,
    pub value: &'a Value,
}

/// An object another one builds from (a build dependency, 0003 §4.1), read from its source
/// file: what [`CookContext::dependency`] gives.
pub struct Dependency {
    pub uuid: Uuid,
    /// The mount it is in (0003 §4.5); its source paths are relative to that mount.
    pub mount: String,
    pub name: String,
    pub type_name: String,
    pub value: Value,
}

impl Dependency {
    /// One of its source paths as [`CookContext::read_source`] takes it from any mount:
    /// qualified, `/Engine/shaders/Lighting/Lighting.mshader`.
    pub fn source(&self, path: &str) -> String {
        format!("/{}/{path}", self.mount)
    }
}

/// What a cooker produced for one object.
pub struct Cooked {
    /// Stable type id of the runtime type the payload decodes to.
    pub type_id: TypeId,
    pub encoding: Encoding,
    pub payload: Vec<u8>,
}

/// What a cooker may do besides returning its result.
pub struct CookContext<'a> {
    pub(crate) platform: &'a Platform,
    pub(crate) profile: Profile,
    /// The mount the object is in (`Engine`, `Game`).
    pub(crate) mount: &'a str,
    /// Source files the object or one of its build dependencies links to (the only files it
    /// may read), by qualified path, each with its file on disk and the content hash the cook
    /// key was computed from.
    pub(crate) sources: &'a HashMap<String, (PathBuf, u128)>,
    pub(crate) build: BuildInputs<'a>,
    pub(crate) bulk: Vec<UnitBulk>,
    pub(crate) warnings: Vec<String>,
}

/// The objects a cook may read besides its own: its build dependencies, transitively.
pub(crate) struct BuildInputs<'a> {
    pub(crate) content: &'a Content,
    pub(crate) cookers: &'a Cookers,
    /// By UUID, each where the cooking object's links resolve it.
    pub(crate) allowed: HashMap<Uuid, ContentObject>,
    pub(crate) read: HashMap<Uuid, Rc<Dependency>>,
}

impl BuildInputs<'_> {
    fn read(&mut self, uuid: Uuid) -> Result<Rc<Dependency>, String> {
        if let Some(d) = self.read.get(&uuid) {
            return Ok(d.clone());
        }
        let content = self.content;
        let Some(&object) = self.allowed.get(&uuid) else {
            return Err(match content.object(uuid) {
                None => format!("links to {uuid}, which no indexed file defines"),
                Some(_) => format!(
                    "{uuid} is not a build dependency of this object (the cooker's \
                     `build_dependency_types` decide which links are)"
                ),
            });
        };
        let db = content.db(object.mount);
        let file = db.object_info(object.object).file;
        let mut info = db.file_info(file);
        let full = db.root().join(&info.path);
        // Messages name it by qualified path.
        info.path = content.file_path(content.object_file(object));
        let text =
            std::fs::read_to_string(full).map_err(|e| format!("reading `{}`: {e}", info.path))?;
        // As for the unit's own text: the bytes must be the ones its key hashed.
        if xxh3_128(text.as_bytes()) != info.hash {
            return Err(format!(
                "`{}` changed since it was indexed; update the index first",
                info.path
            ));
        }
        let doc = parse(&text).map_err(|e| format!("{}: {e}", info.path))?;
        let source = doc
            .objects
            .iter()
            .find(|o| o.uuid == uuid)
            .ok_or_else(|| format!("`{}` no longer defines {uuid}", info.path))?;
        let fail = |message: String| format!("{}: {}: {message}", info.path, source.name);
        let asset_path = db.asset_path(file);
        let links = DocumentLinks {
            document: &doc,
            path: &asset_path,
            fallback: &content.links(object.mount),
        };
        let read = self
            .cookers
            .schema()
            .read_object(source, &links)
            .map_err(|d| fail(d.display(&text)))?;
        if let Some(error) = read
            .diagnostics
            .iter()
            .find(|d| d.severity == Severity::Error)
        {
            return Err(fail(error.display(&text)));
        }
        let dependency = Rc::new(Dependency {
            uuid,
            mount: content.mount(object.mount).name.clone(),
            name: source.name.clone(),
            type_name: source.type_name.clone(),
            value: read.value,
        });
        self.read.insert(uuid, dependency.clone());
        Ok(dependency)
    }
}

impl<'a> CookContext<'a> {
    /// The target being cooked for: decide formats and layouts from its description.
    pub fn platform(&self) -> &'a Platform {
        self.platform
    }

    /// The build being cooked for. A cooker that reads it must put it in its [`Cooker::key`].
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The mount the object is in (0003 §4.5): `Engine` for the engine's content.
    pub fn mount(&self) -> &'a str {
        self.mount
    }

    /// A build dependency of the object (0003 §4.1): an object it links to whose type is one
    /// of the cooker's [`build_dependency_types`], or a build dependency of one of those, read
    /// from its source. Its unit's cook key is part of this unit's, so whatever the cooker
    /// reads of it, and of the source files it links to ([`read_source`]), is tracked. Other
    /// objects are refused: a link to them is a runtime reference.
    ///
    /// [`build_dependency_types`]: Cooker::build_dependency_types
    /// [`read_source`]: CookContext::read_source
    pub fn dependency(&mut self, uuid: Uuid) -> Result<Rc<Dependency>, String> {
        self.build.read(uuid)
    }

    /// The contents of a source file the object or one of its build dependencies links to
    /// (`` `source:path` ``). `path` is relative to the object's mount, or qualified
    /// (`/Engine/…`, [`Dependency::source`]) for a build dependency's file in another one. Files
    /// reached any other way would be invisible to dependency tracking and the cook key
    /// ("string magic"), so they are refused.
    ///
    /// The bytes must be the ones the cook key hashed: a file saved after it was indexed would
    /// otherwise be cooked into a result stored under the stale key, so that is refused too.
    pub fn read_source(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let qualified = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{}/{path}", self.mount)
        };
        let Some((full, indexed)) = self.sources.get(&qualified) else {
            return Err(format!(
                "`{path}` is not a source file this object links to; add a `source:` link"
            ));
        };
        let indexed = *indexed;
        let bytes = std::fs::read(full).map_err(|e| format!("reading `{path}`: {e}"))?;
        if xxh3_128(&bytes) != indexed {
            return Err(format!(
                "`{path}` changed since it was indexed; update the index first"
            ));
        }
        Ok(bytes)
    }

    /// Adds a bulk blob (GPU data); returns its index among this object's blobs.
    pub fn add_bulk(&mut self, kind: BulkKind, data: Vec<u8>) -> u32 {
        self.bulk.push((kind, data).into());
        self.bulk.len() as u32 - 1
    }

    /// Adds a streamed bulk blob: read on its own when the runtime wants it, not with the unit
    /// (a mesh's finer cluster pages); returns its index among this object's blobs.
    pub fn add_streamed_bulk(&mut self, kind: BulkKind, data: Vec<u8>) -> u32 {
        self.bulk.push(UnitBulk {
            kind,
            streamed: true,
            data,
        });
        self.bulk.len() as u32 - 1
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

/// Turns source objects of one type into cooked data.
///
/// Cookers must be deterministic: the same object, source files and platform give the same
/// bytes. Cooked results are cached by a key over exactly those inputs plus [`version`]; bump
/// it whenever the output for the same inputs changes.
///
/// [`version`]: Cooker::version
pub trait Cooker: Send + Sync {
    fn version(&self) -> u32;

    /// What else the output depends on for `platform` and `profile`, beyond the object, its
    /// source files and the platform's texture description: a tool's own fingerprint (the
    /// shader compiler's, 0005 §8), the profile when the cooker reads it. Part of every key
    /// of the type's units; called whenever a key is computed, so it should be cheap.
    fn key(&self, _platform: &Platform, _profile: Profile) -> u128 {
        0
    }

    /// The types (by text name, `ShaderPackage`) whose objects this type's objects are built
    /// from (0003 §4.1). A link to one is a **build dependency**: [`CookContext::dependency`]
    /// may read it, its unit's cook key is part of this unit's, and it is not an import (the
    /// runtime does not load it with the unit). Links to other types are runtime references.
    /// Decided by type, so keys come from the index alone, without parsing.
    fn build_dependency_types(&self) -> &[&str] {
        &[]
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String>;
}

/// Cooks an object as itself: its reflected value in marlin-serial form, for types the runtime
/// uses as they are authored (gameplay data, settings).
struct Generic;

impl Cooker for Generic {
    fn version(&self) -> u32 {
        // The type's structure is in the fingerprint already.
        0
    }

    fn cook(
        &self,
        object: &SourceObject<'_>,
        _ctx: &mut CookContext<'_>,
    ) -> Result<Cooked, String> {
        let mut payload = Vec::new();
        marlin_serial::write(object.value.as_ref(), &mut payload).map_err(|e| format!("{e:?}"))?;
        Ok(Cooked {
            type_id: object.value.info().id,
            encoding: Encoding::Serial,
            payload,
        })
    }
}

pub(crate) struct Registered {
    pub(crate) cooker: Arc<dyn Cooker>,
    /// Source type structure and cooker version: part of every key of the type.
    pub(crate) fingerprint: u64,
    /// Objects of the type are found by `!Name` (the build's name table, 0003 §4.4).
    pub(crate) named: bool,
    /// Linked types that are build dependencies (see [`Cooker::build_dependency_types`]).
    pub(crate) build_types: Vec<String>,
}

/// The source types a cook understands and the cooker of each.
#[derive(Default)]
pub struct Cookers {
    schema: Schema,
    by_type: HashMap<String, Registered>,
}

/// A hash of a type's reflected structure: names, kinds, fields, variants and element types,
/// recursively. Changes when the type's layout as text or as serialized data changes.
pub fn fingerprint(info: &'static TypeInfo) -> u64 {
    fn walk(info: &'static TypeInfo, seen: &mut HashSet<u64>, out: &mut Vec<u8>) {
        out.extend_from_slice(&info.id.0.to_le_bytes());
        if !seen.insert(info.id.0) {
            return; // Recursive types: the id stands for the structure already hashed.
        }
        let field_list = |fields: &'static [marlin_reflect::FieldInfo],
                          seen: &mut HashSet<u64>,
                          out: &mut Vec<u8>| {
            for f in fields {
                out.extend_from_slice(f.name.as_bytes());
                out.push(f.attrs.transient as u8);
                walk((f.ty)(), seen, out);
            }
        };
        match &info.kind {
            TypeKind::Primitive(p) => out.extend_from_slice(format!("{p:?}").as_bytes()),
            TypeKind::String(_) => out.push(1),
            TypeKind::Struct(s) => {
                out.extend_from_slice(format!("{:?}", s.style).as_bytes());
                field_list(s.fields, seen, out);
            }
            TypeKind::Enum(e) => {
                for v in e.variants {
                    out.extend_from_slice(v.name.as_bytes());
                    field_list(v.fields, seen, out);
                }
            }
            TypeKind::Array(a) => {
                out.extend_from_slice(&(a.len as u64).to_le_bytes());
                walk((a.element)(), seen, out);
            }
            TypeKind::List(l) => walk((l.element)(), seen, out),
            TypeKind::Option(o) => walk((o.some)(), seen, out),
            TypeKind::Opaque => out.push(2),
            TypeKind::Tuple(t) => {
                out.push(3);
                field_list(t.fields, seen, out);
            }
            TypeKind::Map(m) => {
                out.push(4);
                walk((m.key)(), seen, out);
                walk((m.value)(), seen, out);
            }
            TypeKind::Set(s) => {
                out.push(5);
                walk((s.element)(), seen, out);
            }
            TypeKind::Pointer(p) => walk((p.pointee)(), seen, out),
        }
    }
    let mut out = Vec::new();
    walk(info, &mut HashSet::new(), &mut out);
    xxh3_64(&out)
}

impl Cookers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Source type `T`, cooked by `cooker`.
    pub fn register<T: Reflect>(
        &mut self,
        cooker: impl Cooker + 'static,
    ) -> Result<(), SchemaError> {
        self.add(type_info::<T>(), Arc::new(cooker), false)
    }

    /// Source type `T`, cooked by `cooker`, whose objects code finds by their `!Name` (shader
    /// packages): the build's name table lists them, and two with one name fail the build.
    pub fn register_named<T: Reflect>(
        &mut self,
        cooker: impl Cooker + 'static,
    ) -> Result<(), SchemaError> {
        self.add(type_info::<T>(), Arc::new(cooker), true)
    }

    /// Source type `T`, cooked as itself (see the generic cooker).
    pub fn register_generic<T: Reflect>(&mut self) -> Result<(), SchemaError> {
        self.add(type_info::<T>(), Arc::new(Generic), false)
    }

    /// Source type `T`, cooked as itself, found by its `!Name` (see [`register_named`]).
    ///
    /// [`register_named`]: Self::register_named
    pub fn register_named_generic<T: Reflect>(&mut self) -> Result<(), SchemaError> {
        self.add(type_info::<T>(), Arc::new(Generic), true)
    }

    fn add(
        &mut self,
        info: &'static TypeInfo,
        cooker: Arc<dyn Cooker>,
        named: bool,
    ) -> Result<(), SchemaError> {
        self.schema.register_info(info, 0)?;
        let name = Schema::text_name(info).to_owned();
        let fingerprint = fingerprint(info) ^ u64::from(cooker.version()).rotate_left(32);
        let build_types = cooker
            .build_dependency_types()
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        self.by_type.insert(
            name,
            Registered {
                cooker,
                fingerprint,
                named,
                build_types,
            },
        );
        Ok(())
    }

    /// The schema of every registered source type.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The schema, to register upgrades for registered types.
    pub fn schema_mut(&mut self) -> &mut Schema {
        &mut self.schema
    }

    pub(crate) fn get(&self, type_name: &str) -> Option<&Registered> {
        self.by_type.get(type_name)
    }

    /// Whether a link from an object of type `from` to one of type `to` is a build dependency.
    pub fn is_build_dependency(&self, from: &str, to: &str) -> bool {
        self.by_type
            .get(from)
            .is_some_and(|r| r.build_types.iter().any(|t| t == to))
    }

    /// Whether objects of `type_name` are found by name (see [`Cookers::register_named`]).
    pub fn is_named(&self, type_name: &str) -> bool {
        self.by_type.get(type_name).is_some_and(|r| r.named)
    }
}
