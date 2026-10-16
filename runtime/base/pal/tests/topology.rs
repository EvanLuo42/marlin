use marlin_pal::sysinfo::{LogicalCpu, cpu_topology};
use marlin_pal::thread::{self, available_parallelism};
use marlin_pal::{CpuSet, ErrorKind, ThreadPriority};

fn topology() -> Vec<LogicalCpu> {
    let count = cpu_topology(&mut []).unwrap();
    let mut cpus = vec![LogicalCpu::default(); count];
    assert_eq!(cpu_topology(&mut cpus).unwrap(), count);
    cpus
}

#[test]
fn topology_matches_parallelism() {
    let cpus = topology();
    // Linux's available_parallelism respects the affinity mask; the topology lists every CPU.
    if cfg!(windows) {
        assert_eq!(cpus.len(), available_parallelism());
    } else {
        assert!(cpus.len() >= available_parallelism());
    }
}

#[test]
fn topology_is_consistent() {
    let cpus = topology();
    assert!(
        cpus.windows(2).all(|w| w[0].id < w[1].id),
        "sorted, unique ids"
    );

    // SMT siblings share a class, and every core has at least one CPU.
    for cpu in &cpus {
        let siblings = cpus.iter().filter(|c| c.core == cpu.core);
        assert!(
            siblings
                .clone()
                .all(|c| c.efficiency_class == cpu.efficiency_class)
        );
        assert!(siblings.count() <= 8);
    }

    // A short buffer is filled without overrunning and still reports the total.
    let mut short = [LogicalCpu::default(); 1];
    assert_eq!(cpu_topology(&mut short).unwrap(), cpus.len());
    assert!(cpus.contains(&short[0]));
}

#[test]
fn cpu_set_basics() {
    let mut set = CpuSet::new();
    assert!(set.is_empty());
    set.insert(3);
    set.insert(64);
    set.insert(1023);
    assert_eq!(set.len(), 3);
    assert!(set.contains(64) && !set.contains(65) && !set.contains(5000));
    assert_eq!(set.iter().collect::<Vec<_>>(), [3, 64, 1023]);
    assert_eq!(set.word(1), 1);
    set.remove(64);
    assert_eq!(format!("{set:?}"), "{3, 1023}");
    assert_eq!(CpuSet::single(7).iter().collect::<Vec<_>>(), [7]);
}

#[test]
fn affinity_to_one_cpu_and_back() {
    let original = match thread::current_affinity() {
        Err(e) if e.kind() == ErrorKind::Unsupported => return, // macOS
        r => r.unwrap(),
    };
    assert!(!original.is_empty());

    // The last allowed CPU, so the test does not just keep running where it already is.
    let target = original.iter().last().unwrap();
    thread::set_current_affinity(&CpuSet::single(target)).unwrap();
    assert_eq!(thread::current_affinity().unwrap(), CpuSet::single(target));
    for _ in 0..10 {
        thread::yield_now();
    }

    thread::set_current_affinity(&original).unwrap();
    assert_eq!(thread::current_affinity().unwrap(), original);

    let err = thread::set_current_affinity(&CpuSet::new()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}

#[cfg(windows)]
#[test]
fn affinity_spanning_groups_is_rejected() {
    let mut set = CpuSet::new();
    set.insert(0);
    set.insert(64);
    let err = thread::set_current_affinity(&set).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
}

#[test]
fn set_priority() {
    use ThreadPriority::*;
    for priority in [Background, Low, Normal, High, TimeCritical, Normal] {
        match thread::set_current_priority(priority) {
            Ok(()) => {}
            // Linux: raising priority (including back to Normal) needs CAP_SYS_NICE.
            Err(e) if !cfg!(windows) && e.kind() == ErrorKind::PermissionDenied => {}
            Err(e) => panic!("{priority:?}: {e}"),
        }
    }
}

#[cfg(marlin_ext = "hybrid_cores")]
#[test]
fn hybrid_cores_modes() {
    use marlin_pal::ext::hybrid_cores::{EfficiencyMode, HybridCores};

    let Some(hybrid) = HybridCores::get() else {
        return;
    };
    for mode in [
        EfficiencyMode::Efficient,
        EfficiencyMode::Performance,
        EfficiencyMode::Default,
    ] {
        hybrid.set_current_mode(mode).unwrap();
    }
}
