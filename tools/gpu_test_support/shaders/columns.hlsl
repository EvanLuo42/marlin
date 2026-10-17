// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees columns.*.dxil and columns.*.spv. Compiled once by hand with dxc
// (commands in marlin-gpu-test-support's src/lib.rs). Never part of any build.
//
// Draws vertical columns into an R32Uint target: column `first` (push-constant word 31, which
// indirect draws set per draw) through `first + span - 1` of `columns`, written as `first + 1 +
// offset`. Six vertices (two triangles) per draw.

struct Push {
    uint4 w[8]; // w[0] = (columns, span, offset, -); w[7].w = first
};
ConstantBuffer<Push> push : register(b0);

struct VsOut {
    float4 position : SV_Position;
    nointerpolation uint value : VALUE;
};

static const float2 corners[6] = {
    float2(0, 0), float2(1, 0), float2(1, 1),
    float2(0, 0), float2(1, 1), float2(0, 1),
};

VsOut vs_main(uint vertex : SV_VertexID) {
    uint columns = push.w[0].x;
    uint span = max(push.w[0].y, 1);
    uint first = push.w[7].w;
    float2 c = corners[vertex % 6];
    float x = lerp(float(first), float(first + span), c.x) / float(columns);
    VsOut o;
    o.position = float4(x * 2 - 1, c.y * 2 - 1, 0.5, 1);
    o.value = first + 1 + push.w[0].z;
    return o;
}

uint ps_main(VsOut i) : SV_Target {
    return i.value;
}
