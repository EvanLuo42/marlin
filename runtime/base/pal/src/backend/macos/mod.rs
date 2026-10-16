//! macOS backend: the shared POSIX implementation plus the macOS-only pieces below (mach_vm and
//! dispatch_io later).

mod futex;

use core::ptr;

pub(crate) use super::unix::Backend;

/// Apple Technical Q&A QA1361: sysctl `KERN_PROC_PID` and test `P_TRACED` in `kp_proc.p_flag`.
pub(super) fn is_debugger_present() -> bool {
    // libc does not bind `struct kinfo_proc`, so read `p_flag` at its fixed offset in the 64-bit
    // layout: after a 16-byte union and two pointers.
    const KINFO_PROC_SIZE: usize = 648;
    const P_FLAG_OFFSET: usize = 32;
    const P_TRACED: i32 = 0x0000_0800;

    let mut info = [0u64; KINFO_PROC_SIZE / 8];
    let mut size = KINFO_PROC_SIZE;
    // SAFETY: no preconditions.
    let pid = unsafe { libc::getpid() };
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    // SAFETY: `info` is valid for `size` bytes; `mib` holds 4 entries.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            info.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if r != 0 || size < P_FLAG_OFFSET + 4 {
        return false;
    }
    // SAFETY: `info` is at least P_FLAG_OFFSET + 4 bytes and `p_flag` is 4-byte aligned there.
    let p_flag = unsafe {
        info.as_ptr()
            .cast::<u8>()
            .add(P_FLAG_OFFSET)
            .cast::<i32>()
            .read()
    };
    p_flag & P_TRACED != 0
}
