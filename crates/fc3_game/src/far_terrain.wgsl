// The coarse far land: the standard lighting with its vertex colours, cut
// away where the streamed terrain lies, with shadows of distant land.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#import fc3::horizon::{remove_hidden_sun, atlas_uv}

struct FarParams {
    // xy: the sector the streamed terrain centres on, z: its radius in sectors.
    range: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> far: FarParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var horizon_atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var horizon_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let p = in.world_position.xyz;
    let sector = floor(vec2<f32>(p.x, -p.z) / 64.0) + vec2<f32>(80.0);
    let d = abs(sector - far.range.xy);
    if far.range.z >= 0.0 && max(d.x, d.y) <= far.range.z {
        discard;
    }
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    let horizon = textureSampleLevel(horizon_atlas, horizon_sampler, atlas_uv(p), 0.0).rg;
    out.color = remove_hidden_sun(out.color, pbr_input, horizon);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
