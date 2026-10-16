use crate::Result;
use crate::ext::hybrid_cores::EfficiencyMode;

pub(crate) trait HybridCores {
    /// Whether the OS accepts per-thread efficiency hints. Must be cheap after the first call.
    fn hybrid_cores_available() -> bool;
    /// Only called after [`hybrid_cores_available`](Self::hybrid_cores_available) returned true.
    fn set_current_efficiency_mode(mode: EfficiencyMode) -> Result<()>;
}
