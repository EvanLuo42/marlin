//! Component types and their type-erased descriptions.

use alloc::vec::Vec;
use core::alloc::Layout;
use core::any::TypeId;

use marlin_containers::{HashMap, HashMapExt};
use marlin_reflect::{Reflect, TypeInfo};

use crate::command::Commands;
use crate::entity::Entity;
use crate::query::AccessKey;
use crate::world::World;

/// Any `Send + Sync + 'static` type can be a component.
pub trait Component: Send + Sync + 'static {}

impl<T: Send + Sync + 'static> Component for T {}

/// A component type's index in its world. Ids are per world and assigned on first use.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ComponentId(pub(crate) u32);

impl ComponentId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Where a component type's values live.
///
/// - [`Table`](Self::Table) (the default): one column per component in the table of every
///   archetype that has it. Fastest to iterate; adding or removing the component moves the
///   entity's whole row to another table.
/// - [`SparseSet`](Self::SparseSet): one map from entity to value for the whole world. Adding and
///   removing only touch that map and the entity's archetype membership, never its other
///   components, so it suits tags that come and go every few frames. Iterating it costs a lookup
///   per entity.
///
/// Chosen once, when the type is registered ([`World::register_component_with`]); registration
/// otherwise happens on first use with the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StorageType {
    #[default]
    Table,
    SparseSet,
}

/// What a component hook receives.
///
/// Hooks run synchronously, inside the world operation that triggered them, but they only get a
/// shared view of the world: they may read anything, and they change the world through
/// `commands`, which run right after the triggering operation (see [`ComponentHooks`]).
pub struct HookContext<'w, 's> {
    pub world: &'w World,
    pub entity: Entity,
    pub component: ComponentId,
    pub commands: Commands<'w, 's>,
}

/// A component hook: see [`ComponentHooks`].
pub type ComponentHook = fn(&mut HookContext<'_, '_>);

/// Functions a world calls when a component type is added to, written on, or removed from an
/// entity. Registered per world with [`World::component_hooks`]; each slot takes one hook.
///
/// | hook         | when                                                                  |
/// |--------------|-----------------------------------------------------------------------|
/// | `on_add`     | after the component is added to an entity that did not have it        |
/// | `on_insert`  | after every insert of the component, new or replacing                 |
/// | `on_replace` | before a value is overwritten by an insert, or removed                |
/// | `on_remove`  | before the component is removed from an entity (despawn included)     |
///
/// **Ordering.** For a spawn or an insert, the values are written and the entity is in its final
/// archetype first; then `on_add` runs for each newly added component, then `on_insert` for each
/// inserted one, both in bundle order. `on_replace` for replaced values runs before anything is
/// written. For a remove or a despawn, `on_replace` runs for each component going away, then
/// `on_remove` for each (in component id order for a despawn), while the entity still has all of
/// them, so hooks can read the values going away.
///
/// **World access.** A hook gets `&World` and [`Commands`]. Commands it records go to a world
/// queue that is applied when the outermost operation finishes, first in, first out; operations
/// those commands perform may trigger more hooks, whose commands are appended to the same queue.
/// So hook effects are visible once the call that triggered them (`spawn`, `insert`,
/// [`World::apply`], ...) returns, and the triggering operation never sees the world change under
/// it.
#[derive(Clone, Copy, Default, Debug)]
pub struct ComponentHooks {
    pub(crate) on_add: Option<ComponentHook>,
    pub(crate) on_insert: Option<ComponentHook>,
    pub(crate) on_replace: Option<ComponentHook>,
    pub(crate) on_remove: Option<ComponentHook>,
}

fn set_hook(slot: &mut Option<ComponentHook>, hook: ComponentHook, which: &str) {
    assert!(
        slot.is_none(),
        "the component already has an `{which}` hook"
    );
    *slot = Some(hook);
}

impl ComponentHooks {
    /// # Panics
    /// If an `on_add` hook is already registered.
    pub fn on_add(&mut self, hook: ComponentHook) -> &mut Self {
        set_hook(&mut self.on_add, hook, "on_add");
        self
    }

    /// # Panics
    /// If an `on_insert` hook is already registered.
    pub fn on_insert(&mut self, hook: ComponentHook) -> &mut Self {
        set_hook(&mut self.on_insert, hook, "on_insert");
        self
    }

    /// # Panics
    /// If an `on_replace` hook is already registered.
    pub fn on_replace(&mut self, hook: ComponentHook) -> &mut Self {
        set_hook(&mut self.on_replace, hook, "on_replace");
        self
    }

    /// # Panics
    /// If an `on_remove` hook is already registered.
    pub fn on_remove(&mut self, hook: ComponentHook) -> &mut Self {
        set_hook(&mut self.on_remove, hook, "on_remove");
        self
    }
}

