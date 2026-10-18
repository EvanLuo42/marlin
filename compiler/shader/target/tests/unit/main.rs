//! marlin-shaderc-target's tests (quick tier, docs/design/0014-testing.md §5): the registry
//! and capability checks. Platform-independent.
//! Run alone with `cargo test -p marlin-shaderc-target --test unit`.

use std::collections::BTreeSet;
use std::sync::Arc;

use marlin_shaderc_ir::{FuncId, Module};
use marlin_shaderc_target::{Backend, BackendError, Blob, Registry, Target};
use marlin_test_support::MemCap;

#[global_allocator]
static HEAP: MemCap<std::alloc::System> = MemCap::new(std::alloc::System, 1 << 30);

fn target(name: &str, version: u32, caps: &[&str]) -> Target {
    Target {
        name: name.into(),
        version,
        capabilities: caps.iter().map(|c| (*c).into()).collect::<BTreeSet<_>>(),
        wave_sizes: (32, 32),
        calls: false,
        vectors: false,
        register_budget: 64,
    }
}

struct Fake(Vec<Target>);

impl Backend for Fake {
    fn targets(&self) -> Vec<Target> {
        self.0.clone()
    }
    fn write(&self, _: &Module, _: FuncId, target: &Target) -> Result<Blob, BackendError> {
        Ok(Blob {
            bytes: target.name.as_bytes().to_vec(),
        })
    }
    fn validate(&self, _: &Blob, _: &Target) -> Vec<String> {
        Vec::new()
    }
}

#[test]
fn targets_are_found_by_name_and_later_ones_replace_earlier() {
    let mut r = Registry::new();
    r.add(Arc::new(Fake(vec![
        target("a-1", 1, &[]),
        target("b-2", 1, &[]),
    ])));
    r.add(Arc::new(Fake(vec![target("a-1", 2, &["Int64"])])));
    assert_eq!(r.names(), ["b-2", "a-1"]);
    let (t, _) = r.get("a-1").unwrap();
    assert_eq!(t.version, 2);
    assert!(r.get("c-3").is_none());
}

#[test]
fn missing_capabilities_are_listed() {
    let t = target("x", 1, &["Int64", "Wave"]);
    assert!(t.has("Wave"));
    assert_eq!(
        t.missing(["Wave", "Float64", "Int64", "Native16Bit"]),
        ["Float64", "Native16Bit"]
    );
}
