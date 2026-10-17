//! The packager: a build's cooked units into one `.mpak` (`marlin_asset::package`).

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use marlin_asset::cooked::{UnitView, platform_id};
use marlin_asset::package::{
    BLOB_FILE_ALIGN, BlobRecord, CHUNK_FILE_ALIGN, ChunkRecord, MAX_CHUNK, ObjectRecord,
    PackageHeader, UnitRecord, write,
};
use marlin_compress::{Codec, Level};
use marlin_hash::xxh3_64;

use crate::ddc::Ddc;
use crate::manifest::Manifest;
use crate::platform::Platform;

#[derive(Clone, Copy, Debug)]
pub struct PackageOptions {
    /// Codec of unit heads (tables and payloads), decoded on the CPU.
    pub head_codec: Codec,
    /// Codec of bulk blobs (texture mips, mesh pages): GDeflate where the GPU decompresses.
    pub bulk_codec: Codec,
    pub level: Level,
    /// Raw bytes per chunk, at most [`MAX_CHUNK`].
    pub chunk_size: u32,
    /// Compression threads; 0: one per logical CPU.
    pub threads: usize,
}

impl PackageOptions {
    /// The defaults for packaging for `platform`: its codecs.
    pub fn for_platform(platform: &Platform) -> Self {
        Self {
            head_codec: platform.head_codec,
            bulk_codec: platform.bulk_codec,
            level: Level::DEFAULT,
            chunk_size: MAX_CHUNK,
            threads: 0,
        }
    }
}

