//! Linux backend: the shared POSIX implementation plus the Linux-only pieces below (io_uring and
//! hugetlb later).

mod file;
mod futex;
mod process;
mod sysinfo;
mod thread;
mod watch;

pub(crate) use super::unix::Backend;
pub(super) use process::{argv, current_exe};
pub(super) use thread::{current_affinity, set_current_affinity, set_current_priority};

/// Reads `TracerPid` from `/proc/self/status`.
pub(super) fn is_debugger_present() -> bool {
    // SAFETY: the path is NUL-terminated.
    let fd = unsafe {
        libc::open(
            c"/proc/self/status".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return false;
    }
    // TracerPid is near the top; the whole file is ~1.5 KiB.
    let mut buf = [0u8; 4096];
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    // SAFETY: we own `fd`.
    unsafe { libc::close(fd) };
    if n <= 0 {
        return false;
    }

    const KEY: &[u8] = b"TracerPid:";
    let status = &buf[..n as usize];
    status
        .windows(KEY.len())
        .position(|w| w == KEY)
        .is_some_and(|i| {
            let value = status[i + KEY.len()..]
                .iter()
                .find(|b| !b.is_ascii_whitespace());
            value.is_some_and(|&b| b != b'0')
        })
}
