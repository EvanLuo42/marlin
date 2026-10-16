//! Virtual memory: reserving address space separately from committing physical memory.
//!
//! Lifecycle: [`reserve`] → any number of page-granular [`commit`] / [`decommit`] /
//! [`protect`] calls → [`release`] of the whole reservation.

use core::ptr::NonNull;

use crate::backend::imp::Backend;
use crate::contract::VMem;
use crate::{Error, ErrorKind, Protection, Result};

/// OS page size: the granularity of [`commit`], [`decommit`] and [`protect`].
#[inline]
pub fn page_size() -> usize {
    Backend::page_size()
}

/// Alignment and size granularity of [`reserve`]: 64 KiB on Windows, the page size on most other
/// platforms.
#[inline]
pub fn allocation_granularity() -> usize {
    Backend::allocation_granularity()
}

/// Reserves `size` bytes of inaccessible address space with no physical memory behind it.
///
/// The result is aligned to [`allocation_granularity`]. The OS rounds `size` up to that
/// granularity.
#[inline]
pub fn reserve(size: usize) -> Result<NonNull<u8>> {
    check_size(size)?;
    Backend::reserve(size)
}

/// Releases a whole reservation.
///
/// # Safety
///
/// `base` and `size` must be exactly what a previous [`reserve`] returned and was given (or an
/// allocation from an extension whose docs point here). Nothing may access the region afterwards.
#[inline]
pub unsafe fn release(base: NonNull<u8>, size: usize) -> Result<()> {
    check_size(size)?;
    // SAFETY: forwarded from the caller.
    unsafe { Backend::release(base, size) }
}

/// Commits pages so they are accessible with `prot`. Fresh pages read as zero; committing pages
/// that are already committed keeps their contents.
///
/// On Linux, overcommit means physical memory is only taken when pages are first touched.
///
/// # Safety
///
/// The range must be page aligned and lie inside one live reservation.
#[inline]
pub unsafe fn commit(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()> {
    check_range(ptr, size)?;
    // SAFETY: forwarded from the caller.
    unsafe { Backend::commit(ptr, size, prot) }
}

/// Returns the physical memory behind pages to the OS. The range stays reserved and becomes
/// inaccessible, and its contents are lost.
///
/// # Safety
///
/// The range must be page aligned and lie inside one live reservation. Nothing may access it until
/// it is committed again.
#[inline]
pub unsafe fn decommit(ptr: NonNull<u8>, size: usize) -> Result<()> {
    check_range(ptr, size)?;
    // SAFETY: forwarded from the caller.
    unsafe { Backend::decommit(ptr, size) }
}

/// Changes the protection of committed pages.
///
/// # Safety
///
/// The range must be page aligned and committed, and no live reference may rely on access the new
/// protection removes.
#[inline]
pub unsafe fn protect(ptr: NonNull<u8>, size: usize, prot: Protection) -> Result<()> {
    check_range(ptr, size)?;
    // SAFETY: forwarded from the caller.
    unsafe { Backend::protect(ptr, size, prot) }
}

/// Zero sizes mean different things per OS (Windows `VirtualFree` treats 0 as "the whole
/// region"), so they are rejected everywhere.
#[inline]
fn check_size(size: usize) -> Result<()> {
    if size == 0 {
        return Err(Error::from_kind(ErrorKind::InvalidInput));
    }
    Ok(())
}

#[inline]
fn check_range(ptr: NonNull<u8>, size: usize) -> Result<()> {
    debug_assert!(ptr.addr().get().is_multiple_of(page_size()), "address is not page aligned");
    check_size(size)
}
