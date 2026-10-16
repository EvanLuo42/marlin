//! Affinity and priority of the calling thread.

use core::mem;

use crate::backend::unix::last_error;
use crate::{CpuSet, Result, ThreadPriority};

pub(in crate::backend) fn set_current_affinity(cpus: &CpuSet) -> Result<()> {
    // SAFETY: `cpu_set_t` is plain data; all-zero is an empty set.
    let mut set: libc::cpu_set_t = unsafe { mem::zeroed() };
    for cpu in cpus.iter() {
        // SAFETY: CpuSet::CAPACITY == CPU_SETSIZE, so `cpu` is in range.
        unsafe { libc::CPU_SET(cpu as usize, &mut set) };
    }
    // SAFETY: pid 0 is the calling thread; `set` is valid for its size.
    if unsafe { libc::sched_setaffinity(0, mem::size_of_val(&set), &set) } == 0 {
        Ok(())
    } else {
        Err(last_error())
    }
}

pub(in crate::backend) fn current_affinity() -> Result<CpuSet> {
    // SAFETY: `cpu_set_t` is plain data; all-zero is an empty set.
    let mut set: libc::cpu_set_t = unsafe { mem::zeroed() };
    // SAFETY: pid 0 is the calling thread; `set` is a valid out pointer of the given size.
    if unsafe { libc::sched_getaffinity(0, mem::size_of_val(&set), &mut set) } != 0 {
        return Err(last_error());
    }
    let mut cpus = CpuSet::new();
    for cpu in 0..CpuSet::CAPACITY {
        // SAFETY: `cpu` < CPU_SETSIZE.
        if unsafe { libc::CPU_ISSET(cpu, &set) } {
            cpus.insert(cpu as u32);
        }
    }
    Ok(cpus)
}

pub(in crate::backend) fn set_current_priority(priority: ThreadPriority) -> Result<()> {
    // Linux applies nice per thread (task), so PRIO_PROCESS with the tid affects only this thread.
    let nice = match priority {
        ThreadPriority::Background => 10,
        ThreadPriority::Low => 5,
        ThreadPriority::Normal => 0,
        ThreadPriority::High => -5,
        ThreadPriority::TimeCritical => -10,
    };
    // SAFETY: no preconditions.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::id_t;
    // SAFETY: no preconditions. EPERM/EACCES (raising priority without CAP_SYS_NICE) map to
    // PermissionDenied.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, nice) } == 0 {
        Ok(())
    } else {
        Err(last_error())
    }
}
