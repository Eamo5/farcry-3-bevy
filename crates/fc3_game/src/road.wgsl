// Roads: the road's own map tiled across and along the ribbon, dyed by the
// material's colour, laid over the ground through its coverage map (ragged
// edges, cracks), which spans the ribbon once.
//
// UV0: x 0..1 across the ribbon, y the distance along it in ribbon widths;
// vertex colour alpha: the section's weight.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}

struct RoadParams {
    // rgb: the dye (linear), w: how many times the map repeats across.
    tint: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> road: RoadParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var road_map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var road_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var coverage: texture_2d<f32>;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, is_front);
    let uv = in.uv;
    // Texels kept square: a map h/w times as tall as wide covers that many
    // widths along.
    let map_size = vec2<f32>(textureDimensions(road_map));
    let cov_size = vec2<f32>(textureDimensions(coverage));
    let across = max(road.tint.w, 1.0);
    let map_uv = vec2<f32>(uv.x * across, uv.y * across * map_size.x / map_size.y);
    let cov_uv = vec2<f32>(uv.x, uv.y * cov_size.x / cov_size.y);
    let ground = textureSample(road_map, road_sampler, map_uv);
    let cov = textureSample(coverage, road_sampler, cov_uv);
    // Coverage maps whose colour is lighter than their alpha carry cracks and
    // wear in it; the rest are plain masks.
    let wear = clamp(cov.rgb / max(cov.a, 0.02), vec3<f32>(0.0), vec3<f32>(1.0));
    var alpha = cov.a;
#ifdef VERTEX_COLORS
    // Faded in and out where roads meet.
    alpha *= in.color.a;
#endif
    pbr.material.base_color = vec4<f32>(ground.rgb * road.tint.rgb * wear, alpha);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);
    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
