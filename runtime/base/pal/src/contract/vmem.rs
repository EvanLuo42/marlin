use core::ptr::NonNull;

use crate::{Protection, Result};

/// Virtual memory: address-space reservation separate from physical commit.
///
/// The public wrappers in [`crate::vmem`] reject zero sizes before calling in, so implementations
/// never see `size == 0`.
///
/// # Safety
///
/// Implementations must return reservations aligned to [`allocation_granularity`] that stay valid
/// until released, and must never touch memory outside the ranges they are given.
///
/// [`allocation_granularity`]: VMem::allocation_granularity
pub(crate) unsafe trait VMem {
    /// Granularity of commit, decommit and protect.
    fn page_size() -> usize;
    /// Alignment and size granularity of reservations.
    fn allocation_granularity() -> usize;

    fn reserve(size: usize) -> Result<NonNull<u8>>;
    /// `align` is a power of two above the allocation granularity (checked by the caller). The
    /// result is released with `release(base, size)` like any other reservation.
    fn reserve_aligned(size: usize, align: usize) -> Result<NonNull<u8>>;
    /// # Safety
    /// `base`/`size` describe exactly one live reservation.
    unsafe fn release(base: NonNull<u8>, size: usize) -> Result<()>;
    /// # Safety
    /// The range is page aligned and inside one live reservation.
    unsafe fn commit(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()>;
    /// # Safety
    /// The range is page aligned and inside one live reservation.
    unsafe fn decommit(ptr: NonNull<u8>, size: usize) -> Result<()>;
    /// # Safety
    /// The range is page aligned and committed.
    unsafe fn protect(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()>;
}
