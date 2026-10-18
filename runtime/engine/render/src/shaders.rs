//! The renderer's compiled shaders, for the backend's [`SHADER_FORMAT`].
//!
//! PLACEHOLDERS: `shaders/forward.hlsl` compiled once by hand with dxc, until the Marlin shader
//! compiler exists (then the cooker compiles Marlin shaders and the renderer loads them as
//! assets). The SPIR-V uses the set 0 bindings of docs/design/0016-rhi-vulkan.md §4.2, and the
//! vertex shader subtracts the base instance so `SV_InstanceID` starts at 0 as on D3D12 (0002 §3):
//!
//! ```text
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T vs_6_6 -E vs_main -Fo forward.vs.dxil forward.hlsl
//! dxc -HV 2021 -Qstrip_debug -Qstrip_reflect -T ps_6_6 -E ps_main -Fo forward.ps.dxil forward.hlsl
//! dxc -spirv -fspv-target-env=vulkan1.3 -HV 2021 -fvk-bind-resource-heap 0 0 -fvk-bind-sampler-heap 1 0 -fvk-bind-register b0 0 2 0 -fvk-use-scalar-layout -fvk-support-nonzero-base-instance -T vs_6_6 -E vs_main -Fo forward.vs.spv forward.hlsl
//! dxc -spirv -fspv-target-env=vulkan1.3 -HV 2021 -fvk-bind-resource-heap 0 0 -fvk-bind-sampler-heap 1 0 -fvk-bind-register b0 0 2 0 -fvk-use-scalar-layout -T ps_6_6 -E ps_main -Fo forward.ps.spv forward.hlsl
//! ```

use marlin_rhi::{SHADER_FORMAT, ShaderFormat, ShaderModule};

/// Vertex and pixel shaders of the forward pass; `None` if none exist for this backend's format.
pub(crate) fn forward() -> Option<(ShaderModule<'static>, ShaderModule<'static>)> {
    let (vs, ps): (&'static [u8], &'static [u8]) = match SHADER_FORMAT {
        ShaderFormat::Dxil => (
            include_bytes!("../shaders/forward.vs.dxil"),
            include_bytes!("../shaders/forward.ps.dxil"),
        ),
        ShaderFormat::SpirV => (
            include_bytes!("../shaders/forward.vs.spv"),
            include_bytes!("../shaders/forward.ps.spv"),
        ),
        _ => return None,
    };
    Some((
        ShaderModule {
            code: vs,
            entry: "vs_main",
        },
        ShaderModule {
            code: ps,
            entry: "ps_main",
        },
    ))
}
