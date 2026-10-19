//! GPU memory allocation shared by the backends (docs/design/0017-rhi-memory.md): committed
//! resources are suballocated from large blocks per memory type and resource class, with a
//! [`Tlsf`] per block; large resources and those the driver asks for get dedicated allocations.
//!
//! A backend implements [`MemoryBackend`] (allocate and free a raw block, mapped if
//! host-visible) and describes its memory types ([`MemoryType`]); it keeps choosing the memory
//! type, creating resources and binding them, and makes dedicated allocations itself (they need
//! the resource), reporting them with [`Allocator::dedicated_allocated`].

pub(crate) mod tlsf;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use alloc::vec::Vec;
use marlin_sync::Mutex;

use crate::{HeapStats, MemoryStats, Result};
pub use tlsf::{Tlsf, TlsfAllocation};

/// What a backend does for the allocator: raw blocks of driver memory.
pub(crate) trait MemoryBackend {
    /// A block of driver memory (`VkDeviceMemory`, `ID3D12Heap`).
    type Raw: Copy;

    /// `size` bytes of `memory_type`, mapped whole for its life if host-visible.
    fn allocate_block(
        &self,
        memory_type: u32,
        size: u64,
    ) -> Result<(Self::Raw, Option<NonNull<u8>>)>;

    /// # Safety
    /// No resource is bound to `raw` any more and no submitted work uses it.
    unsafe fn free_block(&self, raw: Self::Raw);
}

/// Resources that must not share a block, where the API says so: Vulkan's
/// `bufferImageGranularity` (linear buffers next to optimal images), D3D12 resource heap tier 1
/// (buffers, render targets and other textures in separate heaps). The backend maps each class to
/// a pool group ([`Allocator::new`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResourceClass {
    Buffer = 0,
    Texture = 1,
    /// Render targets and depth-stencil textures.
    Target = 2,
}

/// A memory type as the allocator sees it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemoryType {
    /// On a device-local heap (counted as local memory in [`MemoryStats`]).
    pub(crate) local: bool,
    /// The block size this type grows to; blocks start at an eighth of it.
    pub(crate) block_size: u64,
}

/// A suballocation: `raw` memory at `offset`. Give it back with [`Allocator::free`].
#[must_use = "the memory stays allocated until it is freed"]
pub(crate) struct Allocation<R> {
    pub(crate) raw: R,
    pub(crate) offset: u64,
    /// The CPU address of `offset`, in host-visible memory.
    pub(crate) mapped: Option<NonNull<u8>>,
    pool: u32,
    block: u32,
    range: TlsfAllocation,
}

// SAFETY: the mapping is a plain address into memory the allocator keeps mapped; who reads and
// writes through it is the resource's business.
unsafe impl<R: Send> Send for Allocation<R> {}
// SAFETY: as above.
unsafe impl<R: Sync> Sync for Allocation<R> {}

struct Block<R> {
    raw: R,
    mapped: Option<NonNull<u8>>,
    ranges: Tlsf,
}

/// The blocks of one memory type and pool group.
struct Pool<R> {
    /// `None` where a freed block was; allocations name blocks by index.
    blocks: Vec<Option<Block<R>>>,
}

// SAFETY: the pool is only reached through its mutex; see `Allocation`.
unsafe impl<R: Send> Send for Pool<R> {}

/// Counters of one side (local or non-local) of [`MemoryStats`].
#[derive(Default)]
struct Counters {
    dedicated: AtomicU32,
    dedicated_bytes: AtomicU64,
}

/// Whether a resource gets driver memory of its own (VMA's and D3D12MA's policy): when the driver
/// requires or prefers it (`VK_KHR_dedicated_allocation`; render targets on several vendors),
/// when its memory is exported or imported, and when it would take more than half a block.
pub(crate) fn wants_dedicated(size: u64, block_size: u64, driver_prefers: bool) -> bool {
    driver_prefers || size > block_size / 2
}

