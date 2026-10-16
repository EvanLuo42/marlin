//! Process control.

use crate::backend::imp::Backend;
use crate::contract::Process;

/// Terminates the process abnormally, without running destructors or exit handlers.
#[inline]
pub fn abort() -> ! {
    Backend::abort()
}
