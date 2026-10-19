// Particle sprites: one flipbook texture per batch, tinted and faded by the
// vertex colour, written premultiplied so one blend state serves additive
// sprites (alpha 0), alpha-blended ones and grey sheets whose brightness is
// their coverage. Soft ones fade where they near what's behind them (with
// the depth prepass).

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::prepass_utils
#import bevy_pbr::view_transformations::depth_ndc_to_view_z

struct ParticleParams {
    // x: blend mode; yz: optional flipbook grid; w: interpolate frames.
    mode: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: ParticleParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var sheet: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var sheet_sampler: sampler;
// x: the depth (m) a soft sprite fades over as it nears what's behind it
// (0: cut hard); y: 1 when it dissolves as it fades.
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var<uniform> soft: vec4<f32>;

// How much of a sprite shows for its nearness to the scene behind it.
fn softness(frag_coord: vec4<f32>) -> f32 {
#ifdef DEPTH_PREPASS
    if soft.x <= 0.0 {
        return 1.0;
    }
    let scene = depth_ndc_to_view_z(prepass_utils::prepass_depth(frag_coord, 0u));
    let sprite = depth_ndc_to_view_z(frag_coord.z);
    return clamp((sprite - scene) / soft.x, 0.0, 1.0);
#else
    return 1.0;
#endif
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var t = textureSample(sheet, sheet_sampler, in.uv);
#ifdef VERTEX_UVS_B
    if params.mode.w > 0.5 {
        let grid = max(params.mode.yz, vec2<f32>(1.0));
        // UVs can lie exactly on the right/bottom boundary of a cell.
        // Carry the sprite's frame explicitly rather than floor those UVs.
        let index = round(in.uv_b.y);
        let cell = vec2<f32>(index % grid.x, floor(index / grid.x));
        let local_uv = in.uv * grid - cell;
        let inset = 0.5 * grid / vec2<f32>(textureDimensions(sheet));
        let safe_uv = clamp(local_uv, inset, vec2<f32>(1.0) - inset);
        t = textureSample(sheet, sheet_sampler, (cell + safe_uv) / grid);
        let next_index = (index + 1.0) % (grid.x * grid.y);
        let next_cell = vec2<f32>(next_index % grid.x, floor(next_index / grid.x));
        let next = textureSample(sheet, sheet_sampler, (next_cell + safe_uv) / grid);
        t = mix(t, next, clamp(in.uv_b.x, 0.0, 1.0));
    }
#endif
#ifdef VERTEX_COLORS
    var c = in.color;
#else
    var c = vec4<f32>(1.0);
#endif
    let near = softness(in.position);
    let mode = params.mode.x;
    if mode < 0.5 {
        // Additive: no coverage, just light.
        return vec4<f32>(t.rgb * c.rgb * c.a * near, 0.0);
    }
    var a = t.a;
    var rgb = t.rgb;
    if mode > 1.5 {
        a = max(max(t.r, t.g), t.b);
        rgb = vec3<f32>(1.0);
    }
    if soft.y > 0.5 {
        // Dissolving: what's left of its coverage, the thin parts gone
        // first (all of it at full alpha, none at none).
        a = clamp((a - (1.0 - c.a)) / max(c.a, 0.001), 0.0, 1.0);
    } else {
        a *= c.a;
    }
    a *= near;
    return vec4<f32>(rgb * c.rgb * a, a);
}
