// Particle sprites: one flipbook texture per batch, tinted and faded by the
// vertex colour, written premultiplied so one blend state serves additive
// sprites (alpha 0), alpha-blended ones and grey sheets whose brightness is
// their coverage.

#import bevy_pbr::forward_io::VertexOutput

struct ParticleParams {
    // x: 0 additive, 1 texture alpha, 2 brightness as alpha.
    mode: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: ParticleParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var sheet: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var sheet_sampler: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = textureSample(sheet, sheet_sampler, in.uv);
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
