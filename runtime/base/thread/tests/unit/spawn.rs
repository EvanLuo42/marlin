use std::hint::black_box;
use std::time::Duration;

use marlin_test_support::{DropCounter, wait_until};
use marlin_thread::{Builder, ThreadPriority, current, spawn};

#[test]
fn join_returns_value() {
    assert_eq!(spawn(|| 6 * 7).join(), 42);
    let owned = spawn(|| vec![String::from("a"), String::from("b")]).join();
    assert_eq!(owned, ["a", "b"]);
}

#[test]
fn closure_moves_captures() {
    let data = vec![1u64, 2, 3].into_boxed_slice();
    assert_eq!(spawn(move || data.iter().sum::<u64>()).join(), 6);
}

#[test]
fn many_threads() {
    let handles: Vec<_> = (0..64u64).map(|i| spawn(move || i * i)).collect();
    let sum: u64 = handles.into_iter().map(|h| h.join()).sum();
    assert_eq!(sum, (0..64u64).map(|i| i * i).sum());
}

#[test]
fn threads_have_distinct_ids() {
    let main = current::id();
    assert_eq!(main, current::id());
    let other = spawn(current::id).join();
    assert_ne!(main, other);
}

#[test]
fn stack_size_takes_effect() {
    // Larger than the 1 MiB Windows default and the 512 KiB macOS secondary-thread default.
    let handle = Builder::new()
        .name("big-stack")
        .stack_size(8 << 20)
        .spawn(|| {
            let mut buf = [1u8; 2 << 20];
            black_box(&mut buf);
            buf.iter().map(|&b| b as usize).sum::<usize>()
        })
        .unwrap();
    assert_eq!(handle.join(), 2 << 20);
}

#[test]
fn priority_applies() {
    // Lowering priority needs no privileges anywhere.
    let handle = Builder::new()
        .name("low")
        .priority(ThreadPriority::Low)
        .spawn(|| 1)
        .unwrap();
    assert_eq!(handle.join(), 1);
}

#[cfg(not(marlin_platform = "macos"))]
#[test]
fn affinity_takes_effect() {
    use marlin_thread::CpuSet;

    let allowed = current::affinity().unwrap();
    let cpu = allowed.iter().next().unwrap();
    let handle = Builder::new()
        .affinity(CpuSet::single(cpu))
        .spawn(current::affinity)
        .unwrap();
    assert_eq!(handle.join().unwrap(), CpuSet::single(cpu));
}

#[cfg(not(marlin_platform = "macos"))]
#[test]
fn failed_setup_skips_closure() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use marlin_thread::CpuSet;

    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ran.clone();
    // No machine has CPU 1023 available to this process.
    let result = Builder::new()
        .affinity(CpuSet::single(CpuSet::CAPACITY as u32 - 1))
        .spawn(move || flag.store(true, Ordering::SeqCst));
    assert_eq!(
        result.unwrap_err().kind(),
        marlin_thread::ErrorKind::InvalidInput
    );
    assert!(!ran.load(Ordering::SeqCst));
    // The closure (and its capture) was dropped.
    assert_eq!(Arc::strong_count(&ran), 1);
}

#[test]
fn detached_result_is_dropped() {
    let drops = DropCounter::new();
    let token = drops.token();
    spawn(move || token).detach();
    wait_until(
        Duration::from_secs(10),
        "detached result never dropped",
        || drops.dropped() > 0,
    );
}

#[test]
fn topology_is_consistent() {
    let mut cpus = vec![Default::default(); 1024];
    let count = marlin_thread::cpu_topology(&mut cpus).unwrap();
    assert!(count >= 1);
    assert!(marlin_thread::available_parallelism() >= 1);
    assert!(marlin_thread::available_parallelism() <= count);
}

#[test]
fn handles_can_be_joined_from_another_thread() {
    fn assert_send<T: Send>(_: &T) {}
    let inner = spawn(|| String::from("inner"));
    assert_send(&inner);
    assert_eq!(spawn(move || inner.join()).join(), "inner");
}

#[test]
fn current_name_reads_back() {
    let handle = marlin_thread::Builder::new()
        .name("named-worker")
        .spawn(marlin_thread::current::name)
        .unwrap();
    assert_eq!(handle.join().as_deref(), Some("named-worker"));
}
