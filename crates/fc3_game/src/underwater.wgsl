// Sunlight under the water (see `underwater.rs`): with the camera below the
// surface, the ripples' focused light plays over whatever lies under it (the
// bed, rocks, wrecks), and shafts of it slant down through the water from
// the surface toward the sun. Both fade with depth and into the water's fog.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Underwater {
    world_from_clip: mat4x4<f32>,
    // xyz: the eye, w: the surface's height.
    eye: vec4<f32>,
    // xyz: toward the sun, w: strength (0: off).
    sun: vec4<f32>,
    // rgb: the sunlight's colour, w: time (s).
    colour: vec4<f32>,
    // x: the fog's density, y: the caustics' gain, z: the shafts' gain, w: 1
    // with the eye under the water.
    fog: vec4<f32>,
    // rgb: the water's fog colour (linear).
    tint: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var depth: texture_depth_2d;
@group(0) @binding(3) var<uniform> u: Underwater;

// The focused-light pattern the surface shows on the bed from above
// (`water.wgsl`), finer: seen up close its cells are a metre or two across.
fn caustic_layer(q: vec2<f32>, t: f32) -> f32 {
    let w = q + 0.6 * vec2<f32>(sin(q.y * 1.3 + t * 0.8), sin(q.x * 1.1 - t * 0.7));
    let v = sin(w.x * 2.0 + t * 0.5) * sin(w.y * 2.0 - t * 0.4);
    return pow(1.0 - abs(v), 10.0);
}

fn caustics(xz: vec2<f32>, t: f32) -> f32 {
    let r = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);
    let q = xz * 3.5;
    let c = caustic_layer(q * 0.35, t * 1.6) * 0.65 + caustic_layer(r * q * 0.23 + vec2<f32>(3.7, 1.9), t * 1.8) * 0.65;
    return clamp(c, 0.0, 1.0);
}

// Where the light comes through bright enough to throw a shaft: soft bands
// a few metres apart drifting with the swell, 0..1.
fn shafts_at(xz: vec2<f32>, t: f32) -> f32 {
    let a = sin(xz.x * 0.9 + t * 0.6 + sin(xz.y * 0.5 + t * 0.35) * 1.7);
    let b = sin(xz.y * 0.75 - t * 0.5 + sin(xz.x * 0.45 - t * 0.3) * 1.9);
    return pow(clamp(0.5 + 0.5 * a * b, 0.0, 1.0), 6.0);
}

fn world_at(uv: vec2<f32>, z: f32) -> vec3<f32> {
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let w = u.world_from_clip * vec4<f32>(ndc, z, 1.0);
    return w.xyz / w.w;
}

const STEPS: i32 = 16;
const SHAFT_REACH: f32 = 40.0;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(screen, linear_sampler, in.uv);
    if u.fog.w < 0.5 {
        return base;
    }
    let k = u.sun.w;
    let size = vec2<i32>(textureDimensions(depth));
    let z = textureLoad(depth, clamp(vec2<i32>(in.position.xy), vec2<i32>(0), size - vec2<i32>(1)), 0);
    let eye = u.eye.xyz;
    let level = u.eye.w;
    let t = u.colour.w;
    let density = u.fog.x;
    let sun = normalize(u.sun.xyz);
    let slant = sun.xz / max(sun.y, 0.25);
    let dir = normalize(world_at(in.uv, 1.0) - eye);
    var dist = 1.0e4;
    var rgb = base.rgb;
    // Nothing drawn: the sky, past the surface's edge or below the bed's,
    // which the water's fog never reaches. Lost in the fog as far off as the
    // ray meets the surface (it shows the surface itself nearer).
    if z <= 0.0 {
        let hit = select(1.0e4, (level - eye.y) / dir.y, dir.y > 0.001);
        rgb = mix(rgb, u.tint.rgb, smoothstep(30.0, 90.0, hit));
    }
    // On what lies under the surface: where its sunlight came through.
    if z > 0.0 {
        let p = world_at(in.uv, z);
        dist = distance(p, eye);
        let d = level - p.y;
        if d > 0.05 {
            let c = caustics(p.xz + slant * d, t) * exp(-d / 10.0) * smoothstep(0.05, 0.6, d);
            rgb *= 1.0 + c * u.fog.y * k * exp(-density * dist);
        }
    }
    // Shafts: the light scattered toward the eye along the view, marched
    // from the eye (each pixel's start jittered against banding).
    let jitter = fract(52.9829189 * fract(dot(in.position.xy, vec2<f32>(0.06711056, 0.00583715))));
    let reach = min(dist, SHAFT_REACH);
    let step = reach / f32(STEPS);
    var shaft = 0.0;
    for (var i = 0; i < STEPS; i++) {
        let s = (f32(i) + jitter) * step;
        let q = eye + dir * s;
        let d = level - q.y;
        if d <= 0.0 {
            continue;
        }
        shaft += shafts_at(q.xz + slant * d, t) * exp(-density * s) * exp(-d / 30.0) * step;
    }
    // Brighter looking toward the sun (the water scatters it forward).
    let phase = 0.35 + 0.65 * pow(max(dot(dir, sun), 0.0), 3.0);
    let tint = vec3<f32>(0.45, 0.85, 0.9) * u.colour.rgb;
    rgb += tint * shaft * phase * u.fog.z * k;
    return vec4<f32>(rgb, base.a);
}
