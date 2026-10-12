// Water on the lens: the view bent through each drop's normal map, blurred
// a little through the water, the rims catching the surroundings.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct LensDrops {
    strengths: vec4<f32>,
    tint: vec4<f32>,
    grid: vec4<f32>,
    rects: array<vec4<f32>, 32>,
    cells: array<vec4<f32>, 32>,
}
@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var shapes: texture_2d<f32>;
@group(0) @binding(3) var normals: texture_2d<f32>;
@group(0) @binding(4) var surroundings: texture_2d<f32>;
@group(0) @binding(5) var<uniform> drops: LensDrops;

fn view_at(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(screen, linear_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let original = textureSampleLevel(screen, linear_sampler, uv, 0.0);
    var bend = vec2<f32>(0.0);
    var cover = 0.0;
    let count = min(u32(drops.tint.w), 32u);
    for (var i = 0u; i < count; i += 1u) {
        let rect = drops.rects[i];
        let local = (uv - rect.xy) / max(rect.zw, vec2<f32>(1e-5));
        if (abs(local.x) > 1.0 || abs(local.y) > 1.0) {
            continue;
        }
        let cell = drops.cells[i];
        let mirror = u32(cell.z);
        var t = local * 0.5 + 0.5;
        var flip = vec2<f32>(1.0);
        if ((mirror & 1u) != 0u) {
            t.x = 1.0 - t.x;
            flip.x = -1.0;
        }
        if ((mirror & 2u) != 0u) {
            t.y = 1.0 - t.y;
            flip.y = -1.0;
        }
        let atlas = (cell.xy + t) / drops.grid.xy;
        let a = textureSampleLevel(shapes, linear_sampler, atlas, 0.0).a * cell.w;
        // Two-channel normal map: across in alpha, up and down in green.
        let packed = textureSampleLevel(normals, linear_sampler, atlas, 0.0);
        bend += (vec2<f32>(packed.a, packed.g) * 2.0 - 1.0) * flip * a;
        cover = max(cover, a);
    }
    if (cover <= 0.001) {
        return original;
    }
    let n = clamp(bend / max(cover, 1e-3), vec2<f32>(-1.0), vec2<f32>(1.0));
    let at = uv + bend * drops.strengths.x;
    // A little blur through the water.
    let texel = 1.0 / vec2<f32>(textureDimensions(screen));
    let r = drops.strengths.y * cover * 2.0 * texel;
    var seen = view_at(at) * 0.4
        + (view_at(at + vec2<f32>(r.x, 0.0)) + view_at(at - vec2<f32>(r.x, 0.0))
        + view_at(at + vec2<f32>(0.0, r.y)) + view_at(at - vec2<f32>(0.0, r.y))) * 0.15;
    // The rims (where the surface turns away) catch the surroundings; the
    // water's own colour, faintly.
    let facing = sqrt(max(1.0 - dot(n, n), 0.0));
    let rim = pow(1.0 - facing, 3.0);
    let around = textureSampleLevel(surroundings, linear_sampler, n * 0.5 + 0.5, 0.0).rgb;
    seen += around * drops.strengths.w * rim;
    seen = mix(seen, drops.tint.rgb, clamp(drops.strengths.z, 0.0, 1.0));
    return vec4<f32>(mix(original.rgb, seen, cover), original.a);
}