impl Default for PackageOptions {
    /// For the host platform.
    fn default() -> Self {
        Self::for_platform(Platform::host())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PackageReport {
    pub units: usize,
    pub chunks: usize,
    pub raw_bytes: u64,
    /// Compressed bytes in the file (chunks only).
    pub stored_bytes: u64,
    pub file_bytes: u64,
}

#[derive(Debug, marlin_error::Error)]
pub enum PackageWriteError {
    #[error("unit {path} is not in the cache; cook first")]
    Missing { path: String },
    #[error("unit {path} is damaged in the cache")]
    Damaged { path: String },
    #[error("compressing {path}: {message}")]
    Compress { path: String, message: String },
    #[error("writing the package")]
    Io(#[from] std::io::Error),
}

/// One chunk to compress: unit, unit offset, raw bytes range, whether it starts a blob.
struct Job<'a> {
    unit: usize,
    unit_offset: u64,
    raw: &'a [u8],
    codec: Codec,
    blob_start: bool,
}

/// Writes every unit of `manifest` (in asset path order) from `ddc` into a package at `out`.
pub fn write_package(
    manifest: &Manifest,
    ddc: &dyn Ddc,
    options: &PackageOptions,
    out: &Path,
) -> Result<PackageReport, PackageWriteError> {
    let chunk_size = options.chunk_size.clamp(4096, MAX_CHUNK) as usize;
    let mut order: Vec<&crate::manifest::ManifestUnit> = manifest.units.iter().collect();
    order.sort_by(|a, b| a.path.cmp(&b.path));
    let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(order.len());
    for u in &order {
        let data = ddc.get(u.key)?.ok_or_else(|| PackageWriteError::Missing {
            path: u.path.clone(),
        })?;
        bytes.push(data);
    }

    // Cut every unit into chunk jobs: the head, then each blob.
    let mut jobs: Vec<Job<'_>> = Vec::new();
    // Per unit: the blob ranges (unit offset, size) and the index of the first job of each.
    let mut blob_ranges: Vec<Vec<(u64, u64)>> = Vec::with_capacity(order.len());
    for (i, data) in bytes.iter().enumerate() {
        let view = UnitView::new(data).map_err(|_| PackageWriteError::Damaged {
            path: order[i].path.clone(),
        })?;
        let h = *view.header();
        let head_end = (h.payload_offset + h.payload_size) as usize;
        for start in (0..head_end).step_by(chunk_size) {
            jobs.push(Job {
                unit: i,
                unit_offset: start as u64,
                raw: &data[start..(start + chunk_size).min(head_end)],
                codec: options.head_codec,
                blob_start: false,
            });
        }
        let mut ranges = Vec::with_capacity(view.bulk_count());
        for b in 0..view.bulk_count() {
            let entry = view.bulk(b).map_err(|_| PackageWriteError::Damaged {
                path: order[i].path.clone(),
            })?;
            let start = (h.bulk_offset + entry.offset) as usize;
            let end = start + entry.size as usize;
            ranges.push((start as u64, entry.size));
            for (k, at) in (start..end).step_by(chunk_size).enumerate() {
                jobs.push(Job {
                    unit: i,
                    unit_offset: at as u64,
                    raw: &data[at..(at + chunk_size).min(end)],
                    codec: options.bulk_codec,
                    blob_start: k == 0,
                });
            }
        }
        blob_ranges.push(ranges);
    }

    // Compress in parallel; keep the stored form when compression does not pay.
    let threads = match options.threads {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
        n => n,
    }
    .min(jobs.len().max(1));
    let results: Vec<OnceLock<Compressed>> = (0..jobs.len()).map(|_| OnceLock::new()).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else { break };
                    let _ = results[i].set(compress(job.codec, options.level, job.raw));
                }
            });
        }
    });

    // Lay out: header, chunks, index.
    let mut file = BufWriter::new(File::create(out)?);
    file.write_all(&[0u8; PackageHeader::SIZE])?;
    let mut at = PackageHeader::SIZE as u64;
    let mut chunks: Vec<ChunkRecord> = Vec::with_capacity(jobs.len());
    let mut unit_first_chunk = vec![u32::MAX; order.len()];
    let mut blob_first_chunks: Vec<Vec<u32>> = vec![Vec::new(); order.len()];
    let mut report = PackageReport::default();
    for (job, result) in jobs.iter().zip(results) {
        let (codec, stored) = result
            .into_inner()
            .expect("every job ran")
            .map_err(|message| PackageWriteError::Compress {
                path: order[job.unit].path.clone(),
                message,
            })?;
        let align = if job.blob_start {
            BLOB_FILE_ALIGN
        } else {
            CHUNK_FILE_ALIGN
        };
        let aligned = at.next_multiple_of(align);
        file.write_all(&vec![0u8; (aligned - at) as usize])?;
        at = aligned;
        let index = chunks.len() as u32;
        if unit_first_chunk[job.unit] == u32::MAX {
            unit_first_chunk[job.unit] = index;
        }
        if job.blob_start {
            blob_first_chunks[job.unit].push(index);
        }
        chunks.push(ChunkRecord {
            file_offset: at,
            unit_offset: job.unit_offset,
            stored_size: stored.len() as u32,
            raw_size: job.raw.len() as u32,
            codec,
            checksum: xxh3_64(&stored) as u32,
        });
        file.write_all(&stored)?;
        at += stored.len() as u64;
        report.raw_bytes += job.raw.len() as u64;
        report.stored_bytes += stored.len() as u64;
    }

    // Jobs are in unit order, so each unit's chunks are contiguous.
    let mut chunks_per_unit = vec![0usize; order.len()];
    for job in &jobs {
        chunks_per_unit[job.unit] += 1;
    }
    let mut units = Vec::with_capacity(order.len());
    let mut blobs = Vec::new();
    for (i, u) in order.iter().enumerate() {
        let first_chunk = unit_first_chunk[i];
        let chunk_count = chunks_per_unit[i];
        let first_blob = blobs.len() as u32;
        let ranges = &blob_ranges[i];
        for (b, &(unit_offset, size)) in ranges.iter().enumerate() {
            let first = blob_first_chunks[i][b];
            let end = blob_first_chunks[i]
                .get(b + 1)
                .copied()
                .unwrap_or(first_chunk + chunk_count as u32);
            blobs.push(BlobRecord {
                unit_offset,
                size,
                first_chunk: first,
                chunk_count: end - first,
            });
        }
        units.push(UnitRecord {
            unit: u.unit,
            size: bytes[i].len() as u64,
            first_chunk,
            chunk_count: chunk_count as u32,
            first_blob,
            blob_count: ranges.len() as u32,
        });
    }
    units.sort_by_key(|u| u.unit);
    let objects: Vec<ObjectRecord> = manifest
        .objects
        .iter()
        .map(|&(uuid, unit)| ObjectRecord { uuid, unit })
        .collect();

    let index = write::index(&units, &blobs, &chunks, &objects);
    let index_offset = at.next_multiple_of(CHUNK_FILE_ALIGN);
    file.write_all(&vec![0u8; (index_offset - at) as usize])?;
    file.write_all(&index)?;
    let header = PackageHeader {
        platform: platform_id(&manifest.platform),
        unit_count: units.len() as u32,
        blob_count: blobs.len() as u32,
        chunk_count: chunks.len() as u32,
        object_count: objects.len() as u32,
        build: manifest.key(),
        index_offset,
        index_size: index.len() as u64,
    };
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&write::header(&header))?;
    file.flush()?;
    report.units = units.len();
    report.chunks = chunks.len();
    report.file_bytes = index_offset + index.len() as u64;
    Ok(report)
}

/// A chunk as stored, with the codec that stored it.
type Compressed = Result<(Codec, Vec<u8>), String>;

fn compress(codec: Codec, level: Level, raw: &[u8]) -> Compressed {
    if codec == Codec::None {
        return Ok((Codec::None, raw.to_vec()));
    }
    let mut out = Vec::with_capacity(marlin_compress::max_compressed_size(codec, raw.len()));
    marlin_compress::compress(codec, level, raw, &mut out).map_err(|e| e.to_string())?;
    // Under 3% saved is not worth the decode.
    if out.len() as u64 * 100 >= raw.len() as u64 * 97 {
        return Ok((Codec::None, raw.to_vec()));
    }
    Ok((codec, out))
}
