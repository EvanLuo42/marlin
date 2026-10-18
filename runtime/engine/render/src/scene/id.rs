//! Stable row handles of a [`RenderScene`](super::RenderScene)'s tables (0009 §1.1–1.2).
//!
//! A handle is a row index plus the row's generation: the index is the row's place in the CPU
//! tables and in the GPU Scene, the generation tells a removed row's handle from its successor's.
//! Handles are reserved where the changes are produced (extract jobs, any thread), and freed
//! rows return to the allocator only when the scene applies the removal, so a reservation never
//! gets a row the scene still uses.

use alloc::vec::Vec;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use marlin_jobs::sync::Mutex;

/// A row of a scene table. `T` names the table.
pub struct Id<T> {
    pub(crate) index: u32,
    pub(crate) generation: u32,
    _table: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    pub(crate) const fn new(index: u32, generation: u32) -> Self {
        Self {
            index,
            generation,
            _table: PhantomData,
        }
    }

    /// The row: the same in the CPU tables and the GPU Scene.
    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.index, self.generation) == (other.index, other.generation)
    }
}

impl<T> Eq for Id<T> {}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.index, self.generation).hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({}v{})", self.index, self.generation)
    }
}

#[derive(Debug)]
pub enum InstanceRow {}
#[derive(Debug)]
pub enum MeshRow {}
#[derive(Debug)]
pub enum LightRow {}
#[derive(Debug)]
pub enum MaterialRow {}

/// A renderable instance.
pub type InstanceId = Id<InstanceRow>;
/// A mesh asset the scene draws, resident on the GPU or not yet.
pub type MeshId = Id<MeshRow>;
/// A light.
pub type LightId = Id<LightRow>;
/// A material instance.
pub type MaterialId = Id<MaterialRow>;

/// Hands out rows of one table to any thread; takes rows back when the scene applies their
/// removal.
pub struct IdAllocator<T> {
    /// Rows never handed out start here.
    next: AtomicU32,
    /// Freed rows with the generation their next handle gets.
    free: Mutex<Vec<(u32, u32)>>,
    _table: PhantomData<fn() -> T>,
}

impl<T> IdAllocator<T> {
    pub const fn new() -> Self {
        Self {
            next: AtomicU32::new(0),
            free: Mutex::new(Vec::new()),
            _table: PhantomData,
        }
    }

    /// A row for a new entry: a freed one, or a new one.
    pub fn reserve(&self) -> Id<T> {
        if let Some((index, generation)) = self.free.lock().pop() {
            return Id::new(index, generation);
        }
        Id::new(self.next.fetch_add(1, Ordering::Relaxed), 0)
    }

    /// Rows ever handed out: the tables' length.
    pub fn len(&self) -> u32 {
        self.next.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Takes back the row of `id`, whose removal the scene has applied.
    pub(crate) fn release(&self, id: Id<T>) {
        self.free
            .lock()
            .push((id.index, id.generation.wrapping_add(1)));
    }
}

impl<T> Default for IdAllocator<T> {
    fn default() -> Self {
        Self::new()
    }
}
