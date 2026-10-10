// Ocean surface: Gerstner swell in the vertex stage; ripples, depth-based
// colour and opacity, shoreline and crest foam in the fragment stage, lit by
// Bevy's PBR (sun glints, sky reflection from the atmosphere environment map).
//
// Water depth comes from a sea-floor texture the game builds from the terrain
// around the player (r: metres of water, g: the ground's height). The water is
// drawn in the transmissive pass with a copy of the scene behind it: the shader
// blends that in, bent by the ripples, and reflects the shore by marching the
// mirrored view ray over the ground and sampling the scene where it lands.

#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput, FragmentOutput},
    view_transformations::position_world_to_clip,
    mesh_view_bindings::{globals, view, view_transmission_texture, view_transmission_sampler},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}

struct WaterParams {
    // xy: world XZ of the sea-floor texture's corner, z: its size (m), w: sea level.
    floor: vec4<f32>,
    // x: swell scale, y: 1 for inland water (rivers, lakes), z: caustics' strength.
    shape: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var ripples: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var wrap_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var swell: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var floor_depth: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var clamp_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var foam: texture_2d<f32>;

const WIND = vec2<f32>(0.8, 0.6);

// Sunlight focused by the ripples onto the bed: thin bright curves where a
// warped interference pattern crosses zero, two layers turned against each
// other and drifting, 0..1.
fn caustic_layer(q: vec2<f32>, t: f32) -> f32 {
    let w = q + 0.6 * vec2<f32>(sin(q.y * 1.3 + t * 0.8), sin(q.x * 1.1 - t * 0.7));
    let v = sin(w.x * 2.0 + t * 0.5) * sin(w.y * 2.0 - t * 0.4);
    return pow(1.0 - abs(v), 10.0);
}

fn caustics(xz: vec2<f32>, t: f32) -> f32 {
    let r = mat2x2<f32>(0.8, 0.6, -0.6, 0.8);
    let c = caustic_layer(xz * 0.35, t) * 0.65 + caustic_layer(r * xz * 0.23 + vec2<f32>(3.7, 1.9), t * 1.1) * 0.65;
    return clamp(c, 0.0, 1.0);
}

// Metres of water below the surface at a world XZ (deep where unknown).
fn depth_at(xz: vec2<f32>) -> f32 {
    let uv = (xz - water.floor.xy) / water.floor.z;
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 60.0;
    }
    return textureSampleLevel(floor_depth, clamp_sampler, uv, 0.0).r;
}

// The ground's height at a world XZ (far below where unknown).
fn ground_at(xz: vec2<f32>) -> f32 {
    let uv = (xz - water.floor.xy) / water.floor.z;
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return -1000.0;
    }
    return textureSampleLevel(floor_depth, clamp_sampler, uv, 0.0).g;
}

// What stands on the ground for a reflected ray to meet: the trees on land
// (beaches stay low). Where there are none the scene shows the sky behind.
fn solid_top(xz: vec2<f32>, level: f32) -> f32 {
    let g = ground_at(xz);
    return g + 12.0 * smoothstep(1.5, 5.0, g - level);
}

// The scene reflected at `p` along `r`: where the ray first passes under the
// ground (and its trees), as seen on screen; alpha 0 where it goes off the
// screen or meets nothing (the sky reflection stays).
fn reflection(p: vec3<f32>, r: vec3<f32>, level: f32) -> vec4<f32> {
    var t = 1.0;
    var prev = 0.0;
    for (var i = 0; i < 20; i++) {
        let q = p + r * t;
        // Well above anything nearby: the sky.
        if q.y > level + 150.0 {
            break;
        }
        if q.y < solid_top(q.xz, level) {
            var lo = prev;
            var hi = t;
            for (var j = 0; j < 4; j++) {
                let m = (lo + hi) * 0.5;
                let qm = p + r * m;
                if qm.y < solid_top(qm.xz, level) {
                    hi = m;
                } else {
                    lo = m;
                }
            }
            let clip = view.clip_from_world * vec4<f32>(p + r * hi, 1.0);
            if clip.w <= 0.0 {
                return vec4<f32>(0.0);
            }
            let ndc = clip.xy / clip.w;
            let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
            let edge = smoothstep(0.0, 0.06, uv.x) * smoothstep(0.0, 0.06, 1.0 - uv.x) * smoothstep(0.0, 0.06, uv.y) * smoothstep(0.0, 0.06, 1.0 - uv.y);
            if edge <= 0.0 {
                return vec4<f32>(0.0);
            }
            // Where the copy has no scene (alpha 0) the sky shows there, which
            // may not be drawn yet: keep the sky's own reflection.
            let c = textureSampleLevel(view_transmission_texture, view_transmission_sampler, uv, 0.0);
            return vec4<f32>(c.rgb, edge * step(0.5, c.a));
        }
        prev = t;
        t = t * 1.25 + 0.6;
        if t > 500.0 {
            break;
        }
    }
    return vec4<f32>(0.0);
}

