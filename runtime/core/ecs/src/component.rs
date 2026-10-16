//! Component types and their type-erased descriptions.

use alloc::vec::Vec;
use core::alloc::Layout;
use core::any::TypeId;

use marlin_containers::{HashMap, HashMapExt};
use marlin_reflect::{Reflect, TypeInfo};

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

/// What storage needs to know about a component type without knowing the type.
#[derive(Clone, Copy, Debug)]
pub struct ComponentInfo {
    name: &'static str,
    type_id: TypeId,
    layout: Layout,
    drop: Option<unsafe fn(*mut u8)>,
    reflect: Option<&'static TypeInfo>,
}

impl ComponentInfo {
    fn of<T: Component>() -> Self {
        /// # Safety
        /// `p` points to a valid, owned `T`.
        unsafe fn drop_as<T>(p: *mut u8) {
            // SAFETY: forwarded from the caller.
            unsafe { p.cast::<T>().drop_in_place() }
        }
        Self {
            name: core::any::type_name::<T>(),
            type_id: TypeId::of::<T>(),
            layout: Layout::new::<T>(),
            drop: core::mem::needs_drop::<T>().then_some(drop_as::<T> as unsafe fn(*mut u8)),
            reflect: None,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub(crate) fn drop_fn(&self) -> Option<unsafe fn(*mut u8)> {
        self.drop
    }

    /// The reflected description, if registered with [`crate::World::register_reflected`]: what
    /// the editor and the VM use to inspect and bind the component.
    pub fn reflect(&self) -> Option<&'static TypeInfo> {
        self.reflect
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

    pub(crate) fn register<T: Component>(&mut self) -> ComponentId {
        if let Some(&id) = self.by_type.get(&TypeId::of::<T>()) {
            return id;
        }
        let id = ComponentId(u32::try_from(self.infos.len()).expect("too many component types"));
        self.infos.push(ComponentInfo::of::<T>());
        self.by_type.insert(TypeId::of::<T>(), id);
        id
    }

    pub(crate) fn register_reflected<T: Component + Reflect>(&mut self) -> ComponentId {
        let id = self.register::<T>();
        self.infos[id.index()].reflect = Some(marlin_reflect::type_info::<T>());
        id
    }

    #[inline]
    pub(crate) fn id_of<T: 'static>(&self) -> Option<ComponentId> {
        self.by_type.get(&TypeId::of::<T>()).copied()
    }

    #[inline]
    pub(crate) fn info(&self, id: ComponentId) -> &ComponentInfo {
        &self.infos[id.index()]
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ComponentId, &ComponentInfo)> {
        self.infos
            .iter()
            .enumerate()
            .map(|(i, info)| (ComponentId(i as u32), info))
    }
}
