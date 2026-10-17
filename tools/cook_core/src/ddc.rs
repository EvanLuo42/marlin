//! The derived data cache: cooked results by 128-bit content key.
//!
//! Keys hash every input of a result, so an entry never goes stale: a changed input is a new key.
//! [`Ddc`] is the interface (the shared network store plugs in behind it later); [`LocalDdc`] is
//! the per-machine cache.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use marlin_hash::xxh3_64;

/// A content-addressed store of cooked data.
pub trait Ddc: Send + Sync {
    fn get(&self, key: u128) -> io::Result<Option<Vec<u8>>>;
    fn contains(&self, key: u128) -> bool;
    /// Stores `data` under `key`. Storing an existing key again is a no-op.
    fn put(&self, key: u128, data: &[u8]) -> io::Result<()>;
}

const RECORD_MAGIC: u32 = u32::from_le_bytes(*b"DDC1");
/// magic u32, key u128, length u64, checksum u64.
const RECORD_HEADER: u64 = 4 + 16 + 8 + 8;
/// A new pack file starts beyond this size.
const PACK_LIMIT: u64 = 4 << 30;

/// Where a record's data lies.
#[derive(Clone, Copy)]
struct Location {
    pack: u32,
    offset: u64,
    len: u64,
    checksum: u64,
}

/// The local cache: append-only pack files of records, indexed in memory.
///
/// Millions of small cooked results as individual files would make the file system the
/// bottleneck (opening a file costs far more than reading it); pack files keep it to one open
/// file per pack. Opening scans the record headers to rebuild the index; a record cut off by a
/// crash is dropped by truncating the pack there. Every read verifies its checksum.
///
/// Several processes may share one directory (the cook service and command-line cooks both use
/// `<project>/.marlin/ddc`). Every change to the pack files — the repair on open and each append
/// — happens under an OS lock on `<dir>/lock`, and an append first catches up with the records
/// other processes appended since, so it writes at the true end and skips keys already stored.
/// Records another process adds are otherwise unseen until then: a miss, never a wrong result.
pub struct LocalDdc {
    dir: PathBuf,
    index: RwLock<HashMap<u128, Location>>,
    packs: RwLock<Vec<File>>,
    /// The pack being appended to and the end of its records known to this process.
    writer: Mutex<(u32, u64)>,
    /// Locked around changes to the packs; shared with the other processes using `dir`.
    lock: File,
}

fn pack_path(dir: &Path, pack: u32) -> PathBuf {
    dir.join(format!("pack-{pack:04}.ddc"))
}

fn open_pack(dir: &Path, pack: u32) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(pack_path(dir, pack))
}

/// Holds the directory's lock file locked until dropped.
struct DirLock<'a>(&'a File);

impl<'a> DirLock<'a> {
    fn take(file: &'a File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for DirLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Reads every whole record of a pack from `start` (a record boundary); returns the end of the
/// valid records. Bytes beyond are a record cut off by a crash: only call this holding the
/// directory lock, so they are not one another process is still writing.
fn scan(
    file: &File,
    pack: u32,
    start: u64,
    index: &mut HashMap<u128, Location>,
) -> io::Result<u64> {
    let len = file.metadata()?.len();
    let mut at = start;
    let mut header = [0u8; RECORD_HEADER as usize];
    while at + RECORD_HEADER <= len {
        read_at(file, at, &mut header)?;
        let magic = u32::from_le_bytes(header[0..4].try_into().unwrap_or_default());
        let key = u128::from_le_bytes(header[4..20].try_into().unwrap_or_default());
        let size = u64::from_le_bytes(header[20..28].try_into().unwrap_or_default());
        let checksum = u64::from_le_bytes(header[28..36].try_into().unwrap_or_default());
        let end = at + RECORD_HEADER + size;
        if magic != RECORD_MAGIC || end > len {
            break;
        }
        index.insert(
            key,
            Location {
                pack,
                offset: at + RECORD_HEADER,
                len: size,
                checksum,
            },
        );
        at = end;
    }
    Ok(at)
}

fn read_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
}

/// Positional, like [`read_at`]: never moves a cursor a concurrent reader could move too.
fn write_at(file: &File, offset: u64, buf: &[u8]) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = file.seek_write(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, offset)
    }
}

