//! The renderer's compiled shaders, for the backend's [`SHADER_FORMAT`].
//!
//! PLACEHOLDERS: `shaders/forward.hlsl` compiled once with the Windows SDK's dxc, until the
//! Marlin shader compiler exists (then the cooker compiles Marlin shaders and the renderer loads
//! them as assets):
//!
//! ```text
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T vs_6_6 -E vs_main -Fo forward.vs.dxil forward.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ps_6_6 -E ps_main -Fo forward.ps.dxil forward.hlsl
//! ```

use marlin_rhi::{SHADER_FORMAT, ShaderFormat, ShaderModule};

/// Vertex and pixel shaders of the forward pass; `None` if none exist for this backend's format.
pub(crate) fn forward() -> Option<(ShaderModule<'static>, ShaderModule<'static>)> {
    match SHADER_FORMAT {
        ShaderFormat::Dxil => Some((
            ShaderModule {
                code: include_bytes!("../shaders/forward.vs.dxil"),
                entry: "vs_main",
            },
            ShaderModule {
                code: include_bytes!("../shaders/forward.ps.dxil"),
                entry: "ps_main",
            },
        )),
        _ => None,
    }
}