/// Suballocates resources' memory from blocks, a pool of them per memory type and pool group.
/// Thread-safe: each pool has its own lock, held while a block is allocated or freed.
pub(crate) struct Allocator<R> {
    types: Vec<MemoryType>,
    /// Pool group of each [`ResourceClass`].
    groups: [u8; 3],
    group_count: usize,
    /// `types.len() * group_count` pools, type-major.
    pools: Vec<Mutex<Pool<R>>>,
    /// Dedicated allocations and heaps, local and non-local.
    counters: [Counters; 2],
    /// Driver allocations ever made (blocks, dedicated allocations, heaps).
    driver_allocations: AtomicU64,
}

impl<R: Copy> Allocator<R> {
    /// `groups`: the pool group of each [`ResourceClass`], `0..` (all 0 where classes may share
    /// blocks).
    pub(crate) fn new(types: Vec<MemoryType>, groups: [u8; 3]) -> Self {
        let group_count = groups.iter().map(|&g| g as usize + 1).max().unwrap_or(1);
        let pools = (0..types.len() * group_count)
            .map(|_| Mutex::new(Pool { blocks: Vec::new() }))
            .collect();
        Self {
            types,
            groups,
            group_count,
            pools,
            counters: Default::default(),
            driver_allocations: AtomicU64::new(0),
        }
    }

    /// Whether a resource of `size` bytes of `memory_type` gets dedicated memory; see
    /// [`wants_dedicated`].
    pub(crate) fn wants_dedicated(
        &self,
        memory_type: u32,
        size: u64,
        driver_prefers: bool,
    ) -> bool {
        wants_dedicated(
            size,
            self.types[memory_type as usize].block_size,
            driver_prefers,
        )
    }

    /// `size` bytes aligned to `align` (a power of two) in a block of `memory_type`, for a
    /// resource of `class`. Allocates a new block when no block has room.
    pub(crate) fn allocate<B: MemoryBackend<Raw = R>>(
        &self,
        backend: &B,
        memory_type: u32,
        class: ResourceClass,
        size: u64,
        align: u64,
    ) -> Result<Allocation<R>> {
        let pool_index =
            memory_type as usize * self.group_count + self.groups[class as usize] as usize;
        let mut pool = self.pools[pool_index].lock();
        let found = pool.blocks.iter_mut().enumerate().find_map(|(i, b)| {
            let b = b.as_mut()?;
            Some((i, b.ranges.allocate(size, align)?))
        });
        let (index, range) = match found {
            Some(found) => found,
            None => {
                let block = self.new_block(backend, memory_type, &pool, size, align)?;
                let index = match pool.blocks.iter().position(Option::is_none) {
                    Some(i) => i,
                    None => {
                        pool.blocks.push(None);
                        pool.blocks.len() - 1
                    }
                };
                let block = pool.blocks[index].insert(block);
                let range = block
                    .ranges
                    .allocate(size, align)
                    .expect("a new block holds the request");
                (index, range)
            }
        };
        let block = pool.blocks[index].as_ref().expect("a live block");
        Ok(Allocation {
            raw: block.raw,
            offset: range.offset,
            // SAFETY: the range lies within the mapped block.
            mapped: block
                .mapped
                .map(|m| unsafe { m.add(range.offset as usize) }),
            pool: pool_index as u32,
            block: index as u32,
            range,
        })
    }

    /// A block for a request of `size` bytes: the type's block size, or smaller (down to an
    /// eighth) while the pool's blocks are smaller still and the request fits twice; halved
    /// further if the driver is out of memory.
    fn new_block<B: MemoryBackend<Raw = R>>(
        &self,
        backend: &B,
        memory_type: u32,
        pool: &Pool<R>,
        size: u64,
        align: u64,
    ) -> Result<Block<R>> {
        let preferred = self.types[memory_type as usize].block_size;
        let largest = pool
            .blocks
            .iter()
            .flatten()
            .map(|b| b.ranges.size())
            .max()
            .unwrap_or(0);
        let need = size.saturating_add(align - 1);
        let mut block_size = preferred;
        for shift in 1..=3 {
            let smaller = preferred >> shift;
            if smaller > largest && smaller >= need.saturating_mul(2) {
                block_size = smaller;
            }
        }
        block_size = block_size.max(need);
        loop {
            match backend.allocate_block(memory_type, block_size) {
                Ok((raw, mapped)) => {
                    self.driver_allocations.fetch_add(1, Relaxed);
                    return Ok(Block {
                        raw,
                        mapped,
                        ranges: Tlsf::new(block_size),
                    });
                }
                Err(e) if block_size / 2 < need => return Err(e),
                Err(_) => block_size /= 2,
            }
        }
    }

