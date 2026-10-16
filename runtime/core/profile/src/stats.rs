//! Per-zone statistics and per-frame call trees.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::time::Duration;

use crate::collector::{Frame, History, ticks_to_duration};

/// Minimum, mean and maximum of a set of durations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub min: Duration,
    pub avg: Duration,
    pub max: Duration,
}

/// Statistics of every zone with one name over the frames in the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneStats {
    pub name: &'static str,
    pub calls: u64,
    /// Frames in which the zone occurred.
    pub frames: u32,
    pub per_call: Summary,
    /// Total time per frame, over the frames in which the zone occurred.
    pub per_frame: Summary,
}

#[derive(Default)]
struct Acc {
    calls: u64,
    call_min: u64,
    call_max: u64,
    sum: u64,
    frames: u32,
    frame_min: u64,
    frame_max: u64,
}

/// `frames` yields, per frame, `(name, ticks)` per zone.
fn accumulate<F, Z>(frames: F) -> Vec<ZoneStats>
where
    F: Iterator<Item = Z>,
    Z: Iterator<Item = (&'static str, u64)>,
{
    let mut all = BTreeMap::<&'static str, Acc>::new();
    let mut frame = BTreeMap::<&'static str, u64>::new();
    for zones in frames {
        frame.clear();
        for (name, ticks) in zones {
            *frame.entry(name).or_default() += ticks;
            let acc = all.entry(name).or_default();
            if acc.calls == 0 {
                acc.call_min = ticks;
            }
            acc.calls += 1;
            acc.call_min = acc.call_min.min(ticks);
            acc.call_max = acc.call_max.max(ticks);
            acc.sum += ticks;
        }
        for (name, &total) in &frame {
            let acc = all.get_mut(name).expect("accumulated above");
            if acc.frames == 0 {
                acc.frame_min = total;
            }
            acc.frames += 1;
            acc.frame_min = acc.frame_min.min(total);
            acc.frame_max = acc.frame_max.max(total);
        }
    }
    let mut stats: Vec<ZoneStats> = all
        .into_iter()
        .map(|(name, a)| ZoneStats {
            name,
            calls: a.calls,
            frames: a.frames,
            per_call: Summary {
                min: ticks_to_duration(a.call_min),
                avg: ticks_to_duration(a.sum / a.calls),
                max: ticks_to_duration(a.call_max),
            },
            per_frame: Summary {
                min: ticks_to_duration(a.frame_min),
                avg: ticks_to_duration(a.sum / u64::from(a.frames)),
                max: ticks_to_duration(a.frame_max),
            },
        })
        .collect();
    stats.sort_by_key(|s| (Reverse(s.per_frame.avg), s.name));
    stats
}

impl History<'_> {
    /// CPU zone statistics by name over every frame in the history, most expensive per frame
    /// first.
    pub fn zone_stats(&self) -> Vec<ZoneStats> {
        accumulate(self.frames().map(|f| {
            f.zones()
                .iter()
                .map(|z| (z.name(), z.end_ticks().saturating_sub(z.start_ticks())))
        }))
    }

    /// Like [`zone_stats`](Self::zone_stats), for the GPU zones (all queues together).
    pub fn gpu_zone_stats(&self) -> Vec<ZoneStats> {
        accumulate(self.frames().map(|f| {
            f.gpu_zones()
                .iter()
                .map(|z| (z.name, z.end.saturating_sub(z.start)))
        }))
    }

    /// The call tree of `frame`, one per thread.
    pub fn tree(&self, frame: &Frame) -> FrameTree {
        let zones = frame.zones();
        let mut threads = Vec::new();
        let mut lo = 0;
        while lo < zones.len() {
            let thread = zones[lo].thread();
            let hi = lo + zones[lo..].partition_point(|z| z.thread() == thread);

            // Merge instances with the same name under the same parent node. Parents precede
            // their children in `zones`.
            let mut nodes: Vec<Node> = Vec::new();
            let mut roots: Vec<usize> = Vec::new();
            let mut node_of: Vec<usize> = Vec::with_capacity(hi - lo);
            for z in &zones[lo..hi] {
                let parent = z.parent().map(|p| node_of[p - lo]);
                let siblings: &[usize] = match parent {
                    Some(p) => &nodes[p].children,
                    None => &roots,
                };
                let found = siblings
                    .iter()
                    .copied()
                    .find(|&n| nodes[n].name == z.name());
                let node = found.unwrap_or_else(|| {
                    let n = nodes.len();
                    nodes.push(Node {
                        name: z.name(),
                        calls: 0,
                        ticks: 0,
                        children: Vec::new(),
                    });
                    match parent {
                        Some(p) => nodes[p].children.push(n),
                        None => roots.push(n),
                    }
                    n
                });
                nodes[node].calls += 1;
                nodes[node].ticks += z.end_ticks().saturating_sub(z.start_ticks());
                node_of.push(node);
            }

            let info = self.thread(thread);
            threads.push(ThreadTree {
                thread,
                os_id: info.map_or(0, |t| t.os_id()),
                name: info.and_then(|t| t.name()).map(String::from),
                roots: roots.iter().map(|&n| build(&nodes, n)).collect(),
            });
            lo = hi;
        }
        FrameTree { threads }
    }

    /// [`tree`](Self::tree) of the last frame.
    pub fn last_tree(&self) -> Option<FrameTree> {
        self.last().map(|f| self.tree(f))
    }
}

struct Node {
    name: &'static str,
    calls: u32,
    ticks: u64,
    children: Vec<usize>,
}

fn build(nodes: &[Node], n: usize) -> TreeNode {
    let node = &nodes[n];
    let children: Vec<TreeNode> = node.children.iter().map(|&c| build(nodes, c)).collect();
    let total = ticks_to_duration(node.ticks);
    let inner: Duration = children.iter().map(|c| c.total).sum();
    TreeNode {
        name: node.name,
        calls: node.calls,
        total,
        self_time: total.saturating_sub(inner),
        children,
    }
}

/// A frame's zones as call trees, one per thread, ordered by thread record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameTree {
    pub threads: Vec<ThreadTree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadTree {
    /// The thread record, as in [`ZoneRecord::thread`](crate::ZoneRecord::thread).
    pub thread: u32,
    pub os_id: u64,
    pub name: Option<String>,
    pub roots: Vec<TreeNode>,
}

/// The zones with one name under one parent node; children in order of first occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub name: &'static str,
    pub calls: u32,
    pub total: Duration,
    /// `total` minus the children's totals.
    pub self_time: Duration,
    pub children: Vec<TreeNode>,
}

impl FrameTree {
    /// The tree of the thread with this record.
    pub fn thread(&self, thread: u32) -> Option<&ThreadTree> {
        self.threads.iter().find(|t| t.thread == thread)
    }
}

impl TreeNode {
    pub fn child(&self, name: &str) -> Option<&TreeNode> {
        self.children.iter().find(|c| c.name == name)
    }
}
