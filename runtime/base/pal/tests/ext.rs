//! Capability extensions, each tested where the platform manifest lists it.

#[cfg(marlin_ext = "dylib")]
mod dylib {
    use marlin_pal::ErrorKind;
    use marlin_pal::ext::dylib::Dylib;

    #[cfg(windows)]
    const LIBRARY: &str = "kernel32.dll";
    #[cfg(target_os = "linux")]
    const LIBRARY: &str = "libc.so.6";
    #[cfg(target_vendor = "apple")]
    const LIBRARY: &str = "/usr/lib/libSystem.B.dylib";

    #[cfg(windows)]
    const GET_PID: &core::ffi::CStr = c"GetCurrentProcessId";
    #[cfg(not(windows))]
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
        let path = std::env::temp_dir().join("marlin-no-such-library.bin");
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

#[cfg(marlin_ext = "crash_dump")]
mod crash_dump {
    use marlin_pal::ext::crash_dump::{CrashDump, CrashInfo};

    const CHILD_ENV: &str = "MARLIN_PAL_CRASH_CHILD_DUMP";

    #[test]
    fn writes_dump_of_running_process() {
        let path = std::env::temp_dir().join(format!("marlin-pal-{}.dmp", std::process::id()));
        CrashDump::get()
            .unwrap()
            .write(path.to_str().unwrap(), false)
            .unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::remove_file(&path).unwrap();
        // Header, thread list, module list and stacks: well over a few KiB.
        assert!(len > 8 << 10, "{len}");
    }

    fn on_crash(info: &CrashInfo) {
        let path = std::env::var(CHILD_ENV).unwrap();
        info.write_dump(&path, false).unwrap();
        // Exit with a recognizable code instead of letting WER handle the crash.
        let code = if info.code == 0xC000_0005 && info.access == Some((16, true)) {
            42
        } else {
            43
        };
        std::process::exit(code);
    }

    /// Runs only as the child of `handler_sees_access_violation`.
    #[test]
    #[ignore]
    fn crash_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        CrashDump::get().unwrap().set_handler(Some(on_crash));
        // SAFETY: deliberately faults; the handler ends the process.
        unsafe { std::ptr::without_provenance_mut::<u8>(16).write_volatile(1) };
        unreachable!();
    }

    #[test]
    fn handler_sees_access_violation() {
        let path =
            std::env::temp_dir().join(format!("marlin-pal-crash-{}.dmp", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "crash_dump::crash_child",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD_ENV, &path)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(42));
        let len = std::fs::metadata(&path).unwrap().len();
        std::fs::remove_file(&path).unwrap();
        assert!(len > 8 << 10, "{len}");
    }
}

#[cfg(all(marlin_ext = "large_pages", target_os = "linux"))]
mod large_pages {
    use marlin_pal::ext::large_pages::LargePages;

    /// Most machines reserve no huge pages, so this only checks the allocation when they do.
    #[test]
    fn allocates_when_reserved() {
        let Some(lp) = LargePages::get() else {
            return;
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
}
