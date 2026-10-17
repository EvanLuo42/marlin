//! CPU topology from `hw.*` sysctls. macOS only reports counts per performance level, so the
//! per-CPU assignment is synthetic (see `crate::sysinfo::cpu_topology`).

use core::ffi::CStr;
use core::{mem, ptr};

use super::Backend;
use crate::Result;
use crate::backend::unix::last_error;
use crate::contract::SysInfo;
use crate::sysinfo::{LogicalCpu, MemoryInfo};

impl SysInfo for Backend {
    fn memory() -> Result<MemoryInfo> {
        memory()
    }

    fn cpu_topology(out: &mut [LogicalCpu]) -> Result<usize> {
        // Level 0 is the most performant. Intel Macs and older releases have no perflevel keys.
        let levels = sysctl_u32(c"hw.nperflevels").unwrap_or(1).max(1);
        let mut count = 0;
        let mut core = 0;
        // Least performant level first, so ids and classes ascend together.
        for level in (0..levels).rev() {
            let (logical, physical) = if levels == 1 {
                let logical = sysctl_u32(c"hw.logicalcpu").ok_or_else(last_error)?;
                (logical, sysctl_u32(c"hw.physicalcpu").unwrap_or(logical))
            } else {
                let mut name = [0u8; 40];
                let logical = sysctl_u32(perflevel_key(&mut name, level, b"logicalcpu"))
                    .ok_or_else(last_error)?;
                let physical =
                    sysctl_u32(perflevel_key(&mut name, level, b"physicalcpu")).unwrap_or(logical);
                (logical, physical)
            };
            // SMT siblings assumed adjacent.
            let threads_per_core = (logical / physical.max(1)).max(1);
            for i in 0..logical {
                if let Some(slot) = out.get_mut(count) {
                    *slot = LogicalCpu {
                        id: count as u32,
                        core: core + i / threads_per_core,
                        efficiency_class: (levels - 1 - level) as u8,
                    };
                }
                count += 1;
            }
            core += logical.div_ceil(threads_per_core);
        }
        Ok(count)
    }
}

/// `hw.perflevel<level>.<field>`, NUL-terminated.
fn perflevel_key<'a>(buf: &'a mut [u8; 40], level: u32, field: &[u8]) -> &'a CStr {
    const PREFIX: &[u8] = b"hw.perflevel";
    let mut len = PREFIX.len();
    buf[..len].copy_from_slice(PREFIX);
    // Real systems have 1-3 levels.
    buf[len] = b'0' + level.min(9) as u8;
    buf[len + 1] = b'.';
    len += 2;
    buf[len..len + field.len()].copy_from_slice(field);
    len += field.len();
    buf[len] = 0;
    CStr::from_bytes_until_nul(&buf[..=len]).unwrap()
}

fn sysctl_u32(name: &CStr) -> Option<u32> {
    let mut value: libc::c_int = 0;
    let mut size = mem::size_of_val(&value);
    // SAFETY: `name` is NUL-terminated; `value` is valid for `size` bytes.
    let r = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    (r == 0 && size == mem::size_of_val(&value)).then_some(value as u32)
}

fn memory() -> Result<MemoryInfo> {
    let total_physical = sysctl_u64(c"hw.memsize").ok_or_else(last_error)?;
    let page = sysctl_u64(c"hw.pagesize").unwrap_or(16384);

    // SAFETY: plain data; all-zero is valid.
    let mut vm: libc::vm_statistics64 = unsafe { mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `vm` holds `count` integers; the host port is a send right we own.
    let kr = unsafe {
        let host = mach_host_self();
        let kr =
            libc::host_statistics64(host, libc::HOST_VM_INFO64, (&raw mut vm).cast(), &mut count);
        mach_port_deallocate(mach_task_self_, host);
        kr
    };
    let available_physical = if kr == libc::KERN_SUCCESS {
        (u64::from(vm.free_count) + u64::from(vm.inactive_count) + u64::from(vm.speculative_count))
            * page
    } else {
        0
    };

    // SAFETY: plain data; all-zero is valid.
    let mut basic: libc::mach_task_basic_info = unsafe { mem::zeroed() };
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: `basic` holds `count` integers.
    let kr = unsafe {
        libc::task_info(
            mach_task_self_,
            libc::MACH_TASK_BASIC_INFO,
            (&raw mut basic).cast(),
            &mut count,
        )
    };
    let (process_resident, process_peak_resident) = if kr == libc::KERN_SUCCESS {
        (basic.resident_size, basic.resident_size_max)
    } else {
        (0, 0)
    };

    Ok(MemoryInfo {
        total_physical,
        available_physical,
        commit_limit: None,
        committed: None,
        process_resident,
        process_peak_resident,
        process_private: phys_footprint().unwrap_or(process_resident),
    })
}

/// `task_vm_info.phys_footprint` (<mach/task_info.h>). libc does not bind `task_vm_info`, so read
/// the field at its fixed offset: 18 eight-byte slots precede it (one holds two 32-bit fields).
fn phys_footprint() -> Option<u64> {
    const TASK_VM_INFO: libc::task_flavor_t = 22;
    const PHYS_FOOTPRINT: usize = 18;
    // `TASK_VM_INFO_REV1_COUNT`: everything up to and including phys_footprint, in natural_t.
    const REV1_COUNT: u32 = ((PHYS_FOOTPRINT + 1) * 8 / 4) as u32;
    let mut info = [0u64; 64];
    let mut count = (info.len() * 2) as u32;
    // SAFETY: `info` holds `count` integers.
    let kr = unsafe {
        libc::task_info(
            mach_task_self_,
            TASK_VM_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    (kr == libc::KERN_SUCCESS && count >= REV1_COUNT).then(|| info[PHYS_FOOTPRINT])
}

fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut value: u64 = 0;
    let mut size = mem::size_of_val(&value);
    // SAFETY: `name` is NUL-terminated; `value` is valid for `size` bytes.
    let r = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    match (r, size) {
        (0, 8) => Some(value),
        (0, 4) => Some(value & 0xFFFF_FFFF),
        _ => None,
    }
}

// libc deprecates these bindings in favour of the mach2 crate; the symbols are stable Mach API.
unsafe extern "C" {
    static mach_task_self_: libc::mach_port_t;
    fn mach_host_self() -> libc::mach_port_t;
    fn mach_port_deallocate(
        task: libc::mach_port_t,
        name: libc::mach_port_t,
    ) -> libc::kern_return_t;
}
