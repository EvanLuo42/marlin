//! Cooked payloads used where they were loaded: [`InPlace<T>`] keeps the unit's bytes alive and
//! reads the payload through `T`'s checked view ([`CookedAsset`]), without copying.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::marker::PhantomData;
use core::ops::Range;

use marlin_reflect::{Reflect, type_info};

use crate::server::{LoadContext, Loaders};
use crate::vfs::UnitBytes;

/// A cooked type read in place: its view checks the payload (and the sizes of its bulk
/// blobs) once, when loaded, and is then made from the same bytes without checking again.
/// Implemented by each cooked type's crate (`Texture`, `Font`, ...).
pub trait CookedAsset: Reflect + Send + Sync + 'static {
    type View<'a>;
    type Error: fmt::Debug;

    fn view<'a>(payload: &'a [u8], bulk_sizes: &[u64]) -> Result<Self::View<'a>, Self::Error>;
}

/// A cooked payload used where it was loaded: the unit's bytes stay alive with it.
pub struct InPlace<T> {
    unit: Arc<UnitBytes>,
    payload: Range<usize>,
    bulk: Vec<Range<usize>>,
    _type: PhantomData<fn() -> T>,
}

impl<T> InPlace<T> {
    /// The object `ctx` is loading, kept in its unit's bytes.
    pub fn new(ctx: &LoadContext<'_>) -> Self {
        let unit = ctx.unit_bytes();
        let base = unit.as_slice().as_ptr() as usize;
        let range = |s: &[u8]| {
            // A streamed blob the unit was loaded without has no bytes, and none in the unit.
            if s.is_empty() {
                return 0..0;
            }
            let start = s.as_ptr() as usize - base;
            start..start + s.len()
        };
        Self {
            payload: range(ctx.payload),
            bulk: ctx.bulk.iter().map(|(_, b)| range(b)).collect(),
            unit,
            _type: PhantomData,
        }
    }

    pub fn payload(&self) -> &[u8] {
        &self.unit.as_slice()[self.payload.clone()]
    }

    pub fn bulk(&self, index: usize) -> &[u8] {
        &self.unit.as_slice()[self.bulk[index].clone()]
    }

    pub fn bulk_count(&self) -> usize {
        self.bulk.len()
    }

    pub fn bulk_sizes(&self) -> Vec<u64> {
        self.bulk.iter().map(|r| r.len() as u64).collect()
    }
}

impl<T: CookedAsset> InPlace<T> {
    /// The payload's view, checked when it was loaded.
    pub fn view(&self) -> T::View<'_> {
        T::view(self.payload(), &self.bulk_sizes()).expect("checked at load")
    }
}

impl Loaders {
    /// Loads `T` as [`InPlace<T>`], its view checked once.
    pub fn register_in_place<T: CookedAsset>(&mut self) {
        self.register::<InPlace<T>>(type_info::<T>().id.0, |ctx| {
            let sizes: Vec<u64> = ctx.bulk.iter().map(|(_, b)| b.len() as u64).collect();
            T::view(ctx.payload, &sizes).map_err(|e| -> String { format!("{e:?}") })?;
            Ok(InPlace::new(ctx))
        });
    }
}
