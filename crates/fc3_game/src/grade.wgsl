// The game's tone mapping and colour grade (see `grade.rs`): each channel of
// the exposed HDR colour through the tone curve (a table, denser toward the
// dark end), then the colour cube, whose display value is written out linear.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Grade {
    // x: the curve's top input, y: the cube's edge, z: the bloom's strength.
    params: vec4<f32>,
    curve: array<vec4<f32>, 64>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var cube: texture_3d<f32>;
@group(0) @binding(3) var<uniform> grade: Grade;
// The game bloom's levels, half to a sixteenth of the view (black without it).
@group(0) @binding(4) var bloom0: texture_2d<f32>;
@group(0) @binding(5) var bloom1: texture_2d<f32>;
@group(0) @binding(6) var bloom2: texture_2d<f32>;
@group(0) @binding(7) var bloom3: texture_2d<f32>;

// A level, smoothed by four bilinear taps half a texel apart (the small ones
// would show their texels otherwise).
fn level(t: texture_2d<f32>, uv: vec2<f32>) -> vec3<f32> {
    let d = 0.5 / vec2<f32>(textureDimensions(t));
    return (textureSampleLevel(t, linear_sampler, uv + vec2<f32>(-d.x, -d.y), 0.0).rgb
        + textureSampleLevel(t, linear_sampler, uv + vec2<f32>(d.x, -d.y), 0.0).rgb
        + textureSampleLevel(t, linear_sampler, uv + vec2<f32>(-d.x, d.y), 0.0).rgb
        + textureSampleLevel(t, linear_sampler, uv + vec2<f32>(d.x, d.y), 0.0).rgb) * 0.25;
}

const ENTRIES: f32 = 256.0;

fn entry(i: u32) -> f32 {
    return grade.curve[i / 4u][i % 4u];
}

fn curve(x: f32) -> f32 {
    let t = sqrt(clamp(x / grade.params.x, 0.0, 1.0)) * (ENTRIES - 1.0);
    let i = min(u32(t), u32(ENTRIES) - 2u);
    return mix(entry(i), entry(i + 1u), t - f32(i));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let hdr = textureSample(screen, linear_sampler, in.uv);
    // The bloom joins the scene before the tone curve, as in the game.
    let bloom = (textureSampleLevel(bloom0, linear_sampler, in.uv, 0.0).rgb + textureSampleLevel(bloom1, linear_sampler, in.uv, 0.0).rgb
        + level(bloom2, in.uv) + level(bloom3, in.uv)) * grade.params.z;
    let c = max(hdr.rgb + bloom, vec3(0.0));
    let u = vec3(curve(c.r), curve(c.g), curve(c.b));
    // Texel centres from the first to the last, as the game samples it.
    let n = grade.params.y;
    let shown = textureSampleLevel(cube, linear_sampler, u * (n - 1.0) / n + 0.5 / n, 0.0).rgb;
    // Half a step of 8-bit dither against banding in the sky.
    let p = in.position.xy;
    let noise = fract(52.9829189 * fract(dot(p, vec2(0.06711056, 0.00583715)))) - 0.5;
    return vec4(to_linear(clamp(shown + noise / 255.0, vec3(0.0), vec3(1.0))), hdr.a);
}