impl LocalDdc {
    /// Opens (creating) the cache in `dir`.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        let mut index = HashMap::new();
        let mut packs = Vec::new();
        let mut writer = (0, 0);
        {
            let _locked = DirLock::take(&lock)?;
            packs.push(open_pack(&dir, 0)?);
            Self::catch_up(&dir, &mut packs, &mut writer, &mut index)?;
        }
        Ok(Self {
            dir,
            index: RwLock::new(index),
            packs: RwLock::new(packs),
            writer: Mutex::new(writer),
            lock,
        })
    }

    /// Reads the records appended to the packs after `writer` (by this process before it opened
    /// them, or by others since), opening packs started meanwhile, and drops a record cut off by
    /// a crash at the end. Call holding the directory lock; `packs` ends at `writer.0`.
    fn catch_up(
        dir: &Path,
        packs: &mut Vec<File>,
        writer: &mut (u32, u64),
        index: &mut HashMap<u128, Location>,
    ) -> io::Result<()> {
        loop {
            let (pack, start) = *writer;
            let file = &packs[pack as usize];
            let valid = scan(file, pack, start, index)?;
            if valid < file.metadata()?.len() {
                file.set_len(valid)?;
            }
            *writer = (pack, valid);
            if !pack_path(dir, pack + 1).exists() {
                return Ok(());
            }
            packs.push(open_pack(dir, pack + 1)?);
            *writer = (pack + 1, 0);
        }
    }

    pub fn len(&self) -> usize {
        self.index.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total bytes in pack files.
    pub fn size(&self) -> u64 {
        let packs = self.packs.read().unwrap_or_else(|e| e.into_inner());
        packs
            .iter()
            .filter_map(|f| f.metadata().ok())
            .map(|m| m.len())
            .sum()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Ddc for LocalDdc {
    fn get(&self, key: u128) -> io::Result<Option<Vec<u8>>> {
        let Some(location) = self
            .index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .copied()
        else {
            return Ok(None);
        };
        let mut data = vec![0; location.len as usize];
        {
            let packs = self.packs.read().unwrap_or_else(|e| e.into_inner());
            let file = packs
                .get(location.pack as usize)
                .ok_or_else(|| io::Error::other("ddc pack missing"))?;
            read_at(file, location.offset, &mut data)?;
        }
        if xxh3_64(&data) != location.checksum {
            // Damaged on disk: drop the entry so it is cooked again.
            self.index
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
            return Ok(None);
        }
        Ok(Some(data))
    }

    fn contains(&self, key: u128) -> bool {
        self.index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key)
    }

    fn put(&self, key: u128, data: &[u8]) -> io::Result<()> {
        if self.contains(key) {
            return Ok(());
        }
        let checksum = xxh3_64(data);
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _locked = DirLock::take(&self.lock)?;
        {
            let mut found = HashMap::new();
            let mut packs = self.packs.write().unwrap_or_else(|e| e.into_inner());
            Self::catch_up(&self.dir, &mut packs, &mut writer, &mut found)?;
            self.index
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .extend(found);
        }
        if self.contains(key) {
            return Ok(());
        }
        if writer.1 > 0 && writer.1 + RECORD_HEADER + data.len() as u64 > PACK_LIMIT {
            // Caught up: no pack beyond this one exists.
            let pack = writer.0 + 1;
            let file = open_pack(&self.dir, pack)?;
            self.packs
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push(file);
            *writer = (pack, 0);
        }
        let (pack, offset) = *writer;
        let mut record = Vec::with_capacity(RECORD_HEADER as usize + data.len());
        record.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        record.extend_from_slice(&key.to_le_bytes());
        record.extend_from_slice(&(data.len() as u64).to_le_bytes());
        record.extend_from_slice(&checksum.to_le_bytes());
        record.extend_from_slice(data);
        {
            let packs = self.packs.read().unwrap_or_else(|e| e.into_inner());
            write_at(&packs[pack as usize], offset, &record)?;
        }
        writer.1 += record.len() as u64;
        self.index
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                key,
                Location {
                    pack,
                    offset: offset + RECORD_HEADER,
                    len: data.len() as u64,
                    checksum,
                },
            );
        Ok(())
    }
}

/// A local cache in front of a shared one: reads try the local cache first and keep what the
/// shared one had; writes go to both. The shared cache is best effort: when it fails, cooking
/// carries on with the local one alone.
pub struct LayeredDdc<L: Ddc, S: Ddc> {
    pub local: L,
    pub shared: S,
}

impl<L: Ddc, S: Ddc> LayeredDdc<L, S> {
    pub fn new(local: L, shared: S) -> Self {
        Self { local, shared }
    }
}

impl<L: Ddc, S: Ddc> Ddc for LayeredDdc<L, S> {
    fn get(&self, key: u128) -> io::Result<Option<Vec<u8>>> {
        if let Some(data) = self.local.get(key)? {
            return Ok(Some(data));
        }
        match self.shared.get(key) {
            Ok(Some(data)) => {
                self.local.put(key, &data)?;
                Ok(Some(data))
            }
            Ok(None) | Err(_) => Ok(None),
        }
    }

    fn contains(&self, key: u128) -> bool {
        self.local.contains(key) || self.shared.contains(key)
    }

    fn put(&self, key: u128, data: &[u8]) -> io::Result<()> {
        self.local.put(key, data)?;
        // Another machine may have stored it meanwhile: keys are content, so that is fine.
        let _ = self.shared.put(key, data);
        Ok(())
    }
}