/// Writes a clone of the value at `src` to the uninitialized `dst`; false (writing nothing) if it
/// cannot. The [`marlin_reflect::TypeOps::clone`] signature.
pub type CloneFn = unsafe fn(src: *const u8, dst: *mut u8) -> bool;

/// # Safety
/// `src` is a live `T`, `dst` aligned storage for one.
unsafe fn clone_as<T: Clone>(src: *const u8, dst: *mut u8) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe { dst.cast::<T>().write((*src.cast::<T>()).clone()) };
    true
}

/// # Safety
/// `p` points to a valid, owned `T`.
unsafe fn drop_as<T>(p: *mut u8) {
    // SAFETY: forwarded from the caller.
    unsafe { p.cast::<T>().drop_in_place() }
}

/// A component type for [`World::register_dynamic`]: what the world needs to store values it
/// only knows as bytes, as the VM and the editor see them.
#[derive(Clone, Copy, Debug)]
pub struct ComponentDescriptor {
    name: &'static str,
    type_id: Option<TypeId>,
    layout: Layout,
    drop: Option<unsafe fn(*mut u8)>,
    clone: Option<CloneFn>,
    reflect: Option<&'static TypeInfo>,
    storage: StorageType,
}

impl ComponentDescriptor {
    /// The description of the Rust type `T`.
    pub fn new<T: Component>() -> Self {
        Self {
            name: core::any::type_name::<T>(),
            type_id: Some(TypeId::of::<T>()),
            layout: Layout::new::<T>(),
            drop: core::mem::needs_drop::<T>().then_some(drop_as::<T> as unsafe fn(*mut u8)),
            clone: None,
            reflect: None,
            storage: StorageType::Table,
        }
    }

    /// A component described by its reflected type: layout, drop and clone come from `info`. It
    /// shares its id with the Rust type it describes, however that type is registered.
    ///
    /// # Safety
    /// The described type must be `Send + Sync`: systems on several threads share components.
    pub unsafe fn from_reflect(info: &'static TypeInfo) -> Self {
        let name = match *info.name {
            marlin_reflect::TypeName::Path { path, .. } => path,
            marlin_reflect::TypeName::Array { .. } => "[array]",
            marlin_reflect::TypeName::Tuple(_) => "(tuple)",
            // Const arguments only appear inside other names.
            marlin_reflect::TypeName::Const(_) => "(const)",
        };
        Self {
            name,
            type_id: Some((info.ops.rust_id)()),
            layout: Layout::from_size_align(info.size, info.align)
                .expect("reflected types have valid layouts"),
            drop: Some(info.ops.drop),
            clone: Some(info.ops.clone),
            reflect: Some(info),
            storage: StorageType::Table,
        }
    }

    /// A component without a Rust type, such as one a script defines. Every registration makes a
    /// new component type.
    ///
    /// # Safety
    /// `drop`, if any, must accept any value written to storage of `layout` by the code using the
    /// component, and those values must be safe to send to and share between threads.
    pub unsafe fn from_raw(
        name: &'static str,
        layout: Layout,
        drop: Option<unsafe fn(*mut u8)>,
    ) -> Self {
        Self {
            name,
            type_id: None,
            layout,
            drop,
            clone: None,
            reflect: None,
            storage: StorageType::Table,
        }
    }

    pub fn with_storage(mut self, storage: StorageType) -> Self {
        self.storage = storage;
        self
    }

    /// # Safety
    /// `clone` must write a valid value of this component's type.
    pub unsafe fn with_clone(mut self, clone: CloneFn) -> Self {
        self.clone = Some(clone);
        self
    }
}

/// What storage needs to know about a component type without knowing the type.
#[derive(Clone, Copy, Debug)]
pub struct ComponentInfo {
    id: ComponentId,
    name: &'static str,
    type_id: Option<TypeId>,
    layout: Layout,
    drop: Option<unsafe fn(*mut u8)>,
    clone: Option<CloneFn>,
    reflect: Option<&'static TypeInfo>,
    storage: StorageType,
    pub(crate) hooks: ComponentHooks,
}

impl ComponentInfo {
    fn new(id: ComponentId, desc: ComponentDescriptor) -> Self {
        Self {
            id,
            name: desc.name,
            type_id: desc.type_id,
            layout: desc.layout,
            drop: desc.drop,
            clone: desc.clone,
            reflect: desc.reflect,
            storage: desc.storage,
            hooks: ComponentHooks::default(),
        }
    }

