//! Streamed bulk blobs (0003 §7, 0009 §6.1): blobs marked `BULK_STREAMED` that a source loaded
//! their unit without, read one by one when something wants them (a mesh's finer cluster
//! pages, later a texture's mips).
//!
//! Anyone holding a unit's id may [`request`](BlobStreamer::request) one of its blobs, from any
//! thread, and gets a [`BlobRequest`] to [`take`](BlobRequest::take) the bytes from once they
//! have arrived. The thread that owns the IO drives the reads with
//! [`update`](BlobStreamer::update) once a frame: it starts reads on its own [`IoQueue`] (a
//! package's blob is one file range of whole chunks), takes the completions and decodes them
//! there. A request whose [`BlobRequest`] was dropped before its read started is skipped.

use alloc::sync::Arc;
use alloc::vec::Vec;

use marlin_containers::HashMap;
use marlin_io::{IoBuffer, IoQueue, RequestId};
use marlin_sync::Mutex;

use crate::vfs::{Read, SourceError, UnitBytes, Vfs};

type Result<T> = core::result::Result<T, SourceError>;

#[derive(Default)]
struct Slot {
    result: Mutex<Option<Result<UnitBytes>>>,
}

/// A blob on its way: [`take`](Self::take) its bytes once they have arrived.
pub struct BlobRequest(Arc<Slot>);

impl BlobRequest {
    /// The blob, or why it could not be read; `None` while it is on its way. Taken once.
    pub fn take(&self) -> Option<Result<UnitBytes>> {
        self.0.result.lock().take()
    }
}

struct Pending {
    unit: u64,
    blob: u32,
    slot: Arc<Slot>,
}

#[derive(Default)]
struct State {
    queued: Vec<Pending>,
    reading: HashMap<RequestId, Pending>,
}

/// Reads streamed blobs on request (see the module docs).
#[derive(Default)]
pub struct BlobStreamer {
    state: Mutex<State>,
}

/// What one [`BlobStreamer::update`] did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StreamStats {
    pub reads_started: usize,
    pub blobs_read: usize,
    pub failed: usize,
}

impl BlobStreamer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks for blob `blob` (its index in the unit's bulk table) of `unit`.
    pub fn request(&self, unit: u64, blob: u32) -> BlobRequest {
        let slot = Arc::new(Slot::default());
        self.state.lock().queued.push(Pending {
            unit,
            blob,
            slot: slot.clone(),
        });
        BlobRequest(slot)
    }

    /// Requests not delivered yet: queued or being read.
    pub fn in_flight(&self) -> usize {
        let state = self.state.lock();
        state.queued.len() + state.reading.len()
    }

    /// Starts the queued reads on `io` (up to its capacity; the rest wait for the next call),
    /// takes its completions and decodes them. `io` is the streamer's alone: completions it
    /// does not know are dropped.
    pub fn update<'f>(&self, vfs: &'f Vfs, io: &mut IoQueue<'f>) -> StreamStats {
        let mut stats = StreamStats::default();
        let queued = core::mem::take(&mut self.state.lock().queued);
        let mut waiting = Vec::new();
        for p in queued {
            // Nobody wants it any more.
            if Arc::strong_count(&p.slot) == 1 {
                continue;
            }
            match begin(vfs, p.unit, p.blob) {
                Err(e) => fail(&p, e, &mut stats),
                Ok(Read::Ready(bytes)) => deliver(&p, Ok(bytes), &mut stats),
                Ok(Read::Pending) => waiting.push(p),
                Ok(Read::File { file, offset, len }) => {
                    match io.read(file, IoBuffer::new(len), offset) {
                        Ok(id) => {
                            stats.reads_started += 1;
                            self.state.lock().reading.insert(id, p);
                        }
                        Err(e) if e.error.kind() == marlin_io::ErrorKind::WouldBlock => {
                            waiting.push(p);
                        }
                        Err(e) => fail(&p, e.error.into(), &mut stats),
                    }
                }
            }
        }
        loop {
            let done = match io.poll() {
                Ok(Some(done)) => done,
                Ok(None) => break,
                Err(e) => {
                    // The queue failed: everything in flight with it.
                    let reading = core::mem::take(&mut self.state.lock().reading);
                    for (_, p) in reading {
                        fail(&p, SourceError::Remote(alloc::format!("{e}")), &mut stats);
                    }
                    break;
                }
            };
            let Some(p) = self.state.lock().reading.remove(&done.id) else {
                continue;
            };
            let result = match done.result {
                Ok(n) if n == done.buffer.len() => finish(vfs, p.unit, p.blob, &done.buffer[..n]),
                Ok(n) => Err(SourceError::Remote(alloc::format!(
                    "short read: {n} of {} bytes",
                    done.buffer.len()
                ))),
                Err(e) => Err(SourceError::Io(e)),
            };
            deliver(&p, result, &mut stats);
        }
        self.state.lock().queued.extend(waiting);
        stats
    }

    /// Reads every queued blob now, synchronously: for tools, tests and loading screens.
    pub fn update_blocking(&self, vfs: &Vfs) -> StreamStats {
        let mut stats = StreamStats::default();
        let queued = core::mem::take(&mut self.state.lock().queued);
        for p in queued {
            if Arc::strong_count(&p.slot) == 1 {
                continue;
            }
            let result = match begin(vfs, p.unit, p.blob) {
                Err(e) => Err(e),
                Ok(Read::Ready(bytes)) => Ok(bytes),
                Ok(Read::Pending) => Err(SourceError::NotFound),
                Ok(Read::File { file, offset, len }) => {
                    stats.reads_started += 1;
                    let mut buffer = alloc::vec![0u8; len];
                    file.read_exact_at(&mut buffer, offset)
                        .map_err(SourceError::Io)
                        .and_then(|()| finish(vfs, p.unit, p.blob, &buffer))
                }
            };
            deliver(&p, result, &mut stats);
        }
        stats
    }
}

fn begin(vfs: &Vfs, unit: u64, blob: u32) -> Result<Read<'_>> {
    vfs.source(unit)
        .ok_or(SourceError::NotFound)?
        .begin_blob(unit, blob)
}

fn finish(vfs: &Vfs, unit: u64, blob: u32, read: &[u8]) -> Result<UnitBytes> {
    vfs.source(unit)
        .ok_or(SourceError::NotFound)?
        .finish_blob(unit, blob, read)
}

fn deliver(p: &Pending, result: Result<UnitBytes>, stats: &mut StreamStats) {
    match &result {
        Ok(_) => stats.blobs_read += 1,
        Err(_) => stats.failed += 1,
    }
    *p.slot.result.lock() = Some(result);
}

fn fail(p: &Pending, e: SourceError, stats: &mut StreamStats) {
    deliver(p, Err(e), stats);
}
