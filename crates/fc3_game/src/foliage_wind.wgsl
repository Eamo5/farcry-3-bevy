// Foliage: the standard mesh vertex stage plus wind sway, and the standard
// lighting with shadows of distant land.
//
// Vertices bend sideways in proportion to their height above the model's base
// (so trunks and grass roots stay planted), with a slow gust wave whose phase
// comes from the instance position (neighbouring plants move out of step) and
// a faster flutter for leaves. Grass and bushes also bend away from, and down
// under, people, animals and vehicles passing through them. Grass takes the
// ground's colour from its mesh tag (see `grass_tint_tag`).

#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput, FragmentOutput},
    view_transformations::position_world_to_clip,
    mesh_view_bindings::{globals, lights, view},
    shadows::fetch_directional_shadow,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}
#import fc3::horizon::{remove_hidden_sun, atlas_uv, beyond_cascades}
#import fc3::foliage_sway::{swayed, wind}

@group(#{MATERIAL_BIND_GROUP}) @binding(102) var horizon_atlas: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var horizon_sampler: sampler;

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);

#ifdef VERTEX_NORMALS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(vertex.normal, vertex.instance_index);
#endif

#ifdef VERTEX_POSITIONS
    var world = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    world = vec4<f32>(swayed(world.xyz, world_from_local[3].xyz, vertex.position, globals.time), world.w);
    out.world_position = world;
    out.position = position_world_to_clip(world.xyz);
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(world_from_local, vertex.tangent, vertex.instance_index);
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif

#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(vertex.instance_index, world_from_local[3]);
#endif

    return out;
}

// The standard lighting, then shadows of distant land (the world's horizon
// atlas) beyond the shadow cascades.
@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let tag = mesh_functions::get_tag(in.instance_index);
    if (tag >> 24u) != 0u {
        let tint = vec3<f32>(f32(tag & 0xffu), f32((tag >> 8u) & 0xffu), f32((tag >> 16u) & 0xffu)) * (1.6 / 255.0);
        pbr_input.material.base_color = vec4<f32>(pbr_input.material.base_color.rgb * tint, pbr_input.material.base_color.a);
    }
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    // Sunlight through the leaves, as the game's vegetation SSS: the sun from
    // behind the leaf (wrapped round), in the SSS colour, shadowed.
    if wind.sss.w > 0.5 && lights.n_directional_lights > 0u {
        let sun = lights.directional_lights[0];
        let back = clamp(0.6 * dot(pbr_input.N, -sun.direction_to_light) + 0.2, 0.0, 1.0);
        let view_z = dot(vec4<f32>(view.view_from_world[0].z, view.view_from_world[1].z, view.view_from_world[2].z, view.view_from_world[3].z), in.world_position);
        let shadow = fetch_directional_shadow(0u, in.world_position, -pbr_input.world_normal, view_z, in.position.xy);
        let light = sun.color.rgb * view.exposure * (1.0 / 3.14159265);
        out.color = vec4<f32>(out.color.rgb + pbr_input.material.base_color.rgb * light * wind.sss.rgb * back * shadow, out.color.a);
    }
    if beyond_cascades(in.world_position) {
        let horizon = textureSampleLevel(horizon_atlas, horizon_sampler, atlas_uv(in.world_position.xyz), 0.0).rg;
        out.color = remove_hidden_sun(out.color, pbr_input, horizon);
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
