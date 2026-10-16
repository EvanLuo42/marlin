//! Frame marks: draining the thread buffers, pairing begins with ends, and the frame ring.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use marlin_containers::{HashMap, HashMapExt};
use marlin_std::sync::Mutex;
use marlin_std::time::Instant;

use crate::gpu::{GpuTimings, GpuZone};
use crate::record::{self, Event};
use crate::{ENABLED, ZoneSite};

const DEFAULT_HISTORY: usize = 128;
/// Frame marks an end waits for its begin, which may sit in a buffer drained before the end's.
const ORPHAN_FRAMES: u64 = 2;

pub(crate) fn ticks_to_duration(ticks: u64) -> Duration {
    Instant::from_ticks(ticks).duration_since(Instant::from_ticks(0))
}

/// A completed zone. Times are `marlin_std::time` ticks ([`Instant::from_ticks`]).
#[derive(Clone, Copy, Debug)]
pub struct ZoneRecord {
    site: &'static ZoneSite,
    id: u64,
    thread: u32,
    end_thread: u32,
    start: u64,
    end: u64,
    /// Positions in the begin and end threads' buffers; exact order where ticks tie.
    begin_seq: u64,
    end_seq: u64,
    parent: Option<u32>,
    suspended: bool,
}

impl ZoneRecord {
    pub fn name(&self) -> &'static str {
        self.site.name
    }

    pub fn site(&self) -> &'static ZoneSite {
        self.site
    }

    /// Unique for the process; pairs the begin with the end.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The thread the zone began on, which it belongs to; see [`History::thread`].
    pub fn thread(&self) -> u32 {
        self.thread
    }

    /// The thread the zone ended on.
    pub fn end_thread(&self) -> u32 {
        self.end_thread
    }

    pub fn start_ticks(&self) -> u64 {
        self.start
    }

    pub fn end_ticks(&self) -> u64 {
        self.end
    }

    pub fn duration(&self) -> Duration {
        ticks_to_duration(self.end.saturating_sub(self.start))
    }

    /// Index in [`Frame::zones`] of the innermost zone of the same thread containing this one.
    pub fn parent(&self) -> Option<usize> {
        self.parent.map(|p| p as usize)
    }

    /// Ended on another thread, or overlaps a zone of its thread without nesting: the job it ran
    /// in waited inside it. See the crate docs.
    pub fn suspended(&self) -> bool {
        self.suspended
    }
}

/// One frame: the zones whose ends were collected by its [`frame_mark`].
#[derive(Debug, Default)]
pub struct Frame {
    index: u64,
    start: u64,
    end: u64,
    zones: Vec<ZoneRecord>,
    pub(crate) gpu: Vec<GpuZone>,
}

impl Frame {
    /// Counts frame marks since the process started.
    pub fn index(&self) -> u64 {
        self.index
    }

    /// The previous frame mark (or the first zone, for the first frame).
    pub fn start_ticks(&self) -> u64 {
        self.start
    }

    /// This frame's mark.
    pub fn end_ticks(&self) -> u64 {
        self.end
    }

    pub fn duration(&self) -> Duration {
        ticks_to_duration(self.end.saturating_sub(self.start))
    }

    /// Sorted by thread, then begin order; parents come before their children.
    pub fn zones(&self) -> &[ZoneRecord] {
        &self.zones
    }

    /// GPU zones the [`GpuTimings`] source attributed to this frame so far.
    pub fn gpu_zones(&self) -> &[GpuZone] {
        &self.gpu
    }

    /// Sorts the zones and works out parents and suspended zones, one thread at a time.
    fn finalize(&mut self, active: &mut Vec<usize>) {
        // Whether `a`, begun earlier on `z`'s thread, is still open at `z`'s begin, and whether it
        // contains `z`. Exact by buffer position for zones that ended on that thread; zones that
        // ended elsewhere can only be compared by ticks.
        fn open_at(a: &ZoneRecord, z: &ZoneRecord) -> bool {
            if a.end_thread == a.thread {
                a.end_seq > z.begin_seq
            } else {
                a.end >= z.start
            }
        }
        fn contains(a: &ZoneRecord, z: &ZoneRecord) -> bool {
            if a.end_thread == a.thread && z.end_thread == z.thread {
                a.end_seq > z.end_seq
            } else {
                a.end >= z.end
            }
        }

        self.zones.sort_unstable_by_key(|z| (z.thread, z.begin_seq));
        let zones = &mut self.zones[..];
        let mut lo = 0;
        while lo < zones.len() {
            let thread = zones[lo].thread;
            let hi = lo + zones[lo..].partition_point(|z| z.thread == thread);
            // `active`: earlier zones still open at the current one's begin. A zone that ended on
            // this thread and overlaps another without nesting means both waited, so both are
            // suspended. The end of a zone that moved away says nothing about this thread.
            active.clear();
            for i in lo..hi {
                active.retain(|&a| open_at(&zones[a], &zones[i]));
                for &a in active.iter() {
                    if zones[a].end_thread == thread && !contains(&zones[a], &zones[i]) {
                        zones[a].suspended = true;
                        zones[i].suspended = true;
                    }
                }
                if zones[i].end_thread != thread {
                    zones[i].suspended = true;
                }
                active.push(i);
            }
            // The non-suspended open zones now nest, so the last containing one is the parent.
            active.clear();
            for i in lo..hi {
                active.retain(|&a| open_at(&zones[a], &zones[i]));
                zones[i].parent = active
                    .iter()
                    .rev()
                    .find(|&&a| !zones[a].suspended && contains(&zones[a], &zones[i]))
                    .map(|&a| a as u32);
                active.push(i);
            }
            lo = hi;
        }
    }
}

