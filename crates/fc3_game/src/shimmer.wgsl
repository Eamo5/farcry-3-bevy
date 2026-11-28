// A highlight's flash (see `shimmer.rs`): an object's meshes drawn again
// over themselves, added on in a colour pulsing between two at its rate.
// The pulse runs on the shader's clock, so the material itself never
// changes (a material changed in a frame has Bevy look over every entity of
// its type again).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::main_pass_post_lighting_processing,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::globals,
}

struct ShimmerParams {
    // rgb: the colour at the low of each flash.
    low: vec4<f32>,
    // rgb: the colour at its high; w: flashes a second.
    high: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> shimmer: ShimmerParams;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    let k = 0.5 - 0.5 * cos(6.2831853 * shimmer.high.w * globals.time);
    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, vec4<f32>(mix(shimmer.low.rgb, shimmer.high.rgb, k), 1.0));
    return out;
}
