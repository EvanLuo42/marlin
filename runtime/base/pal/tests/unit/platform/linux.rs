//! Linux-only behavior: io_uring cancelling a FIFO read, and huge pages.

use marlin_pal::file::{Engine, File, OpenOptions, Queue};
use marlin_test_support::{TempFile, skip};

use crate::cancel::cancel_pending_read;

#[test]
fn cancel_pending_fifo_read_linux() {
    let path = TempFile::new("pal-fifo");
    let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: the path is NUL-terminated.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    // Read-write, so opening does not wait for a writer and reads wait for data.
    let file = File::open(path.to_str().unwrap(), &OpenOptions::read_write()).unwrap();
    let mut queue = Queue::new(4).unwrap();
    if queue.engine() != Engine::IoUring {
        skip!("io_uring is unavailable; the blocking engine cannot cancel a pending read");
    }
    cancel_pending_read(&mut queue, &file);
    drop(queue);
    drop(file);
    std::fs::remove_file(&path).unwrap();
}

/// Most machines reserve no huge pages, so this only checks the allocation when they do.
#[cfg(marlin_ext = "large_pages")]
#[test]
fn allocates_when_reserved() {
    use marlin_pal::ext::large_pages::LargePages;

    let Some(lp) = LargePages::get() else {
        skip!("no huge pages reserved");
    };
    let p = lp
        .alloc(lp.page_size(), marlin_pal::Protection::ReadWrite)
        .unwrap();
    assert!(p.as_ptr().addr().is_multiple_of(lp.page_size()));
    // SAFETY: the allocation is ours, then released whole.
    unsafe {
        p.as_ptr().write_bytes(1, lp.page_size());
        marlin_pal::vmem::release(p, lp.page_size()).unwrap();
    }
}