    /// Returns `allocation`'s memory. A block left empty is freed unless it is the pool's only
    /// empty one (kept so a resource created and dropped every frame does not allocate a block
    /// every frame).
    ///
    /// # Safety
    /// `allocation` came from this allocator; no submitted work uses it, and the resource bound
    /// to it is destroyed.
    pub(crate) unsafe fn free<B: MemoryBackend<Raw = R>>(
        &self,
        backend: &B,
        allocation: Allocation<R>,
    ) {
        let freed = {
            let mut pool = self.pools[allocation.pool as usize].lock();
            let block = pool.blocks[allocation.block as usize]
                .as_mut()
                .expect("a live block");
            block.ranges.free(allocation.range);
            let other_empty = |pool: &Pool<R>| {
                pool.blocks.iter().enumerate().any(|(i, b)| {
                    i != allocation.block as usize
                        && b.as_ref().is_some_and(|b| b.ranges.is_empty())
                })
            };
            if block.ranges.is_empty() && other_empty(&pool) {
                pool.blocks[allocation.block as usize].take()
            } else {
                None
            }
        };
        if let Some(block) = freed {
            // SAFETY: the block is empty, so nothing is bound to it (the caller's contract).
            unsafe { backend.free_block(block.raw) };
        }
    }

    /// Counts a driver allocation the backend made itself (a dedicated allocation or a heap).
    pub(crate) fn dedicated_allocated(&self, memory_type: u32, size: u64) {
        let c = &self.counters[self.side(memory_type)];
        c.dedicated.fetch_add(1, Relaxed);
        c.dedicated_bytes.fetch_add(size, Relaxed);
        self.driver_allocations.fetch_add(1, Relaxed);
    }

    /// Counts a [`dedicated_allocated`](Self::dedicated_allocated) allocation freed.
    pub(crate) fn dedicated_freed(&self, memory_type: u32, size: u64) {
        let c = &self.counters[self.side(memory_type)];
        c.dedicated.fetch_sub(1, Relaxed);
        c.dedicated_bytes.fetch_sub(size, Relaxed);
    }

    fn side(&self, memory_type: u32) -> usize {
        usize::from(!self.types[memory_type as usize].local)
    }

    pub(crate) fn stats(&self) -> MemoryStats {
        let mut stats = MemoryStats {
            driver_allocations: self.driver_allocations.load(Relaxed),
            ..Default::default()
        };
        for (i, pool) in self.pools.iter().enumerate() {
            let side = self.side((i / self.group_count) as u32);
            let heap = match side {
                0 => &mut stats.local,
                _ => &mut stats.non_local,
            };
            for block in pool.lock().blocks.iter().flatten() {
                heap.blocks += 1;
                heap.block_bytes += block.ranges.size();
                heap.suballocations += block.ranges.allocation_count();
                heap.suballocated_bytes += block.ranges.used();
            }
        }
        for (c, heap) in self
            .counters
            .iter()
            .zip([&mut stats.local, &mut stats.non_local])
        {
            heap.dedicated = c.dedicated.load(Relaxed);
            heap.dedicated_bytes = c.dedicated_bytes.load(Relaxed);
        }
        stats
    }

    /// Frees every block. Allocations still live are leaked resources: logged, their memory
    /// freed with the blocks.
    ///
    /// # Safety
    /// The device is idle and no resource will use the blocks again.
    pub(crate) unsafe fn destroy<B: MemoryBackend<Raw = R>>(&self, backend: &B) {
        for pool in &self.pools {
            for block in pool.lock().blocks.drain(..).flatten() {
                if !block.ranges.is_empty() {
                    marlin_diag::warn!(
                        "{} GPU memory allocation(s) still live when the device was destroyed",
                        block.ranges.allocation_count()
                    );
                }
                // SAFETY: the caller's contract.
                unsafe { backend.free_block(block.raw) };
            }
        }
    }
}

impl HeapStats {
    /// Bytes of driver memory: blocks and dedicated allocations.
    pub const fn total_bytes(&self) -> u64 {
        self.block_bytes + self.dedicated_bytes
    }
}
