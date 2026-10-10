// The player's arm with the tattoo's earned pieces: the standard PBR path,
// its base colour multiplied by the tattoo's design (second UV set) where the
// mask's piece id is revealed (see `tattoo.rs`).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

struct TattooReveal {
    bits: array<vec4<u32>, 2>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> reveal: TattooReveal;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var design_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var design_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var mask_texture: texture_2d<f32>;

fn shown(id: u32) -> bool {
    let word = reveal.bits[id / 128u][(id / 32u) % 4u];
    return id > 0u && ((word >> (id % 32u)) & 1u) == 1u;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
#ifdef VERTEX_UVS_B
    let uv = in.uv_b;
    let size = vec2<f32>(textureDimensions(mask_texture));
    let texel = clamp(vec2<i32>(fract(uv) * size), vec2<i32>(0), vec2<i32>(size) - vec2<i32>(1));
    let id = u32(round(textureLoad(mask_texture, texel, 0).r * 255.0));
    if shown(id) {
        let ink = textureSample(design_texture, design_sampler, uv);
        pbr_input.material.base_color = vec4<f32>(pbr_input.material.base_color.rgb * ink.rgb, pbr_input.material.base_color.a);
    }
#endif
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