/// A thread that recorded zones.
#[derive(Clone, Debug, Default)]
pub struct ThreadInfo {
    os_id: u64,
    name: Option<String>,
}

impl ThreadInfo {
    /// The OS thread id, as shown by debuggers (and as `tid` in traces).
    pub fn os_id(&self) -> u64 {
        self.os_id
    }

    /// Set by [`set_thread_name`](crate::set_thread_name).
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

struct Open {
    site: &'static ZoneSite,
    thread: u32,
    start: u64,
    seq: u64,
    frame: u64,
}

struct Orphan {
    id: u64,
    thread: u32,
    ticks: u64,
    seq: u64,
    frame: u64,
}

pub(crate) struct Collector {
    frames: VecDeque<Frame>,
    history: usize,
    next_index: u64,
    /// The last mark; 0 before the first.
    frame_start: u64,
    /// Begins waiting for their end, by zone id.
    open: Option<HashMap<u64, Open>>,
    /// Ends waiting for their begin.
    orphans: Vec<Orphan>,
    staging: Vec<(u32, u64, Event)>,
    active: Vec<usize>,
    /// By thread record; record 0 is unused.
    threads: Vec<ThreadInfo>,
    dropped: u64,
    pub(crate) gpu: Option<&'static dyn GpuTimings>,
}

static COLLECTOR: Mutex<Collector> = Mutex::new(Collector {
    frames: VecDeque::new(),
    history: DEFAULT_HISTORY,
    next_index: 0,
    frame_start: 0,
    open: None,
    orphans: Vec::new(),
    staging: Vec::new(),
    active: Vec::new(),
    threads: Vec::new(),
    dropped: 0,
    gpu: None,
});

/// Mirrors `Collector::next_index` for lock-free reads.
static FRAME_INDEX: AtomicU64 = AtomicU64::new(0);

fn thread_entry(threads: &mut Vec<ThreadInfo>, thread: u32, os_id: u64) -> &mut ThreadInfo {
    let i = thread as usize;
    if threads.len() <= i {
        threads.resize_with(i + 1, ThreadInfo::default);
    }
    let info = &mut threads[i];
    info.os_id = os_id;
    info
}

pub(crate) fn name_thread(thread: u32, os_id: u64, name: String) {
    thread_entry(&mut COLLECTOR.lock().threads, thread, os_id).name = Some(name);
}

pub(crate) fn set_gpu(source: Option<&'static dyn GpuTimings>) {
    COLLECTOR.lock().gpu = source;
}

impl Collector {
    /// Drains every buffer into `staging`.
    fn drain(&mut self, limit: u64) {
        self.staging.clear();
        let (staging, threads, dropped) = (&mut self.staging, &mut self.threads, &mut self.dropped);
        // SAFETY: we hold the collector's mutex, so we are the only consumer.
        unsafe {
            record::drain_all(
                limit,
                |d| {
                    thread_entry(threads, d.thread, d.os_id);
                    *dropped += d.dropped;
                },
                |thread, seq, event| staging.push((thread, seq, event)),
            );
        }
    }

