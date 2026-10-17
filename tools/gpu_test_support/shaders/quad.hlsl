// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees quad.*.dxil and quad.*.spv. Compiled once by hand with dxc
// (commands in marlin-gpu-test-support's src/lib.rs). Never part of any build.
//
// Exercises the whole interface: vertex pulling from a structured buffer, a uniform buffer, a
// sampled texture and a sampler, all reached through push-constant indices.

struct Push {
    uint vertices; // StructuredBuffer<float2>
    uint uniforms; // ConstantBuffer<Uniforms>
    uint texture;  // Texture2D<float4>
    uint sampler_; // SamplerState
    float depth;
};
ConstantBuffer<Push> push : register(b0);

struct Uniforms {
    float4 tint;
};

struct VsOut {
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

VsOut vs_main(uint vertex : SV_VertexID) {
    StructuredBuffer<float2> vertices = ResourceDescriptorHeap[push.vertices];
    float2 p = vertices[vertex];
    VsOut o;
    o.position = float4(p, push.depth, 1.0);
    o.uv = p * float2(0.5, -0.5) + 0.5;
    return o;
}

float4 ps_main(VsOut i) : SV_Target {
    ConstantBuffer<Uniforms> u = ResourceDescriptorHeap[push.uniforms];
    Texture2D<float4> t = ResourceDescriptorHeap[push.texture];
    SamplerState s = SamplerDescriptorHeap[push.sampler_];
    return t.SampleLevel(s, i.uv, 0) * u.tint;
}
