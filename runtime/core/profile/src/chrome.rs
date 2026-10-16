//! Chrome trace event JSON export, for Perfetto and `chrome://tracing`.
//!
//! Process 1 holds one track per CPU thread (`tid` = OS thread id) plus a "Frames" track
//! (`tid` 0). Nested zones are complete events (`X`); suspended zones are async slices (`b`/`e`)
//! because they break their thread's stack. GPU zones go to process 2, one track per queue.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::convert::Infallible;
use core::fmt::{self, Write};

use marlin_std::time::ticks_per_second;

use crate::collector::History;

/// Where an export writes its bytes.
pub trait ByteSink {
    type Error;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

impl ByteSink for Vec<u8> {
    type Error = Infallible;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Infallible> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

impl<S: ByteSink + ?Sized> ByteSink for &mut S {
    type Error = S::Error;

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), S::Error> {
        (**self).write_all(bytes)
    }
}

/// Buffers writes so the sink sees few large ones, and keeps the sink's error, which
/// `fmt::Write` cannot carry.
struct Out<'s, S: ByteSink + ?Sized> {
    sink: &'s mut S,
    buf: [u8; 4096],
    len: usize,
    error: Option<S::Error>,
    first: bool,
}

impl<S: ByteSink + ?Sized> Out<'_, S> {
    fn flush(&mut self) -> fmt::Result {
        let len = core::mem::take(&mut self.len);
        if let Err(e) = self.sink.write_all(&self.buf[..len]) {
            self.error = Some(e);
            return Err(fmt::Error);
        }
        Ok(())
    }

    /// Starts the next element of `traceEvents`.
    fn event(&mut self) -> fmt::Result {
        if !core::mem::take(&mut self.first) {
            self.write_str(",\n")?;
        }
        Ok(())
    }
}

impl<S: ByteSink + ?Sized> Write for Out<'_, S> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty() {
            if self.len == self.buf.len() {
                self.flush()?;
            }
            let n = bytes.len().min(self.buf.len() - self.len);
            self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
            self.len += n;
            bytes = &bytes[n..];
        }
        Ok(())
    }
}

/// A JSON string literal.
struct Json<'a>(&'a str);

impl fmt::Display for Json<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;
        let mut rest = self.0;
        while let Some(i) = rest.find(|c: char| c == '"' || c == '\\' || c < ' ') {
            f.write_str(&rest[..i])?;
            match rest.as_bytes()[i] {
                b'"' => f.write_str("\\\"")?,
                b'\\' => f.write_str("\\\\")?,
                b'\n' => f.write_str("\\n")?,
                b'\t' => f.write_str("\\t")?,
                c => write!(f, "\\u{:04x}", c)?,
            }
            rest = &rest[i + 1..];
        }
        f.write_str(rest)?;
        f.write_char('"')
    }
}

const CPU_PID: u32 = 1;
const GPU_PID: u32 = 2;
const FRAMES_TID: u64 = 0;

