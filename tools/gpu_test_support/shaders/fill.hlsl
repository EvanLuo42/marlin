// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees fill.cs.dxil. Compiled once with the Windows SDK's dxc (see
// tests/support/mod.rs). Never part of any build.
//
// Writes `base + 3 * i` to word i of a raw buffer reached through the bindless table.

struct Push {
    uint output; // ShaderView index of a writable raw view
    uint base;
};
ConstantBuffer<Push> push : register(b0);

[numthreads(64, 1, 1)]
void main(uint id : SV_DispatchThreadID) {
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    output.Store(id * 4, push.base + id * 3);
}
