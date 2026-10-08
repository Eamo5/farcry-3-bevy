// The world's depth of field (see `dof.rs`): each pixel takes some of a disc
// blur of what is around it, as much as its distance calls for (from nothing
// at the focus distance to the blur scale at the far distance). Taps nearer
// than the pixel (sharper) are left out of its blur, so near things don't
// bleed into far ones.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct DistanceBlur {
    // x: focus distance, y: far distance (m), z: blur scale, w: blur radius
    // (fraction of the view's height).
    params: vec4<f32>,
    // x: the camera's near plane, y: 1 to blur the sky too.
    extra: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var depth: texture_depth_2d;
@group(0) @binding(3) var<uniform> dof: DistanceBlur;

// How much blur a pixel takes, from the depth there (reverse Z, infinite far
// plane: 0 is the sky).
fn blur_at(p: vec2<i32>) -> f32 {
    let size = vec2<i32>(textureDimensions(depth));
    let z = textureLoad(depth, clamp(p, vec2<i32>(0), size - vec2<i32>(1)), 0);
    if z <= 0.0 {
        return select(0.0, dof.params.z, dof.extra.y > 0.5);
    }
    let distance = dof.extra.x / z;
    return clamp((distance - dof.params.x) / (dof.params.y - dof.params.x), 0.0, 1.0) * dof.params.z;
}

const TAPS: i32 = 12;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let sharp = textureSample(screen, linear_sampler, in.uv);
    let p = vec2<i32>(in.position.xy);
    let amount = blur_at(p);
    if amount <= 0.001 {
        return sharp;
    }
    let size = vec2<f32>(textureDimensions(screen));
    let radius = dof.params.w * size.y;
    var sum = sharp.rgb;
    var weight = 1.0;
    for (var i = 0; i < TAPS; i++) {
        // A disc of taps on a golden-angle spiral.
        let r = sqrt((f32(i) + 0.5) / f32(TAPS)) * radius;
        let a = f32(i) * 2.39996323;
        let offset = vec2<f32>(cos(a), sin(a)) * r;
        let q = in.position.xy + offset;
        let w = select(0.0, 1.0, blur_at(vec2<i32>(q)) >= amount * 0.5);
        sum += textureSampleLevel(screen, linear_sampler, q / size, 0.0).rgb * w;
        weight += w;
    }
    return vec4<f32>(mix(sharp.rgb, sum / weight, clamp(amount, 0.0, 1.0)), sharp.a);
}
