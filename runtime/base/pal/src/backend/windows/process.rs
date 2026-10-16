use super::Backend;
use crate::contract::Process;

/// `FAST_FAIL_FATAL_APP_EXIT`, the code std uses too.
const FAST_FAIL_FATAL_APP_EXIT: u32 = 7;

impl Process for Backend {
    fn abort() -> ! {
        // `__fastfail`: terminates immediately, reports to WER, cannot be intercepted.
        // SAFETY: never returns and touches no memory.
        unsafe {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            core::arch::asm!("int $$0x29", in("ecx") FAST_FAIL_FATAL_APP_EXIT, options(noreturn, nostack));
            #[cfg(target_arch = "aarch64")]
            core::arch::asm!("brk 0xF003", in("x0") FAST_FAIL_FATAL_APP_EXIT, options(noreturn, nostack));
        }
    }
}
