//! The set of types source text can name, with their text-side field names and versions.

use std::collections::HashMap;

use marlin_reflect::{FieldInfo, Reflect, TypeInfo, TypeKind, TypeName, type_info};

use crate::ast::Object;
use crate::case::{short_type_name, to_pascal};
use crate::error::SchemaError;

/// Pinned reflected names of the identity types (see `marlin_asset::id`).
pub(crate) const UUID_TYPE: &str = "marlin_asset::Uuid";
pub(crate) const LINK_TYPE: &str = "marlin_asset::Link";
pub(crate) const SOURCE_FILE_TYPE: &str = "marlin_asset::SourceFile";

/// Rewrites an object from one schema version to the next, on the syntax tree before binding.
pub type Upgrade = fn(&mut Object) -> Result<(), String>;

pub(crate) struct ObjectType {
    pub(crate) info: &'static TypeInfo,
    pub(crate) version: u32,
    /// `upgrades[v]` turns version `v` into `v + 1`.
    pub(crate) upgrades: Vec<Option<Upgrade>>,
}

/// Object types by text name, plus PascalCase field names for every reachable struct and variant.
#[derive(Default)]
pub struct Schema {
    pub(crate) types: HashMap<String, ObjectType>,
    /// PascalCase names of a field list, keyed by the address of its `&'static [FieldInfo]`.
    fields: HashMap<usize, Vec<String>>,
}

pub(crate) fn path_of(info: &TypeInfo) -> &'static str {
    match info.name {
        TypeName::Path { path, .. } => path,
        TypeName::Array { .. } => "",
    }
}

fn fields_key(fields: &'static [FieldInfo]) -> usize {
    fields.as_ptr().addr()
}

impl Schema {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `T` usable as an object type, at schema version 0.
    pub fn register<T: Reflect>(&mut self) -> Result<(), SchemaError> {
        self.register_info(type_info::<T>(), 0)
    }

    /// Makes `T` usable as an object type at schema version `version`. Files written with older
    /// versions are brought up to date by the upgrades registered with
    /// [`register_upgrade`](Self::register_upgrade).
    pub fn register_versioned<T: Reflect>(&mut self, version: u32) -> Result<(), SchemaError> {
        self.register_info(type_info::<T>(), version)
    }

    pub fn register_info(
        &mut self,
        info: &'static TypeInfo,
        version: u32,
    ) -> Result<(), SchemaError> {
        let full = info.name.to_string();
        if let TypeName::Path { args, .. } = info.name
            && !args.is_empty()
        {
            return Err(SchemaError::Generic(full));
        }
        if !matches!(info.kind, TypeKind::Struct(_)) {
            return Err(SchemaError::NotStruct(full));
        }
        let name = short_type_name(path_of(info)).to_owned();
        if let Some(existing) = self.types.get(&name) {
            if existing.info.same_type(info) {
                return Ok(());
            }
            return Err(SchemaError::DuplicateType {
                name,
                first: existing.info.name.to_string(),
                second: full,
            });
        }
        self.index_fields(info)?;
        self.types.insert(
            name,
            ObjectType {
                info,
                version,
                upgrades: vec![None; version as usize],
            },
        );
        Ok(())
    }

    /// Registers the step from `from` to `from + 1` for `type_name`.
    pub fn register_upgrade(&mut self, type_name: &str, from: u32, upgrade: Upgrade) -> bool {
        let Some(ty) = self.types.get_mut(type_name) else {
            return false;
        };
        match ty.upgrades.get_mut(from as usize) {
            Some(slot) => {
                *slot = Some(upgrade);
                true
            }
            None => false,
        }
    }

    /// The registered object type called `name` in text.
    pub fn object_type(&self, name: &str) -> Option<&'static TypeInfo> {
        self.types.get(name).map(|t| t.info)
    }

    /// The text name of an object type.
    pub fn text_name(info: &TypeInfo) -> &'static str {
        short_type_name(path_of(info))
    }

    /// Walks every type reachable from `info`, recording PascalCase field names.
    fn index_fields(&mut self, info: &'static TypeInfo) -> Result<(), SchemaError> {
        let mut pending = vec![info];
        let mut seen = std::collections::HashSet::new();
        while let Some(info) = pending.pop() {
            if !seen.insert(core::ptr::from_ref(info).addr()) {
                continue;
            }
            match &info.kind {
                TypeKind::Struct(s) => {
                    self.index_field_list(info, s.fields)?;
                    pending.extend(s.fields.iter().map(|f| (f.ty)()));
                }
                TypeKind::Enum(e) => {
                    for v in e.variants {
                        self.index_field_list(info, v.fields)?;
                        pending.extend(v.fields.iter().map(|f| (f.ty)()));
                    }
                }
                TypeKind::Array(a) => pending.push((a.element)()),
                TypeKind::List(l) => pending.push((l.element)()),
                TypeKind::Option(o) => pending.push((o.some)()),
                TypeKind::Primitive(_) | TypeKind::String(_) | TypeKind::Opaque => {}
            }
        }
        Ok(())
    }

    fn index_field_list(
        &mut self,
        owner: &TypeInfo,
        fields: &'static [FieldInfo],
    ) -> Result<(), SchemaError> {
        if fields.is_empty() || self.fields.contains_key(&fields_key(fields)) {
            return Ok(());
        }
        let names: Vec<String> = fields.iter().map(|f| to_pascal(f.name)).collect();
        for (i, a) in names.iter().enumerate() {
            if let Some(j) = names[..i].iter().position(|b| b == a) {
                return Err(SchemaError::FieldCollision {
                    ty: owner.name.to_string(),
                    first: fields[j].name.to_owned(),
                    second: fields[i].name.to_owned(),
                    pascal: a.clone(),
                });
            }
        }
        self.fields.insert(fields_key(fields), names);
        Ok(())
    }

    /// PascalCase name of field `index` of `fields`.
    pub(crate) fn field_name(&self, fields: &'static [FieldInfo], index: usize) -> String {
        match self.fields.get(&fields_key(fields)) {
            Some(names) => names[index].clone(),
            None => to_pascal(fields[index].name),
        }
    }

    /// Index of the field called `pascal` in text.
    pub(crate) fn find_field(&self, fields: &'static [FieldInfo], pascal: &str) -> Option<usize> {
        match self.fields.get(&fields_key(fields)) {
            Some(names) => names.iter().position(|n| n == pascal),
            None => fields.iter().position(|f| to_pascal(f.name) == pascal),
        }
    }
}
