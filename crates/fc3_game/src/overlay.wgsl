// The game's screen overlays (`overlays.rs`): a texture drawn over the view,
// tinted, either as it is or with its brightness as how much it covers (the
// scopes' masks).

#import bevy_ui::ui_vertex_output::UiVertexOutput

struct OverlayParams {
    // Tint (rgb) and strength (a).
    color: vec4<f32>,
    // x: 1 when the texture's brightness is its cover.
    mode: vec4<f32>,
}

@group(1) @binding(0) var<uniform> params: OverlayParams;
@group(1) @binding(1) var image: texture_2d<f32>;
@group(1) @binding(2) var image_sampler: sampler;

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let t = textureSample(image, image_sampler, in.uv);
    if params.mode.x > 0.5 {
        let cover = dot(t.rgb, vec3<f32>(0.299, 0.587, 0.114));
        return vec4<f32>(params.color.rgb, cover * params.color.a);
    }
    return vec4<f32>(t.rgb * params.color.rgb, t.a * params.color.a);
}
