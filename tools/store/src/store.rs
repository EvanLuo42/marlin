//! The store's state and request handling.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use marlin_asset::wire::{Frame, Reader, WireError, Writer};
use marlin_cook_core::{Ddc, LocalDdc};
use marlin_hash::xxh3_64;

use crate::protocol::*;
use crate::server::{ConnId, Outbox, Service};

const REF_MAGIC: &[u8; 4] = b"REF1";

/// Named pointers to keys, kept in an append-only log: `REF1 len:u32 name key:u128
/// checksum:u64` per change, the last record of a name winning. A torn last record is dropped on
/// open.
pub struct Refs {
    path: PathBuf,
    file: File,
    refs: HashMap<String, u128>,
}

impl Refs {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let mut refs = HashMap::new();
        let mut bytes = Vec::new();
        if path.exists() {
            File::open(&path)?.read_to_end(&mut bytes)?;
        }
        let mut valid = 0;
        let mut at = 0;
        while at + 8 <= bytes.len() && &bytes[at..at + 4] == REF_MAGIC {
            let len =
                u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap_or_default()) as usize;
            let end = at + 8 + len + 16 + 8;
            if end > bytes.len() {
                break;
            }
            let body = &bytes[at..end - 8];
            let checksum = u64::from_le_bytes(bytes[end - 8..end].try_into().unwrap_or_default());
            let Ok(name) = std::str::from_utf8(&bytes[at + 8..at + 8 + len]) else {
                break;
            };
            if xxh3_64(body) != checksum {
                break;
            }
            let key =
                u128::from_le_bytes(bytes[at + 8 + len..end - 8].try_into().unwrap_or_default());
            refs.insert(name.to_owned(), key);
            at = end;
            valid = end;
        }
        if (valid as u64) < bytes.len() as u64 {
            // An append-only handle may not truncate (Windows): drop the torn tail first.
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(valid as u64)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file, refs })
    }

    pub fn get(&self, name: &str) -> Option<u128> {
        self.refs.get(name).copied()
    }

    pub fn set(&mut self, name: &str, key: u128) -> io::Result<()> {
        let mut record = Vec::with_capacity(8 + name.len() + 24);
        record.extend_from_slice(REF_MAGIC);
        record.extend_from_slice(&(name.len() as u32).to_le_bytes());
        record.extend_from_slice(name.as_bytes());
        record.extend_from_slice(&key.to_le_bytes());
        let checksum = xxh3_64(&record);
        record.extend_from_slice(&checksum.to_le_bytes());
        self.file.write_all(&record)?;
        self.file.sync_data()?;
        self.refs.insert(name.to_owned(), key);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The shared cache: content-addressed entries plus refs.
pub struct Store {
    data: LocalDdc,
    refs: Refs,
}

impl Store {
    /// Opens (creating) a store in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            data: LocalDdc::open(dir.join("data"))?,
            refs: Refs::open(dir.join("refs.log"))?,
        })
    }

    pub fn data(&self) -> &LocalDdc {
        &self.data
    }

    /// Handles one request: the response kind and payload.
    pub fn handle(&mut self, kind: u16, payload: &[u8]) -> (u16, Vec<u8>) {
        match self.try_handle(kind, payload) {
            Ok(response) => response,
            Err(Failure::Wire(e)) => (ERROR, error(&e.to_string())),
            Err(Failure::Io(e)) => (ERROR, error(&e.to_string())),
        }
    }

    fn try_handle(&mut self, kind: u16, payload: &[u8]) -> Result<(u16, Vec<u8>), Failure> {
        let mut r = Reader::new(payload);
        Ok(match kind {
            GET => {
                let key = r.u128()?;
                r.end()?;
                match self.data.get(key)? {
                    Some(data) => (OK, data),
                    None => (MISSING, Vec::new()),
                }
            }
            HAS => {
                let key = r.u128()?;
                r.end()?;
                (
                    if self.data.contains(key) { OK } else { MISSING },
                    Vec::new(),
                )
            }
            PUT => {
                let key = r.u128()?;
                self.data.put(key, r.rest())?;
                (OK, Vec::new())
            }
            BATCH => {
                let items = read_batch(payload)?;
                let mut out = Vec::with_capacity(items.len());
                for (kind, payload) in items {
                    if kind == BATCH {
                        out.push((ERROR, error("batches do not nest")));
                    } else {
                        out.push(self.handle(kind, &payload));
                    }
                }
                (OK, batch(&out))
            }
            REF_GET => {
                let name = r.str()?;
                r.end()?;
                match self.refs.get(&name) {
                    Some(key) => (OK, crate::protocol::key(key)),
                    None => (MISSING, Vec::new()),
                }
            }
            REF_SET => {
                let name = r.str()?;
                let key = r.u128()?;
                let expected = if r.remaining() > 0 {
                    Some(r.u128()?)
                } else {
                    None
                };
                r.end()?;
                let current = self.refs.get(&name);
                match expected {
                    Some(e) if current != Some(e) => {
                        let mut w = Writer::new();
                        if let Some(c) = current {
                            w.u128(c);
                        }
                        (CONFLICT, w.finish())
                    }
                    _ => {
                        self.refs.set(&name, key)?;
                        (OK, Vec::new())
                    }
                }
            }
            STATS => {
                let mut w = Writer::new();
                w.u64(self.data.len() as u64)
                    .u64(self.data.size())
                    .u64(self.refs.len() as u64);
                (OK, w.finish())
            }
            other => (ERROR, error(&format!("unknown request {other}"))),
        })
    }
}

fn error(message: &str) -> Vec<u8> {
    let mut w = Writer::new();
    w.str(message);
    w.finish()
}

enum Failure {
    Wire(WireError),
    Io(io::Error),
}

impl From<WireError> for Failure {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl Service for Store {
    fn request(&mut self, conn: ConnId, frame: Frame, out: &mut Outbox) {
        let (kind, payload) = self.handle(frame.kind, &frame.payload);
        out.send(conn, kind, frame.id, &payload);
    }
}
