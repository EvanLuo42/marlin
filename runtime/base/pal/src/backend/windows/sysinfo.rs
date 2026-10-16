use core::ptr::{self, NonNull};

use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::System::SystemInformation::{
    GROUP_AFFINITY, GetLogicalProcessorInformationEx, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

use super::Backend;
use super::error::last_error;
use crate::contract::{SysInfo, VMem};
use crate::sysinfo::LogicalCpu;
use crate::{Protection, Result};

impl SysInfo for Backend {
    fn cpu_topology(out: &mut [LogicalCpu]) -> Result<usize> {
        // The result is variable length and the PAL cannot use the heap, so size it, then read it
        // into scratch pages. Retry if CPUs are hot-added in between.
        let mut scratch: Option<Scratch> = None;
        loop {
            let (buf, mut len) = scratch.as_ref().map_or((ptr::null_mut(), 0), |s| {
                (
                    s.base.as_ptr().cast(),
                    s.size.try_into().unwrap_or(u32::MAX),
                )
            });
            // SAFETY: `buf` is null with `len == 0` (size query) or valid for `len` bytes.
            if unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, buf, &mut len) }
                != 0
            {
                // SAFETY: the call wrote `len` bytes of records to `buf`.
                return Ok(unsafe { parse(buf.cast(), len as usize, out) });
            }
            let err = last_error();
            if err.raw_os_error() != ERROR_INSUFFICIENT_BUFFER as i32 {
                return Err(err);
            }
            drop(scratch.take());
            scratch = Some(Scratch::new(len as usize)?);
        }
    }
}

/// Walks `RelationProcessorCore` records, writing up to `out.len()` CPUs; returns the total.
///
/// # Safety
/// `buf` holds `len` bytes of records from `GetLogicalProcessorInformationEx`.
unsafe fn parse(buf: *const u8, len: usize, out: &mut [LogicalCpu]) -> usize {
    let mut count = 0;
    let mut offset = 0;
    let mut core = 0;
    while offset < len {
        // Records are variable length: only touch fields inside `Size`, never the whole struct.
        // SAFETY: `offset < len`, and each record holds its `Relationship` and `Size` header.
        let (rec, size) = unsafe {
            let rec = buf
                .add(offset)
                .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
            (rec, (&raw const (*rec).Size).read_unaligned() as usize)
        };
        if size == 0 {
            break;
        }
        // SAFETY: we asked for RelationProcessorCore only, so every record is a processor record
        // whose `GroupCount` masks follow inside `Size`.
        let (efficiency_class, group_count, masks) = unsafe {
            let p = &raw const (*rec).Anonymous.Processor;
            (
                (&raw const (*p).EfficiencyClass).read_unaligned(),
                (&raw const (*p).GroupCount).read_unaligned(),
                (&raw const (*p).GroupMask).cast::<GROUP_AFFINITY>(),
            )
        };
        for g in 0..usize::from(group_count) {
            // SAFETY: `g < GroupCount`, within the record.
            let affinity = unsafe { masks.add(g).read_unaligned() };
            let mut bits = affinity.Mask as u64;
            while bits != 0 {
                let number = bits.trailing_zeros();
                bits &= bits - 1;
                if let Some(slot) = out.get_mut(count) {
                    *slot = LogicalCpu {
                        id: u32::from(affinity.Group) * 64 + number,
                        core,
                        efficiency_class,
                    };
                }
                count += 1;
            }
        }
        core += 1;
        offset += size;
    }
    count
}

/// Committed read-write pages, released on drop.
struct Scratch {
    base: NonNull<u8>,
    size: usize,
}

impl Scratch {
    fn new(size: usize) -> Result<Self> {
        let size = size
            .max(1)
            .next_multiple_of(Backend::allocation_granularity());
        let scratch = Self {
            base: Backend::reserve(size)?,
            size,
        };
        // SAFETY: the range is exactly the fresh reservation.
        unsafe { Backend::commit(scratch.base, size, Protection::ReadWrite) }?;
        Ok(scratch)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // SAFETY: `base`/`size` are exactly our reservation, and nothing borrows it any more.
        unsafe { Backend::release(self.base, self.size) }.ok();
    }
}
