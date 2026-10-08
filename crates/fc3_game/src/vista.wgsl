// The far forest: each tree's billboard card turned to face the eye about
// its upright, folded away where the trees are loaded as models, and lit
// with the card's rounded normals (see `vista.rs`).

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}
#import fc3::horizon::{remove_hidden_sun, atlas_uv}

struct VistaParams {
    // xy: the sector the loaded trees centre on, z: their radius in sectors
    // (negative: none loaded).
    range: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> vista: VistaParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var horizon_atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var horizon_sampler: sampler;

struct VistaVertex {
    @builtin(instance_index) instance_index: u32,
    // The tree's base (world space).
    @location(0) position: vec3<f32>,
    // The card's normal (x across, y up, z toward the eye).
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // Where this corner lies on the card (metres across and up).
    @location(3) corner: vec2<f32>,
}

@vertex
fn vertex(v: VistaVertex) -> VertexOutput {
    var out: VertexOutput;
    let base = v.position;
    // The tree's sector (Dunia: x east, y north = Bevy -z).
    let sector = floor(vec2<f32>(base.x, -base.z) / 64.0) + vec2<f32>(80.0);
    let d = abs(sector - vista.range.xy);
    let loaded = vista.range.z >= 0.0 && max(d.x, d.y) <= vista.range.z;
    let to_eye = view.world_position.xz - base.xz;
    let f = to_eye / max(length(to_eye), 1e-3);
    let forward = vec3<f32>(f.x, 0.0, f.y);
    let right = vec3<f32>(forward.z, 0.0, -forward.x);
    let up = vec3<f32>(0.0, 1.0, 0.0);
    var world = base + right * v.corner.x + up * v.corner.y;
    if loaded {
        world = base;
    }
    out.world_position = vec4<f32>(world, 1.0);
    out.position = position_world_to_clip(world);
    out.world_normal = normalize(right * v.normal.x + up * v.normal.y + forward * v.normal.z);
#ifdef VERTEX_UVS_A
    out.uv = v.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = v.corner;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = v.instance_index;
#endif
    return out;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    let horizon = textureSampleLevel(horizon_atlas, horizon_sampler, atlas_uv(in.world_position.xyz), 0.0).rg;
    out.color = remove_hidden_sun(out.color, pbr_input, horizon);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
