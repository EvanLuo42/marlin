//! Startup sequence, entry-point macros and panic reporting.
//!
//! Library crates never define global items (`#[global_allocator]`, `#[panic_handler]`, the entry
//! symbol); only binaries in `apps/` do. That is what lets the std editor and the no_std game link
//! the same runtime. The game gets its items from [`game_entry!`]; std binaries call
//! [`hosted::init`].
#![cfg_attr(not(test), no_std)]

pub mod cpu;
pub mod hosted;
pub mod panic;

#[doc(hidden)]
pub mod __private {
    pub use marlin_mem::MarlinHeap;
}

// no_std binaries must link the C runtime themselves; under std this is redundant but harmless.
#[cfg(all(target_env = "msvc", not(target_feature = "crt-static")))]
#[link(name = "msvcrt")]
unsafe extern "C" {}
#[cfg(all(target_env = "msvc", target_feature = "crt-static"))]
#[link(name = "libcmt")]
unsafe extern "C" {}
#[cfg(unix)]
#[link(name = "c")]
unsafe extern "C" {}

/// Runs the engine startup sequence, then `main`. Returns the process exit code: 1, without
/// running `main`, if the CPU lacks an instruction set the build requires.
pub fn start(main: fn() -> i32) -> i32 {
    if let Err(missing) = cpu::check() {
        use core::fmt::Write;
        let mut out = marlin_pal::debug::OutputBuffer::new();
        let _ = writeln!(out, "marlin: {missing}");
        return 1;
    }
    // Startup order once the subsystems exist: pal → heap → diag → jobs.
    main()
}

/// Defines the global allocator, panic handler and C `main` symbol of a no_std game binary.
///
/// ```ignore
/// #![no_std]
/// #![no_main]
/// marlin_rt::game_entry!(game_main);
/// fn game_main() -> i32 { 0 }
/// ```
#[macro_export]
macro_rules! game_entry {
    ($main:path) => {
        #[global_allocator]
        static __MARLIN_HEAP: $crate::__private::MarlinHeap = $crate::__private::MarlinHeap::new();

        #[panic_handler]
        fn __marlin_panic(info: &::core::panic::PanicInfo<'_>) -> ! {
            $crate::panic::report(info)
        }

        #[unsafe(export_name = "main")]
        extern "C" fn __marlin_main(
            argc: ::core::ffi::c_int,
            argv: *const *const ::core::ffi::c_char,
        ) -> ::core::ffi::c_int {
            // SAFETY: the C runtime passes `argc` valid strings that live as long as the process.
            unsafe { $crate::set_args(argc, argv) };
            $crate::start($main)
        }
    };
}

static ARGC: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static ARGV: core::sync::atomic::AtomicPtr<*const core::ffi::c_char> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Records the process arguments for [`args`]. Called by [`game_entry!`].
///
/// # Safety
/// `argv` points to `argc` NUL-terminated strings that stay valid and unchanged for the rest of
/// the process.
#[doc(hidden)]
pub unsafe fn set_args(argc: core::ffi::c_int, argv: *const *const core::ffi::c_char) {
    ARGV.store(argv.cast_mut(), core::sync::atomic::Ordering::Release);
    ARGC.store(argc.max(0) as usize, core::sync::atomic::Ordering::Release);
}

/// The process arguments after the program name, as the C runtime passed them; ones that are
/// not UTF-8 are left out. Empty in binaries that did not start through [`game_entry!`].
pub fn args() -> impl Iterator<Item = &'static str> {
    let argc = ARGC.load(core::sync::atomic::Ordering::Acquire);
    let argv = ARGV.load(core::sync::atomic::Ordering::Acquire);
    (1..argc).filter_map(move |i| {
        // SAFETY: `set_args`' contract: `argc` valid, immortal strings.
        let s = unsafe { core::ffi::CStr::from_ptr(*argv.add(i)) };
        s.to_str().ok()
    })
}
