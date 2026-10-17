// PLACEHOLDER: stands in for a Marlin shader until the in-house shader language and compiler
// exist; the tests only ever see texel_load.cs.dxil and texel_load.cs.spv. Compiled once by hand
// with dxc (commands in tests/gpu/conformance.rs). Never part of any build.
//
// Loads every texel of one mip and layer of a texture, unfiltered, and stores it as four floats
// at `offset + (y * width + x) * 16` of a raw buffer.

struct Push {
    uint texture; // ShaderView index of a read-only 2D array view
    uint output;  // ShaderView index of a writable raw view
    uint width;
    uint height;
    uint mip;
    uint layer;
    uint offset;
};
ConstantBuffer<Push> push : register(b0);

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= push.width || id.y >= push.height) {
        return;
    }
    Texture2DArray<float4> texture = ResourceDescriptorHeap[push.texture];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    float4 texel = texture.Load(int4(id.xy, push.layer, push.mip));
    output.Store4(push.offset + (id.y * push.width + id.x) * 16, asuint(texel));
}
