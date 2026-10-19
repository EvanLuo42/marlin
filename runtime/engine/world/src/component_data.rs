//! [`ComponentData`]: one component of any registered type, as levels and Prefabs list them
//! (0019 §5.1).

use core::fmt;

use marlin_ecs::Component;
use marlin_reflect::{
    DynamicHolder, Mut, Ref, Reflect, ReflectDynamic, TypeInfo, TypeKind, TypeName, Value,
    find_data,
};

/// Type data marking a reflected type as a component registered with a
/// [`ComponentRegistry`](crate::ComponentRegistry): only those may be held by [`ComponentData`],
/// so data names no other type, and every held value is `Send + Sync`.
pub struct IsComponent;

/// Marks `info` as a component type (idempotent).
pub(crate) fn mark_component(info: &'static TypeInfo) {
    if find_data::<IsComponent>(info).is_none() {
        // A collision means another type has the same id, which registering the component in
        // an ECS world reports anyway.
        let _ = marlin_reflect::add_data(info, IsComponent);
    }
}

/// One component value of any registered component type, or none. Serialized with its type's
/// id (binary) or name (text, `Light { Intensity = 800.0 }`); a type that is no longer
/// registered reads back as none.
#[derive(Default)]
pub struct ComponentData(Option<Value>);

// SAFETY: a `ComponentData` only ever holds a value of a type `T: Component` (`Send + Sync`):
// `new` requires the bound, and `hold` (what readers use) only accepts types marked
// `IsComponent`, which only `ComponentRegistry` adds, for `T: Component`.
unsafe impl Send for ComponentData {}
// SAFETY: as above.
unsafe impl Sync for ComponentData {}

impl ComponentData {
    pub fn new<T: Component + Reflect>(value: T) -> Self {
        Self(Some(Value::new(value)))
    }

    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }

    /// The held value.
    pub fn get(&self) -> Option<Ref<'_>> {
        self.0.as_ref().map(Value::as_ref)
    }

    pub fn get_mut(&mut self) -> Option<Mut<'_>> {
        self.0.as_mut().map(Value::as_mut)
    }

    /// The held value's type.
    pub fn type_info(&self) -> Option<&'static TypeInfo> {
        self.0.as_ref().map(Value::info)
    }

    /// The held value as a `T`.
    pub fn downcast_ref<T: Reflect>(&self) -> Option<&T> {
        self.get()?.downcast::<T>()
    }

    pub fn downcast_mut<T: Reflect>(&mut self) -> Option<&mut T> {
        self.get_mut()?.downcast::<T>()
    }
}

impl Clone for ComponentData {
    /// # Panics
    /// If the held type cannot be cloned through reflection (component types derive `Clone`).
    fn clone(&self) -> Self {
        Self(self.0.as_ref().map(|v| {
            v.try_clone()
                .unwrap_or_else(|| panic!("`{}` cannot be cloned", v.info().name))
        }))
    }
}

impl fmt::Debug for ComponentData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "ComponentData({})", v.info().name),
            None => f.write_str("ComponentData(None)"),
        }
    }
}

impl DynamicHolder for ComponentData {
    fn held(&self) -> Option<Ref<'_>> {
        self.get()
    }

    fn hold(&mut self, value: Value) -> Result<(), Value> {
        if Self::accepts(value.info()) {
            self.0 = Some(value);
            Ok(())
        } else {
            Err(value)
        }
    }

    fn accepts(info: &'static TypeInfo) -> bool {
        find_data::<IsComponent>(info).is_some()
    }
}

// SAFETY: an opaque description of `ComponentData` itself.
unsafe impl Reflect for ComponentData {
    const TYPE_NAME: TypeName = TypeName::simple("marlin_world::ComponentData");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectDynamic::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Self(None))
    }

    fn reflect_clone(&self) -> Option<Self> {
        match &self.0 {
            Some(v) => v.try_clone().map(|v| Self(Some(v))),
            None => Some(Self(None)),
        }
    }
}
