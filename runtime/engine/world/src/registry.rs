//! The component types a world knows, and what may be done with each (0019 §5.1).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::TypeId;
use core::ops::BitOr;

use marlin_containers::{HashMap, HashMapExt};
use marlin_ecs::{Children, Component, ComponentId, Parent, StorageType};
use marlin_reflect::{Reflect, TypeInfo};

use crate::guid::EntityGuid;
use crate::levels::{EntityName, InLevel};
use crate::time::Interpolated;
use crate::transform::{GlobalTransform, Transform};

/// What a component type may be used for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct ComponentFlags(u8);

impl ComponentFlags {
    /// Runtime state only: never in levels, Prefabs or saves.
    pub const TRANSIENT: Self = Self(0);
    /// Authored: levels and Prefabs hold it, the editor edits it.
    pub const SAVED: Self = Self(1);
    /// The server sends it to clients (0019 §11; reserved).
    pub const REPLICATED: Self = Self(2);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOr for ComponentFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// One registered component type.
pub struct ComponentRegistration {
    /// The reflected name data refers to it by; for unreflected types, the Rust type name.
    pub name: String,
    pub type_id: TypeId,
    pub reflect: Option<&'static TypeInfo>,
    pub flags: ComponentFlags,
    pub storage: StorageType,
    install: fn(&mut marlin_ecs::World, StorageType) -> ComponentId,
}

impl ComponentRegistration {
    /// Registers the type in an ECS world with its storage (and reflection, if any).
    pub fn install(&self, world: &mut marlin_ecs::World) -> ComponentId {
        (self.install)(world, self.storage)
    }
}

fn install_reflected<T: Component + Reflect>(
    world: &mut marlin_ecs::World,
    storage: StorageType,
) -> ComponentId {
    world.register_component_with::<T>(storage);
    world.register_reflected::<T>()
}

fn install_plain<T: Component>(world: &mut marlin_ecs::World, storage: StorageType) -> ComponentId {
    world.register_component_with::<T>(storage)
}

/// Every component type worlds are built with: filled while modules register (0019 §3.1), then
/// shared, read-only, by every world (`Arc<ComponentRegistry>`), so the same data means the
/// same thing in each.
///
/// [`new`](Self::new) holds the world's own types. Script-defined components arrive with the
/// script module (0019 §10).
pub struct ComponentRegistry {
    entries: Vec<ComponentRegistration>,
    by_name: HashMap<String, usize>,
    by_type: HashMap<TypeId, usize>,
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ComponentRegistry {
    /// The world's own components: [`Transform`], [`EntityGuid`] and [`Interpolated`] (saved),
    /// [`GlobalTransform`], [`EntityName`], [`InLevel`] and the hierarchy's `Parent`/`Children`
    /// (runtime).
    pub fn new() -> Self {
        let mut r = Self {
            entries: Vec::new(),
            by_name: HashMap::new(),
            by_type: HashMap::new(),
        };
        r.add::<Transform>(ComponentFlags::SAVED | ComponentFlags::REPLICATED)
            .add::<EntityGuid>(ComponentFlags::SAVED)
            .add::<Interpolated>(ComponentFlags::SAVED)
            .add::<GlobalTransform>(ComponentFlags::TRANSIENT)
            .add::<EntityName>(ComponentFlags::TRANSIENT)
            .add::<InLevel>(ComponentFlags::TRANSIENT)
            .add_runtime::<Parent>(StorageType::Table)
            .add_runtime::<Children>(StorageType::Table);
        r
    }

    /// Registers a reflected component with table storage.
    ///
    /// # Panics
    /// If the type, or another type with its reflected name, is already registered.
    pub fn add<T: Component + Reflect>(&mut self, flags: ComponentFlags) -> &mut Self {
        self.add_with_storage::<T>(flags, StorageType::Table)
    }

    /// As [`add`](Self::add), with a storage type (sparse sets for tags that come and go).
    pub fn add_with_storage<T: Component + Reflect>(
        &mut self,
        flags: ComponentFlags,
        storage: StorageType,
    ) -> &mut Self {
        let info = marlin_reflect::type_info::<T>();
        // Data finds component types by id (binary) and checks they are components.
        let _ = marlin_reflect::register::<T>();
        crate::component_data::mark_component(info);
        self.push(ComponentRegistration {
            name: info.name.to_string(),
            type_id: TypeId::of::<T>(),
            reflect: Some(info),
            flags,
            storage,
            install: install_reflected::<T>,
        })
    }

    /// Registers a runtime-only component without reflection: it can be neither saved nor
    /// replicated, and the editor and scripts do not see it.
    pub fn add_runtime<T: Component>(&mut self, storage: StorageType) -> &mut Self {
        self.push(ComponentRegistration {
            name: core::any::type_name::<T>().to_string(),
            type_id: TypeId::of::<T>(),
            reflect: None,
            flags: ComponentFlags::TRANSIENT,
            storage,
            install: install_plain::<T>,
        })
    }

    fn push(&mut self, entry: ComponentRegistration) -> &mut Self {
        assert!(
            !self.by_type.contains_key(&entry.type_id),
            "component `{}` is registered twice",
            entry.name
        );
        assert!(
            !self.by_name.contains_key(&entry.name),
            "two component types are named `{}`",
            entry.name
        );
        let index = self.entries.len();
        self.by_name.insert(entry.name.clone(), index);
        self.by_type.insert(entry.type_id, index);
        self.entries.push(entry);
        self
    }

    /// The registration data refers to by `name`.
    pub fn by_name(&self, name: &str) -> Option<&ComponentRegistration> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    pub fn get<T: 'static>(&self) -> Option<&ComponentRegistration> {
        self.by_type
            .get(&TypeId::of::<T>())
            .map(|&i| &self.entries[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &ComponentRegistration> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
