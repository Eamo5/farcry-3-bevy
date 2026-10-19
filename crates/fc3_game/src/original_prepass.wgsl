// Depth, shadow and normal prepass for the original-shader materials: Bevy's
// default outputs, plus cut-out surfaces (fences, rags, tarps) discarded
// where their diffuse map's alpha is low, at the mesh UV times the
// material's tiling, as the glue does in the main pass.

#import bevy_pbr::{prepass_io, pbr_prepass_functions}

@group(#{MATERIAL_BIND_GROUP}) @binding(50) var cutout_texture: texture_2d<f32>;
// The material's shared repeating sampler (`OriginalMaterial::repeat`).
@group(#{MATERIAL_BIND_GROUP}) @binding(55) var cutout_sampler: sampler;
// xy: the diffuse map's tiling, z: the alpha threshold (0: not cut out).
@group(#{MATERIAL_BIND_GROUP}) @binding(52) var<uniform> cutout: vec4<f32>;

fn cut(in: prepass_io::VertexOutput) {
#ifdef VERTEX_UVS_A
    let a = textureSample(cutout_texture, cutout_sampler, in.uv * cutout.xy).a;
    if cutout.z > 0.0 && a < cutout.z {
        discard;
    }
#endif
}

#ifdef PREPASS_FRAGMENT
@fragment
fn fragment(in: prepass_io::VertexOutput) -> prepass_io::FragmentOutput {
    cut(in);
    var out: prepass_io::FragmentOutput;
#ifdef NORMAL_PREPASS
    out.normal = vec4<f32>(normalize(in.world_normal) * 0.5 + vec3<f32>(0.5), 1.0);
#endif
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif
#ifdef MOTION_VECTOR_PREPASS
    out.motion_vector = pbr_prepass_functions::calculate_motion_vector(in.world_position, in.previous_world_position);
#endif
    return out;
}
#else
@fragment
fn fragment(in: prepass_io::VertexOutput) {
    cut(in);
}
#endif
