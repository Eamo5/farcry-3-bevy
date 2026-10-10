// Experimental radial kernel; authored masks/curves are preserved, while
// tap spacing and strength calibration still need original-game comparison.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct ScreenBlur {
    params: vec4<f32>,
    colour: vec4<f32>,
    direction: vec4<f32>,
}
@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var mask: texture_2d<f32>;
@group(0) @binding(3) var<uniform> blur: ScreenBlur;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let original = textureSample(screen, linear_sampler, uv);
    let mask_value = textureSample(mask, linear_sampler, uv).r;
    let amount = clamp(mask_value + blur.params.z - 1.0, 0.0, 1.0) * clamp(blur.params.y, 0.0, 1.0);
    let radial = (uv - vec2<f32>(0.5)) * blur.direction.xy * min(max(blur.params.x, 0.0) * 0.01, 0.5);
    var sum = vec3<f32>(0.0);
    for (var i = 0; i < 16; i += 1) {
        let offset = (f32(i) / 15.0 - 0.5) * radial;
        sum += textureSample(screen, linear_sampler, clamp(uv + offset, vec2<f32>(0.0), vec2<f32>(1.0))).rgb;
    }
    let tinted = mix(sum / 16.0, blur.colour.rgb, clamp(blur.colour.a, 0.0, 1.0));
    return vec4<f32>(mix(original.rgb, tinted, amount), original.a);
}
