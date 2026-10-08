// Shadows of distant land from the game's horizon maps: where the sun stands
// lower than the horizon toward it, its light is taken away (only what the
// shadow cascades left). The horizon is sampled along the sun's path: red
// toward where it sets, green toward where it rises, 0..1 for -90..90 degrees.
#define_import_path fc3::horizon

#import bevy_pbr::{
    mesh_view_bindings::{view, lights},
    mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT,
    shadows::fetch_directional_shadow,
    pbr_types::PbrInput,
}

// The sky's turning axis and where the sun rises (see `sky::SKY_AXIS`).
const SKY_AXIS: vec3<f32> = vec3<f32>(0.8660254, 0.0, 0.5);
const SUN_RISE: vec3<f32> = vec3<f32>(0.5, 0.0, -0.8660254);
const PI: f32 = 3.14159265;

// Where a world position falls in the world-wide horizon atlas (4 m texels,
// rows running north from the world's south-west corner).
fn atlas_uv(world: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(world.x, -world.z) / 10240.0 + vec2<f32>(0.5);
}

// Whether a point lies far enough out for the horizon test: beyond most of
// the shadow cascades (which shadow nearer land themselves), or anywhere when
// shadows are off. Cheap, so callers skip sampling the maps nearer in.
fn beyond_cascades(world: vec4<f32>) -> bool {
    if lights.n_directional_lights == 0u {
        return false;
    }
    let light = lights.directional_lights[0];
    if (light.flags & DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) == 0u || light.num_cascades == 0u {
        return true;
    }
    let view_z = dot(vec4<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), world);
    return -view_z > light.cascades[light.num_cascades - 1u].far_bound * 0.7;
}

// `color` (lit) with the sunlight the land hides taken away.
fn remove_hidden_sun(color: vec4<f32>, pbr: PbrInput, horizon: vec2<f32>) -> vec4<f32> {
    if lights.n_directional_lights == 0u {
        return color;
    }
    let light = lights.directional_lights[0];
    let l = light.direction_to_light;
    // Only the sun (the moon turns off its plane).
    if abs(dot(l, SKY_AXIS)) > 0.05 || l.y <= 0.0 {
        return color;
    }
    let toward = select(horizon.r, horizon.g, dot(l, SUN_RISE) > 0.0);
    let lit = smoothstep((toward - 0.5) * PI - 0.01, (toward - 0.5) * PI + 0.035, asin(l.y));
    // Clear of the land (nearly everywhere): nothing to do.
    if lit >= 1.0 {
        return color;
    }
    var cascade = 1.0;
    if (light.flags & DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u {
        let view_z = dot(vec4<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), pbr.world_position);
        cascade = fetch_directional_shadow(0u, pbr.world_position, pbr.world_normal, view_z, pbr.frag_coord.xy);
    }
    let hidden = max(cascade - lit, 0.0);
    if hidden <= 0.0 {
        return color;
    }
    let direct = pbr.material.base_color.rgb * max(dot(pbr.N, l), 0.0) * light.color.rgb / PI * view.exposure;
    return vec4<f32>(max(color.rgb - direct * hidden, vec3<f32>(0.0)), color.a);
}
