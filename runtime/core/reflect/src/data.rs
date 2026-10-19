//! Type data: values attached to a type's description and found again by their Rust type.
//!
//! A type declares static type data in its [`TypeInfo::data`] (the derive fills it from
//! `#[reflect(impl_trait(...))]` and `#[reflect(data(...))]`); the
//! [`Registry`](crate::Registry) attaches more at run time, for types whose description cannot be
//! changed. [`find_data`](crate::find_data) looks in both. Any `'static + Send + Sync` value can
//! be type data; this module has the ones reflection and its serializers use:
//!
//! - [`ReflectTrait<dyn T>`]: casts a reflected value to `&dyn T` (like Bevy's `ReflectTrait`
//!   type data). [`Ref::cast`] / [`Mut::cast`] use it.
//! - [`ReflectDynamic`]: an opaque holder of one value of a type chosen at run time (a list of
//!   components of any types), which serializers write with the held type's id or name.
//! - [`ReflectProxy`]: a by-value representation of an opaque type as another reflected type, for
//!   serializers and editors.

use crate::access::{Mut, Ref};
use crate::info::TypeInfo;
use crate::traits::{Reflect, type_info};
use crate::value::Value;

/// Type data that casts a value to the trait object `D` (`dyn Trait`).
///
/// Built by `#[reflect(impl_trait(Trait))]`, or by hand with [`reflect_trait!`](crate::reflect_trait)
/// for types described elsewhere. The casts downcast by Rust type id first, so a `ReflectTrait`
/// attached to the wrong type only ever returns `None`.
pub struct ReflectTrait<D: ?Sized + 'static> {
    get: for<'a> fn(Ref<'a>) -> Option<&'a D>,
    get_mut: for<'a> fn(Mut<'a>) -> Option<&'a mut D>,
}

impl<D: ?Sized + 'static> ReflectTrait<D> {
    pub const fn new(
        get: for<'a> fn(Ref<'a>) -> Option<&'a D>,
        get_mut: for<'a> fn(Mut<'a>) -> Option<&'a mut D>,
    ) -> Self {
        Self { get, get_mut }
    }

    pub fn get<'a>(&self, value: Ref<'a>) -> Option<&'a D> {
        (self.get)(value)
    }

    pub fn get_mut<'a>(&self, value: Mut<'a>) -> Option<&'a mut D> {
        (self.get_mut)(value)
    }
}

/// `ReflectTrait::<dyn Trait>` for `Type`, which must implement `Trait`:
/// `reflect_trait!(Light as dyn Visible)`. Register it with
/// [`Registry::add_data`](crate::Registry::add_data) or list it in a hand-written
/// `TypeInfo::data`.
#[macro_export]
macro_rules! reflect_trait {
    ($ty:ty as dyn $($tr:tt)+) => {
        $crate::ReflectTrait::<dyn $($tr)+>::new(
            |value| ::core::option::Option::Some(value.downcast::<$ty>()?),
            |value| ::core::option::Option::Some(value.downcast::<$ty>()?),
        )
    };
}

/// A type that converts to and from another reflected type by value: a `Name` as a `String`, a
/// `Handle` as its `u64` bits. Opaque types use it (through [`ReflectProxy`]) to be serialized and
/// edited without exposing their fields.
pub trait Proxied: Reflect {
    type Proxy: Reflect;

    fn to_proxy(&self) -> Self::Proxy;
    /// Sets `self` from `proxy`. False, leaving `self` unchanged, if `proxy` is not a valid value.
    fn set_from_proxy(&mut self, proxy: &Self::Proxy) -> bool;
}

/// Type data of a [`Proxied`] type.
pub struct ReflectProxy {
    /// The proxy type.
    pub proxy: fn() -> &'static TypeInfo,
    to_proxy: fn(Ref<'_>) -> Option<Value>,
    from_proxy: fn(Mut<'_>, Ref<'_>) -> bool,
}

impl ReflectProxy {
    pub const fn of<T: Proxied>() -> Self {
        Self {
            proxy: type_info::<T::Proxy>,
            to_proxy: to_proxy_shim::<T>,
            from_proxy: from_proxy_shim::<T>,
        }
    }

    /// The proxy of `value`; `None` if `value` is not of the type this data belongs to.
    pub fn to_proxy(&self, value: Ref<'_>) -> Option<Value> {
        (self.to_proxy)(value)
    }

    /// Sets `value` from `proxy`. False (unchanged) if either has the wrong type or the proxy is
    /// rejected.
    pub fn from_proxy(&self, value: Mut<'_>, proxy: Ref<'_>) -> bool {
        (self.from_proxy)(value, proxy)
    }
}

fn to_proxy_shim<T: Proxied>(value: Ref<'_>) -> Option<Value> {
    Some(Value::new(value.downcast::<T>()?.to_proxy()))
}

fn from_proxy_shim<T: Proxied>(value: Mut<'_>, proxy: Ref<'_>) -> bool {
    match (value.downcast::<T>(), proxy.downcast::<T::Proxy>()) {
        (Some(value), Some(proxy)) => value.set_from_proxy(proxy),
        _ => false,
    }
}

/// An opaque type holding at most one value of a reflected type chosen at run time, such as a
/// component of any type in a level's entity description. Its [`ReflectDynamic`] lets
/// serializers write the held value with its type's id (binary) or name (text) and read it back.
pub trait DynamicHolder: Reflect {
    /// The held value, if any.
    fn held(&self) -> Option<Ref<'_>>;
    /// Holds `value` instead of the current one; gives it back if its type is not
    /// [`accepted`](Self::accepts).
    fn hold(&mut self, value: Value) -> Result<(), Value>;
    /// Whether values of `info`'s type may be held. Readers check it before constructing one.
    fn accepts(info: &'static TypeInfo) -> bool;
}

/// Type data of a [`DynamicHolder`].
pub struct ReflectDynamic {
    held: for<'a> fn(Ref<'a>) -> Option<Ref<'a>>,
    hold: fn(Mut<'_>, Value) -> Result<(), Value>,
    accepts: fn(&'static TypeInfo) -> bool,
}

impl ReflectDynamic {
    pub const fn of<T: DynamicHolder>() -> Self {
        Self {
            held: held_shim::<T>,
            hold: hold_shim::<T>,
            accepts: T::accepts,
        }
    }

    /// The value `holder` holds; `None` if it holds none or is not of the type this data
    /// belongs to.
    pub fn held<'a>(&self, holder: Ref<'a>) -> Option<Ref<'a>> {
        (self.held)(holder)
    }

    /// Makes `holder` hold `value`; gives it back if the type is not accepted or `holder` is not
    /// of the type this data belongs to.
    pub fn hold(&self, holder: Mut<'_>, value: Value) -> Result<(), Value> {
        (self.hold)(holder, value)
    }

    pub fn accepts(&self, info: &'static TypeInfo) -> bool {
        (self.accepts)(info)
    }
}

fn held_shim<T: DynamicHolder>(holder: Ref<'_>) -> Option<Ref<'_>> {
    holder.downcast::<T>()?.held()
}

fn hold_shim<T: DynamicHolder>(holder: Mut<'_>, value: Value) -> Result<(), Value> {
    match holder.downcast::<T>() {
        Some(holder) => holder.hold(value),
        None => Err(value),
    }
}
