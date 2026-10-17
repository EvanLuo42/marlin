// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees rayquery.cs.dxil. Compiled once with the Windows SDK's dxc (see
// tests/support/mod.rs). Never part of any build.
//
// Casts `count` rays along +Z from z = -1, spread over x in -2..2 at y = 0.1, against a top-level
// acceleration structure reached through the bindless table. Word i of the output is
// `InstanceID << 16 | PrimitiveIndex` of ray i's closest hit, or ~0 for a miss.

struct Push {
    uint tlas;   // AccelerationStructure index
    uint output; // ShaderView index of a writable raw view
    uint count;
};
ConstantBuffer<Push> push : register(b0);

[numthreads(64, 1, 1)]
void main(uint id : SV_DispatchThreadID) {
    if (id >= push.count) {
        return;
    }
    RaytracingAccelerationStructure tlas = ResourceDescriptorHeap[push.tlas];
    RWByteAddressBuffer output = ResourceDescriptorHeap[push.output];
    RayDesc ray;
    ray.Origin = float3((id + 0.5) / push.count * 4 - 2, 0.1, -1);
    ray.Direction = float3(0, 0, 1);
    ray.TMin = 0;
    ray.TMax = 100;
    RayQuery<RAY_FLAG_FORCE_OPAQUE | RAY_FLAG_CULL_NON_OPAQUE> query;
    query.TraceRayInline(tlas, RAY_FLAG_NONE, 0xff, ray);
    query.Proceed();
    uint result = ~0u;
    if (query.CommittedStatus() == COMMITTED_TRIANGLE_HIT) {
        result = (query.CommittedInstanceID() << 16) | query.CommittedPrimitiveIndex();
    }
    output.Store(id * 4, result);
}
