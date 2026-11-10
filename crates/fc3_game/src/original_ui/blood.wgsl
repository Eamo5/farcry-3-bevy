// The blood on the screen (`original_ui/blood.rs`): Ruffle draws it over
// white, its colours sRGB-encoded and read here as stored (a plain RGBA
// view); decoded, it multiplies the scene under it (the pipeline's blend),
// as the movie's multiply blends did over the game.

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
    return vec4<f32>(to_linear(clamp(t.rgb, vec3<f32>(0.0), vec3<f32>(1.0))), 1.0);
}
