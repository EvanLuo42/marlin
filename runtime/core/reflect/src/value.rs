//! Owned type-erased values.

use alloc::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use core::fmt;
use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};

use crate::access::{Mut, Ref};
use crate::info::TypeInfo;
use crate::traits::{Reflect, type_info};

/// A heap-allocated value of a type known only through its [`TypeInfo`], for example one looked
/// up in the registry by id while loading.
pub struct Value {
    /// Invariant: a live value described by `info`, owned by this `Value`, in storage from
    /// [`allocate`].
    ptr: NonNull<u8>,
    info: &'static TypeInfo,
}

fn layout(info: &TypeInfo) -> Layout {
    // SAFETY: `Reflect` guarantees `size` and `align` come from `size_of`/`align_of` of a real
    // type, which always form a valid layout.
    unsafe { Layout::from_size_align_unchecked(info.size, info.align) }
}

/// Uninitialized storage for one value of `info`.
fn allocate(info: &TypeInfo) -> NonNull<u8> {
    let layout = layout(info);
    if layout.size() == 0 {
        // Zero-sized values need only an aligned, non-null address.
        return NonNull::new(ptr::without_provenance_mut(layout.align()))
            .unwrap_or(NonNull::dangling());
    }
    // SAFETY: the size is non-zero.
    NonNull::new(unsafe { alloc(layout) }).unwrap_or_else(|| handle_alloc_error(layout))
}

/// # Safety
/// `ptr` came from [`allocate`] for the same `info` and holds no live value.
unsafe fn deallocate(ptr: NonNull<u8>, info: &TypeInfo) {
    let layout = layout(info);
    if layout.size() != 0 {
        // SAFETY: allocated with this layout by `allocate`.
        unsafe { dealloc(ptr.as_ptr(), layout) };
    }
}

impl Value {
    pub fn new<T: Reflect>(value: T) -> Self {
        let info = type_info::<T>();
        let ptr = allocate(info);
        // SAFETY: fresh storage for a `T`.
        unsafe { ptr.cast::<T>().write(value) };
        Self { ptr, info }
    }

    /// The default value of `info`'s type, if it has one.
    pub fn default_of(info: &'static TypeInfo) -> Option<Self> {
        let ptr = allocate(info);
        // SAFETY: fresh storage for the type.
        if unsafe { (info.ops.default)(ptr.as_ptr()) } {
            Some(Self { ptr, info })
        } else {
            // SAFETY: nothing was written.
            unsafe { deallocate(ptr, info) };
            None
        }
    }

    /// A clone of `src`, if its type is cloneable.
    pub fn clone_of(src: Ref<'_>) -> Option<Self> {
        let info = src.info();
        let ptr = allocate(info);
        // SAFETY: `src` is a live value of `info`; the destination is fresh storage for it.
        if unsafe { (info.ops.clone)(src.as_ptr(), ptr.as_ptr()) } {
            Some(Self { ptr, info })
        } else {
            // SAFETY: nothing was written.
            unsafe { deallocate(ptr, info) };
            None
        }
    }

    pub fn try_clone(&self) -> Option<Self> {
        Self::clone_of(self.as_ref())
    }

    pub fn info(&self) -> &'static TypeInfo {
        self.info
    }

    pub fn as_ref(&self) -> Ref<'_> {
        // SAFETY: the invariant; the borrow is tied to `self`.
        unsafe { Ref::from_raw(self.ptr.as_ptr(), self.info) }
    }

    pub fn as_mut(&mut self) -> Mut<'_> {
        // SAFETY: the invariant; the exclusive borrow is tied to `self`.
        unsafe { Mut::from_raw(self.ptr.as_ptr(), self.info) }
    }

    /// Takes the value out as a `T`, or gives `self` back if it is another type.
    pub fn downcast<T: Reflect>(self) -> Result<T, Self> {
        if !self.info.is::<T>() {
            return Err(self);
        }
        let this = ManuallyDrop::new(self);
        // SAFETY: the value is a `T`; it is moved out once and the storage freed without dropping.
        unsafe {
            let value = this.ptr.cast::<T>().read();
            deallocate(this.ptr, this.info);
            Ok(value)
        }
    }

    /// Hands the value's storage to `f`, then frees it without dropping the value.
    ///
    /// # Safety
    /// `f` must move the value out of the storage (read it and take ownership), so that it is
    /// neither leaked nor dropped twice.
    pub(crate) unsafe fn consume<R>(self, f: impl FnOnce(*mut u8) -> R) -> R {
        let this = ManuallyDrop::new(self);
        let result = f(this.ptr.as_ptr());
        // SAFETY: `f` moved the value out, so the storage holds no live value.
        unsafe { deallocate(this.ptr, this.info) };
        result
    }

    /// Moves the value to `dst` and frees the storage.
    ///
    /// # Safety
    /// `dst` must be aligned, writable storage for this type holding no live value.
    pub(crate) unsafe fn move_into(self, dst: *mut u8) {
        let this = ManuallyDrop::new(self);
        // SAFETY: the caller's guarantee; ownership of the bytes moves to `dst`.
        unsafe {
            ptr::copy_nonoverlapping(this.ptr.as_ptr(), dst, this.info.size);
            deallocate(this.ptr, this.info);
        }
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: the invariant; the value is dropped once and its storage freed.
        unsafe {
            (self.info.ops.drop)(self.ptr.as_ptr());
            deallocate(self.ptr, self.info);
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Value<{}>", self.info.name)
    }
}
