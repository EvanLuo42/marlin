//! Lookup of type descriptions by stable id or name, and type data attached at run time.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

use marlin_std::sync::{RwLock, RwLockReadGuard};

use crate::data::ReflectProxy;
use crate::id::TypeId;
use crate::info::{TypeData, TypeInfo, TypeKind};
use crate::traits::{Reflect, type_info};

/// A set of type descriptions keyed by [`TypeId`].
///
/// The engine uses the global one ([`register`], [`find`], ...); separate instances are for tools
/// and tests. Registering a type also registers every type reachable from it (field, element,
/// key, pointee and proxy types), so data that refers to them by id can be loaded.
#[derive(Default)]
pub struct Registry {
    types: BTreeMap<TypeId, Entry>,
}

struct Entry {
    info: &'static TypeInfo,
    /// Rendered `info.name`, kept for name lookups and collision reports.
    name: String,
    /// Type data added with [`Registry::add_data`]; leaked, like the descriptions themselves.
    data: Vec<TypeData>,
}

/// Two different types have the same stable id: either the same name (two versions of a crate,
/// or the same `#[reflect(name)]` twice) or, far less likely, a hash collision.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IdCollision {
    pub id: TypeId,
    pub existing: String,
    pub new: String,
}

impl fmt::Display for IdCollision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "type `{}` has the same id ({}) as the different type `{}`; give one a unique \
             #[reflect(name = \"...\")]",
            self.new, self.id, self.existing
        )
    }
}

impl core::error::Error for IdCollision {}

impl Registry {
    pub const fn new() -> Self {
        Self {
            types: BTreeMap::new(),
        }
    }

    pub fn register<T: Reflect>(&mut self) -> Result<(), IdCollision> {
        self.register_info(type_info::<T>())
    }

    /// Registers `info` and the types it refers to. Re-registering a type is a no-op. On a
    /// collision, the types registered before it stay registered.
    pub fn register_info(&mut self, info: &'static TypeInfo) -> Result<(), IdCollision> {
        // A worklist rather than recursion: type graphs can be deep, and recursive types
        // terminate because a type is only expanded the first time it is seen.
        let mut pending: Vec<&'static TypeInfo> = alloc::vec![info];
        while let Some(info) = pending.pop() {
            if let Some(entry) = self.types.get(&info.id) {
                if entry.info.same_type(info) {
                    continue;
                }
                return Err(IdCollision {
                    id: info.id,
                    existing: entry.name.clone(),
                    new: info.name.to_string(),
                });
            }
            self.types.insert(
                info.id,
                Entry {
                    info,
                    name: info.name.to_string(),
                    data: Vec::new(),
                },
            );
            match &info.kind {
                TypeKind::Struct(s) => pending.extend(s.fields.iter().map(|f| (f.ty)())),
                TypeKind::Tuple(t) => pending.extend(t.fields.iter().map(|f| (f.ty)())),
                TypeKind::Enum(e) => {
                    pending.extend(e.variants.iter().flat_map(|v| v.fields).map(|f| (f.ty)()))
                }
                TypeKind::Array(a) => pending.push((a.element)()),
                TypeKind::List(l) => pending.push((l.element)()),
                TypeKind::Option(o) => pending.push((o.some)()),
                TypeKind::Map(m) => pending.extend([(m.key)(), (m.value)()]),
                TypeKind::Set(s) => pending.push((s.element)()),
                TypeKind::Pointer(p) => pending.push((p.pointee)()),
                TypeKind::Primitive(_) | TypeKind::String(_) | TypeKind::Opaque => {}
            }
            if let Some(proxy) = info.data::<ReflectProxy>() {
                pending.push((proxy.proxy)());
            }
        }
        Ok(())
    }

    pub fn get(&self, id: TypeId) -> Option<&'static TypeInfo> {
        self.types.get(&id).map(|e| e.info)
    }

    /// Lookup by full name, such as `"my_game::Player"` or `"Vec<f32>"`.
    pub fn get_by_name(&self, name: &str) -> Option<&'static TypeInfo> {
        self.types
            .get(&TypeId::of_name(name))
            .filter(|e| e.name == name)
            .map(|e| e.info)
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// All registered types, ordered by id.
    pub fn iter(&self) -> impl Iterator<Item = &'static TypeInfo> + '_ {
        self.types.values().map(|e| e.info)
    }

    /// Attaches type data to `info` (registering it first), for types whose static
    /// [`TypeInfo::data`] cannot be changed: casts to traits implemented elsewhere, serializers of
    /// foreign opaque types. Replaces data of the same Rust type added before. The data is leaked,
    /// like the descriptions themselves.
    pub fn add_data<D: Any + Send + Sync>(
        &mut self,
        info: &'static TypeInfo,
        data: D,
    ) -> Result<(), IdCollision> {
        self.register_info(info)?;
        let data: TypeData = Box::leak(Box::new(data));
        if let Some(entry) = self.types.get_mut(&info.id) {
            entry.data.retain(|d| !d.is::<D>());
            entry.data.push(data);
        }
        Ok(())
    }

    /// Type data of type `D` for `info`: its static data first, else data added to this
    /// registry.
    pub fn data<D: Any>(&self, info: &'static TypeInfo) -> Option<&'static D> {
        info.data::<D>().or_else(|| {
            let entry = self.types.get(&info.id)?;
            if !entry.info.same_type(info) {
                return None;
            }
            entry.data.iter().find_map(|d| {
                let d: &'static (dyn Any + Send + Sync) = *d;
                d.downcast_ref::<D>()
            })
        })
    }
}

static GLOBAL: RwLock<Registry> = RwLock::new(Registry::new());

/// Registers `T` (and the types it refers to) in the global registry. Call at startup for every
/// type that is looked up by id or name.
pub fn register<T: Reflect>() -> Result<(), IdCollision> {
    GLOBAL.write().register::<T>()
}

/// Looks up a type in the global registry.
pub fn find(id: TypeId) -> Option<&'static TypeInfo> {
    GLOBAL.read().get(id)
}

/// Looks up a type in the global registry by full name.
pub fn find_by_name(name: &str) -> Option<&'static TypeInfo> {
    GLOBAL.read().get_by_name(name)
}

/// Attaches type data to `info` in the global registry; see [`Registry::add_data`].
pub fn add_data<D: Any + Send + Sync>(info: &'static TypeInfo, data: D) -> Result<(), IdCollision> {
    GLOBAL.write().add_data(info, data)
}

/// Type data of type `D` for `info`: its static data, else data added to the global registry.
/// The static lookup takes no lock.
pub fn find_data<D: Any>(info: &'static TypeInfo) -> Option<&'static D> {
    info.data::<D>().or_else(|| GLOBAL.read().data::<D>(info))
}

/// Read access to the global registry, for iterating it. Do not register types while holding it.
pub fn global() -> RwLockReadGuard<'static, Registry> {
    GLOBAL.read()
}
