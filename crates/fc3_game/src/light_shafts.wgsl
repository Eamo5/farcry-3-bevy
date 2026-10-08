// Light shafts: each pixel marches toward the sun's place on screen and
// gathers the brightest light along the way (the sky showing between leaves,
// round the edges of rocks and roofs), fading with distance, so the gaps glow
// with rays spreading out from the sun.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct LightShafts {
    // xy: the sun on screen (uv), z: strength, w: brightness threshold.
    sun: vec4<f32>,
    // rgb: the sunlight's colour, w: the view's aspect ratio.
    colour: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var clamp_sampler: sampler;
@group(0) @binding(2) var<uniform> shafts: LightShafts;

const SAMPLES: i32 = 48;
const DECAY: f32 = 0.96;
// How much of the way to the sun the march covers.
const REACH: f32 = 0.9;

// Light that sends out rays: only what's brighter than the threshold, and
// only round the sun.
fn source(uv: vec2<f32>) -> f32 {
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 0.0;
    }
    let c = textureSampleLevel(screen, clamp_sampler, uv, 0.0).rgb;
    let lum = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let bright = min(max(lum - shafts.sun.w, 0.0), 4.0 * shafts.sun.w);
    let d = length((uv - shafts.sun.xy) * vec2<f32>(shafts.colour.w, 1.0));
    return bright * (1.0 - smoothstep(0.05, 0.75, d));
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let base = textureSampleLevel(screen, clamp_sampler, in.uv, 0.0);
    let step = (shafts.sun.xy - in.uv) * REACH / f32(SAMPLES);
    // Interleaved gradient noise staggers the start against banding.
    let noise = fract(52.9829189 * fract(dot(in.position.xy, vec2<f32>(0.06711056, 0.00583715))));
    var uv = in.uv + step * noise;
    var weight = 1.0;
    var sum = 0.0;
    for (var i = 0; i < SAMPLES; i++) {
        sum += source(uv) * weight;
        weight *= DECAY;
        uv += step;
    }
    let rays = sum / f32(SAMPLES) * shafts.sun.z;
    return vec4<f32>(base.rgb + shafts.colour.rgb * rays, base.a);
}
