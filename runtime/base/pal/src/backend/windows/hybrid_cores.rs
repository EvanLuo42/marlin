//! EcoQoS via `SetThreadInformation(ThreadPowerThrottling)`.

use core::mem;
use core::sync::atomic::{AtomicU8, Ordering};

use windows_sys::Win32::System::Threading::{
    GetCurrentThread, GetThreadInformation, SetThreadInformation,
    THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
    THREAD_POWER_THROTTLING_STATE, ThreadPowerThrottling,
};

use super::Backend;
use super::error::check;
use crate::Result;
use crate::contract::ext::HybridCores;
use crate::ext::hybrid_cores::EfficiencyMode;

/// 0 = not probed yet, 1 = unavailable, 2 = available.
static STATE: AtomicU8 = AtomicU8::new(0);

impl HybridCores for Backend {
    fn hybrid_cores_available() -> bool {
        let state = match STATE.load(Ordering::Relaxed) {
            0 => {
                let state = if probe() { 2 } else { 1 };
                STATE.store(state, Ordering::Relaxed);
                state
            }
            state => state,
        };
        state == 2
    }

    fn set_current_efficiency_mode(mode: EfficiencyMode) -> Result<()> {
        // ControlMask says which policies we decide; StateMask turns them on. A policy left out of
        // ControlMask goes back to the system default.
        let (control, state) = match mode {
            EfficiencyMode::Default => (0, 0),
            EfficiencyMode::Efficient => (
                THREAD_POWER_THROTTLING_EXECUTION_SPEED,
                THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            ),
            EfficiencyMode::Performance => (THREAD_POWER_THROTTLING_EXECUTION_SPEED, 0),
        };
        let info = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: control,
            StateMask: state,
        };
        // SAFETY: GetCurrentThread returns a pseudo-handle; `info` outlives the call and its size
        // is passed.
        check(unsafe {
            SetThreadInformation(
                GetCurrentThread(),
                ThreadPowerThrottling,
                (&raw const info).cast(),
                mem::size_of_val(&info) as u32,
            )
        })
    }
}

/// Reading the state has no side effects, unlike a probing write. Builds before 1709 reject the
/// information class.
#[cold]
fn probe() -> bool {
    let mut info = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ..Default::default()
    };
    // SAFETY: GetCurrentThread returns a pseudo-handle; `info` is a valid out buffer of the given
    // size.
    unsafe {
        GetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            (&raw mut info).cast(),
            mem::size_of_val(&info) as u32,
        ) != 0
    }
}
