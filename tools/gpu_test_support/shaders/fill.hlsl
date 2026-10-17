// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees fill.*.dxil and fill.*.spv. Compiled once by hand with dxc
// (commands in marlin-gpu-test-support's src/lib.rs). Never part of any build.
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
