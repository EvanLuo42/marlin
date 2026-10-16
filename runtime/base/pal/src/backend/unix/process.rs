use super::Backend;
use crate::contract::Process;

impl Process for Backend {
    fn abort() -> ! {
        // SAFETY: no preconditions.
        unsafe { libc::abort() }
    }
}
