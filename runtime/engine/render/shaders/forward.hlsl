// PLACEHOLDER: stands in for a Marlin shader until the Marlin shader compiler exists; the engine
// only ever sees forward.vs.dxil and forward.ps.dxil. Compiled once with the Windows SDK's dxc
// (see src/shaders.rs). Never part of any build. Interface: docs/design/0002-shader-interface.md.
//
// Forward opaque pass: vertex pulling, per-instance transforms and colors, one directional light
// plus ambient.

struct Push {
    uint frame;         // ConstantBuffer<Frame>
    uint instances;     // StructuredBuffer<Instance>
    uint vertices;      // StructuredBuffer<Vertex>
    uint base_instance; // first instance of this draw in `instances`
};
ConstantBuffer<Push> push : register(b0);

// Matrices are column-major, as marlin-math stores them; `mul(M, v)` is the column-vector product.
struct Frame {
    float4x4 view_projection;
    float4 light_direction; // xyz: direction the light travels, world space
    float4 light_color;     // rgb: linear radiance
    float4 ambient;         // rgb: linear radiance
};

struct Instance {
    float4x4 model;
    float4 color; // linear RGBA
};

struct Vertex {
    float3 position;
    float3 normal;
};

struct VsOut {
    float4 position : SV_Position;
    float3 normal : NORMAL;
    float4 color : COLOR;
};

VsOut vs_main(uint vertex_id : SV_VertexID, uint instance_id : SV_InstanceID) {
    ConstantBuffer<Frame> frame = ResourceDescriptorHeap[push.frame];
    StructuredBuffer<Instance> instances = ResourceDescriptorHeap[push.instances];
    StructuredBuffer<Vertex> vertices = ResourceDescriptorHeap[push.vertices];

    Instance instance = instances[push.base_instance + instance_id];
    Vertex vertex = vertices[vertex_id];
    float4 world = mul(instance.model, float4(vertex.position, 1.0));

    VsOut o;
    o.position = mul(frame.view_projection, world);
    // Exact for rotation and uniform scale; non-uniform scale needs the inverse transpose.
    o.normal = mul((float3x3)instance.model, vertex.normal);
    o.color = instance.color;
    return o;
}

float4 ps_main(VsOut i) : SV_Target {
    ConstantBuffer<Frame> frame = ResourceDescriptorHeap[push.frame];
    float3 n = normalize(i.normal);
    float lit = saturate(dot(n, -frame.light_direction.xyz));
    float3 radiance = frame.ambient.rgb + frame.light_color.rgb * lit;
    return float4(i.color.rgb * radiance, i.color.a);
}
