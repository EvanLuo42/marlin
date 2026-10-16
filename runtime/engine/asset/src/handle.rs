//! Asset handles: typed, reference-counted names of loaded (or loading) objects.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

use marlin_containers::Handle as SlotHandle;
use marlin_sync::Mutex;

use crate::Uuid;

/// The server's slot of one object.
pub(crate) type Slot = SlotHandle<crate::server::ObjectSlot>;

/// Shared by every clone of a handle; dropping the last one tells the server.
pub(crate) struct Shared {
    pub(crate) slot: Slot,
    pub(crate) uuid: Uuid,
    pub(crate) drops: Arc<Mutex<Vec<Slot>>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.drops.lock().push(self.slot);
    }
}

/// A strong reference to an object, typed by what its loader makes (`T`).
///
/// Cloning is cheap (a reference count). While any handle to an object lives, its unit (and the
/// units it links to) stays loaded. [`AssetServer::get`] reads the object once it has loaded;
/// [`AssetServer::state`] says where it is.
///
/// [`AssetServer::get`]: crate::AssetServer::get
/// [`AssetServer::state`]: crate::AssetServer::state
pub struct Handle<T> {
    pub(crate) shared: Arc<Shared>,
    _type: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            _type: PhantomData,
        }
    }

    pub fn uuid(&self) -> Uuid {
        self.shared.uuid
    }

    /// A reference that does not keep the object loaded.
    pub fn downgrade(&self) -> WeakHandle<T> {
        WeakHandle {
            shared: Arc::downgrade(&self.shared),
            uuid: self.shared.uuid,
            _type: PhantomData,
        }
    }

    pub(crate) fn slot(&self) -> Slot {
        self.shared.slot
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self::new(self.shared.clone())
    }
}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl<T> Eq for Handle<T> {}

impl<T> Hash for Handle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.shared.uuid.hash(state);
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({})", self.shared.uuid)
    }
}

/// A reference that does not keep its object loaded; [`upgrade`](Self::upgrade) while some
/// [`Handle`] still does.
pub struct WeakHandle<T> {
    shared: Weak<Shared>,
    uuid: Uuid,
    _type: PhantomData<fn() -> T>,
}

impl<T> WeakHandle<T> {
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub fn upgrade(&self) -> Option<Handle<T>> {
        self.shared.upgrade().map(Handle::new)
    }
}

impl<T> Clone for WeakHandle<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            uuid: self.uuid,
            _type: PhantomData,
        }
    }
}

impl<T> fmt::Debug for WeakHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WeakHandle({})", self.uuid)
    }
}

/// A non-owning reference to an object, as loaders resolve links
/// ([`LoadContext::link`](crate::LoadContext::link)).
///
/// Holding one keeps nothing loaded, so objects linking each other never form reference cycles.
/// Nothing needs to: a unit stays loaded while any unit that links to it does, so the target of
/// a link outlives the object holding it. Read it with
/// [`AssetServer::get_ref`](crate::AssetServer::get_ref); a reference whose object was unloaded
/// reads `None`, never another object.
pub struct AssetRef<T> {
    pub(crate) slot: Slot,
    pub(crate) uuid: Uuid,
    _type: PhantomData<fn() -> T>,
}

impl<T> AssetRef<T> {
    pub(crate) fn new(slot: Slot, uuid: Uuid) -> Self {
        Self {
            slot,
            uuid,
            _type: PhantomData,
        }
    }

    pub fn uuid(&self) -> Uuid {
        self.uuid
    }
}

impl<T> Clone for AssetRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for AssetRef<T> {}

impl<T> PartialEq for AssetRef<T> {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot
    }
}

impl<T> Eq for AssetRef<T> {}

impl<T> Hash for AssetRef<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
    }
}

impl<T> fmt::Debug for AssetRef<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AssetRef({})", self.uuid)
    }
}

impl<T> Handle<T> {
    /// A non-owning reference to the same object.
    pub fn to_ref(&self) -> AssetRef<T> {
        AssetRef::new(self.shared.slot, self.shared.uuid)
    }
}