    pub fn id(&self) -> ComponentId {
        self.id
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The Rust type, unless the component was registered from raw parts.
    pub fn type_id(&self) -> Option<TypeId> {
        self.type_id
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn storage(&self) -> StorageType {
        self.storage
    }

    pub fn hooks(&self) -> &ComponentHooks {
        &self.hooks
    }

    /// Whether [`World::clone_entity`] can copy it.
    pub fn is_cloneable(&self) -> bool {
        self.clone.is_some()
    }

    /// What access declarations name this component by: its Rust type, or its id for components
    /// registered from raw parts.
    pub fn access_key(&self) -> AccessKey {
        match self.type_id {
            Some(t) => AccessKey::Type(t),
            None => AccessKey::Component(self.id),
        }
    }

    pub(crate) fn drop_fn(&self) -> Option<unsafe fn(*mut u8)> {
        self.drop
    }

    pub(crate) fn clone_fn(&self) -> Option<CloneFn> {
        self.clone
    }

    /// The reflected description, if registered with [`crate::World::register_reflected`]: what
    /// the editor and the VM use to inspect and bind the component.
    pub fn reflect(&self) -> Option<&'static TypeInfo> {
        self.reflect
    }

    pub(crate) fn descriptor(&self) -> ComponentDescriptor {
        ComponentDescriptor {
            name: self.name,
            type_id: self.type_id,
            layout: self.layout,
            drop: self.drop,
            clone: self.clone,
            reflect: self.reflect,
            storage: self.storage,
        }
    }
}

/// A world's component registry. Opaque outside this crate: it only appears in
/// [`crate::Bundle::register`], which the built-in tuple bundles implement.
#[derive(Default)]
pub struct Components {
    infos: Vec<ComponentInfo>,
    by_type: HashMap<TypeId, ComponentId>,
}

impl Components {
    pub(crate) fn new() -> Self {
        Self {
            infos: Vec::new(),
            by_type: HashMap::new(),
        }
    }

    #[inline]
    pub(crate) fn register<T: Component>(&mut self) -> ComponentId {
        if let Some(&id) = self.by_type.get(&TypeId::of::<T>()) {
            return id;
        }
        self.register_descriptor(ComponentDescriptor::new::<T>())
    }

    /// Registers the descriptor, or merges it into the existing registration of its Rust type.
    ///
    /// # Panics
    /// If the type is registered with another storage type or layout.
    pub(crate) fn register_descriptor(&mut self, desc: ComponentDescriptor) -> ComponentId {
        if let Some(id) = desc.type_id.and_then(|t| self.by_type.get(&t).copied()) {
            let info = &mut self.infos[id.index()];
            assert!(
                info.storage == desc.storage,
                "component `{}` is already registered with {:?} storage",
                info.name,
                info.storage
            );
            assert_eq!(info.layout, desc.layout, "component `{}` layout", info.name);
            info.reflect = info.reflect.or(desc.reflect);
            info.clone = info.clone.or(desc.clone);
            return id;
        }
        let id = ComponentId(u32::try_from(self.infos.len()).expect("too many component types"));
        self.infos.push(ComponentInfo::new(id, desc));
        if let Some(t) = desc.type_id {
            self.by_type.insert(t, id);
        }
        id
    }

    pub(crate) fn register_reflected<T: Component + Reflect>(&mut self) -> ComponentId {
        let id = self.register::<T>();
        let info = &mut self.infos[id.index()];
        let reflect = marlin_reflect::type_info::<T>();
        info.reflect = Some(reflect);
        info.clone = info.clone.or(Some(reflect.ops.clone));
        id
    }

    pub(crate) fn register_clone<T: Component + Clone>(&mut self) -> ComponentId {
        let id = self.register::<T>();
        self.infos[id.index()].clone = Some(clone_as::<T>);
        id
    }

    #[inline]
    pub(crate) fn id_of<T: 'static>(&self) -> Option<ComponentId> {
        self.by_type.get(&TypeId::of::<T>()).copied()
    }

    #[inline]
    pub(crate) fn id_of_type(&self, type_id: TypeId) -> Option<ComponentId> {
        self.by_type.get(&type_id).copied()
    }

    #[inline]
    pub(crate) fn info(&self, id: ComponentId) -> &ComponentInfo {
        &self.infos[id.index()]
    }

    #[inline]
    pub(crate) fn get(&self, id: ComponentId) -> Option<&ComponentInfo> {
        self.infos.get(id.index())
    }

    #[inline]
    pub(crate) fn hooks_mut(&mut self, id: ComponentId) -> &mut ComponentHooks {
        &mut self.infos[id.index()].hooks
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ComponentId, &ComponentInfo)> {
        self.infos.iter().map(|info| (info.id, info))
    }
}
