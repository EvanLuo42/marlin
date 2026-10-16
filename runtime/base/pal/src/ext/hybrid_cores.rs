//! Per-thread power/performance hints that steer threads between P and E cores.
//!
//! Windows only (EcoQoS, `SetThreadInformation(ThreadPowerThrottling)`, Windows 10 1709+). On
//! macOS the same effect comes from [`ThreadPriority`](crate::ThreadPriority), which maps to QoS
//! classes, so there is no separate extension there.

use crate::Result;
use crate::backend::imp::Backend;
use crate::contract::ext::HybridCores as Contract;

/// How the scheduler should treat the calling thread on a hybrid CPU.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum EfficiencyMode {
    /// Let the OS decide from its own heuristics (foreground window, audio, ...).
    #[default]
    Default,
    /// EcoQoS: prefer E cores and low clocks. For background work whose latency does not matter.
    Efficient,
    /// Opt out of power throttling even when the OS would apply it (for example while the game is
    /// minimized but still simulating).
    Performance,
}

/// Proof that this OS accepts efficiency hints. Obtain one with [`HybridCores::get`].
///
/// Also useful on homogeneous CPUs: EcoQoS lowers clocks there too.
#[derive(Clone, Copy, Debug)]
pub struct HybridCores(());

impl HybridCores {
    #[inline]
    pub fn get() -> Option<Self> {
        Backend::hybrid_cores_available().then_some(Self(()))
    }

    /// Sets the calling thread's efficiency mode. Independent of
    /// [`set_current_priority`](crate::thread::set_current_priority).
    #[inline]
    pub fn set_current_mode(self, mode: EfficiencyMode) -> Result<()> {
        Backend::set_current_efficiency_mode(mode)
    }
}
