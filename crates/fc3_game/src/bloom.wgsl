// The game's bloom (see `bloom.rs`): a 13-tap downsample, the first one
// keeping only what the world's bloom response gives each pixel's brightness.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Bloom {
    // x: the brightness the response table runs to.
    params: vec4<f32>,
    table: array<vec4<f32>, 16>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var<uniform> bloom: Bloom;

const ENTRIES: f32 = 64.0;
const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

fn entry(i: u32) -> f32 {
    return bloom.table[i / 4u][i % 4u];
}

fn response(x: f32) -> f32 {
    let t = clamp(x / bloom.params.x, 0.0, 1.0) * (ENTRIES - 1.0);
    let i = min(u32(t), u32(ENTRIES) - 2u);
    return mix(entry(i), entry(i + 1u), t - f32(i));
}

fn tap(uv: vec2<f32>, texel: vec2<f32>, x: f32, y: f32) -> vec3<f32> {
    return max(textureSampleLevel(source, linear_sampler, uv + texel * vec2<f32>(x, y), 0.0).rgb, vec3<f32>(0.0));
}

// The 13-tap downsampling filter (overlapping boxes, weighted).
fn shrink(uv: vec2<f32>) -> vec3<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(source));
    let a = tap(uv, texel, -2.0, 2.0);
    let b = tap(uv, texel, 0.0, 2.0);
    let c = tap(uv, texel, 2.0, 2.0);
    let d = tap(uv, texel, -2.0, 0.0);
    let e = tap(uv, texel, 0.0, 0.0);
    let f = tap(uv, texel, 2.0, 0.0);
    let g = tap(uv, texel, -2.0, -2.0);
    let h = tap(uv, texel, 0.0, -2.0);
    let i = tap(uv, texel, 2.0, -2.0);
    let j = tap(uv, texel, -1.0, 1.0);
    let k = tap(uv, texel, 1.0, 1.0);
    let l = tap(uv, texel, -1.0, -1.0);
    let m = tap(uv, texel, 1.0, -1.0);
    return e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625 + (j + k + l + m) * 0.125;
}

@fragment
fn prefilter(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let c = shrink(in.uv);
    let brightness = dot(c, LUMA);
    // As much of it as the response gives that brightness (capped near one half).
    return vec4<f32>(c * (response(brightness) / max(brightness, 1e-4)), 1.0);
}

@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(shrink(in.uv), 1.0);
}
