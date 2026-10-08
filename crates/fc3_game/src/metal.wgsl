// Standard lighting for the game's "Metal" materials, with metalness and
// gloss read from their mask texture: green marks bare metal, red the worn,
// polished edges. Wood and polymer parts (low green) stay rough dielectrics.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var mask_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var mask_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let m = textureSample(mask_texture, mask_sampler, in.uv);
    pbr_input.material.metallic = smoothstep(0.35, 0.85, m.g);
    pbr_input.material.perceptual_roughness = mix(0.82, 0.3, m.r);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
