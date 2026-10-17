//! Memory tags: which subsystem owns how much memory.
//!
//! Attribution is explicit: a container allocates through [`Tagged`], an allocator wrapper fixed
//! to one [`MemTag`], which counts the bytes of every block it hands out and takes back. The
//! counts are exact because a container frees through the same allocator instance it allocated
//! with. Other allocators (GPU memory, pools, arenas) report with [`track_alloc`] and
//! [`track_free`].
//!
//! Allocations through `GlobalAlloc` (plain `Vec`, `Box`, ...) are not tagged: the heap keeps no
//! per-block data to remember a tag by, and a thread-local "current tag" would be wrong as soon as
//! a block is freed under a different one. They show up in [`stats`](crate::stats) only.
//!
//! Counters are per-tag atomics, each tag on its own cache line. Budgets live beside them; see
//! [`budget`](crate::budget).

use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::MarlinHeap;

/// The subsystem memory is attributed to.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum MemTag {
    /// Tagged by nobody in particular.
    Untagged,
    /// Engine core: job system, ECS world storage, containers of the framework.
    Core,
    /// CPU-side rendering: render graph, draw lists, upload staging.
    Render,
    /// GPU resources, as reported by the RHI.
    Gpu,
    /// Loaded assets: meshes, textures, materials before upload.
    Asset,
    /// Streaming and IO buffers.
    Streaming,
    Audio,
    Physics,
    Animation,
    Ui,
    /// The script VM: its heap, bytecode and objects.
    Script,
    Network,
    /// Editor and development tools.
    Tools,
}

impl MemTag {
    pub const COUNT: usize = 13;

    /// Every tag, in declaration order.
    pub const ALL: [MemTag; Self::COUNT] = [
        MemTag::Untagged,
        MemTag::Core,
        MemTag::Render,
        MemTag::Gpu,
        MemTag::Asset,
        MemTag::Streaming,
        MemTag::Audio,
        MemTag::Physics,
        MemTag::Animation,
        MemTag::Ui,
        MemTag::Script,
        MemTag::Network,
        MemTag::Tools,
    ];

    /// A short display name.
    pub const fn name(self) -> &'static str {
        match self {
            MemTag::Untagged => "untagged",
            MemTag::Core => "core",
            MemTag::Render => "render",
            MemTag::Gpu => "gpu",
            MemTag::Asset => "asset",
            MemTag::Streaming => "streaming",
            MemTag::Audio => "audio",
            MemTag::Physics => "physics",
            MemTag::Animation => "animation",
            MemTag::Ui => "ui",
            MemTag::Script => "script",
            MemTag::Network => "network",
            MemTag::Tools => "tools",
        }
    }

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }
}

const _: () = assert!(MemTag::Tools as usize == MemTag::COUNT - 1);

/// One tag's counters, alone on a cache line.
#[repr(align(64))]
pub(crate) struct Counters {
    pub(crate) bytes: AtomicUsize,
    pub(crate) peak: AtomicUsize,
    pub(crate) allocations: AtomicUsize,
    /// Bytes allowed, or `usize::MAX` for no budget.
    pub(crate) budget: AtomicUsize,
}

pub(crate) static COUNTERS: [Counters; MemTag::COUNT] = [const {
    Counters {
        bytes: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        allocations: AtomicUsize::new(0),
        budget: AtomicUsize::new(usize::MAX),
    }
}; MemTag::COUNT];

#[inline]
pub(crate) fn counters(tag: MemTag) -> &'static Counters {
    &COUNTERS[tag.index()]
}

/// A tag's counters at one moment (each read on its own, so not one consistent snapshot).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TagStats {
    /// Bytes currently allocated, as requested (layout sizes).
    pub bytes: usize,
    /// Most bytes allocated at once since start or the last [`reset_peak`].
    pub peak_bytes: usize,
    /// Live allocations.
    pub allocations: usize,
}

pub fn tag_stats(tag: MemTag) -> TagStats {
    let c = counters(tag);
    TagStats {
        bytes: c.bytes.load(Ordering::Relaxed),
        peak_bytes: c.peak.load(Ordering::Relaxed),
        allocations: c.allocations.load(Ordering::Relaxed),
    }
}

