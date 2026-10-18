//! Target descriptions and backends (`docs/design/0005-shader-compiler.md` §2, §4.1, step 9a of
//! §11).
//!
//! A [`Target`] says what one kind of GPU code takes: its capabilities (0004 §12.3), the wave
//! sizes it may run with, whether it keeps vectors and function calls, how much register
//! pressure the passes may cause. Shared passes read these fields and never ask which target it
//! is, so adding a backend is adding a description and a [`Backend`], registered in a
//! [`Registry`] that requests name targets from (`"vulkan-1.4"`).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use marlin_shaderc_ir::{FuncId, Module};

/// What one kind of GPU code takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The name requests use (`"vulkan-1.4"`).
    pub name: Box<str>,
    /// Bumped whenever the description or its backend changes what it produces; part of the
    /// cook fingerprint (0005 §8).
    pub version: u32,
    /// The capabilities (0004 §12.3) its hardware has, by the names of the core library's
    /// `Capability` enum.
    pub capabilities: BTreeSet<Box<str>>,
    /// The wave sizes code may run with, smallest and largest.
    pub wave_sizes: (u32, u32),
    /// Function calls exist in its code, so large callees may stay calls (0005 §4.4 pass 1.1).
    pub calls: bool,
    /// Vectors stay vectors (SPIR-V, Metal) rather than becoming scalars (DXIL; pass 5.8).
    pub vectors: bool,
    /// Registers the passes may plan for, for pressure-aware passes (0005 §4.3).
    pub register_budget: u32,
}

impl Target {
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    /// The capabilities of `needed` that this target lacks.
    pub fn missing<'a>(&self, needed: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        needed.into_iter().filter(|c| !self.has(c)).collect()
    }
}

/// What a backend made of one entry point: its code, ready for the graphics API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub bytes: Vec<u8>,
}

/// A backend could not make code for the module (something the target lacks, or a bug).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    pub message: String,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BackendError {}

/// Turns optimized IR into a target's code, and checks that code by the target's own rules.
pub trait Backend: Send + Sync {
    /// The targets this backend writes code for.
    fn targets(&self) -> Vec<Target>;
    /// The code for the entry point `entry` of `module`, which went through the pipeline for
    /// `target`.
    fn write(&self, module: &Module, entry: FuncId, target: &Target) -> Result<Blob, BackendError>;
    /// The problems the target's validation rules find in `blob` (empty: valid).
    fn validate(&self, blob: &Blob, target: &Target) -> Vec<String>;
}

/// The targets requests can name, each with the backend that writes its code.
#[derive(Clone, Default)]
pub struct Registry {
    entries: Vec<(Target, Arc<dyn Backend>)>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Adds every target `backend` writes code for. A later target of the same name replaces an
    /// earlier one.
    pub fn add(&mut self, backend: Arc<dyn Backend>) -> &mut Self {
        for target in backend.targets() {
            self.entries.retain(|(t, _)| t.name != target.name);
            self.entries.push((target, backend.clone()));
        }
        self
    }

    pub fn get(&self, name: &str) -> Option<(&Target, &Arc<dyn Backend>)> {
        self.entries
            .iter()
            .find(|(t, _)| &*t.name == name)
            .map(|(t, b)| (t, b))
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|(t, _)| &*t.name).collect()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.names()).finish()
    }
}
