//! macOS backend. Shares the POSIX implementation; macOS-only pieces (__ulock_wait, mach_vm,
//! dispatch_io) will live next to this file.

pub(crate) use super::unix::Backend;
