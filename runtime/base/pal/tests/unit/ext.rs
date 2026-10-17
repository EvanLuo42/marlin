//! Capability extensions, each tested where the platform manifest lists it.

#[cfg(marlin_ext = "dylib")]
mod dylib {
    use marlin_pal::ErrorKind;
    use marlin_pal::ext::dylib::Dylib;

    #[cfg(marlin_platform = "windows")]
    const LIBRARY: &str = "kernel32.dll";
    #[cfg(marlin_platform = "linux")]
    const LIBRARY: &str = "libc.so.6";
    #[cfg(marlin_platform = "macos")]
    const LIBRARY: &str = "/usr/lib/libSystem.B.dylib";

    #[cfg(marlin_platform = "windows")]
    const GET_PID: &core::ffi::CStr = c"GetCurrentProcessId";
    #[cfg(not(marlin_platform = "windows"))]
    const GET_PID: &core::ffi::CStr = c"getpid";

    #[test]
    fn loads_library_and_calls_symbol() {
        let dylib = Dylib::get().unwrap();
        let library = dylib.open(LIBRARY).unwrap();
        // SAFETY: both functions take nothing and return the process id as a 32-bit integer.
        let get_pid: unsafe extern "C" fn() -> u32 = unsafe { library.symbol(GET_PID) }.unwrap();
        // SAFETY: as above.
        assert_eq!(unsafe { get_pid() }, std::process::id());
        // SAFETY: never called.
        let missing: Option<unsafe extern "C" fn()> =
            unsafe { library.symbol(c"marlin_no_such_symbol") };
        assert!(missing.is_none());
    }

    #[test]
    fn missing_library_is_not_found() {
        let dylib = Dylib::get().unwrap();
        let path = marlin_test_support::TempFile::new("pal-no-such-library.bin");
        let error = dylib.open(path.to_str().unwrap()).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::NotFound, "{error:?}");
        assert_eq!(
            dylib.open("").err().unwrap().kind(),
            ErrorKind::InvalidInput
        );
    }
}

#[cfg(marlin_ext = "vmem_alias")]
mod vmem_alias {
    use std::ptr::NonNull;

    use marlin_pal::ErrorKind;
    use marlin_pal::ext::vmem_alias::VmemAlias;

    #[test]
    fn mirror_views_share_pages() {
        let alias = VmemAlias::get().unwrap();
        let size = alias.granularity() * 2;
        let mirror = alias.mirror(size).unwrap();
        let base = mirror.as_ptr();
        // SAFETY: the mirror owns `2 * size` bytes of read-write memory.
        unsafe {
            for i in [0, 1, size / 2, size - 1] {
                assert_eq!(*base.add(i), 0);
                *base.add(i) = (i % 251) as u8 + 1;
                assert_eq!(*base.add(i + size), (i % 251) as u8 + 1);
            }
            // A write that wraps the end lands at the start.
            base.add(size - 2)
                .copy_from_nonoverlapping(b"wrap".as_ptr(), 4);
            assert_eq!(std::slice::from_raw_parts(base, 2), b"ap");
        }
    }

    #[test]
    fn mirror_rejects_bad_sizes() {
        let alias = VmemAlias::get().unwrap();
        for size in [0, alias.granularity() + 1] {
            let error = alias.mirror(size).err().unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn reset_keeps_pages_usable() {
        let alias = VmemAlias::get().unwrap();
        let size = alias.granularity();
        let region = marlin_pal::vmem::reserve(size).unwrap();
        // SAFETY: commits and uses the reservation just made, then releases it.
        unsafe {
            marlin_pal::vmem::commit(region, size, marlin_pal::Protection::ReadWrite).unwrap();
            region.as_ptr().write_bytes(7, size);
            alias.reset(region, size).unwrap();
            // Contents are now undefined, but the pages stay writable.
            region.as_ptr().write_bytes(9, size);
            assert_eq!(*region.as_ptr().add(size - 1), 9);
            marlin_pal::vmem::release(region, size).unwrap();
        }
        // SAFETY: rejected before touching memory.
        let error = unsafe { alias.reset(NonNull::dangling(), 0) }.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }
}
