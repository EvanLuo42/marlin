#![allow(clippy::undocumented_unsafe_blocks)]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_pal::tls::Key;

static DESTROYED: AtomicUsize = AtomicUsize::new(0);
static LAST_VALUE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn destroy(value: *mut c_void) {
    LAST_VALUE.store(value as usize, Ordering::SeqCst);
    DESTROYED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn per_thread_values_and_exit_destructor() {
    let key = Key::new(Some(destroy)).unwrap();

    unsafe {
        assert!(key.get().is_null());
        key.set(0x10 as *mut c_void).unwrap();
        assert_eq!(key.get() as usize, 0x10);
    }

    std::thread::spawn(move || unsafe {
        // Each thread starts null.
        assert!(key.get().is_null());
        key.set(0x20 as *mut c_void).unwrap();
    })
    .join()
    .unwrap();

    // A thread that never sets a value gets no destructor call.
    std::thread::spawn(move || unsafe { assert!(key.get().is_null()) })
        .join()
        .unwrap();

    assert_eq!(DESTROYED.load(Ordering::SeqCst), 1);
    assert_eq!(LAST_VALUE.load(Ordering::SeqCst), 0x20);

    unsafe {
        assert_eq!(key.get() as usize, 0x10);
        // Windows runs destructors on delete; clear first so the count stays exact.
        key.set(ptr::null_mut()).unwrap();
        key.delete();
    }
    assert_eq!(DESTROYED.load(Ordering::SeqCst), 1);
}
