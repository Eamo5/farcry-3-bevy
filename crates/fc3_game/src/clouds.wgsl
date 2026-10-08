// Sky dome: the world's sky colours (the environment presets' gradients from
// the zenith to the horizon, warmer on the sun's side) with the sun, the
// game's cloud panoramas over them, and the moon and stars at night.
//
// Each panorama wraps 360 degrees around the horizon (bottom row) up to the
// zenith (top row). Alpha is cloud cover, green the cloud's own shading and
// red a rim term that lights up when looking toward the sun. Two panoramas
// are cross-faded as the weather changes; the dome turns slowly with the wind.
// The moon (the game's moon texture) and stars sit behind the clouds.

#import bevy_pbr::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
}

struct CloudParams {
    // xyz: direction toward the light (sun, or moon at night), w: luminance (cd/m2) of a fully lit cloud.
    light: vec4<f32>,
    // rgb: light colour.
    colour: vec4<f32>,
    // x: cross-fade from panorama a to b, y: dome rotation (turns), z: storm 0..1, w: night 0..1.
    shape: vec4<f32>,
    // xyz: direction toward the moon, w: moon visibility.
    moon: vec4<f32>,
    // xyz: direction toward the sun, w: sun visibility.
    sun: vec4<f32>,
    // The sky's colour from the zenith (0) to the horizon (15), on the sun's
    // side and the far side, as scene colours that show as the presets'
    // colours through the camera's tone mapping; w = 0 when there is none
    // (Bevy's atmosphere then shows behind the clouds).
    sky_sun: array<vec4<f32>, 16>,
    sky_far: array<vec4<f32>, 16>,
    // A storm's cloud colour after exposure, from the storm presets (with
    // the game's grade), and w: its fog's grey (0 when not given).
    storm: vec4<f32>,
    // x: the storm fog's strength over the sky, y: the power of (1 -
    // elevation) it falls off by toward the zenith.
    storm_fog: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> sky: CloudParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var pano_a: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var pano_a_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var pano_b: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var pano_b_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var moon_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var moon_sampler: sampler;

const PI: f32 = 3.14159265;
// Half-width of the moon sprite (radians); the disc is about a sixth of it.
const MOON_SIZE: f32 = 0.11;

// Lit cloud colour (premultiplied, relative to a fully lit cloud).
fn shade(s: vec4<f32>, dir: vec3<f32>) -> vec4<f32> {
    let to_light = normalize(sky.light.xyz);
    let light = sky.colour.rgb;
    let storm = sky.shape.z;
    let sky_colour = vec3<f32>(0.55, 0.64, 0.78);
    let toward = pow(max(dot(dir, to_light), 0.0), 6.0);
    let body = s.g * (sky_colour * 0.45 + light * 0.75 * mix(1.0, 0.35, storm));
    let rim = s.r * light * (0.25 + 2.5 * toward);
    return vec4<f32>((body + rim) * s.a, s.a);
}

fn hash3(p: vec3<f32>) -> vec3<f32> {
    var q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yxz + 33.33);
    return fract((q.xxy + q.yxx) * q.zyx);
}

// A sparse field of point stars, fixed to the sky.
fn stars(dir: vec3<f32>) -> f32 {
    let p = dir * 260.0;
    let cell = floor(p);
    let h = hash3(cell);
    if h.x < 0.93 {
        return 0.0;
    }
    let centre = cell + 0.5 + (h - 0.5) * 0.6;
    let d = length(p - centre);
    let brightness = pow((h.x - 0.93) / 0.07, 3.0) * 0.9 + 0.1;
    return brightness * smoothstep(0.16, 0.02, d);
}

// The sky's colour in a direction: along the gradients by elevation, and
// from the far side's toward the sun's by how near the sun's bearing it is.
fn dome(dir: vec3<f32>, elevation: f32) -> vec3<f32> {
    let p = clamp(1.0 - elevation / (0.5 * PI), 0.0, 1.0) * 15.0;
    let i = u32(floor(p));
    let j = min(i + 1u, 15u);
    let f = p - floor(p);
    let near = mix(sky.sky_sun[i].rgb, sky.sky_sun[j].rgb, f);
    let far = mix(sky.sky_far[i].rgb, sky.sky_far[j].rgb, f);
    let bearing = normalize(vec2<f32>(dir.x, dir.z) + vec2<f32>(1e-5, 0.0));
    let toward = normalize(vec2<f32>(sky.sun.x, sky.sun.z) + vec2<f32>(1e-5, 0.0));
    return mix(far, near, dot(bearing, toward) * 0.5 + 0.5);
}

