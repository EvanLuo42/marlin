//! CPU topology from sysfs.

use core::ffi::CStr;

use super::Backend;
use crate::backend::unix::last_error;
use crate::contract::SysInfo;
use crate::sysinfo::LogicalCpu;
use crate::{CpuSet, Result};

const CPU_DIR: &[u8] = b"/sys/devices/system/cpu/cpu";

impl SysInfo for Backend {
    fn cpu_topology(out: &mut [LogicalCpu]) -> Result<usize> {
        let mut buf = [0u8; 4096];
        let online = read(c"/sys/devices/system/cpu/online", &mut buf)
            .map(parse_cpu_list)
            .ok_or_else(last_error)?;

        // Hybrid Intel: the kernel registers separate PMUs for the P and E core types.
        let atom = read(c"/sys/devices/cpu_atom/cpus", &mut buf).map(parse_cpu_list);
        // Otherwise (ARM big.LITTLE / DynamIQ) rank the distinct capacities.
        let capacities = if atom.is_none() {
            Capacities::collect(&online)
        } else {
            Capacities::default()
        };

        let mut count = 0;
        for cpu in online.iter() {
            if let Some(slot) = out.get_mut(count) {
                let efficiency_class = match &atom {
                    Some(atom) => u8::from(!atom.contains(cpu)),
                    None => capacity(cpu).map_or(0, |c| capacities.rank(c)),
                };
                *slot = LogicalCpu {
                    id: cpu,
                    core: core_of(cpu),
                    efficiency_class,
                };
            }
            count += 1;
        }
        Ok(count)
    }
}

/// The lowest CPU among `cpu`'s SMT siblings. Unlike `topology/core_id`, which restarts in each
/// package, this is unique system-wide.
fn core_of(cpu: u32) -> u32 {
    let mut buf = [0u8; 256];
    // `core_cpus_list` since Linux 5.6; `thread_siblings_list` before (now deprecated).
    [
        &b"/topology/core_cpus_list"[..],
        b"/topology/thread_siblings_list",
    ]
    .iter()
    .find_map(|suffix| {
        let mut path = [0u8; 96];
        read(cpu_path(&mut path, cpu, suffix), &mut buf)
            .and_then(|list| parse_cpu_list(list).iter().next())
    })
    .unwrap_or(cpu)
}

fn capacity(cpu: u32) -> Option<u32> {
    let mut path = [0u8; 96];
    let mut buf = [0u8; 32];
    read(cpu_path(&mut path, cpu, b"/cpu_capacity"), &mut buf).and_then(parse_u32)
}

/// Distinct `cpu_capacity` values, ascending. Real systems have at most 3-4 core types.
#[derive(Default)]
struct Capacities {
    values: [u32; 8],
    len: usize,
}

impl Capacities {
    fn collect(cpus: &CpuSet) -> Self {
        let mut this = Self::default();
        for c in cpus.iter().filter_map(capacity) {
            if let Err(i) = this.values[..this.len].binary_search(&c)
                && this.len < this.values.len()
            {
                this.values.copy_within(i..this.len, i + 1);
                this.values[i] = c;
                this.len += 1;
            }
        }
        this
    }

    /// Number of distinct capacities below `c`: 0 for the smallest cores.
    fn rank(&self, c: u32) -> u8 {
        self.values[..self.len].iter().filter(|&&v| v < c).count() as u8
    }
}

/// Builds `/sys/devices/system/cpu/cpu<N><suffix>` with a NUL terminator in `buf`.
fn cpu_path<'a>(buf: &'a mut [u8; 96], cpu: u32, suffix: &[u8]) -> &'a CStr {
    let mut digits = [0u8; 10];
    let mut n = cpu;
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let mut len = 0;
    for part in [CPU_DIR, &digits[start..], suffix] {
        buf[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    buf[len] = 0;
    CStr::from_bytes_until_nul(&buf[..=len]).unwrap()
}

/// Reads a small file into `buf`; `None` if it is missing or unreadable (errno is left set).
fn read<'a>(path: &CStr, buf: &'a mut [u8]) -> Option<&'a [u8]> {
    // SAFETY: `path` is NUL-terminated.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // sysfs attributes are returned whole by the first read.
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    // SAFETY: we own `fd`.
    unsafe { libc::close(fd) };
    (n >= 0).then(|| &buf[..n as usize])
}

/// Parses a kernel CPU list such as `0-3,8,10-11\n`. Ids at or above the capacity are dropped.
fn parse_cpu_list(list: &[u8]) -> CpuSet {
    let mut set = CpuSet::new();
    for range in list.trim_ascii().split(|&b| b == b',') {
        let mut ends = range.splitn(2, |&b| b == b'-');
        let Some(first) = ends.next().and_then(parse_u32) else {
            continue;
        };
        let last = ends.next().and_then(parse_u32).unwrap_or(first);
        for cpu in first..=last.min(CpuSet::CAPACITY as u32 - 1) {
            set.insert(cpu);
        }
    }
    set
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    let s = s.trim_ascii();
    if s.is_empty() {
        return None;
    }
    s.iter().try_fold(0u32, |n, &b| {
        b.is_ascii_digit()
            .then(|| n.checked_mul(10)?.checked_add(u32::from(b - b'0')))
            .flatten()
    })
}
