// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the test
// harness only ever sees the mesh.*.dxil blobs. Compiled once with the Windows SDK's dxc (see
// tests/support/mod.rs). Never part of any build.
//
// Mesh shading into an R32Uint target of `columns` columns: each mesh group emits one quad
// covering one column, written as `column + 1`.
// - ms_main: column = first (push-constant word 31, set per indirect dispatch) + group id.
// - as_main + ms_amplified: each amplification group launches 2 mesh groups, for columns
//   first + 2 * group id and the one after it.

struct Push {
    uint4 w[8]; // w[0].x = columns; w[7].w = first
};
ConstantBuffer<Push> push : register(b0);

struct VsOut {
    float4 position : SV_Position;
    nointerpolation uint value : VALUE;
};

struct Payload {
    uint first;
};

#define QUAD(column)                                                                  \
    SetMeshOutputCounts(4, 2);                                                        \
    float x0 = float(column) / float(push.w[0].x) * 2 - 1;                            \
    float x1 = float(column + 1) / float(push.w[0].x) * 2 - 1;                        \
    verts[0].position = float4(x0, -1, 0.5, 1);                                       \
    verts[1].position = float4(x1, -1, 0.5, 1);                                       \
    verts[2].position = float4(x1, 1, 0.5, 1);                                        \
    verts[3].position = float4(x0, 1, 0.5, 1);                                        \
    [unroll] for (uint v = 0; v < 4; v++) verts[v].value = column + 1;                \
    tris[0] = uint3(0, 1, 2);                                                         \
    tris[1] = uint3(0, 2, 3);

[outputtopology("triangle")]
[numthreads(1, 1, 1)]
void ms_main(uint3 group : SV_GroupID, out vertices VsOut verts[4], out indices uint3 tris[2]) {
    uint column = push.w[7].w + group.x;
    QUAD(column)
}

groupshared Payload shared_payload;

[numthreads(1, 1, 1)]
void as_main(uint3 group : SV_GroupID) {
    shared_payload.first = push.w[7].w + group.x * 2;
    DispatchMesh(2, 1, 1, shared_payload);
}

[outputtopology("triangle")]
[numthreads(1, 1, 1)]
void ms_amplified(
    uint3 group : SV_GroupID,
    in payload Payload p,
    out vertices VsOut verts[4],
    out indices uint3 tris[2]
) {
    uint column = p.first + group.x;
    QUAD(column)
}

uint ps_main(VsOut i) : SV_Target {
    return i.value;
}
