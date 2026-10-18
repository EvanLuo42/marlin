//! Compiled plans kept across frames by topology key, least recently used out (0007 §5.2).

use alloc::vec::Vec;

use marlin_rhi::Result;

use crate::backend::Backend;
use crate::graph::RenderGraph;
use crate::plan::CompiledPlan;

struct Entry {
    plan: CompiledPlan,
    last_used: u64,
}

pub struct PlanCache {
    entries: Vec<Entry>,
    capacity: usize,
    clock: u64,
}

/// Whether [`PlanCache::get_or_compile`] found the plan or compiled it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lookup {
    Hit,
    Miss,
}

impl PlanCache {
    /// Enough for an editor toggling viewports and debug views.
    pub const DEFAULT_CAPACITY: usize = 8;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    /// Keeps at most `capacity` plans. Panics if 0.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "a plan cache holds at least one plan");
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            clock: 0,
        }
    }

    /// The plan for `graph`'s topology: the cached one, or a new one compiled and cached, which
    /// evicts the least recently used plan when the cache is full.
    pub fn get_or_compile<B: Backend>(
        &mut self,
        graph: &RenderGraph<'_, B>,
        device: &B::Device,
    ) -> Result<(&CompiledPlan, Lookup)> {
        self.clock += 1;
        let key = graph.topology_key();
        if let Some(i) = self.entries.iter().position(|e| e.plan.key == key) {
            let entry = &mut self.entries[i];
            entry.last_used = self.clock;
            return Ok((&entry.plan, Lookup::Hit));
        }
        let plan = crate::compile::compile(graph, device, key)?;
        let entry = Entry {
            plan,
            last_used: self.clock,
        };
        let i = if self.entries.len() < self.capacity {
            self.entries.push(entry);
            self.entries.len() - 1
        } else {
            let oldest = (0..self.entries.len())
                .min_by_key(|&i| self.entries[i].last_used)
                .expect("the cache is full, so not empty");
            self.entries[oldest] = entry;
            oldest
        };
        Ok((&self.entries[i].plan, Lookup::Miss))
    }

    /// Plans held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a plan for `key` is held.
    pub fn contains(&self, key: u64) -> bool {
        self.entries.iter().any(|e| e.plan.key == key)
    }
}

impl Default for PlanCache {
    fn default() -> Self {
        Self::new()
    }
}
