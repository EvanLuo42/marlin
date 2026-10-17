// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees count.cs.dxil. Compiled once with the Windows SDK's dxc (see
// tests/support/mod.rs). Never part of any build.
//
// Counts threads per dispatch: every thread adds 1 to word `constant` (push-constant word 31,
// set per indirect dispatch) of a raw buffer.

struct Push {
    uint4 w[8]; // w[0].x = output (ShaderView index of a writable raw view); w[7].w = constant
};
ConstantBuffer<Push> push : register(b0);

[numthreads(64, 1, 1)]
void main() {
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.w[0].x];
    uint ignored;
    output.InterlockedAdd(push.w[7].w * 4, 1, ignored);
}
