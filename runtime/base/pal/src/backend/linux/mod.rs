//! Linux backend. Shares the POSIX implementation; Linux-only pieces (io_uring, futex, hugetlb)
//! will live next to this file.

pub(crate) use super::unix::Backend;
