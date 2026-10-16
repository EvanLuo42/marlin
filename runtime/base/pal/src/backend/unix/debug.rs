use super::{Backend, errno};
use crate::backend::imp;
use crate::contract::Debug;

impl Debug for Backend {
    fn output(s: &str) {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            // SAFETY: `bytes` is valid for `bytes.len()` bytes.
            let n = unsafe { libc::write(libc::STDERR_FILENO, bytes.as_ptr().cast(), bytes.len()) };
            if n > 0 {
                bytes = &bytes[n as usize..];
            } else if n < 0 && errno() == libc::EINTR {
                continue;
            } else {
                return;
            }
        }
    }

    fn is_debugger_present() -> bool {
        imp::is_debugger_present()
    }

    fn break_point() {
        // SAFETY: no preconditions.
        unsafe { libc::raise(libc::SIGTRAP) };
    }
}
