//! macOS has no hard thread affinity.

use marlin_thread::{Builder, CpuSet};

#[test]
fn affinity_ignored_without_hard_affinity() {
    let handle = Builder::new()
        .affinity(CpuSet::single(0))
        .spawn(|| 1)
        .unwrap();
    assert_eq!(handle.join(), 1);
}
