// World map and minimap: the baked island map, explored areas, the player
// arrow and the waypoint. Icons for enemies and pickups are separate UI nodes.

#import bevy_ui::ui_vertex_output::UiVertexOutput

struct MapParams {
    // Map UV at the node centre, and the UV half-size the node covers.
    center: vec2<f32>,
    half_extent: vec2<f32>,
    // Player position (map UV) and heading (radians clockwise from north).
    player: vec2<f32>,
    heading: f32,
    // View rotation (radians clockwise; the minimap turns with the player).
    rotation: f32,
    // xy: waypoint (map UV), z: 1 when set; w: 0 = minimap (round), 1 = full map.
    waypoint: vec4<f32>,
    // The GPS route (map UV), two points to each; route_len.x: how many.
    route: array<vec4<f32>, 32>,
    route_len: vec4<f32>,
}

@group(1) @binding(0) var<uniform> params: MapParams;
@group(1) @binding(1) var map_texture: texture_2d<f32>;
@group(1) @binding(2) var map_sampler: sampler;
@group(1) @binding(3) var fog_texture: texture_2d<f32>;
@group(1) @binding(4) var fog_sampler: sampler;

const OPEN_SEA = vec3<f32>(0.0044, 0.033, 0.12);

fn rot(a: f32) -> mat2x2<f32> {
    let c = cos(a);
    let s = sin(a);
    return mat2x2<f32>(c, s, -s, c);
}

fn route_point(i: u32) -> vec2<f32> {
    let v = params.route[i / 2u];
    return select(v.xy, v.zw, i % 2u == 1u);
}

fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - (a + ab * t));
}

// Signed distance to a triangle arrow pointing up (-y), `size` pixels long.
fn arrow(p: vec2<f32>, size: f32) -> f32 {
    let q = vec2<f32>(abs(p.x), p.y);
    let tip = vec2<f32>(0.0, -size);
    let corner = vec2<f32>(size * 0.62, size * 0.75);
    let notch = vec2<f32>(0.0, size * 0.35);
    // Inside when below both edges.
    let e1 = normalize(vec2<f32>(corner.y - tip.y, tip.x - corner.x));
    let e2 = normalize(vec2<f32>(notch.y - corner.y, corner.x - notch.x));
    return max(dot(q - tip, e1), dot(q - corner, e2));
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let full = params.waypoint.w > 0.5;
    // Node-local position: -1..1 on the short axis, y down.
    let aspect = in.size.x / max(in.size.y, 1.0);
    var p = in.uv * 2.0 - 1.0;
    let r = length(p);
    if !full && r > 1.0 {
        return vec4<f32>(0.0);
    }
    let view = rot(params.rotation);
    let uv = params.center + view * (p * params.half_extent);
    var color = OPEN_SEA;
    if all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0)) {
        color = textureSample(map_texture, map_sampler, uv).rgb;
    }
    // Unexplored ground is shown faded and mostly colourless.
    let explored = textureSample(fog_texture, fog_sampler, uv).r;
    let grey = dot(color, vec3<f32>(0.3, 0.5, 0.2));
    let faded = mix(vec3<f32>(grey), color, 0.3) * 0.72;
    color = mix(faded, color, smoothstep(0.15, 0.85, explored));

    // Pixel offsets from the node centre.
    let px = (in.uv - 0.5) * in.size;
    let to_px = in.size * 0.5 / params.half_extent;
    // Grid lines every 1024 m on the full map.
    if full {
        let cells = uv * 10.0;
        let dist_px = abs(fract(cells + 0.5) - 0.5) / 10.0 * to_px;
        let line = 1.0 - smoothstep(0.4, 1.2, min(dist_px.x, dist_px.y));
        color = mix(color, vec3<f32>(0.9, 0.9, 0.85), line * 0.18);
    }
    // The GPS route to the waypoint, along the roads.
    let n = u32(params.route_len.x);
    if n >= 2u {
        var d = 1e9;
        var prev = transpose(view) * (route_point(0u) - params.center) * to_px;
        for (var i = 1u; i < n; i++) {
            let q = transpose(view) * (route_point(i) - params.center) * to_px;
            d = min(d, segment_distance(px, prev, q));
            prev = q;
        }
        let width = select(2.2, 2.8, full);
        let line = 1.0 - smoothstep(width - 0.8, width + 0.6, d);
        let edge = 1.0 - smoothstep(width + 0.6, width + 2.0, d);
        color = mix(color, vec3<f32>(0.04), edge * 0.6);
        color = mix(color, vec3<f32>(1.0, 0.82, 0.2), line);
    }
    // Waypoint diamond.
    if params.waypoint.z > 0.5 {
        var w = transpose(view) * (params.waypoint.xy - params.center) * to_px;
        if !full {
            // Pinned to the rim when outside the minimap.
            let lim = in.size.x * 0.5 - 8.0;
            if length(w) > lim {
                w = normalize(w) * lim;
            }
        }
        let d = abs(px.x - w.x) + abs(px.y - w.y);
        if d < 8.0 {
            color = mix(vec3<f32>(0.05), vec3<f32>(1.0, 0.82, 0.2), step(d, 6.0));
        }
    }
    // Player arrow (larger on the full map, with a halo).
    let pp = transpose(view) * (params.player - params.center) * to_px;
    let size = select(10.0, 15.0, full);
    if full {
        let halo = length(px - pp);
        color = mix(color, vec3<f32>(1.0, 0.85, 0.3), (1.0 - smoothstep(size * 1.6, size * 1.6 + 1.5, halo)) * 0.25);
    }
    let a = arrow(rot(-(params.heading - params.rotation)) * (px - pp), size);
    if a < 1.5 {
        color = mix(vec3<f32>(0.02), vec3<f32>(1.0, 1.0, 0.95), clamp(0.5 - a, 0.0, 1.0));
    }
    if !full {
        // Rim.
        let rim = smoothstep(0.93, 0.96, r);
        color = mix(color, vec3<f32>(0.03), rim * 0.85);
        let alpha = 1.0 - smoothstep(0.985, 1.0, r);
        return vec4<f32>(color, alpha * 0.92);
    }
    return vec4<f32>(color, 1.0);
}
