// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees volume.*.dxil and volume.*.spv. Compiled once by hand with dxc
// (commands in marlin-gpu-test-support's src/lib.rs). Never part of any build.
//
// 3D textures through the bindless table, `size` texels on a side:
// - write_main: texel (x, y, z) of an R32Uint volume UAV = x + 16 * y + 256 * z + 1.
// - read_main: loads every texel of a volume SRV into a raw buffer, x fastest.

struct Push {
    uint4 w[8]; // w[0] = (volume, output, size, -)
};
ConstantBuffer<Push> push : register(b0);

[numthreads(4, 4, 4)]
void write_main(uint3 id : SV_DispatchThreadID) {
    RWTexture3D<uint> volume = ResourceDescriptorHeap[push.w[0].x];
    if (all(id < push.w[0].z)) {
        volume[id] = id.x + 16 * id.y + 256 * id.z + 1;
    }
}

[numthreads(4, 4, 4)]
void read_main(uint3 id : SV_DispatchThreadID) {
    Texture3D<uint> volume = ResourceDescriptorHeap[push.w[0].x];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.w[0].y];
    uint size = push.w[0].z;
    if (all(id < size)) {
        output.Store(((id.z * size + id.y) * size + id.x) * 4, volume.Load(int4(id, 0)));
    }
}