    fn mark(&mut self, now: u64) {
        self.drain(now);
        let index = self.next_index;
        self.next_index += 1;
        FRAME_INDEX.store(self.next_index, Ordering::Relaxed);

        let mut frame = if self.frames.len() >= self.history {
            self.frames.pop_front().unwrap_or_default()
        } else {
            Frame::default()
        };
        frame.index = index;
        frame.zones.clear();
        frame.gpu.clear();

        // Begins first: within one drain an end can be seen before its begin.
        let open = self.open.get_or_insert_with(HashMap::new);
        for &(thread, seq, event) in &self.staging {
            if let Some(site) = event.site {
                let start = event.ticks;
                open.insert(
                    event.id,
                    Open {
                        site,
                        thread,
                        start,
                        seq,
                        frame: index,
                    },
                );
            }
        }
        let complete = |zones: &mut Vec<ZoneRecord>, begin: Open, end: &Orphan| {
            zones.push(ZoneRecord {
                site: begin.site,
                id: end.id,
                thread: begin.thread,
                end_thread: end.thread,
                start: begin.start,
                end: end.ticks,
                begin_seq: begin.seq,
                end_seq: end.seq,
                parent: None,
                suspended: false,
            });
        };
        self.orphans.retain(|end| match open.remove(&end.id) {
            Some(begin) => {
                complete(&mut frame.zones, begin, end);
                false
            }
            None => index - end.frame < ORPHAN_FRAMES,
        });
        for &(thread, seq, event) in &self.staging {
            if event.site.is_some() {
                continue;
            }
            let end = Orphan {
                id: event.id,
                thread,
                ticks: event.ticks,
                seq,
                frame: index,
            };
            match open.remove(&event.id) {
                Some(begin) => complete(&mut frame.zones, begin, &end),
                None => self.orphans.push(end),
            }
        }
        // An end that never arrives (dropped, or a leaked guard) must not pin its begin forever.
        let history = self.history as u64;
        open.retain(|_, o| index - o.frame < history);

        frame.start = match self.frame_start {
            0 => frame.zones.iter().map(|z| z.start).min().unwrap_or(now),
            start => start,
        };
        frame.end = now;
        self.frame_start = now;
        frame.finalize(&mut self.active);
        self.frames.push_back(frame);

        if let Some(source) = self.gpu {
            let frames = &mut self.frames;
            source.collect(&mut |zone| {
                let first = frames.front().map_or(0, |f| f.index);
                if let Some(frame) = zone
                    .frame
                    .checked_sub(first)
                    .and_then(|i| frames.get_mut(i as usize))
                {
                    frame.gpu.push(zone);
                }
            });
        }
    }
}

/// Ends the current frame and starts the next. Call once per frame, from one thread (usually the
/// main loop's); it drains every thread's buffer, so it costs a lock and work proportional to
/// the zones recorded since the last mark, but allocates nothing once warmed up.
///
/// Calls the [`GpuTimings`] source, if any, under the profiler's lock.
pub fn frame_mark() {
    if !ENABLED {
        return;
    }
    let now = Instant::now().as_ticks();
    COLLECTOR.lock().mark(now);
}

/// The index the next [`frame_mark`] gives its frame, for tagging GPU work. Lock-free.
pub fn frame_index() -> u64 {
    FRAME_INDEX.load(Ordering::Relaxed)
}

/// How many frames [`with_history`] keeps.
pub fn history_len() -> usize {
    COLLECTOR.lock().history
}

/// Keeps the last `frames` frames (at least 1), dropping older ones now if needed.
pub fn set_history_len(frames: usize) {
    let mut c = COLLECTOR.lock();
    c.history = frames.max(1);
    while c.frames.len() > c.history {
        c.frames.pop_front();
    }
}

/// Discards the history, open zones, everything recorded so far and the dropped-event count.
/// Thread names are kept.
pub fn clear() {
    let mut c = COLLECTOR.lock();
    let now = Instant::now().as_ticks();
    c.drain(now);
    c.staging.clear();
    c.frames.clear();
    c.orphans.clear();
    if let Some(open) = &mut c.open {
        open.clear();
    }
    c.dropped = 0;
    c.frame_start = now;
}

/// Runs `f` on the frame history, under the profiler's lock: [`frame_mark`] and
/// [`set_thread_name`](crate::set_thread_name) wait meanwhile, recording does not.
pub fn with_history<R>(f: impl FnOnce(&History<'_>) -> R) -> R {
    let c = COLLECTOR.lock();
    f(&History { c: &c })
}

/// The last [`history_len`] frames; see [`with_history`].
pub struct History<'a> {
    pub(crate) c: &'a Collector,
}

impl History<'_> {
    /// Oldest first.
    pub fn frames(&self) -> impl DoubleEndedIterator<Item = &Frame> + ExactSizeIterator {
        self.c.frames.iter()
    }

    pub fn last(&self) -> Option<&Frame> {
        self.c.frames.back()
    }

    pub fn frame(&self, index: u64) -> Option<&Frame> {
        let first = self.c.frames.front()?.index;
        self.c.frames.get(index.checked_sub(first)? as usize)
    }

    /// A thread by record, as in [`ZoneRecord::thread`].
    pub fn thread(&self, thread: u32) -> Option<&ThreadInfo> {
        self.c.threads.get(thread as usize).filter(|t| t.os_id != 0)
    }

    /// Events lost to full thread buffers since the last [`clear`].
    pub fn dropped_events(&self) -> u64 {
        self.c.dropped
    }

    pub(crate) fn threads(&self) -> &[ThreadInfo] {
        &self.c.threads
    }
}
