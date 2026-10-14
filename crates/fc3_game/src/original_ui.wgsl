// The original interface's canvas (`original_ui.rs`): Ruffle draws it with
// premultiplied alpha, its colours sRGB-encoded, and it is read here as they
// are (a plain RGBA view): divided by alpha, then decoded, for the UI's
// straight-alpha blending.

#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var canvas: texture_2d<f32>;
@group(1) @binding(1) var canvas_sampler: sampler;

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let t = textureSample(canvas, canvas_sampler, in.uv);
    if t.a <= 0.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(to_linear(clamp(t.rgb / t.a, vec3<f32>(0.0), vec3<f32>(1.0))), t.a);
}
