#define_import_path bevy_pbr::irradiance_volume

#import bevy_pbr::light_probe::{light_probe_iterator_new, light_probe_iterator_next}
#import bevy_pbr::mesh_view_bindings::{
    irradiance_volumes,
    irradiance_volume,
    irradiance_volume_sampler,
    light_probes,
};
#import bevy_pbr::clustered_forward::ClusterableObjectIndexRanges

// (fc3 patch: always, for the far volumes' mark below.)
#import bevy_pbr::mesh_view_types::LIGHT_PROBE_FLAG_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE;

#ifdef IRRADIANCE_VOLUMES_ARE_USABLE

// See:
// https://advances.realtimerendering.com/s2006/Mitchell-ShadingInValvesSourceEngine.pdf
// Slide 28, "Ambient Cube Basis"
//
// fc3 patch: returns how much the volumes cover the fragment too (alpha, the
// summed weights, at most 1), so that the environment map's diffuse light
// makes up the rest outside them and across their falloff. And a volume of
// negative intensity is an interior's: within it its light stands in for
// the others' (and the environment map's), by its weight, rather than
// mixing with them. And one marked as affecting lightmapped meshes (there
// are none) is a far one: it lights only what the near ones leave, before
// the environment map.
fn irradiance_volume_light(
    world_position: vec3<f32>,
    N: vec3<f32>,
    clusterable_object_index_ranges: ptr<function, ClusterableObjectIndexRanges>,
) -> vec4<f32> {
    // Find all irradiance volumes that contain the fragment. We're going to
    // accumulate all the irradiance from them in a weighted sum.
    var iterator = light_probe_iterator_new(
        world_position,
        /*is_irradiance_volume=*/ true,
        clusterable_object_index_ranges,
    );

    var total_weight = 0.0;
    var total_irradiance = vec3(0.0);
    var interior_weight = 0.0;
    var interior_irradiance = vec3(0.0);
    var far_weight = 0.0;
    var far_irradiance = vec3(0.0);

    while (true) {
        var query_result = light_probe_iterator_next(&iterator);

        // If there was no irradiance volume found, we're done.
        if (query_result.texture_index < 0) {
            break;
        }

        // If we're lightmapped, and the irradiance volume contributes no diffuse
        // light, then bail out.
    #ifdef LIGHTMAP
        if ((query_result.flags & LIGHT_PROBE_FLAG_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE) == 0u) {
            continue;
        }
    #endif  // LIGHTMAP

    #ifdef MULTIPLE_LIGHT_PROBES_IN_ARRAY
        let irradiance_volume_texture = irradiance_volumes[query_result.texture_index];
    #else
        let irradiance_volume_texture = irradiance_volume;
    #endif

        let atlas_resolution = vec3<f32>(textureDimensions(irradiance_volume_texture));
        let resolution = vec3<f32>(textureDimensions(irradiance_volume_texture) / vec3(1u, 2u, 3u));

        // Make sure to clamp to the edges to avoid texture bleed.
        var unit_pos = (query_result.light_from_world * vec4(world_position, 1.0f)).xyz;
        let stp = clamp((unit_pos + 0.5) * resolution, vec3(0.5f), resolution - vec3(0.5f));
        let uvw = stp / atlas_resolution;

        // The bottom half of each cube slice is the negative part, so choose it if applicable on each
        // slice.
        let neg_offset = select(vec3(0.0f), vec3(0.5f), N < vec3(0.0f));

        let uvw_x = uvw + vec3(0.0f, neg_offset.x, 0.0f);
        let uvw_y = uvw + vec3(0.0f, neg_offset.y, 1.0f / 3.0f);
        let uvw_z = uvw + vec3(0.0f, neg_offset.z, 2.0f / 3.0f);

        let rgb_x = textureSampleLevel(irradiance_volume_texture, irradiance_volume_sampler, uvw_x, 0.0).rgb;
        let rgb_y = textureSampleLevel(irradiance_volume_texture, irradiance_volume_sampler, uvw_y, 0.0).rgb;
        let rgb_z = textureSampleLevel(irradiance_volume_texture, irradiance_volume_sampler, uvw_z, 0.0).rgb;

        // Use Valve's formula to sample.
        let NN = N * N;
        let irradiance = (rgb_x * NN.x + rgb_y * NN.y + rgb_z * NN.z) * abs(query_result.intensity);
        if (query_result.intensity < 0.0) {
            interior_irradiance += irradiance * query_result.weight;
            interior_weight += query_result.weight;
        } else if ((query_result.flags & LIGHT_PROBE_FLAG_AFFECTS_LIGHTMAPPED_MESH_DIFFUSE) != 0u) {
            far_irradiance += irradiance * query_result.weight;
            far_weight += query_result.weight;
        } else {
            total_irradiance += irradiance * query_result.weight;
            total_weight += query_result.weight;
        }
    }

    if (total_weight != 0.0) {
        total_irradiance /= total_weight;
    }
    var coverage = saturate(total_weight);
    if (far_weight > 0.0) {
        let far = saturate(far_weight);
        let all = coverage + (1.0 - coverage) * far;
        total_irradiance = (coverage * total_irradiance + (1.0 - coverage) * far * far_irradiance / far_weight) / max(all, 1e-4);
        coverage = all;
    }
    if (interior_weight > 0.0) {
        let inside = saturate(interior_weight);
        let all = inside + (1.0 - inside) * coverage;
        let light = inside * interior_irradiance / interior_weight + (1.0 - inside) * coverage * total_irradiance;
        return vec4(light / max(all, 1e-4), all);
    }

    return vec4(total_irradiance, coverage);
}

#endif  // IRRADIANCE_VOLUMES_ARE_USABLE
