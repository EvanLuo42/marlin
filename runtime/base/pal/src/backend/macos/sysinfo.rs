//! CPU topology from `hw.*` sysctls. macOS only reports counts per performance level, so the
//! per-CPU assignment is synthetic (see `crate::sysinfo::cpu_topology`).

use core::ffi::CStr;
use core::{mem, ptr};

use super::Backend;
use crate::Result;
use crate::backend::unix::last_error;
use crate::contract::SysInfo;
use crate::sysinfo::LogicalCpu;

impl SysInfo for Backend {
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
