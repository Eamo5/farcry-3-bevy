// Particle sprites: one flipbook texture per batch, tinted and faded by the
// vertex colour, written premultiplied so one blend state serves additive
// sprites (alpha 0), alpha-blended ones and grey sheets whose brightness is
// their coverage.

#import bevy_pbr::forward_io::VertexOutput

struct ParticleParams {
    // x: blend mode; yz: optional flipbook grid; w: interpolate frames.
    mode: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: ParticleParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var sheet: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var sheet_sampler: sampler;

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
    let c = in.color;
#else
    let c = vec4<f32>(1.0);
#endif
    let mode = params.mode.x;
    if mode < 0.5 {
        // Additive: no coverage, just light.
        return vec4<f32>(t.rgb * c.rgb * c.a, 0.0);
    }
    var a = t.a;
    var rgb = t.rgb;
    if mode > 1.5 {
        a = max(max(t.r, t.g), t.b);
        rgb = vec3<f32>(1.0);
    }
    a *= c.a;
    return vec4<f32>(rgb * c.rgb * a, a);
}
