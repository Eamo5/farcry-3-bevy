// Terrain fragment shader: the standard PBR path with the world's terrain
// painter layers. Each sector blends up to four tiling ground textures by the
// atlas mask (R, G, B, remainder), modulated by the colour atlas. Sectors
// without layer data fall back to the baked atlas plus procedural detail.
//
// Supports Bevy's bindless mode (all terrain chunks batch together) and the
// classic per-material bind group. Layer textures are sampled with explicit
// gradients so they can be read inside per-slot branches.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_bindings::mesh,
    mesh_view_bindings::view,
}
#import fc3::horizon::{remove_hidden_sun, beyond_cascades}
#ifdef BINDLESS
#import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d}
#endif

struct TerrainParams {
    // Per slot: texture repeats per metre.
    scale: vec4<f32>,
    // Per slot: projection (0 = along X, 1 = along Z, 2 = top-down,
    // 3 = triplanar), or -1 when the slot is unused.
    mode: vec4<f32>,
    // Per slot: 1 when the slot has a normal map.
    has_normal: vec4<f32>,
    // x: 1 when layer textures are bound, y: 1 when the horizon map is.
    flags: vec4<f32>,
}

#ifdef BINDLESS
// Field order follows the bindless indices 100..112 in `TerrainDetail`.
struct TerrainIndices {
    params: u32,
    mask: u32,
    atlas_sampler: u32,
    color: u32,
    diffuse0: u32,
    layer_sampler: u32,
    diffuse1: u32,
    diffuse2: u32,
    diffuse3: u32,
    normal0: u32,
    normal1: u32,
    normal2: u32,
    normal3: u32,
    horizon: u32,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(150) var<storage> terrain_indices: array<TerrainIndices>;
@group(#{MATERIAL_BIND_GROUP}) @binding(151) var<storage> terrain_params: array<TerrainParams>;
#else
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> terrain_params: TerrainParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var mask_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var atlas_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var color_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var diffuse0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var layer_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var diffuse1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var diffuse2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var diffuse3: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var normal0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var normal1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(111) var normal2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(112) var normal3: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(113) var horizon_texture: texture_2d<f32>;
#endif

fn get_params(slot: u32) -> TerrainParams {
#ifdef BINDLESS
    return terrain_params[terrain_indices[slot].params];
#else
    return terrain_params;
#endif
}

// Atlas textures: 0 = mask, 1 = colour.
fn sample_atlas(slot: u32, which: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec4<f32> {
#ifdef BINDLESS
    let ids = terrain_indices[slot];
    var t = ids.mask;
    if which == 1u {
        t = ids.color;
    }
    return textureSampleGrad(bindless_textures_2d[t], bindless_samplers_filtering[ids.atlas_sampler], uv, gx, gy);
#else
    if which == 1u {
        return textureSampleGrad(color_texture, atlas_sampler, uv, gx, gy);
    }
    return textureSampleGrad(mask_texture, atlas_sampler, uv, gx, gy);
#endif
}

// The sector's horizon map at a point (u north, v east, within the sector).
fn sample_horizon(slot: u32, uv: vec2<f32>) -> vec2<f32> {
#ifdef BINDLESS
    let ids = terrain_indices[slot];
    return textureSampleLevel(bindless_textures_2d[ids.horizon], bindless_samplers_filtering[ids.atlas_sampler], uv, 0.0).rg;
#else
    return textureSampleLevel(horizon_texture, atlas_sampler, uv, 0.0).rg;
#endif
}

// Layer textures: 0..3 = diffuse per slot, 4..7 = normal map per slot.
fn sample_layer_texture(slot: u32, which: u32, uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec4<f32> {
    var out: vec4<f32>;
#ifdef BINDLESS
    let ids = terrain_indices[slot];
    var t: u32;
    switch which {
        case 0u: { t = ids.diffuse0; }
        case 1u: { t = ids.diffuse1; }
        case 2u: { t = ids.diffuse2; }
        case 3u: { t = ids.diffuse3; }
        case 4u: { t = ids.normal0; }
        case 5u: { t = ids.normal1; }
        case 6u: { t = ids.normal2; }
        default: { t = ids.normal3; }
    }
    out = textureSampleGrad(bindless_textures_2d[t], bindless_samplers_filtering[ids.layer_sampler], uv, gx, gy);
#else
    switch which {
        case 0u: { out = textureSampleGrad(diffuse0, layer_sampler, uv, gx, gy); }
        case 1u: { out = textureSampleGrad(diffuse1, layer_sampler, uv, gx, gy); }
        case 2u: { out = textureSampleGrad(diffuse2, layer_sampler, uv, gx, gy); }
        case 3u: { out = textureSampleGrad(diffuse3, layer_sampler, uv, gx, gy); }
        case 4u: { out = textureSampleGrad(normal0, layer_sampler, uv, gx, gy); }
        case 5u: { out = textureSampleGrad(normal1, layer_sampler, uv, gx, gy); }
        case 6u: { out = textureSampleGrad(normal2, layer_sampler, uv, gx, gy); }
        default: { out = textureSampleGrad(normal3, layer_sampler, uv, gx, gy); }
    }
#endif
    return out;
}

struct Layer {
    color: vec3<f32>,
    // World-space normal offset from the layer's normal map.
    bump: vec3<f32>,
}

// World position (and its screen derivatives) projected for a layer.
struct Projection {
    uv: vec2<f32>,
    gx: vec2<f32>,
    gy: vec2<f32>,
    t: vec3<f32>,
    b: vec3<f32>,
}

fn project(p: vec3<f32>, dx: vec3<f32>, dy: vec3<f32>, axis: i32, scale: f32) -> Projection {
    var r: Projection;
    if axis == 0 {
        r.uv = vec2<f32>(p.z, -p.y);
        r.gx = vec2<f32>(dx.z, -dx.y);
        r.gy = vec2<f32>(dy.z, -dy.y);
        r.t = vec3<f32>(0.0, 0.0, 1.0);
        r.b = vec3<f32>(0.0, -1.0, 0.0);
    } else if axis == 1 {
        r.uv = vec2<f32>(p.x, -p.y);
        r.gx = vec2<f32>(dx.x, -dx.y);
        r.gy = vec2<f32>(dy.x, -dy.y);
        r.t = vec3<f32>(1.0, 0.0, 0.0);
        r.b = vec3<f32>(0.0, -1.0, 0.0);
    } else {
        r.uv = p.xz;
        r.gx = dx.xz;
        r.gy = dy.xz;
        r.t = vec3<f32>(1.0, 0.0, 0.0);
        r.b = vec3<f32>(0.0, 0.0, 1.0);
    }
    r.uv *= scale;
    r.gx *= scale;
    r.gy *= scale;
    return r;
}

// Layer normal maps are DXT5 with X in alpha and Y in green.
fn sample_planar(slot: u32, layer: u32, has_n: bool, pr: Projection) -> Layer {
    var out: Layer;
    out.color = sample_layer_texture(slot, layer, pr.uv, pr.gx, pr.gy).rgb;
    out.bump = vec3<f32>(0.0);
    if has_n {
        let s = sample_layer_texture(slot, layer + 4u, pr.uv, pr.gx, pr.gy);
        let nt = vec2<f32>(s.a, s.g) * 2.0 - 1.0;
        out.bump = pr.t * nt.x + pr.b * nt.y;
    }
    return out;
}

fn sample_layer(slot: u32, params: TerrainParams, layer: u32, p: vec3<f32>, dx: vec3<f32>, dy: vec3<f32>, normal: vec3<f32>, bumps: bool) -> Layer {
    let mode = i32(params.mode[layer]);
    let scale = params.scale[layer];
    let has_n = bumps && params.has_normal[layer] > 0.5;
    if mode != 3 {
        return sample_planar(slot, layer, has_n, project(p, dx, dy, mode, scale));
    }
    // Triplanar for cliffs: blend the three projections by the surface normal,
    // skipping projections that barely contribute.
    var w = pow(abs(normal), vec3<f32>(4.0));
    w = select(w, vec3<f32>(0.0), w < vec3<f32>(0.02 * max(w.x, max(w.y, w.z))));
    w /= max(w.x + w.y + w.z, 1e-4);
    var out: Layer;
    out.color = vec3<f32>(0.0);
    out.bump = vec3<f32>(0.0);
    if w.x > 0.0 {
        let l = sample_planar(slot, layer, has_n, project(p, dx, dy, 0, scale));
        out.color += l.color * w.x;
        out.bump += l.bump * w.x;
    }
    if w.y > 0.0 {
        let l = sample_planar(slot, layer, has_n, project(p, dx, dy, 2, scale));
        out.color += l.color * w.y;
        out.bump += l.bump * w.y;
    }
    if w.z > 0.0 {
        let l = sample_planar(slot, layer, has_n, project(p, dx, dy, 1, scale));
        out.color += l.color * w.z;
        out.bump += l.bump * w.z;
    }
    return out;
}

fn hash12(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash12(i);
    let b = hash12(i + vec2<f32>(1.0, 0.0));
    let c = hash12(i + vec2<f32>(0.0, 1.0));
    let d = hash12(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    for (var i = 0; i < 4; i++) {
        sum += amp * value_noise(q);
        q = q * 2.03 + vec2<f32>(17.1, 9.3);
        amp *= 0.5;
    }
    return sum / 0.9375;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    let params = get_params(slot);
    let p = in.world_position.xyz;
    let dx = dpdx(p);
    let dy = dpdy(p);
    let uv_dx = dpdx(in.uv);
    let uv_dy = dpdy(in.uv);
    let mask = sample_atlas(slot, 0u, in.uv, uv_dx, uv_dy).rgb;
    let tint = sample_atlas(slot, 1u, in.uv, uv_dx, uv_dy).rgb;
    let dist = distance(p, view.world_position);
    if params.flags.x > 0.5 {
        var w = vec4<f32>(mask, max(0.0, 1.0 - mask.r - mask.g - mask.b));
        w *= step(vec4<f32>(0.0), params.mode);
        let total = w.x + w.y + w.z + w.w;
        if total < 1e-3 {
            w = vec4<f32>(1.0, 0.0, 0.0, 0.0);
        } else {
            w /= total;
        }
        let n = normalize(in.world_normal);
        let bump_fade = 1.0 - smoothstep(30.0, 120.0, dist);
        var color = vec3<f32>(0.0);
        var bump = vec3<f32>(0.0);
        for (var i = 0u; i < 4u; i++) {
            if params.mode[i] >= 0.0 && w[i] > 0.002 {
                let l = sample_layer(slot, params, i, p, dx, dy, n, bump_fade > 0.0);
                color += l.color * w[i];
                bump += l.bump * w[i];
            }
        }
        // The colour atlas is a x2 modulation map (mid grey = unchanged).
        // A large-scale brightness drift breaks up tiling at mid range.
        let drift = 1.0 + (value_noise(p.xz * 0.05) * 0.6 + value_noise(p.xz * 0.13) * 0.4 - 0.5) * 0.25;
        pbr_input.material.base_color = vec4<f32>(color * tint * 2.0 * drift, 1.0);
        pbr_input.N = normalize(pbr_input.N + bump * 0.8 * bump_fade);
    } else {
        // Baked atlas only: add procedural close-range variation.
        let fade = 1.0 - smoothstep(12.0, 90.0, dist);
        if fade > 0.0 {
            let patches = fbm(p.xz * 0.31);
            let g = fbm(p.xz * 3.1);
            let lum = 1.0 + ((patches - 0.5) * 0.45 + (g - 0.5) * 0.35) * fade;
            pbr_input.material.base_color = vec4<f32>(pbr_input.material.base_color.rgb * lum, 1.0);
        }
    }

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    // Shadows of distant land, which the shadow cascades don't reach (the
    // sector's own horizon map, a texel per metre).
    if params.flags.y > 0.5 && beyond_cascades(in.world_position) {
        out.color = remove_hidden_sun(out.color, pbr_input, sample_horizon(slot, fract(vec2<f32>(-p.z, p.x) / 64.0)));
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
