//! Runtime type reflection.
//!
//! Every reflected type has a static [`TypeInfo`]: its name and stable [`TypeId`], size and
//! alignment, its shape ([`TypeKind`]: fields, variants, elements) and type-erased operations
//! (drop, default construction, clone). On top of that:
//!
//! - [`Ref`] / [`Mut`] navigate values by field name, index or path (`"items[2].name"`) and modify
//!   them without knowing their Rust type; [`Value`] owns one.
//! - The [`registry`] maps ids and names back to descriptions, for loading data by type id.
//! - `marlin-serial` serializes any `Reflect` type; the editor builds property grids from
//!   [`FieldInfo`] and its [`FieldAttrs`]; the VM binds engine types through it.
//!
//! ```ignore
//! #[derive(Reflect)]
//! struct Light {
//!     #[reflect(range = 0.0..=10.0, tooltip = "Brightness in EV")]
//!     intensity: f32,
//!     color: [f32; 3],
//!     #[reflect(skip)]
//!     cache: GpuHandle,
//! }
//!
//! marlin_reflect::register::<Light>()?;
//! let mut light = Light::default();
//! marlin_reflect::set_path(&mut light, "color[1]", 0.5f32)?;
//! ```
#![cfg_attr(not(test), no_std)]

extern crate alloc;

// Lets `#[derive(Reflect)]` output, which names `::marlin_reflect`, compile inside this crate.
extern crate self as marlin_reflect;

mod access;
mod id;
mod info;
pub mod registry;
mod traits;
mod value;

pub use access::{Mut, PathError, PathErrorKind, Ref, get_path, set_path};
pub use id::{TypeId, TypeName, name_hash};
pub use info::{
    ArrayInfo, EnumInfo, FieldAttrs, FieldInfo, ListInfo, OptionInfo, Primitive, RangeAttr,
    StringInfo, StructInfo, StructStyle, TypeInfo, TypeKind, TypeOps, VARIANT_FIELD, VariantInfo,
};
pub use marlin_reflect_derive::Reflect;
pub use registry::{IdCollision, Registry, find, find_by_name, register};
pub use traits::{Reflect, ReflectEnum, ReflectList, ReflectString, type_info};
pub use value::Value;
