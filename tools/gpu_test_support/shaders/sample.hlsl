// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees sample.*.dxil and sample.*.spv. Compiled once by hand with dxc
// (commands in marlin-gpu-test-support's src/lib.rs). Never part of any build.
//
// Reads textures through views and samplers the tests make, writing what it saw to a raw buffer.

struct Push {
    uint texture; // ShaderView index
    uint sampler; // Sampler index
    uint output;  // ShaderView index of a writable raw view
    uint x;
    float u;
    float v;
};
ConstantBuffer<Push> push : register(b0);

// One filtered sample of a float texture at (u, v), mip 0.
[numthreads(1, 1, 1)]
void sample_main() {
    Texture2D<float> t = ResourceDescriptorHeap[push.texture];
    SamplerState s = SamplerDescriptorHeap[push.sampler];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    output.Store(0, asuint(t.SampleLevel(s, float2(push.u, push.v), 0)));
}

// Texels (0, 0) through (3, 0) of a stencil view.
[numthreads(4, 1, 1)]
void stencil_main(uint id : SV_DispatchThreadID) {
    Texture2D<uint> t = ResourceDescriptorHeap[push.texture];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    output.Store(id * 4, t.Load(int3(id, 0, 0)));
}

// The red of each of 4 samples of pixel (x, x) of a multisampled texture, as 0..255.
[numthreads(4, 1, 1)]
void ms_main(uint id : SV_DispatchThreadID) {
    Texture2DMS<float4> t = ResourceDescriptorHeap[push.texture];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    output.Store(id * 4, uint(t.Load(int2(push.x, push.x), id).r * 255 + 0.5));
}

// The red of cube `x` of a cube array, seen along +X, as 0..255.
[numthreads(1, 1, 1)]
void cube_main() {
    TextureCubeArray<float4> t = ResourceDescriptorHeap[push.texture];
    SamplerState s = SamplerDescriptorHeap[push.sampler];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    float4 c = t.SampleLevel(s, float4(1, 0, 0, float(push.x)), 0);
    output.Store(0, uint(c.r * 255 + 0.5));
}
