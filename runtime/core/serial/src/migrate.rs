//! Explicit migrations between data versions.
//!
//! Name-hash field matching already absorbs added, removed, reordered and retyped fields. For
//! everything else (a field split in two, units changed, a renamed field whose old data should
//! be kept) a type declares a data version and a migration:
//!
//! ```ignore
//! #[derive(Reflect)]
//! #[reflect(version = 3, data(marlin_serial::Migrate::new(Settings::migrate)))]
//! struct Settings { volume: f32, fullscreen: bool }
//!
//! impl Settings {
//!     fn migrate(from: u32, value: Mut<'_>, old: &OldFields<'_>) -> Result<(), Error> {
//!         let settings = value.downcast::<Self>().ok_or(Error::Incompatible)?;
//!         match from {
//!             // v1 stored the volume in percent.
//!             1 => settings.volume /= 100.0,
//!             // v2 had `mode: u8`; 2 meant fullscreen.
//!             2 => settings.fullscreen = old.get::<u8>("mode")? == Some(2),
//!             _ => {}
//!         }
//!         Ok(())
//!     }
//! }
//! ```
//!
//! Versioned structs and enums are written with their version. When data of an older version
//! `v` is read into a type of version `V > v`, the fields are first matched by name as usual,
//! then the migration runs once per step: `migrate(v, ..)`, `migrate(v + 1, ..)`, ...,
//! `migrate(V - 1, ..)`, each upgrading the value from that version to the next. Data written
//! before the type had a version counts as version 0. Data of a newer version is read without
//! migrating. Migrations can also be attached at run time with [`register_migration`].

use alloc::vec::Vec;

use marlin_reflect::{IdCollision, Mut, Reflect, TypeInfo, find_data, name_hash, type_info};

use crate::Error;
use crate::read::Reader;
use crate::wire::{self, Input};

/// Upgrades `value` from data version `from` to `from + 1`. `old` holds the fields of the data
/// that matched no current field.
pub type MigrateFn = fn(from: u32, value: Mut<'_>, old: &OldFields<'_>) -> Result<(), Error>;

/// Type data: the migration of a versioned type.
#[derive(Clone, Copy)]
pub struct Migrate {
    pub migrate: MigrateFn,
}

impl Migrate {
    pub const fn new(migrate: MigrateFn) -> Self {
        Self { migrate }
    }
}

/// Attaches a migration to `T` in the global reflection registry, for types whose
/// `#[reflect(data(...))]` cannot be changed. `T` still needs a `#[reflect(version = N)]`.
pub fn register_migration<T: Reflect>(migrate: MigrateFn) -> Result<(), IdCollision> {
    marlin_reflect::add_data(type_info::<T>(), Migrate::new(migrate))
}

/// The migration to run for data of version `data_version` read into `info`, if any.
pub(crate) fn migration(info: &'static TypeInfo, data_version: u32) -> Option<&'static Migrate> {
    if info.attrs.version > data_version {
        find_data::<Migrate>(info)
    } else {
        None
    }
}

/// Fields of older data that no current field took: removed or renamed fields, and fields whose
/// type changed beyond conversion. Read them by their old name.
pub struct OldFields<'a> {
    /// Name hash and encoded body (signature and value) of each field.
    fields: Vec<(u32, &'a [u8])>,
    variant: Option<u32>,
    reader: Reader,
    depth: u32,
}

impl<'a> OldFields<'a> {
    pub(crate) fn new(
        fields: Vec<(u32, &'a [u8])>,
        variant: Option<u32>,
        reader: Reader,
        depth: u32,
    ) -> Self {
        Self {
            fields,
            variant,
            reader,
            depth,
        }
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.body(name).is_some()
    }

    /// Name hashes of the fields, in data order.
    pub fn hashes(&self) -> impl Iterator<Item = u32> + '_ {
        self.fields.iter().map(|(hash, _)| *hash)
    }

    /// For an enum whose data names a variant the type no longer has: that variant's name hash.
    /// The value then still holds its previous variant, and `self` holds the old variant's
    /// fields.
    pub fn unknown_variant(&self) -> Option<u32> {
        self.variant
    }

    /// Whether [`unknown_variant`](Self::unknown_variant) is `name`.
    pub fn variant_is(&self, name: &str) -> bool {
        self.variant == Some(name_hash(name))
    }

    fn body(&self, name: &str) -> Option<&'a [u8]> {
        let hash = name_hash(name);
        self.fields
            .iter()
            .find(|(h, _)| *h == hash)
            .map(|(_, b)| *b)
    }

    /// Reads old field `name` into `target`, with the usual conversions. `Ok(false)` if there is
    /// no such field or it cannot be converted to `target`'s type.
    pub fn read(&self, name: &str, target: Mut<'_>) -> Result<bool, Error> {
        let Some(bytes) = self.body(name) else {
            return Ok(false);
        };
        let mut body = Input::new(bytes);
        let sig = body.sig(self.reader.max_depth)?;
        if !wire::compatible(target.info(), sig) {
            return Ok(false);
        }
        self.reader.decode(&mut body, target, sig, self.depth)?;
        if body.remaining() != 0 {
            return Err(Error::InvalidLength);
        }
        Ok(true)
    }

    /// Old field `name` as a `T`, starting from `T`'s default; `Ok(None)` if absent or not
    /// convertible.
    pub fn get<T: Reflect>(&self, name: &str) -> Result<Option<T>, Error> {
        let mut value = T::reflect_default().ok_or(Error::NoDefault)?;
        Ok(self.read(name, Mut::new(&mut value))?.then_some(value))
    }
}
