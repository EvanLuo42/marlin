//! The compiled graph as JSON (0007 §10.3): for the editor's graph view and for snapshot tests.
//! A small writer of its own: the runtime is `no_std` and has no JSON library.

use alloc::string::String;
use core::fmt::Write;

use marlin_rhi::{BufferState, QueueType, SubresourceRange, TextureState};

use crate::frame::PassTiming;
use crate::plan::{CompiledPlan, Lifetime, Placement};

struct Json {
    out: String,
    /// Whether the current object or array has had an element.
    first: alloc::vec::Vec<bool>,
}

impl Json {
    fn new() -> Self {
        Self {
            out: String::new(),
            first: alloc::vec::Vec::new(),
        }
    }

    fn separate(&mut self) {
        if let Some(first) = self.first.last_mut() {
            if !*first {
                self.out.push(',');
            }
            *first = false;
        }
    }

    fn key(&mut self, key: &str) -> &mut Self {
        self.separate();
        self.string_raw(key);
        self.out.push(':');
        // The value that follows is not a new element.
        if let Some(first) = self.first.last_mut() {
            *first = true;
        }
        self
    }

    fn open(&mut self, c: char) -> &mut Self {
        self.separate();
        self.out.push(c);
        self.first.push(true);
        self
    }

    fn close(&mut self, c: char) -> &mut Self {
        self.first.pop();
        self.out.push(c);
        if let Some(first) = self.first.last_mut() {
            *first = false;
        }
        self
    }

    fn string_raw(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                '\n' => self.out.push_str("\\n"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(self.out, "\\u{:04x}", c as u32);
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    fn string(&mut self, s: &str) -> &mut Self {
        self.separate();
        self.string_raw(s);
        self
    }

    fn number(&mut self, n: u64) -> &mut Self {
        self.separate();
        let _ = write!(self.out, "{n}");
        self
    }

    fn null(&mut self) -> &mut Self {
        self.separate();
        self.out.push_str("null");
        self
    }

    fn debug(&mut self, value: impl core::fmt::Debug) -> &mut Self {
        let mut s = String::new();
        let _ = write!(s, "{value:?}");
        self.string(&s)
    }
}

fn queue_name(q: QueueType) -> &'static str {
    match q {
        QueueType::Graphics => "graphics",
        QueueType::Compute => "compute",
        QueueType::Copy => "copy",
    }
}

fn texture_state(j: &mut Json, s: &TextureState) {
    j.open('{');
    j.key("stage").number(u64::from(s.stage.bits()));
    j.key("access").number(u64::from(s.access.bits()));
    j.key("layout").debug(s.layout);
    j.close('}');
}

fn buffer_state(j: &mut Json, s: &BufferState) {
    j.open('{');
    j.key("stage").number(u64::from(s.stage.bits()));
    j.key("access").number(u64::from(s.access.bits()));
    j.close('}');
}

fn range(j: &mut Json, r: &SubresourceRange) {
    j.open('[');
    j.number(u64::from(r.base_mip))
        .number(u64::from(r.mip_count))
        .number(u64::from(r.base_layer))
        .number(u64::from(r.layer_count));
    j.close(']');
}

fn lifetime(j: &mut Json, l: Option<Lifetime>) {
    match l {
        None => {
            j.null();
        }
        Some(l) => {
            j.open('[');
            j.number(u64::from(l.first)).number(u64::from(l.last));
            j.close(']');
        }
    }
}

fn placement(j: &mut Json, p: Option<Placement>) {
    match p {
        None => {
            j.null();
        }
        Some(p) => {
            j.open('{');
            j.key("heap").debug(p.heap);
            j.key("offset").number(p.offset);
            j.key("size").number(p.size);
            j.close('}');
        }
    }
}

impl CompiledPlan {
    /// The plan as JSON: passes with their queues and barriers, segments and waits, resources
    /// with lifetimes, usage and memory, dependencies and heap sizes; with `timings` (a frame's
    /// GPU times, [`FrameContext::gpu_timings`](crate::FrameContext::gpu_timings)) when given.
    pub fn to_json(&self, timings: &[PassTiming]) -> String {
        let mut j = Json::new();
        j.open('{');
        j.key("key").number(self.key);
        j.key("passes").open('[');
        for (i, p) in self.passes.iter().enumerate() {
            j.open('{');
            j.key("name").string(&p.name);
            j.key("node").number(u64::from(p.node));
            j.key("kind").debug(p.kind);
            j.key("queue").string(queue_name(p.queue));
            j.key("texture_barriers").open('[');
            for t in self.texture_barriers(i) {
                j.open('{');
                j.key("texture")
                    .string(&self.texture_names[t.texture as usize]);
                j.key("range");
                range(&mut j, &t.range);
                j.key("before");
                texture_state(&mut j, &t.before);
                j.key("after");
                texture_state(&mut j, &t.after);
                j.close('}');
            }
            j.close(']');
            j.key("buffer_barriers").open('[');
            for b in self.buffer_barriers(i) {
                j.open('{');
                j.key("buffer")
                    .string(&self.buffer_names[b.buffer as usize]);
                j.key("before");
                buffer_state(&mut j, &b.before);
                j.key("after");
                buffer_state(&mut j, &b.after);
                j.close('}');
            }
            j.close(']');
            if let Some(t) = timings.iter().find(|t| t.name == p.name) {
                j.key("gpu_ns").number(t.nanoseconds);
            }
            j.close('}');
        }
        j.close(']');
        j.key("culled").open('[');
        for &c in &self.culled {
            j.number(u64::from(c));
        }
        j.close(']');
        j.key("edges").open('[');
        for &(a, b) in &self.edges {
            j.open('[');
            j.number(u64::from(a)).number(u64::from(b));
            j.close(']');
        }
        j.close(']');
        j.key("segments").open('[');
        for (i, s) in self.segments.iter().enumerate() {
            j.open('{');
            j.key("queue").string(queue_name(s.queue));
            j.key("passes").open('[');
            for &p in self.segment_passes(i) {
                j.number(u64::from(p));
            }
            j.close(']');
            j.key("waits").open('[');
            for &w in &s.waits {
                j.number(u64::from(w));
            }
            j.close(']');
            j.close('}');
        }
        j.close(']');
        j.key("textures").open('[');
        for (i, t) in self.textures.iter().enumerate() {
            j.open('{');
            j.key("name").string(&self.texture_names[i]);
            j.key("usage").number(u64::from(t.usage.bits()));
            j.key("lifetime");
            lifetime(&mut j, t.lifetime);
            j.key("placement");
            placement(&mut j, t.placement);
            j.close('}');
        }
        j.close(']');
        j.key("buffers").open('[');
        for (i, b) in self.buffers.iter().enumerate() {
            j.open('{');
            j.key("name").string(&self.buffer_names[i]);
            j.key("usage").number(u64::from(b.usage.bits()));
            j.key("lifetime");
            lifetime(&mut j, b.lifetime);
            j.key("placement");
            placement(&mut j, b.placement);
            j.close('}');
        }
        j.close(']');
        j.key("heaps").open('{');
        j.key("buffers").number(self.heap_sizes[0]);
        j.key("textures").number(self.heap_sizes[1]);
        j.key("targets").number(self.heap_sizes[2]);
        j.close('}');
        j.close('}');
        j.out
    }
}
