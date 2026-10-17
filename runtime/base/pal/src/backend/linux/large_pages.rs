//! Large pages through hugetlbfs (`MAP_HUGETLB`).
//!
//! Only explicitly reserved huge pages are used (`vm.nr_hugepages`); without a reservation the
//! extension reports itself unavailable. Transparent huge pages need nothing from the engine.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::backend::unix::{Backend, last_error};
use crate::contract::ext::LargePages;
use crate::{Protection, Result};

/// 0 = not queried yet, [`UNAVAILABLE`], or the huge page size.
static STATE: AtomicUsize = AtomicUsize::new(0);
const UNAVAILABLE: usize = 1;

// SAFETY: hugetlb mappings are aligned to the huge page size and released with munmap, which is
// what `VMem::release` does.
unsafe impl LargePages for Backend {
    fn large_page_size() -> Option<usize> {
        let state = match STATE.load(Ordering::Relaxed) {
            0 => {
                let state = query();
                STATE.store(state, Ordering::Relaxed);
                state
            }
            state => state,
        };
        (state != UNAVAILABLE).then_some(state)
    }

    fn alloc_large(size: usize, prot: Protection) -> Result<NonNull<u8>> {
        let prot = match prot {
            Protection::NoAccess => libc::PROT_NONE,
            Protection::Read => libc::PROT_READ,
            Protection::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
            Protection::ReadExecute => libc::PROT_READ | libc::PROT_EXEC,
            Protection::ReadWriteExecute => libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        };
        // SAFETY: a fresh anonymous mapping; MAP_POPULATE takes the pages now, so running out of
        // reserved huge pages fails here rather than with SIGBUS on first touch.
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                prot,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_HUGETLB | libc::MAP_POPULATE,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(last_error());
        }
        // mmap never returns null on success.
        Ok(NonNull::new(p.cast()).unwrap())
    }
}

/// The default huge page size if any huge pages are reserved.
#[cold]
fn query() -> usize {
    let mut buf = [0u8; 8192];
    // SAFETY: the path is NUL-terminated.
    let fd = unsafe { libc::open(c"/proc/meminfo".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return UNAVAILABLE;
    }
    // SAFETY: `buf` is valid for its length.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    // SAFETY: we own `fd`.
    unsafe { libc::close(fd) };
    if n <= 0 {
        return UNAVAILABLE;
    }
    let text = &buf[..n as usize];
    let field = |key: &[u8]| {
        text.split(|&b| b == b'\n').find_map(|line| {
            let value = line.strip_prefix(key)?.trim_ascii();
            let digits = value.strip_suffix(b"kB").unwrap_or(value).trim_ascii();
            digits.iter().try_fold(0usize, |n, &b| {
                b.is_ascii_digit()
                    .then(|| n.checked_mul(10)?.checked_add(usize::from(b - b'0')))
                    .flatten()
            })
        })
    };
    match (field(b"HugePages_Total:"), field(b"Hugepagesize:")) {
        (Some(total), Some(kib)) if total > 0 && kib > 0 => kib * 1024,
        _ => UNAVAILABLE,
    }
}