struct Swell {
    offset: vec3<f32>,
    normal: vec3<f32>,
    crest: f32,
}

// Four Gerstner waves around the wind direction, scaled by `amp`.
fn gerstner(xz: vec2<f32>, t: f32, amp: f32) -> Swell {
    var out: Swell;
    out.offset = vec3<f32>(0.0);
    var tx = vec3<f32>(1.0, 0.0, 0.0);
    var tz = vec3<f32>(0.0, 0.0, 1.0);
    let lengths = vec4<f32>(46.0, 23.0, 13.0, 7.5);
    // Calm tropical swell: about half a metre on the longest wave.
    let steep = vec4<f32>(0.075, 0.065, 0.055, 0.045);
    let turn = vec4<f32>(0.0, 0.45, -0.6, 1.1);
    for (var i = 0; i < 4; i++) {
        let c = cos(turn[i]);
        let s = sin(turn[i]);
        let d = normalize(vec2<f32>(WIND.x * c - WIND.y * s, WIND.x * s + WIND.y * c));
        let k = 6.2831853 / lengths[i];
        let speed = sqrt(9.81 / k);
        let a = steep[i] / k * amp;
        let f = k * (dot(d, xz) - speed * t);
        let cf = cos(f);
        let sf = sin(f);
        out.offset += vec3<f32>(d.x * a * cf, a * sf, d.y * a * cf);
        let q = steep[i] * amp;
        tx += vec3<f32>(-d.x * d.x * q * sf, d.x * q * cf, -d.x * d.y * q * sf);
        tz += vec3<f32>(-d.x * d.y * q * sf, d.y * q * cf, -d.y * d.y * q * sf);
        out.crest += max(sf, 0.0) * q;
    }
    out.normal = normalize(cross(tz, tx));
    return out;
}

fn swell_amp(xz: vec2<f32>) -> f32 {
    let dist = distance(xz, view.world_position.xz);
    // Calm in the shallows, fading out with distance (no aliasing).
    let shore = smoothstep(0.5, 9.0, depth_at(xz));
    return water.shape.x * shore * (1.0 - smoothstep(250.0, 900.0, dist));
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    var world = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    let amp = swell_amp(world.xz);
    let w = gerstner(world.xz, globals.time, amp);
    world = vec4<f32>(world.xyz + w.offset, 1.0);
    out.world_position = world;
    out.world_normal = w.normal;
    out.position = position_world_to_clip(world.xyz);
#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
    return out;
}

