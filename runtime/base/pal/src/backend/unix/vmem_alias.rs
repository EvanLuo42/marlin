//! Mirrored mappings of an anonymous shared-memory file (memfd on Linux, an unlinked POSIX shm
//! object on macOS), and discarding page contents with `madvise`.

use core::ffi::c_int;
use core::ptr::{self, NonNull};

use super::{Backend, errno, last_error};
use crate::Result;
use crate::contract::ext::VmemAlias;

/// A file descriptor for `size` bytes of anonymous shared memory.
#[cfg(target_os = "linux")]
fn shared_memory(size: usize) -> Result<c_int> {
    // SAFETY: the name is NUL-terminated and only shows in /proc.
    let fd = unsafe { libc::memfd_create(c"marlin-mirror".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(last_error());
    }
    truncate(fd, size)
}

#[cfg(target_vendor = "apple")]
fn shared_memory(size: usize) -> Result<c_int> {
    use core::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    loop {
        // "/marlin.<pid>.<n>", under the 31 bytes macOS allows.
        let mut name = [0u8; 32];
        let mut len = 0;
        let mut put = |bytes: &[u8]| {
            name[len..len + bytes.len()].copy_from_slice(bytes);
            len += bytes.len();
        };
        put(b"/marlin.");
        // SAFETY: no preconditions.
        put_decimal(&mut put, unsafe { libc::getpid() } as u32);
        put(b".");
        put_decimal(&mut put, NEXT.fetch_add(1, Ordering::Relaxed));
        // SAFETY: `name` is NUL-terminated (zero-filled past `len`).
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr().cast(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            if errno() == libc::EEXIST {
                continue;
            }
            return Err(last_error());
        }
        // The object lives on through the descriptor and the mappings.
        // SAFETY: as above.
        unsafe { libc::shm_unlink(name.as_ptr().cast()) };
        return truncate(fd, size);
    }
}

#[cfg(target_vendor = "apple")]
fn put_decimal(put: &mut impl FnMut(&[u8]), mut n: u32) {
    let mut digits = [0u8; 10];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    put(&digits[start..]);
}

fn truncate(fd: c_int, size: usize) -> Result<c_int> {
    let ok = libc::off_t::try_from(size).is_ok_and(|len| {
        // SAFETY: we own the descriptor.
        unsafe { libc::ftruncate(fd, len) == 0 }
    });
    if ok {
        Ok(fd)
    } else {
        let error = last_error();
        // SAFETY: we own the descriptor.
        unsafe { libc::close(fd) };
        Err(error)
    }
}

// SAFETY: both halves map the same file pages, at `base` and `base + size`, until `mirror_free`.
unsafe impl VmemAlias for Backend {
    fn mirror_alloc(size: usize) -> Result<NonNull<u8>> {
        let fd = shared_memory(size)?;
        // SAFETY: a fresh reservation, then two shared mappings of our descriptor fixed inside
        // it; the descriptor is closed once the mappings hold the file.
        unsafe {
            let base = libc::mmap(
                ptr::null_mut(),
                2 * size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if base == libc::MAP_FAILED {
                let error = last_error();
                libc::close(fd);
                return Err(error);
            }
            let rw = libc::PROT_READ | libc::PROT_WRITE;
            let flags = libc::MAP_SHARED | libc::MAP_FIXED;
            let second = base.cast::<u8>().add(size).cast();
            let ok = libc::mmap(base, size, rw, flags, fd, 0) != libc::MAP_FAILED
                && libc::mmap(second, size, rw, flags, fd, 0) != libc::MAP_FAILED;
            let error = last_error();
            libc::close(fd);
            if !ok {
                libc::munmap(base, 2 * size);
                return Err(error);
            }
            Ok(NonNull::new_unchecked(base.cast()))
        }
    }

    unsafe fn mirror_free(base: NonNull<u8>, size: usize) -> Result<()> {
        // SAFETY: the caller passes exactly the region `mirror_alloc` made.
        if unsafe { libc::munmap(base.as_ptr().cast(), 2 * size) } == 0 {
            Ok(())
        } else {
            Err(last_error())
        }
    }

    unsafe fn reset(ptr: NonNull<u8>, size: usize) -> Result<()> {
        #[cfg(target_vendor = "apple")]
        const ADVICE: c_int = libc::MADV_FREE_REUSABLE;
        #[cfg(not(target_vendor = "apple"))]
        const ADVICE: c_int = libc::MADV_FREE;
        // SAFETY: the caller guarantees the range is committed read-write.
        if unsafe { libc::madvise(ptr.as_ptr().cast(), size, ADVICE) } == 0 {
            return Ok(());
        }
        // MADV_FREE needs Linux 4.5 and private anonymous memory; DONTNEED always works there
        // (it zeroes at once instead of lazily).
        #[cfg(not(target_vendor = "apple"))]
        if errno() == libc::EINVAL {
            // SAFETY: as above.
            if unsafe { libc::madvise(ptr.as_ptr().cast(), size, libc::MADV_DONTNEED) } == 0 {
                return Ok(());
            }
        }
        Err(last_error())
    }
}
