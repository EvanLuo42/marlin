//! What the fuzz targets share: the crash hook and the types the serial targets read into.
//!
//! Every target feeds untrusted bytes to a public API that must return an error on bad input,
//! never panic, read out of bounds or run away with memory. Run them with
//! `cargo xtask test --tier fuzz` (docs/design/0014-testing.md §7.3), which builds them with
//! libFuzzer and AddressSanitizer and bounds every run in time and memory.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use marlin_reflect::{Reflect, Registry};
use marlin_serial::cooked::{Pod, RelPtr, RelSlice};

unsafe extern "C" {
    /// The C runtime's `abort`: it raises `SIGABRT`, which libFuzzer catches to save the input.
    /// `std::process::abort` on Windows is `__fastfail`, which ends the process before any
    /// handler runs, so the crashing input would be lost.
    safe fn abort() -> !;
}

/// Replaces libfuzzer-sys's panic hook with one that ends the process through the C runtime's
/// `abort` (see [`abort`]). Call it in every target's `init:`.
pub fn init() {
    let _ = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        eprintln!("{info}\n{backtrace}");
        abort();
    }));
}

/// Reads `N` bytes off the front of `data`, for targets that take a few parameters before the
/// payload.
pub fn split<const N: usize>(data: &[u8]) -> Option<([u8; N], &[u8])> {
    let (head, rest) = data.split_first_chunk::<N>()?;
    Some((*head, rest))
}

// --- serial_read: one value of every kind the format has ---

#[derive(Reflect, Debug, Clone, PartialEq)]
pub struct Everything {
    pub b: bool,
    pub c: char,
    pub u8: u8,
    pub u16: u16,
    pub u32: u32,
    pub u64: u64,
    pub u128: u128,
    pub usize: usize,
    pub i8: i8,
    pub i16: i16,
    pub i32: i32,
    pub i64: i64,
    pub i128: i128,
    pub isize: isize,
    pub f32: f32,
    pub f64: f64,
    pub text: String,
    pub list: Vec<Option<Vec<i16>>>,
    pub grid: [[u8; 2]; 3],
    pub maybe: Option<Box<Bounds>>,
    pub shapes: Vec<Shape>,
    pub unit: Unit,
    pub tuple: Tuple,
    pub pair: (u32, String),
    pub map: BTreeMap<String, Vec<u8>>,
    pub set: BTreeSet<i64>,
    pub queue: VecDeque<f32>,
    pub tree: Node,
    pub nothing: (),
}

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
pub struct Unit;

#[derive(Reflect, Debug, Clone, PartialEq)]
pub struct Tuple(pub i8, pub String);

#[derive(Reflect, Debug, Clone, PartialEq)]
pub enum Shape {
    Empty,
    Circle(f32),
    Rect { w: f32, h: f32 },
    Path(Vec<[f32; 2]>, bool),
}

/// Recursive, for the depth limit.
#[derive(Reflect, Debug, Clone, PartialEq)]
pub struct Node {
    pub value: i32,
    pub children: Vec<Node>,
}

/// The registry `read_value` resolves root types through.
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register::<Everything>().unwrap();
    registry.register::<Node>().unwrap();
    registry.register::<Shape>().unwrap();
    registry
}

// --- serial_cooked: a root with every kind of relative reference ---

#[derive(Pod, Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct CookedRoot {
    pub name: RelSlice<u8>,
    pub items: RelSlice<CookedItem>,
    pub first: RelPtr<CookedItem>,
    pub scale: f32,
}

#[derive(Pod, Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct CookedItem {
    pub id: u64,
    pub label: RelSlice<u8>,
    pub next: RelPtr<CookedItem>,
    pub weights: [f32; 3],
}
