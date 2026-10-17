//! Cookers: what turns a source object into runtime data.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use marlin_asset::Uuid;
use marlin_asset::cooked::{BulkKind, Encoding};
use marlin_hash::{xxh3_64, xxh3_128};
use marlin_masset::{Schema, SchemaError};
use marlin_reflect::{Reflect, TypeId, TypeInfo, TypeKind, Value, type_info};

/// A source object, bound to its reflected type.
pub struct SourceObject<'a> {
    pub uuid: Uuid,
    pub name: &'a str,
    pub type_name: &'a str,
    pub value: &'a Value,
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
    pub(crate) platform: &'a str,
    pub(crate) root: &'a Path,
    /// Source files the object links to (the only files it may read), each with the content
    /// hash the cook key was computed from.
    pub(crate) sources: &'a HashMap<String, u128>,
    pub(crate) bulk: Vec<(BulkKind, Vec<u8>)>,
    pub(crate) warnings: Vec<String>,
}

impl CookContext<'_> {
    /// The platform being cooked for (`windows`, `ps5`, …).
    pub fn platform(&self) -> &str {
        self.platform
    }

    /// The contents of a source file the object links to (`` `source:path` ``). Files reached
    /// any other way would be invisible to dependency tracking and the cook key ("string
    /// magic"), so they are refused.
    ///
    /// The bytes must be the ones the cook key hashed: a file saved after it was indexed would
    /// otherwise be cooked into a result stored under the stale key, so that is refused too.
    pub fn read_source(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let Some(&indexed) = self.sources.get(path) else {
            return Err(format!(
                "`{path}` is not a source file this object links to; add a `source:` link"
            ));
        };
        let bytes =
            std::fs::read(self.root.join(path)).map_err(|e| format!("reading `{path}`: {e}"))?;
        if xxh3_128(&bytes) != indexed {
            return Err(format!(
                "`{path}` changed since it was indexed; update the index first"
            ));
        }
        Ok(bytes)
    }

    /// Adds a bulk blob (GPU data); returns its index among this object's blobs.
    pub fn add_bulk(&mut self, kind: BulkKind, data: Vec<u8>) -> u32 {
        self.bulk.push((kind, data));
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
        self.add(type_info::<T>(), Arc::new(cooker))
    }

    /// Source type `T`, cooked as itself (see the generic cooker).
    pub fn register_generic<T: Reflect>(&mut self) -> Result<(), SchemaError> {
        self.add(type_info::<T>(), Arc::new(Generic))
    }

    fn add(&mut self, info: &'static TypeInfo, cooker: Arc<dyn Cooker>) -> Result<(), SchemaError> {
        self.schema.register_info(info, 0)?;
        let name = Schema::text_name(info).to_owned();
        let fingerprint = fingerprint(info) ^ u64::from(cooker.version()).rotate_left(32);
        self.by_type.insert(
            name,
            Registered {
                cooker,
                fingerprint,
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
}
