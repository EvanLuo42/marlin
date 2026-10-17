//! System information: CPU topology and memory.

use crate::Result;
use crate::backend::imp::Backend;
use crate::contract::SysInfo;

/// One logical CPU (hardware thread).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct LogicalCpu {
    /// The CPU's index in a [`CpuSet`](crate::CpuSet).
    ///
    /// - Windows: `group * 64 + number`, the processor group and the number within it, so a
    ///   group's CPUs share one `CpuSet` word. Not dense when a group has fewer than 64 CPUs.
    /// - Linux: the kernel CPU number (`/sys/devices/system/cpu/cpuN`).
    /// - macOS: synthetic, `0..hw.logicalcpu`; see [`cpu_topology`].
    pub id: u32,
    /// Shared by the logical CPUs of one physical core (SMT siblings) and unique across the
    /// system. Not dense.
    pub core: u32,
    /// 0 = most power efficient; higher = faster. On a hybrid CPU the E cores have a lower class
    /// than the P cores; on a homogeneous CPU every CPU has class 0.
    pub efficiency_class: u8,
}

/// Writes the system's logical CPUs to `out`, sorted by [`id`](LogicalCpu::id), and returns how
/// many there are.
///
/// Fills at most `out.len()` entries; call with an empty slice first to size the buffer (a shorter
/// `out` receives an unspecified subset). Lists
/// every online CPU, not only those the process's affinity allows. On Windows this equals
/// [`available_parallelism`](crate::thread::available_parallelism); on Linux that function
/// respects the affinity mask and can be smaller.
///
/// Platform notes:
///
/// - Windows: `GetLogicalProcessorInformationEx(RelationProcessorCore)`.
/// - Linux: sysfs. Hybrid Intel CPUs are classified through `/sys/devices/cpu_atom/cpus` (class 0)
///   and `cpu_core/cpus` (class 1); otherwise distinct `cpu_capacity` values (ARM big.LITTLE) are
///   ranked into classes. CPUs at or above [`CpuSet::CAPACITY`](crate::CpuSet::CAPACITY) are
///   omitted.
/// - macOS: only per-level counts exist (`hw.perflevelN.logicalcpu`). The count, the number of
///   classes and the CPUs per class are exact, but which CPU is which cannot be known: ids are
///   synthetic, assigned from the least to the most performant level. SMT siblings (Intel Macs)
///   are assumed to be adjacent ids. There is no affinity API to use ids with anyway.
#[inline]
pub fn cpu_topology(out: &mut [LogicalCpu]) -> Result<usize> {
    let count = Backend::cpu_topology(out)?;
    let filled = count.min(out.len());
    out[..filled].sort_unstable_by_key(|cpu| cpu.id);
    Ok(count)
}

/// System and process memory, in bytes. A snapshot: every field may change at any moment.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct MemoryInfo {
    /// Physical memory installed and usable by the OS.
    pub total_physical: u64,
    /// Physical memory available to new allocations without paging: Windows `ullAvailPhys`, Linux
    /// `MemAvailable`, macOS free + inactive + speculative pages.
    pub available_physical: u64,
    /// The system commit limit (physical memory plus page file or swap) and the memory committed
    /// against it, where the OS has the notion (Windows, Linux). `None` on macOS.
    pub commit_limit: Option<u64>,
    pub committed: Option<u64>,
    /// This process's resident set (working set) and its peak.
    pub process_resident: u64,
    pub process_peak_resident: u64,
    /// Memory charged to this process alone: Windows private bytes (commit charge), Linux
    /// anonymous resident plus swapped memory, macOS physical footprint (what Xcode and the OS
    /// memory limits count).
    pub process_private: u64,
}

/// The system's and this process's memory. See [`MemoryInfo`] for what each platform reports.
#[inline]
pub fn memory() -> Result<MemoryInfo> {
    Backend::memory()
}
