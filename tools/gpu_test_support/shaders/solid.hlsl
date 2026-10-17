// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees solid.*.dxil and solid.*.spv. Compiled once by hand with dxc
// (commands in marlin-gpu-test-support's src/lib.rs). Never part of any build.
//
// One triangle from SV_VertexID alone, shaded flat. Push constants: color (words 0-3), mode
// (word 4: 0 = covers the whole target, 1 = the lower-left half below the diagonal, 2 = a sliver
// in the top-left pixel of a 4x4 target that misses its center), depth (word 5), a second color
// for dual-source blending (words 8-11).

struct Push {
    float4 color;
    uint mode;
    float depth;
    uint2 unused;
    float4 color1;
};
ConstantBuffer<Push> push : register(b0);

float4 vs_main(uint vertex : SV_VertexID) : SV_Position {
    float2 p;
    uint v = vertex % 3;
    if (push.mode == 0) {
        p = float2(v == 1 ? 3 : -1, v == 2 ? 3 : -1);
    } else if (push.mode == 1) {
        p = float2(v == 1 ? 1 : -1, v == 2 ? 1 : -1);
    } else {
        p = float2(v == 1 ? -0.9 : -0.99, v == 2 ? 0.9 : 0.99);
    }
    return float4(p, push.depth, 1);
}

float4 ps_main() : SV_Target {
    return push.color;
}

// Vulkan takes the second source at location 0, index 1 (D3D12 at SV_Target1).
struct Dual {
#ifdef __spirv__
    [[vk::location(0)]] float4 color0 : SV_Target0;
    [[vk::location(0), vk::index(1)]] float4 color1 : SV_Target1;
#else
    float4 color0 : SV_Target0;
    float4 color1 : SV_Target1;
#endif
};

Dual ps_dual() {
    Dual d;
    d.color0 = push.color;
    d.color1 = push.color1;
    return d;
}

// The pixel position the shader ran at, packed (x | y << 16): coarse shading repeats it.
uint ps_coord(float4 position : SV_Position) : SV_Target {
    return uint(position.x) | (uint(position.y) << 16);
}
