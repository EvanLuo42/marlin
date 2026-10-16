use crate::Result;
use crate::sysinfo::LogicalCpu;

pub(crate) trait SysInfo {
    /// Writes up to `out.len()` logical CPUs, in any order, and returns the total count.
    fn cpu_topology(out: &mut [LogicalCpu]) -> Result<usize>;
}
