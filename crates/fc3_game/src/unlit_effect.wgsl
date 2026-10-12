// An additive `Unlit` effect (see `unlit_effects.rs`): the standard
// material's dyed map, moved by the material's UV animation, times its HDR
// multiplier and faded toward grazing angles by its normal attenuation.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::main_pass_post_lighting_processing,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::{view, globals},
}

struct UnlitEffectParams {
    // HDRMul, NormalAttenuationPower, AnimType, AngularSpeed
    shade: vec4<f32>,
    // USpeed, VSpeed
    scroll: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> effect: UnlitEffectParams;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var moved = in;
#ifdef VERTEX_UVS_A
    let t = globals.time;
    let kind = u32(effect.shade.z + 0.5);
    if kind == 1u || kind == 2u {
        // Scrolling.
        moved.uv = in.uv + effect.scroll.xy * t;
    } else if kind == 3u {
        // Back and forth.
        moved.uv = in.uv + effect.scroll.xy * (1.0 - abs(fract(t * 0.5) * 2.0 - 1.0));
    } else if kind == 4u {
        // Turning about the map's middle.
        let a = effect.shade.w * t;
        let d = in.uv - vec2<f32>(0.5);
        moved.uv = vec2<f32>(cos(a) * d.x - sin(a) * d.y, sin(a) * d.x + cos(a) * d.y) + vec2<f32>(0.5);
    }
#endif
    var pbr_input = pbr_input_from_standard_material(moved, is_front);
    var fade = 1.0;
    if effect.shade.y > 0.0 {
        let to_eye = normalize(view.world_position - in.world_position.xyz);
        fade = pow(abs(dot(normalize(in.world_normal), to_eye)), effect.shade.y);
    }
    let base = pbr_input.material.base_color;
    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, vec4<f32>(base.rgb * effect.shade.x * fade, base.a));
    return out;
}