impl History<'_> {
    /// Writes every frame in the history as Chrome trace event JSON. Times are microseconds
    /// since the start of the oldest frame.
    pub fn write_chrome_trace<S: ByteSink + ?Sized>(&self, sink: &mut S) -> Result<(), S::Error> {
        let mut out = Out {
            sink,
            buf: [0; 4096],
            len: 0,
            error: None,
            first: true,
        };
        match self.write_trace(&mut out).and_then(|()| out.flush()) {
            Ok(()) => Ok(()),
            // Only `Out` fails, and it always keeps the error.
            Err(fmt::Error) => Err(out.error.take().expect("sink error")),
        }
    }

    fn write_trace<S: ByteSink + ?Sized>(&self, out: &mut Out<'_, S>) -> fmt::Result {
        let epoch = self.frames().next().map_or(0, |f| f.start_ticks());
        let scale = 1e6 / ticks_per_second() as f64;
        let us = |ticks: u64| (i128::from(ticks) - i128::from(epoch)) as f64 * scale;
        let dur = |start: u64, end: u64| end.saturating_sub(start) as f64 * scale;
        let os_id = |thread: u32| self.thread(thread).map_or(0, |t| t.os_id());

        out.write_str("{\"displayTimeUnit\":\"ms\",\"traceEvents\":[\n")?;
        metadata(out, CPU_PID, None, "process_name", "marlin")?;
        metadata(out, CPU_PID, Some(FRAMES_TID), "thread_name", "Frames")?;
        for info in self.threads().iter().filter(|t| t.os_id() != 0) {
            if let Some(name) = info.name() {
                metadata(out, CPU_PID, Some(info.os_id()), "thread_name", name)?;
            }
        }

        let mut queues = BTreeSet::new();
        for frame in self.frames() {
            out.event()?;
            write!(
                out,
                "{{\"ph\":\"X\",\"pid\":{CPU_PID},\"tid\":{FRAMES_TID},\"cat\":\"frame\",\
                 \"name\":\"Frame {}\",\"ts\":{:.3},\"dur\":{:.3}}}",
                frame.index(),
                us(frame.start_ticks()),
                dur(frame.start_ticks(), frame.end_ticks()),
            )?;
            for z in frame.zones() {
                let tid = os_id(z.thread());
                let (name, file, line) = (Json(z.name()), Json(z.site().file), z.site().line);
                out.event()?;
                if !z.suspended() {
                    write!(
                        out,
                        "{{\"ph\":\"X\",\"pid\":{CPU_PID},\"tid\":{tid},\"cat\":\"cpu\",\
                         \"name\":{name},\"ts\":{:.3},\"dur\":{:.3},\
                         \"args\":{{\"file\":{file},\"line\":{line}}}}}",
                        us(z.start_ticks()),
                        dur(z.start_ticks(), z.end_ticks()),
                    )?;
                    continue;
                }
                let id = z.id();
                write!(
                    out,
                    "{{\"ph\":\"b\",\"pid\":{CPU_PID},\"tid\":{tid},\"cat\":\"cpu.suspended\",\
                     \"id\":\"{id:#x}\",\"name\":{name},\"ts\":{:.3},\
                     \"args\":{{\"file\":{file},\"line\":{line},\"end_tid\":{}}}}}",
                    us(z.start_ticks()),
                    os_id(z.end_thread()),
                )?;
                out.event()?;
                write!(
                    out,
                    "{{\"ph\":\"e\",\"pid\":{CPU_PID},\"tid\":{tid},\"cat\":\"cpu.suspended\",\
                     \"id\":\"{id:#x}\",\"name\":{name},\"ts\":{:.3}}}",
                    us(z.end_ticks()),
                )?;
            }
            for z in frame.gpu_zones() {
                queues.insert(z.queue);
                out.event()?;
                write!(
                    out,
                    "{{\"ph\":\"X\",\"pid\":{GPU_PID},\"tid\":{},\"cat\":\"gpu\",\"name\":{},\
                     \"ts\":{:.3},\"dur\":{:.3},\"args\":{{\"frame\":{}}}}}",
                    z.queue,
                    Json(z.name),
                    us(z.start),
                    dur(z.start, z.end),
                    z.frame,
                )?;
            }
        }
        if !queues.is_empty() {
            metadata(out, GPU_PID, None, "process_name", "GPU")?;
            for queue in queues {
                let name = self.c.gpu.map_or("GPU", |g| g.queue_name(queue));
                metadata(out, GPU_PID, Some(u64::from(queue)), "thread_name", name)?;
            }
        }
        out.write_str("\n]}\n")
    }
}

fn metadata<S: ByteSink + ?Sized>(
    out: &mut Out<'_, S>,
    pid: u32,
    tid: Option<u64>,
    kind: &str,
    name: &str,
) -> fmt::Result {
    out.event()?;
    write!(out, "{{\"ph\":\"M\",\"pid\":{pid},")?;
    if let Some(tid) = tid {
        write!(out, "\"tid\":{tid},")?;
    }
    write!(
        out,
        "\"name\":\"{kind}\",\"args\":{{\"name\":{}}}}}",
        Json(name)
    )
}
