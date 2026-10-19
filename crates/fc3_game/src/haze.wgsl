// Heat haze: particles that bend the view behind them. A sprite's shape (its
// sheet's red, or its alpha) times its alpha is how much it bends; its
// distortion map, scrolling and tiled across it, which way. What's behind is
// the scene as it stood before the transparent things (the view's
// transmission texture), looked up that far off and a little blurred, and
// laid over what's there by how much it bends.

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, globals, view_transmission_texture, view_transmission_sampler}
#import bevy_pbr::prepass_utils
#import bevy_pbr::view_transformations::depth_ndc_to_view_z

// xy: the map's scroll (a second); zw: its tiling across the sprite.
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> motion: vec4<f32>;
// x: how far it bends (a share of the view); y: how much it blurs; z: the
// depth (m) it fades over nearing what's behind (0: cut hard); w: 1 when the
// shape is the sheet's red, else its alpha.
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> look: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var shape: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var shape_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var map_sampler: sampler;

// How much of a sprite shows for its nearness to the scene behind it.
fn softness(frag_coord: vec4<f32>) -> f32 {
#ifdef DEPTH_PREPASS
    if look.z <= 0.0 {
        return 1.0;
    }
    let scene = depth_ndc_to_view_z(prepass_utils::prepass_depth(frag_coord, 0u));
    let sprite = depth_ndc_to_view_z(frag_coord.z);
    return clamp((sprite - scene) / look.z, 0.0, 1.0);
#else
    return 1.0;
#endif
}

fn behind(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(view_transmission_texture, view_transmission_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let s = textureSample(shape, shape_sampler, in.uv);
    let bend = textureSample(map, map_sampler, in.uv * motion.zw + motion.xy * globals.time).rg * 2.0 - 1.0;
    var amount = select(s.a, s.r, look.w > 0.5);
#ifdef VERTEX_COLORS
    amount *= in.color.a;
#endif
    amount = clamp(amount * softness(in.position), 0.0, 1.0);
    let offset = bend * look.x * amount;
    let uv = (in.position.xy - view.viewport.xy) / view.viewport.zw;
    var c = behind(uv + offset);
    // (Blurred across as far again as it's bent, by its blur.)
    let r = vec2<f32>(length(offset) * look.y);
    if r.x > 0.0 {
        c = (c * 2.0 + behind(uv + offset + vec2<f32>(r.x, 0.0)) + behind(uv + offset - vec2<f32>(r.x, 0.0))
            + behind(uv + offset + vec2<f32>(0.0, r.y)) + behind(uv + offset - vec2<f32>(0.0, r.y))) / 6.0;
    }
    return vec4<f32>(c * amount, amount);
}
