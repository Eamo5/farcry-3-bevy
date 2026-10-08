// Foliage sway, shared by the main pass (`foliage_wind.wgsl`) and the depth,
// normal and shadow passes (`foliage_prepass.wgsl`), so all of them see the
// plant where it is.

#define_import_path fc3::foliage_sway

struct Wind {
    // x: sway amplitude per metre of height, y: height where the bend saturates,
    // z: flutter amplitude, w: speed multiplier.
    params: vec4<f32>,
    // xy: direction the wind blows toward, z: strength (1 = a breeze).
    air: vec4<f32>,
    // Sunlight through the leaves: rgb colour times strength, w 1 when on.
    sss: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> wind: Wind;

struct Pushers {
    // xyz: feet, w: reach (0: unused).
    items: array<vec4<f32>, 8>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<storage, read> pushers: Pushers;

// Parting round passers-by: away from each one within its reach, more toward
// the tips, and only for the parts of the plant about their legs.
fn push(world: vec3<f32>, height: f32) -> vec3<f32> {
    var off = vec3<f32>(0.0);
    for (var i = 0; i < 8; i++) {
        let p = pushers.items[i];
        if p.w <= 0.0 {
            continue;
        }
        let d = world.xz - p.xz;
        let dist = length(d);
        let k = (1.0 - smoothstep(p.w * 0.3, p.w, dist)) * (1.0 - smoothstep(1.2, 2.2, world.y - p.y));
        let amount = k * min(height, 1.2);
        off += vec3<f32>(d.x / max(dist, 0.05) * 0.55, -0.4, d.y / max(dist, 0.05) * 0.55) * amount;
    }
    return off;
}

fn sway(origin: vec3<f32>, local: vec3<f32>, height: f32, time: f32) -> vec3<f32> {
    let t = time * wind.params.w;
    let phase = dot(origin.xz, vec2<f32>(0.071, 0.053));
    // The weather's wind, with gusts; stronger wind bends further and faster.
    let dir = normalize(wind.air.xy + vec2<f32>(1e-4, 0.0));
    let strength = wind.air.z;
    let gust = (0.6 + 0.4 * sin(t * 0.37 + phase * 0.5)) * strength;
    let wave = sin(t * 1.3 + phase) * 0.7 + sin(t * 2.9 + phase * 1.9) * 0.3;
    let bend = wind.params.x * min(height, wind.params.y) * height / max(wind.params.y, 0.01);
    var offset = vec3<f32>(dir.x, 0.0, dir.y) * wave * gust * bend;
    // Leaf flutter: small, fast, varying across the model.
    let flutter = sin(t * (7.0 + strength * 2.0) + dot(local, vec3<f32>(3.1, 2.3, 4.7)) + phase * 3.0);
    offset += vec3<f32>(0.3, 0.6, -0.4) * flutter * wind.params.z * min(height, 1.0) * strength;
    return offset;
}

// Where a vertex is with the wind and passers-by, in world space (`time`:
// the globals' time; the passes bind it in different places).
fn swayed(world: vec3<f32>, origin: vec3<f32>, local: vec3<f32>, time: f32) -> vec3<f32> {
    // Height above the base in world units (instances may be scaled).
    let height = max(world.y - origin.y, 0.0);
    var offset = sway(origin, local, height, time);
    if wind.params.y < 3.0 {
        offset += push(world, height);
    }
    return world + offset;
}