fn decode_normal(s: vec4<f32>) -> vec2<f32> {
    // DXT5 normal maps keep X in alpha and Y in green.
    return vec2<f32>(s.a, s.g) * 2.0 - 1.0;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, is_front);
    let p = in.world_position.xyz;
    let t = globals.time;
    let dist = distance(p, view.world_position.xyz);
    let depth = depth_at(p.xz);

    // Surface water ends at terrain, including when viewed from a cave
    // beneath dry ground. Depth clamping alone leaves reflective foam above
    // the cave even though no surface-water basin exists at this position.
    if ground_at(p.xz) >= p.y {
        discard;
    }

    // Ripples: two scrolling normal maps over the swell normal.
    let n1 = decode_normal(textureSample(ripples, wrap_sampler, p.xz * 0.06 + vec2<f32>(t * 0.011, t * 0.006)));
    let n2 = decode_normal(textureSample(swell, wrap_sampler, p.xz * 0.017 - vec2<f32>(t * 0.004, -t * 0.008)));
    // Detail normals fade out with distance (no visible tiling at the horizon).
    let detail = (n1 * 0.55 * (1.0 - smoothstep(40.0, 220.0, dist)) + n2 * 0.6) * (1.0 - smoothstep(150.0, 700.0, dist) * 0.85);
    var n = normalize(in.world_normal + vec3<f32>(detail.x, 0.0, detail.y) * 0.45);
    if !is_front {
        n = -n;
    }

    // Colour by depth: turquoise shallows to deep blue.
    // Rivers and lakes: green-brown, murkier.
    let inland = water.shape.y;
    let shallow = mix(vec3<f32>(0.06, 0.34, 0.33), vec3<f32>(0.07, 0.2, 0.14), inland);
    let deep = mix(vec3<f32>(0.006, 0.04, 0.085), vec3<f32>(0.02, 0.065, 0.055), inland);
    let absorb = 1.0 - exp(-depth / mix(9.0, 3.5, inland));
    var colour = mix(shallow, deep, absorb);
    var alpha = clamp(1.0 - exp(-depth * mix(0.32, 0.9, inland)), 0.0, 0.94);

    // Foam along the shore and on steep crests.
    let swell_h = p.y - water.floor.w;
    let foam_tex = textureSample(foam, wrap_sampler, p.xz * 0.12 + vec2<f32>(t * 0.02, 0.0)).r;
    // A thin band at the waterline that laps in and out.
    let lap = 0.25 + 0.2 * sin(t * 0.9 + p.x * 0.05 + p.z * 0.04);
    let shore = (1.0 - smoothstep(0.0, lap + 0.15, depth)) * smoothstep(0.45, 0.8, foam_tex + 0.2 * sin(t * 1.3 + depth * 9.0));
    let crest = smoothstep(0.42, 0.7, swell_h) * smoothstep(0.45, 0.85, foam_tex) * (1.0 - inland);
    let foam_amt = clamp(shore + crest * 0.6, 0.0, 1.0) * (1.0 - smoothstep(150.0, 400.0, dist));
    colour = mix(colour, vec3<f32>(0.85, 0.88, 0.86), foam_amt);
    alpha = max(alpha, foam_amt * 0.85);

    // Grazing views see more reflection than water.
    let v = normalize(view.world_position.xyz - p);
    let fresnel = pow(1.0 - clamp(dot(v, n), 0.0, 1.0), 5.0);
    alpha = mix(alpha, 1.0, fresnel);

    pbr.material.base_color = vec4<f32>(colour, alpha);
    pbr.material.perceptual_roughness = mix(0.06, 0.55, foam_amt);
    pbr.material.reflectance = vec3<f32>(0.35);
    // (The transmission is done here, not by the lighting.)
    pbr.material.specular_transmission = 0.0;
    pbr.N = n;
    pbr.world_normal = n;
    var lit = apply_pbr_lighting(pbr);
    lit = main_pass_post_lighting_processing(pbr, lit);

    // The scene behind, bent by the ripples where there's water enough.
    let screen = (in.position.xy - view.viewport.xy) / view.viewport.zw;
    let bend = detail * 0.025 * clamp(depth * 0.5, 0.0, 1.0) * (1.0 - smoothstep(20.0, 250.0, dist));
    let seen = textureSampleLevel(view_transmission_texture, view_transmission_sampler, clamp(screen + bend, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    // (Up through the surface to a sky not drawn yet: the water's own colour.)
    var behind = select(lit.rgb, seen.rgb, seen.a > 0.5);
    // Caustics on the bed in the shallows, by day; where the view meets the
    // bed (along the view ray, `depth` down).
    if is_front && depth < 14.0 {
        let down = max(-(-v).y, 0.2);
        let bed = p.xz - v.xz / down * depth;
        let sun = clamp(water.shape.z, 0.0, 1.0);
        let k = caustics(bed, t) * (1.0 - smoothstep(1.5, 14.0, depth)) * smoothstep(0.1, 0.8, depth) * sun * (1.0 - smoothstep(60.0, 220.0, dist));
        behind *= 1.0 + k * 2.2;
    }
    var rgb = lit.rgb * alpha + behind * (1.0 - alpha);

    // The shore mirrored in it, stronger at grazing angles; the ripples break it up.
    if is_front {
        let level = mix(water.floor.w, p.y, inland);
        let nr = normalize(vec3<f32>(n.x * 0.1, 1.0, n.z * 0.1));
        let r = reflect(-v, nr);
        let f = (0.02 + 0.98 * pow(1.0 - clamp(dot(v, nr), 0.0, 1.0), 5.0)) * (1.0 - foam_amt) * 0.9;
        // (Not worth the march where it would hardly show.)
        if r.y > 0.0 && f > 0.04 {
            let refl = reflection(p, r, level);
            rgb = mix(rgb, refl.rgb, refl.a * f);
        }
    }
    var out: FragmentOutput;
    out.color = vec4<f32>(rgb, 1.0);
    return out;
}
