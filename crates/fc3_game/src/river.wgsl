// Rivers: a ribbon along the river's spline over its water plane, carrying
// the current: the river material's normal map scrolled downstream at its
// flow speed, and in rapids its foam, fading out at the banks and the ends.
//
// UV0: x metres across, y metres along (downstream); vertex colour: r 0..1
// across, a the section's weight.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    mesh_view_bindings::{globals, view},
}

struct RiverParams {
    // rgb: the water's colour (linear), a: how much foam (rapids).
    colour: vec4<f32>,
    // x: metres per tile of the maps, y: flow speed (m/s).
    flow: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> river: RiverParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var ripples: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var wrap_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var foam: texture_2d<f32>;

fn decode_normal(s: vec4<f32>) -> vec2<f32> {
    // Two-channel normal maps keep X in alpha and Y in green.
    return vec2<f32>(s.a, s.g) * 2.0 - 1.0;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, is_front);
    let t = globals.time;
    let tile = max(river.flow.x, 1.0);
    let speed = river.flow.y;
    let uv = in.uv;
    let p = in.world_position.xyz;
    let dist = distance(p, view.world_position.xyz);

    // Two layers drifting downstream at different rates.
    let a = decode_normal(textureSample(ripples, wrap_sampler, vec2<f32>(uv.x, uv.y - t * speed) / tile));
    let b = decode_normal(textureSample(ripples, wrap_sampler, vec2<f32>(uv.x * 0.61 + 0.37, uv.y * 0.61 - t * speed * 0.8) / tile));
    let detail = (a * 0.6 + b * 0.4) * (1.0 - smoothstep(60.0, 300.0, dist) * 0.8);

    // The ribbon's own frame from the screen derivatives: across and along.
    let n0 = normalize(in.world_normal);
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);
    let dp2perp = cross(dp2, n0);
    let dp1perp = cross(n0, dp1);
    let tangent = dp2perp * duv1.x + dp1perp * duv2.x;
    let bitangent = dp2perp * duv1.y + dp1perp * duv2.y;
    let scale = inverseSqrt(max(max(dot(tangent, tangent), dot(bitangent, bitangent)), 1e-12));
    var n = normalize(n0 + (tangent * detail.x + bitangent * detail.y) * scale * 0.5);
    if !is_front {
        n = -n;
    }

    // Faded at the banks and where the river's pieces meet.
    var edge = 1.0;
#ifdef VERTEX_COLORS
    let across = in.color.r;
    edge = smoothstep(0.0, 0.18, across) * smoothstep(1.0, 0.82, across) * in.color.a;
#endif

    // Rapids: foam streaming with the current.
    let f = textureSample(foam, wrap_sampler, vec2<f32>(uv.x, uv.y - t * speed * 1.1) / (tile * 0.75));
    // (Foam is light on dark: a material without foam names a black map.)
    let foam_amt = clamp(max(f.r, max(f.g, f.b)) * f.a * 1.6, 0.0, 1.0) * river.colour.a * (1.0 - smoothstep(80.0, 250.0, dist));

    let v = normalize(view.world_position.xyz - p);
    let fresnel = pow(1.0 - clamp(dot(v, n), 0.0, 1.0), 5.0);
    let colour = mix(river.colour.rgb, vec3<f32>(0.85, 0.88, 0.86), foam_amt);
    let alpha = clamp(0.3 + 0.6 * fresnel + 0.6 * foam_amt, 0.0, 0.95) * edge;
    pbr.material.base_color = vec4<f32>(colour, alpha);
    pbr.material.perceptual_roughness = mix(0.05, 0.5, foam_amt);
    pbr.N = n;
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);
    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