/// Lowers `tag`'s peak to its current bytes, e.g. at a level change.
pub fn reset_peak(tag: MemTag) {
    let c = counters(tag);
    c.peak
        .store(c.bytes.load(Ordering::Relaxed), Ordering::Relaxed);
}

#[inline]
fn add_bytes(c: &Counters, size: usize) {
    let bytes = c.bytes.fetch_add(size, Ordering::Relaxed) + size;
    if bytes > c.peak.load(Ordering::Relaxed) {
        c.peak.fetch_max(bytes, Ordering::Relaxed);
    }
}

/// Counts an allocation of `size` bytes against `tag`.
#[inline]
pub fn track_alloc(tag: MemTag, size: usize) {
    let c = counters(tag);
    c.allocations.fetch_add(1, Ordering::Relaxed);
    add_bytes(c, size);
}

/// Takes back an allocation of `size` bytes counted by [`track_alloc`].
#[inline]
pub fn track_free(tag: MemTag, size: usize) {
    let c = counters(tag);
    c.allocations.fetch_sub(1, Ordering::Relaxed);
    c.bytes.fetch_sub(size, Ordering::Relaxed);
}

/// Accounts for an allocation counted by [`track_alloc`] changing size in place or moving.
#[inline]
pub fn track_resize(tag: MemTag, old_size: usize, new_size: usize) {
    let c = counters(tag);
    if new_size >= old_size {
        add_bytes(c, new_size - old_size);
    } else {
        c.bytes.fetch_sub(old_size - new_size, Ordering::Relaxed);
    }
}

/// An allocator that counts everything allocated through it against one tag.
///
/// ```
/// use marlin_mem::{MemTag, Tagged, tag_stats};
///
/// let mut voices: Vec<u32, Tagged> = Vec::new_in(Tagged::heap(MemTag::Audio));
/// voices.push(7);
/// assert_eq!(tag_stats(MemTag::Audio).bytes, voices.capacity() * 4);
/// ```
///
/// Counts layout sizes, not what the inner allocator rounds them up to; zero-sized blocks are not
/// counted.
#[derive(Clone, Copy, Debug)]
pub struct Tagged<A = MarlinHeap> {
    inner: A,
    tag: MemTag,
}

impl<A> Tagged<A> {
    pub const fn new(tag: MemTag, inner: A) -> Self {
        Self { inner, tag }
    }

    #[inline]
    pub const fn tag(&self) -> MemTag {
        self.tag
    }

    #[inline]
    pub const fn inner(&self) -> &A {
        &self.inner
    }
}

impl Tagged<MarlinHeap> {
    /// The general heap, counted against `tag`.
    pub const fn heap(tag: MemTag) -> Self {
        Self::new(tag, MarlinHeap::new())
    }
}

#[inline]
fn counted(tag: MemTag, block: NonNull<[u8]>, size: usize) -> NonNull<[u8]> {
    if size != 0 {
        track_alloc(tag, size);
    }
    block
}

// SAFETY: every call is forwarded to `inner` unchanged; the counting has no effect on the blocks.
unsafe impl<A: Allocator> Allocator for Tagged<A> {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let block = self.inner.allocate(layout)?;
        Ok(counted(self.tag, block, layout.size()))
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let block = self.inner.allocate_zeroed(layout)?;
        Ok(counted(self.tag, block, layout.size()))
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded from the caller.
        unsafe { self.inner.deallocate(ptr, layout) };
        if layout.size() != 0 {
            track_free(self.tag, layout.size());
        }
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: forwarded from the caller.
        let block = unsafe { self.inner.grow(ptr, old, new)? };
        self.resized(old.size(), new.size());
        Ok(block)
    }

    #[inline]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: forwarded from the caller.
        let block = unsafe { self.inner.grow_zeroed(ptr, old, new)? };
        self.resized(old.size(), new.size());
        Ok(block)
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: forwarded from the caller.
        let block = unsafe { self.inner.shrink(ptr, old, new)? };
        self.resized(old.size(), new.size());
        Ok(block)
    }
}

impl<A> Tagged<A> {
    /// Counts a resize, where either side may be a zero-sized (uncounted) block.
    #[inline]
    fn resized(&self, old: usize, new: usize) {
        match (old, new) {
            (0, 0) => {}
            (0, new) => track_alloc(self.tag, new),
            (old, 0) => track_free(self.tag, old),
            (old, new) => track_resize(self.tag, old, new),
        }
    }
}
