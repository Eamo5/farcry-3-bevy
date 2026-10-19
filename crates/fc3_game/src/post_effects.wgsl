// The game's screen post-effects in one pass (see post_effects.rs). The
// colour masks' blend modes, the desaturation, the RGB separation and the
// masked blur's composite follow the game's own pixel shaders; the
// original squares its gamma-space source to work in (near) linear light,
// which this pass is already in after tone mapping.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct PostView {
    blur: array<vec4<f32>, 3>,
    masks: array<vec4<f32>, 8>,
    desat: vec4<f32>,
    burn: array<vec4<f32>, 2>,
    rgb: array<vec4<f32>, 4>,
    hue: vec4<f32>,
    noise: vec4<f32>,
    distort: array<vec4<f32>, 2>,
    screen: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var clamped: sampler;
@group(0) @binding(2) var blur_mask: texture_2d<f32>;
@group(0) @binding(3) var mask_a: texture_2d<f32>;
@group(0) @binding(4) var mask_b: texture_2d<f32>;
@group(0) @binding(5) var rgb_mask: texture_2d<f32>;
@group(0) @binding(6) var noise_texture: texture_2d<f32>;
@group(0) @binding(7) var distort_texture: texture_2d<f32>;
@group(0) @binding(8) var repeating: sampler;
@group(0) @binding(9) var<uniform> fx: PostView;

const LUMA = vec3<f32>(0.3, 0.59, 0.11);

// A 12-tap disc (unit radius) for the 2D blurs.
const DISC = array<vec2<f32>, 12>(
    vec2<f32>(-0.326, -0.406), vec2<f32>(-0.840, -0.074), vec2<f32>(-0.696, 0.457), vec2<f32>(-0.203, 0.621),
    vec2<f32>(0.962, -0.195), vec2<f32>(0.473, -0.480), vec2<f32>(0.519, 0.767), vec2<f32>(0.185, -0.893),
    vec2<f32>(0.507, 0.064), vec2<f32>(0.896, 0.412), vec2<f32>(-0.322, -0.933), vec2<f32>(-0.792, -0.598),
);

fn source(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(screen, clamped, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
}

// A channel's place under RGB separation: zoomed about the centre by its
// scale, moved by its offset (pixels).
fn separated(uv: vec2<f32>, p: vec4<f32>) -> vec2<f32> {
    return (uv - 0.5) / max(abs(p.xy), vec2<f32>(0.01)) + 0.5 + p.zw * fx.screen.xy;
}

// The blur by its kind (`selBlurType`: Gaussian, Radial, Diamond,
// Directional, GaussianSeparate), `factor` its strength.
fn blurred(uv: vec2<f32>, factor: f32, kind: f32, dir: vec2<f32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    if kind > 0.5 && kind < 1.5 {
        // Radial: along the line from the centre, each axis by its
        // direction (20 by 5 for speed); each tap spread a little more, as
        // the game blurs a smaller copy of the screen.
        let span = (uv - 0.5) * dir * factor * 0.003;
        let soft = factor * 3.0 * fx.screen.xy;
        for (var i = 0; i < 16; i += 1) {
            sum += source(uv + (f32(i) / 15.0 - 0.5) * span + DISC[i % 12] * soft);
        }
        return sum / 16.0;
    }
    if kind > 2.5 && kind < 3.5 {
        let span = dir * factor * 8.0 * fx.screen.xy;
        for (var i = 0; i < 16; i += 1) {
            sum += source(uv + (f32(i) / 15.0 - 0.5) * span);
        }
        return sum / 16.0;
    }
    // The rest: a soft disc (the game blurs a quarter-size copy).
    let radius = factor * 10.0 * fx.screen.xy;
    sum = source(uv);
    for (var i = 0; i < 12; i += 1) {
        sum += source(uv + DISC[i] * radius) + source(uv - DISC[(i + 5) % 12] * radius * 0.5);
    }
    return sum / 25.0;
}

// A colour mask (`k` 0 or 1) over `c`, in the game's blend modes
// (`selBlendType`): none and alpha lerp, then additive, screen, multiply,
// multiply 2x and subtractive.
fn colour_mask(c: vec3<f32>, k: u32, uv: vec2<f32>) -> vec3<f32> {
    let colour = fx.masks[k * 4u];
    let scale_offset = fx.masks[k * 4u + 1u];
    let tiling = fx.masks[k * 4u + 2u];
    let flags = fx.masks[k * 4u + 3u];
    if flags.z <= 0.0 {
        return c;
    }
    var m = colour.rgb;
    var a = colour.a;
    if flags.x > 0.5 {
        // The mask's place: the screen scaled and moved, then tiled.
        let p = (uv * 2.0 - 1.0) * scale_offset.xy + scale_offset.zw;
        let tuv = (p + 1.0) * 0.5 * tiling.xy;
        var t: vec4<f32>;
        let tiled = max(tiling.x, tiling.y) > 1.001;
        if k == 0u {
            t = select(textureSampleLevel(mask_a, clamped, tuv, 0.0), textureSampleLevel(mask_a, repeating, tuv, 0.0), tiled);
        } else {
            t = select(textureSampleLevel(mask_b, clamped, tuv, 0.0), textureSampleLevel(mask_b, repeating, tuv, 0.0), tiled);
        }
        if flags.x > 1.5 {
            m = t.rgb;
            a = clamp(t.a + tiling.z, 0.0, 1.0);
        } else {
            m = t.rgb * colour.rgb;
            a = clamp(colour.a * t.a + tiling.z, 0.0, 1.0);
        }
    }
    let mode = u32(tiling.w + 0.5);
    switch mode {
        case 2u: { return c + m * a; }
        case 3u: { return c + m * a * (1.0 - clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))); }
        case 4u: { return c * mix(vec3<f32>(1.0), m, a); }
        case 5u: { return c * mix(vec3<f32>(1.0), 2.0 * m, a); }
        case 6u: { return max(c - m * a, vec3<f32>(0.0)); }
        default: { return mix(c, m, clamp(a, 0.0, 1.0)); }
    }
}