// The sun's disc and glow (the dome covers the atmosphere's own).
fn sun(dir: vec3<f32>) -> vec3<f32> {
    let c = max(dot(dir, normalize(sky.sun.xyz)), 0.0);
    let disc = smoothstep(0.99985, 0.99993, c);
    let glow = pow(c, 600.0) * 0.8 + pow(c, 40.0) * 0.1;
    return sky.colour.rgb * (disc * 60.0 + glow) * sky.sun.w;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);
    let elevation = asin(clamp(dir.y, -1.0, 1.0));
    // Nothing below the horizon (the sea or terrain covers it).
    if elevation < -0.04 {
        return vec4<f32>(0.0);
    }
    let u = atan2(dir.x, -dir.z) / (2.0 * PI) + sky.shape.y;
    let v = clamp(1.0 - elevation / (0.5 * PI), 0.0, 0.999);
    let uv = vec2<f32>(u, v);
    let sa = textureSampleLevel(pano_a, pano_a_sampler, uv, 0.0);
    let sb = textureSampleLevel(pano_b, pano_b_sampler, uv, 0.0);
    var c = mix(shade(sa, dir), shade(sb, dir), sky.shape.x);
    // Thin toward the horizon haze (less so under a storm's full cover).
    let thin = mix(smoothstep(-0.04, 0.12, elevation), 1.0, sky.shape.z * smoothstep(-0.04, 0.0, elevation));
    c *= thin;
    // Physical luminance under the camera's exposure.
    var rgb = c.rgb * sky.light.w * view.exposure;
    // In a storm with the storm presets: the clouds' shading (as stored: the
    // panoramas are read as colour, which darkens it) in the storm's cloud
    // colour.
    if sky.storm.w > 0.0 {
        let g = pow(mix(sa.g * sa.a, sb.g * sb.a, sky.shape.x), 1.0 / 2.2) * thin;
        // The storm's fog covers the sky low down, as it does the land.
        let low = sky.storm_fog.x * pow(clamp(1.0 - max(elevation, 0.0) / (0.5 * PI), 0.0, 1.0), sky.storm_fog.y);
        let storm_sky = mix(g * sky.storm.rgb, vec3<f32>(sky.storm.w), clamp(low, 0.0, 1.0));
        rgb = mix(rgb, storm_sky, sky.shape.z);
    }
    var alpha = c.a;

    // Behind the clouds: the moon and stars (already in display terms).
    let night = sky.shape.w;
    if night > 0.0 {
        let behind = (1.0 - alpha) * night * smoothstep(-0.01, 0.05, elevation);
        let m = normalize(sky.moon.xyz);
        if sky.moon.w > 0.0 && dot(dir, m) > 0.9 {
            let right = normalize(cross(m, vec3<f32>(0.0, 1.0, 0.0)));
            let up = cross(right, m);
            let p = vec2<f32>(dot(dir, right), dot(dir, up)) / MOON_SIZE;
            if max(abs(p.x), abs(p.y)) < 1.0 {
                let glow = textureSampleLevel(moon_tex, moon_sampler, p * vec2<f32>(0.5, -0.5) + 0.5, 0.0).a;
                let lit = glow * sky.moon.w * behind;
                rgb += vec3<f32>(0.85, 0.9, 1.0) * lit * 1.4;
                // The disc itself hides what's behind it.
                alpha += (1.0 - alpha) * smoothstep(0.55, 0.9, glow) * sky.moon.w * night;
            }
        }
        rgb += vec3<f32>(0.8, 0.85, 1.0) * stars(dir) * behind * (1.0 - sky.shape.z);
    }
    // Behind it all, the sky dome, greyer and darker under a storm; it fades
    // into the atmosphere just below the horizon.
    if sky.sky_sun[0].w > 0.5 {
        var colour = dome(dir, max(elevation, 0.0));
        let grey = dot(colour, vec3<f32>(0.2126, 0.7152, 0.0722));
        colour = mix(colour, vec3<f32>(grey * 0.55), sky.shape.z);
        rgb += (1.0 - alpha) * (colour + sun(dir) * (1.0 - sky.shape.z));
        alpha = max(alpha, smoothstep(-0.04, -0.01, elevation));
    }
    return vec4<f32>(rgb, alpha);
}
