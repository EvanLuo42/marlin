//! An in-memory sink keeping the most recent log text, and byte-ring helpers.

use crate::Level;
use crate::lock::Locked;
use crate::sink::{LINE_LEN, LineSink, Record, Sink, format_line};

/// Copies `data` into `ring` starting at `at % ring.len()`, wrapping around. `data` must not be
/// longer than `ring`.
pub(crate) fn copy_in(ring: &mut [u8], at: u64, data: &[u8]) {
    if ring.is_empty() {
        return;
    }
    let start = (at % ring.len() as u64) as usize;
    let first = data.len().min(ring.len() - start);
    ring[start..start + first].copy_from_slice(&data[..first]);
    let rest = (data.len() - first).min(start);
    ring[..rest].copy_from_slice(&data[first..first + rest]);
}

/// Fills `out` from `ring` starting at `at % ring.len()`, wrapping around. `out` must not be
/// longer than `ring`.
pub(crate) fn copy_out(ring: &[u8], at: u64, out: &mut [u8]) {
    if ring.is_empty() {
        return;
    }
    let start = (at % ring.len() as u64) as usize;
    let first = out.len().min(ring.len() - start);
    out[..first].copy_from_slice(&ring[start..start + first]);
    let rest = (out.len() - first).min(start);
    out[first..first + rest].copy_from_slice(&ring[..rest]);
}

struct Ring<const N: usize> {
    buf: [u8; N],
    /// Bytes ever written; the ring holds the last `min(written, N)`.
    written: u64,
}

impl<const N: usize> Ring<N> {
    fn byte(&self, at: u64) -> u8 {
        self.buf[(at % N as u64) as usize]
    }

    /// Where the readable text starts: the oldest of the last `N - 1` bytes, moved past a partial
    /// first line once the ring has wrapped. The byte before it, still in the ring, tells whether
    /// a line starts there.
    fn first_line(&self) -> u64 {
        let oldest = self.written.saturating_sub(N as u64 - 1);
        if oldest == 0 || self.byte(oldest - 1) == b'\n' {
            return oldest;
        }
        (oldest..self.written)
            .find(|&at| self.byte(at) == b'\n')
            .map_or(self.written, |at| at + 1)
    }

    /// The first line start at or after `at`.
    fn line_start_from(&self, start: u64, at: u64) -> u64 {
        if at <= start || self.byte(at - 1) == b'\n' {
            return at.max(start);
        }
        (at..self.written)
            .find(|&i| self.byte(i) == b'\n')
            .map_or(self.written, |i| i + 1)
    }
}

/// Keeps the last `N` bytes of formatted log text in memory, for crash reports.
///
/// Lines are formatted as [`Record`] displays. Readers see the whole lines within the last
/// `N - 1` bytes (the byte before them shows where a line starts); a line partly overwritten by
/// the wraparound is skipped. Readers try the lock for a short while and give up if it stays
/// taken, so they are safe to call from a panic handler.
///
/// ```ignore
/// static RECENT: marlin_diag::RingSink<{ 64 * 1024 }> = marlin_diag::RingSink::new();
/// marlin_diag::register_sink(&RECENT)?;
/// ```
pub struct RingSink<const N: usize> {
    ring: Locked<Ring<N>>,
}

impl<const N: usize> RingSink<N> {
    pub const fn new() -> Self {
        const { assert!(N > 1, "a RingSink needs room") };
        Self {
            ring: Locked::new(Ring {
                buf: [0; N],
                written: 0,
            }),
        }
    }

    /// Appends text as is. Only the last `N` bytes are kept.
    pub fn write(&self, text: &str) {
        let bytes = text.as_bytes();
        let bytes = &bytes[bytes.len().saturating_sub(N)..];
        self.ring.with(|ring| {
            copy_in(&mut ring.buf, ring.written, bytes);
            ring.written += bytes.len() as u64;
        });
    }

    /// Bytes ever written, including those overwritten since.
    pub fn bytes_written(&self) -> u64 {
        self.ring.with(|ring| ring.written)
    }

    pub fn clear(&self) {
        self.ring.with(|ring| ring.written = 0);
    }

    /// Copies the newest whole lines that fit into `out`, oldest first, and returns the bytes
    /// copied. 0 if the ring stays locked by another thread.
    pub fn copy_to(&self, out: &mut [u8]) -> usize {
        self.ring
            .try_with(|ring| {
                let start = ring.first_line();
                let from = ring.written.saturating_sub(out.len() as u64);
                let from = ring.line_start_from(start, from);
                let len = (ring.written - from) as usize;
                copy_out(&ring.buf, from, &mut out[..len]);
                len
            })
            .unwrap_or(0)
    }

    /// Calls `f` on each whole line, oldest first, without its `'\n'`. A final line without one is
    /// included; lines longer than [`LINE_LEN`] bytes are cut short. Returns whether the lock was
    /// taken; `false` if another thread kept it.
    ///
    /// `f` runs with the ring locked, so it must not log to this sink.
    pub fn for_each_line(&self, mut f: impl FnMut(&str)) -> bool {
        self.ring
            .try_with(|ring| {
                let mut line = [0u8; LINE_LEN];
                let mut at = ring.first_line();
                while at < ring.written {
                    let end = (at..ring.written)
                        .find(|&i| ring.byte(i) == b'\n')
                        .unwrap_or(ring.written);
                    let len = ((end - at) as usize).min(LINE_LEN);
                    copy_out(&ring.buf, at, &mut line[..len]);
                    f(utf8_prefix(&line[..len]));
                    at = end + 1;
                }
            })
            .is_some()
    }
}

/// The longest valid UTF-8 prefix of `bytes`.
fn utf8_prefix(bytes: &[u8]) -> &str {
    match core::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(error) => {
            let valid = &bytes[..error.valid_up_to()];
            // SAFETY: `valid_up_to` bytes are valid UTF-8.
            unsafe { core::str::from_utf8_unchecked(valid) }
        }
    }
}

impl<const N: usize> Default for RingSink<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Sink for RingSink<N> {
    fn log(&self, record: &Record<'_>) {
        self.write(format_line(record).as_str());
    }
}

impl<const N: usize> LineSink for RingSink<N> {
    fn write_line(&self, _level: Level, line: &str) {
        self.write(line);
    }
}
