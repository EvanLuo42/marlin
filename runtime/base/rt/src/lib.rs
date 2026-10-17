//! Startup sequence, entry-point macros, panic reporting and the process API.
//!
//! Library crates never define global items (`#[global_allocator]`, `#[panic_handler]`, the entry
//! symbol); only binaries in `apps/` do. That is what lets the std editor and the no_std game link
//! the same runtime. The game gets its items from [`game_entry!`]; std binaries call
//! [`hosted::init`].
//!
//! [`start`] (and [`hosted::init`]) run the startup sequence: CPU feature check, the log clock,
//! the crash handler where the platform has one ([`crash`]), and on POSIX one warm-up stack
//! capture, so the first capture in a crash does not have to load the unwinder. Jobs and every
//! subsystem above L0 start in `marlin-framework`.
//!
//! A no_std game built for Windows with `#![windows_subsystem = "windows"]` gets no console and
//! still enters through the C `main` that [`game_entry!`] defines (rustc points the linker's entry
//! at it), so no `WinMain` is needed.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cpu;
pub mod crash;
pub mod hosted;
pub mod panic;
pub mod process;

pub use marlin_pal::PLATFORM;
pub use process::args;

/// Capability extensions of the PAL that belong to the process; each module exists only where
/// the platform manifest lists the capability.
pub mod ext {
    /// Loading shared libraries at run time (desktop only).
    #[cfg(marlin_ext = "dylib")]
    pub mod dylib {
        pub use marlin_pal::ext::dylib::{Dylib, Library};
    }
}

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

/// The steps both entry paths share. Fails if the CPU lacks an instruction set the build needs.
fn startup() -> Result<(), cpu::MissingFeatures> {
    cpu::check()?;
    marlin_diag::init();
    crash::install_default();
    // glibc loads libgcc_s for the first backtrace; do it now rather than inside a crash.
    #[cfg(unix)]
    {
        let mut frame = [0usize; 1];
        marlin_pal::debug::capture_stack(0, &mut frame);
    }
    Ok(())
}

/// Runs the engine startup sequence, then `main`. Returns the process exit code: 1, without
/// running `main`, if the CPU lacks an instruction set the build requires.
pub fn start(main: fn() -> i32) -> i32 {
    if let Err(missing) = startup() {
        use core::fmt::Write;
        let mut out = marlin_pal::debug::OutputBuffer::new();
        let _ = writeln!(out, "marlin: {missing}");
        return 1;
    }
    let code = main();
    marlin_diag::flush();
    code
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

        /// Arguments are read from the OS by `marlin_rt::process`, not from `argv`.
        #[unsafe(export_name = "main")]
        extern "C" fn __marlin_main(
            _argc: ::core::ffi::c_int,
            _argv: *const *const ::core::ffi::c_char,
        ) -> ::core::ffi::c_int {
            $crate::start($main)
        }
    };
}
