// Sprites drawn from a storage buffer rather than a mesh rebuilt every
// frame: the batch's mesh is a fixed run of quads whose first UV says which
// quad and which corner each vertex is, and the quads themselves (centre,
// half-extents, frame, normal, colour) are rewritten into `quads` each
// frame. A mesh changed in a frame has Bevy look over every material's
// entities again; a storage buffer rewritten in place doesn't.

#import bevy_pbr::forward_io::{Vertex, VertexOutput}
#import bevy_pbr::mesh_functions
#import bevy_pbr::view_transformations::position_world_to_clip

struct Quad {
    // xyz: its centre (the batch's space); w unused.
    centre: vec4<f32>,
    // Its half-extents across and up.
    x: vec4<f32>,
    y: vec4<f32>,
    // Its frame's UV rectangle: xy the low corner, zw the high.
    uv: vec4<f32>,
    normal: vec4<f32>,
    colour: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(10) var<storage, read> quads: array<Quad>;

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
#ifdef VERTEX_UVS_A
    let index = u32(vertex.uv.x + 0.5);
    let corner = u32(vertex.uv.y + 0.5);
#else
    let index = 0u;
    let corner = 0u;
#endif
    let q = quads[min(index, arrayLength(&quads) - 1u)];
    // (Corners in turn: -x-y, +x-y, +x+y, -x+y; the sheet's V runs down.)
    var sx = -1.0;
    var sy = -1.0;
    var uv = vec2<f32>(q.uv.x, q.uv.w);
    if corner == 1u {
        sx = 1.0;
        uv = vec2<f32>(q.uv.z, q.uv.w);
    } else if corner == 2u {
        sx = 1.0;
        sy = 1.0;
        uv = vec2<f32>(q.uv.z, q.uv.y);
    } else if corner == 3u {
        sy = 1.0;
        uv = vec2<f32>(q.uv.x, q.uv.y);
    }
    let local = q.centre.xyz + q.x.xyz * sx + q.y.xyz * sy;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(local, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = q.normal.xyz;
#ifdef VERTEX_UVS_A
    out.uv = uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vec2<f32>(0.0);
#endif
#ifdef VERTEX_COLORS
    out.color = q.colour;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
    return out;
}
