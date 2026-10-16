//! Priority of the calling thread through QoS classes; macOS has no hard affinity.

use crate::backend::unix::check_code;
use crate::{CpuSet, Error, ErrorKind, Result, ThreadPriority};

pub(in crate::backend) fn set_current_affinity(_cpus: &CpuSet) -> Result<()> {
    // THREAD_AFFINITY_POLICY is only a cache-sharing hint, and is ignored on Apple Silicon.
    Err(Error::from_kind(ErrorKind::Unsupported))
}

pub(in crate::backend) fn current_affinity() -> Result<CpuSet> {
    Err(Error::from_kind(ErrorKind::Unsupported))
}

pub(in crate::backend) fn set_current_priority(priority: ThreadPriority) -> Result<()> {
    use libc::qos_class_t::*;

    // QoS also picks the core type on Apple Silicon: BACKGROUND runs on E cores only.
    let class = match priority {
        ThreadPriority::Background => QOS_CLASS_BACKGROUND,
        ThreadPriority::Low => QOS_CLASS_UTILITY,
        ThreadPriority::Normal => QOS_CLASS_DEFAULT,
        ThreadPriority::High => QOS_CLASS_USER_INITIATED,
        ThreadPriority::TimeCritical => QOS_CLASS_USER_INTERACTIVE,
    };
    // SAFETY: no preconditions. Fails with EPERM if the thread's scheduling policy was set
    // directly through pthread_setschedparam.
    check_code(unsafe { libc::pthread_set_qos_class_self_np(class, 0) })
}
