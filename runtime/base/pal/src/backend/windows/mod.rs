//! Win32 backend. The Xbox backend reuses most of it.

mod debug;
mod error;
mod fiber;
mod futex;
#[cfg(marlin_ext = "hybrid_cores")]
mod hybrid_cores;
#[cfg(marlin_ext = "large_pages")]
mod large_pages;
mod process;
mod sysinfo;
mod thread;
mod time;
mod tls;
mod vmem;

pub(crate) struct Backend;

impl crate::contract::Platform for Backend {}

/// Encodes `s` as UTF-16 in chunks of at most 255 code units, each followed by a NUL (included in
/// the slice passed to `f`). Chunks never split a surrogate pair.
fn for_each_wide_chunk(s: &str, mut f: impl FnMut(&[u16])) {
    let mut buf = [0u16; 256];
    let mut len = 0;
    for c in s.chars() {
        if len + 2 >= buf.len() {
            buf[len] = 0;
            f(&buf[..=len]);
            len = 0;
        }
        len += c.encode_utf16(&mut buf[len..]).len();
    }
    if len > 0 {
        buf[len] = 0;
        f(&buf[..=len]);
    }
}