// The hue turned by `turns` about the grey axis.
fn hue_turn(c: vec3<f32>, turns: f32) -> vec3<f32> {
    let angle = turns * 6.2831853;
    let k = vec3<f32>(0.57735);
    let cosa = cos(angle);
    return c * cosa + cross(k, c) * sin(angle) + k * dot(k, c) * (1.0 - cosa);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv;
    let original = textureSampleLevel(screen, clamped, uv, 0.0);

    // Distortion: the texture's offsets (pixels), above its threshold.
    if fx.distort[1].w > 0.0 && fx.distort[1].y != 0.0 {
        let duv = uv * fx.distort[0].xy + fx.distort[0].zw * 0.01;
        let d = textureSampleLevel(distort_texture, repeating, duv, 0.0).rg * 2.0 - 1.0;
        let keep = select(0.0, 1.0, length(d) > fx.distort[1].z);
        uv += d * keep * fx.distort[1].y * fx.screen.xy;
    }

    var c = source(uv);

    // RGB separation: each channel from its own place, through the mask,
    // the original shown through by the opacity.
    if fx.rgb[3].w > 0.0 && fx.rgb[3].x < 0.999 {
        var m = vec3<f32>(1.0);
        if fx.rgb[3].z > 0.0 {
            m = textureSampleLevel(rgb_mask, clamped, in.uv, 0.0).rgb * fx.rgb[3].y;
        }
        let split = vec3<f32>(source(separated(uv, fx.rgb[0])).r, source(separated(uv, fx.rgb[1])).g, source(separated(uv, fx.rgb[2])).b);
        let parted = c + m * (split - c);
        c = mix(parted, c, clamp(fx.rgb[3].x, 0.0, 1.0));
    }

    // The blur, through its mask (moved in by the mask offset) at the
    // texture alpha, tinted by its colour.
    if fx.blur[0].x > 0.0 && fx.blur[0].z > 0.0 {
        var m = 1.0;
        if fx.blur[2].z > 0.0 {
            m = textureSampleLevel(blur_mask, clamped, in.uv, 0.0).r;
        }
        let amount = clamp((m + fx.blur[0].w) * fx.blur[0].z, 0.0, 1.0);
        if amount > 0.0 {
            let b = blurred(uv, fx.blur[0].x, fx.blur[0].y, fx.blur[2].xy);
            let tinted = mix(b, fx.blur[1].rgb, clamp(fx.blur[1].a, 0.0, 1.0));
            c = mix(c, tinted, amount);
        }
    }

    // Desaturation toward a colour.
    if fx.desat.w > 0.0 {
        c = mix(c, dot(c, LUMA) * fx.desat.rgb, fx.desat.w);
    }

    // Saturation and hue.
    if fx.hue.z > 0.0 {
        let grey = vec3<f32>(dot(c, LUMA));
        c = max(mix(grey, c, 1.0 + fx.hue.x), vec3<f32>(0.0));
        if fx.hue.y != 0.0 {
            c = max(hue_turn(c, fx.hue.y), vec3<f32>(0.0));
        }
    }

    // Colour burn: grey but for the colours near its own, with contrast.
    if fx.burn[0].x > 0.0 {
        let lum = dot(c, LUMA);
        let like = dot(normalize(c + vec3<f32>(1e-4)), normalize(fx.burn[1].rgb + vec3<f32>(1e-4)));
        let keep = smoothstep(fx.burn[0].y, 1.0, like);
        let kept = mix(vec3<f32>(lum), c, fx.burn[0].w);
        var b = mix(vec3<f32>(lum), kept, keep);
        b = max((b - 0.18) * fx.burn[0].z + 0.18, vec3<f32>(0.0));
        c = mix(c, b, clamp(fx.burn[0].x, 0.0, 1.0));
    }

    c = colour_mask(c, 0u, in.uv);

    // Noise over the picture, tiled square on the screen.
    if fx.noise.x > 0.0 && fx.noise.w > 0.0 {
        let aspect = vec2<f32>(fx.screen.y / fx.screen.x, 1.0);
        let n = textureSampleLevel(noise_texture, repeating, in.uv * aspect * fx.noise.y + vec2<f32>(fx.noise.z, fract(fx.noise.z * 7.31)), 0.0).r;
        c = max(c * (1.0 + (n - 0.5) * 2.0 * fx.noise.x), vec3<f32>(0.0));
    }

    c = colour_mask(c, 1u, in.uv);
    return vec4<f32>(c, original.a);
}
