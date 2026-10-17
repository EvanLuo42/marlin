//! A buffered sink writing to a file.

use alloc::boxed::Box;
use alloc::vec;

use marlin_pal::file::{Disposition, File, OpenOptions};

use crate::Level;
use crate::lock::Locked;
use crate::sink::{LineSink, Record, Sink, format_line};

struct Buffer {
    bytes: Box<[u8]>,
    len: usize,
    /// Where the next write goes in the file.
    offset: u64,
}

/// Writes log lines, formatted as [`Record`] displays, to a file.
///
/// Lines collect in a [`FileSink::BUFFER`]-byte buffer that is written out when full, on
/// [`flush`](Sink::flush), after every `Error` record, and on drop. Write errors drop the buffered
/// text; they are not reported, since there is nowhere left to report them.
///
/// To register it, leak it ([`leak`](Self::leak)) or keep it in a static.
pub struct FileSink {
    file: File,
    buffer: Locked<Buffer>,
}

impl FileSink {
    pub const BUFFER: usize = 64 * 1024;

    /// Creates the file at `path`, or truncates it.
    pub fn open(path: &str) -> Result<Self, marlin_pal::Error> {
        let options = OpenOptions::read_write().disposition(Disposition::CreateOrTruncate);
        Self::with_file(File::open(path, &options)?, 0)
    }

    /// Opens or creates the file at `path` and appends to it.
    pub fn open_append(path: &str) -> Result<Self, marlin_pal::Error> {
        let options = OpenOptions::read_write().disposition(Disposition::OpenOrCreate);
        let file = File::open(path, &options)?;
        let end = file.size()?;
        Self::with_file(file, end)
    }

    fn with_file(file: File, offset: u64) -> Result<Self, marlin_pal::Error> {
        Ok(Self {
            file,
            buffer: Locked::new(Buffer {
                bytes: vec![0; Self::BUFFER].into_boxed_slice(),
                len: 0,
                offset,
            }),
        })
    }

    /// Moves the sink to the heap for good, to register it.
    pub fn leak(self) -> &'static Self {
        Box::leak(Box::new(self))
    }

    fn append(&self, line: &[u8], write_through: bool) {
        self.buffer.with(|buffer| {
            if buffer.len + line.len() > buffer.bytes.len() {
                write_out(&self.file, buffer);
            }
            if line.len() > buffer.bytes.len() {
                write_all(&self.file, line, &mut buffer.offset);
            } else {
                buffer.bytes[buffer.len..buffer.len + line.len()].copy_from_slice(line);
                buffer.len += line.len();
            }
            if write_through {
                write_out(&self.file, buffer);
            }
        });
    }
}

/// Writes the buffer to the file and empties it.
fn write_out(file: &File, buffer: &mut Buffer) {
    write_all(file, &buffer.bytes[..buffer.len], &mut buffer.offset);
    buffer.len = 0;
}

/// Writes `bytes` at `*offset`, advancing it by what was written. Stops at the first error.
fn write_all(file: &File, mut bytes: &[u8], offset: &mut u64) {
    while !bytes.is_empty() {
        match file.write_at(bytes, *offset) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let n = n.min(bytes.len());
                bytes = &bytes[n..];
                *offset += n as u64;
            }
        }
    }
}

impl Sink for FileSink {
    fn log(&self, record: &Record<'_>) {
        self.write_line(record.level(), format_line(record).as_str());
    }

    fn flush(&self) {
        LineSink::flush(self);
    }
}

impl LineSink for FileSink {
    fn write_line(&self, level: Level, line: &str) {
        self.append(line.as_bytes(), level == Level::Error);
    }

    fn flush(&self) {
        self.buffer.with(|buffer| write_out(&self.file, buffer));
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        let buffer = self.buffer.get_mut();
        write_out(&self.file, buffer);
    }
}
