//! Runtime type reflection.
//!
//! Every reflected type has a static [`TypeInfo`]: its name and stable [`TypeId`], size and
//! alignment, its shape ([`TypeKind`]: fields, variants, elements, entries, pointee), type-erased
//! operations (drop, default construction, clone), editor hints ([`TypeAttrs`], [`FieldAttrs`])
//! and type data. On top of that:
//!
//! - [`Ref`] / [`Mut`] navigate values by field name, index or path (`"items[2].name"`) and modify
//!   them without knowing their Rust type; [`Value`] owns one.
//! - The [`registry`] maps ids and names back to descriptions, for loading data by type id, and
//!   attaches type data at run time.
//! - Type data ([`TypeInfo::data`]) attaches arbitrary static values to a type: casts to trait
//!   objects ([`ReflectTrait`], [`Ref::cast`]) and by-value proxies of opaque types
//!   ([`ReflectProxy`]).
//! - `marlin-serial` serializes any `Reflect` type; the editor builds property grids from
//!   [`FieldInfo`] and its [`FieldAttrs`]; `tools/masset` reads and writes it as text.
//!
//! Kinds and the types implementing them here:
//!
//! | kind      | types                                                                         |
//! |-----------|-------------------------------------------------------------------------------|
//! | primitive | `bool`, `char`, integers, floats                                              |
//! | string    | `String`, `Cow<'static, str>`, `marlin_text::InlineString<N>`                 |
//! | struct    | derived structs, `()`, `PhantomData<T>`, the `marlin_math` types              |
//! | tuple     | `(A,)` to `(A, ..., L)` (12 elements)                                         |
//! | array     | `[T; N]`                                                                      |
//! | list      | `Vec`, `VecDeque`, `SmallVec`, `ArrayVec`                                     |
//! | map / set | `HashMap`, `BTreeMap` / `HashSet`, `BTreeSet`                                 |
//! | option    | `Option<T>`                                                                   |
//! | pointer   | `Box<T>`; `Rc<T>`, `Arc<T>` (writable only while unique, see [`Mut::pointee`]) |
//! | opaque    | `marlin_text::Name` (proxy `String`), `marlin_containers::Handle<T>` (`u64`)    |
//!
//! ```ignore
//! #[derive(Reflect)]
//! struct Light {
//!     /// Brightness in EV.
//!     #[reflect(range = 0.0..=10.0, step = 0.1, units = "EV", category = "Lighting")]
//!     intensity: f32,
//!     #[reflect(widget = Color)]
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
mod data;
mod engine;
mod id;
mod info;
pub mod registry;
mod traits;
mod value;

pub use access::{Mut, PathError, PathErrorKind, Ref, get_path, set_path};
pub use data::{Proxied, ReflectProxy, ReflectTrait};
pub use id::{ConstArg, TypeId, TypeName, name_hash};
pub use info::{
    ArrayInfo, EnumInfo, FieldAttrs, FieldInfo, ListInfo, MapInfo, OptionInfo, PointerInfo,
    PointerKind, Primitive, RangeAttr, SetInfo, StringInfo, StructInfo, StructStyle, TupleInfo,
    TypeAttrs, TypeData, TypeInfo, TypeKind, TypeOps, VARIANT_FIELD, VariantInfo, Widget,
};
pub use marlin_reflect_derive::Reflect;
pub use registry::{IdCollision, Registry, add_data, find, find_by_name, find_data, register};
pub use traits::{
    Reflect, ReflectEnum, ReflectList, ReflectMap, ReflectPointer, ReflectSet, ReflectString,
    type_info,
};
pub use value::Value;
