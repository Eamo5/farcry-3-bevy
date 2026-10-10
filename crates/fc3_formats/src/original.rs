//! Pairing materials with the game's original shaders.
//!
//! The engine chooses its vertex/pixel shader pair at runtime from technique
//! tables that aren't in the data, so the converter infers one per material:
//!
//! 1. Rank the G-buffer pixel shaders (four render targets, 2D samplers) by how
//!    many of their material bindings the material provides. The material's
//!    colour map must be among the textures sampled.
//! 2. For each, find static-mesh vertex shaders (plain inputs plus a
//!    `WorldMatrix` constant) whose outputs feed the pixel shader correctly,
//!    using the dataflow flags recorded per shader.
//! 3. Run the pair on a sample vertex with the material's constants and the
//!    average colour of its textures (see `d3d9_interp`), and accept the first
//!    pair whose albedo matches the material's colour and whose normal output
//!    decodes to a sensible direction.
//!
//! The result (pair, constant blocks, sampler bindings) is baked into the cached
//! material, so the game only loads it.

use std::collections::{HashMap, HashSet};

use crate::cache::{CachedMaterial, OriginalBinding, ShaderEntry};
use crate::d3d9::{self, ShaderObject};
use crate::d3d9_interp::{self, Machine, Vec4};
use crate::hash::crc32;

pub mod names {
    pub const VIEW_ROT_PROJECTION: &str = "ViewRotProjectionMatrix";
    pub const VIEW_PROJECTION: &str = "ViewProjectionMatrix";
    pub const PROJECTION: &str = "ProjectionMatrix";
    pub const VIEW: &str = "ViewMatrix";
    pub const INV_PROJECTION: &str = "InvProjectionMatrix";
    pub const INV_PROJECTION_DEPTH: &str = "InvProjectionMatrixDepth";
    pub const INV_VIEW: &str = "InvViewMatrix";
    pub const CAMERA_POSITION: &str = "CameraPosition_DistanceScale";
    pub const CAMERA_DIRECTION: &str = "CameraDirection_FOVDeg";
    pub const CAMERA_RIGHT: &str = "CameraRight";
    pub const CAMERA_UP: &str = "CameraUp";
    pub const VIEWPORT_SIZE: &str = "ViewportSize";
    pub const CAMERA_DISTANCES: &str = "CameraDistances";
    pub const AMBIENT_MULTIPLIER: &str = "AmbientColorMultiplier";
    pub const BLOOM_ADAPTATION: &str = "BloomAdaptationFactor";
    /// Wind sway oscillators per axis (Dunia X and Y), set by the engine each frame.
    pub const WIND_SIM_X: &str = "WindSimParamsX";
    pub const WIND_SIM_Y: &str = "WindSimParamsY";
    pub const WORLD_MATRIX: &str = "WorldMatrix";
    pub const MESH_DECOMPRESSION: &str = "MeshDecompression";
    /// Bone palette of skinned vertex shaders: three rows per bone.
    pub const BLEND_MATRICES: &str = "BlendMatrices";

    // Terrain.
    pub const SECTOR_OFFSET: &str = "SectorOffset";
    pub const SECTOR_UV_TRANSFORM: &str = "SectorUVTransform";
    pub const SECTOR_UV_MIN: &str = "SectorUVMin";
    pub const CURRENT_TWEENING: &str = "CurrentTweening";
    pub const MASK_SAMPLER: &str = "MaskSampler";
    pub const COLOR_SAMPLER: &str = "ColorSampler";
    pub const DIFFUSE_SAMPLER: &str = "DiffuseSampler";
    pub const VISTA_NOISE_SAMPLER: &str = "VistaNoiseSampler";
    pub const PERLIN_NOISE_SAMPLER: &str = "PerlinNoiseSampler";
    pub const MASK_CHANNEL_SELECTORS: &str = "MaskChannelSelectors";
    pub const DETAIL_UV_SCALING_GLOSS: &str = "DetailUVScalingGloss";
    pub const NOISE_LEVELS: &str = "NoiseLevels";
    pub const NOISE_TERRAIN_TRANSITION: &str = "NoiseTerrainTransitionParams";
    pub const MATERIAL_LOD_PARAMS: &str = "MaterialLODParams";
    pub const BLEND_DETAIL: &str = "BlendDetail";

    /// Terrain layer `k` (0..4) diffuse or normal map sampler.
    pub fn detail_sampler(k: usize, normal: bool) -> String {
        format!("Detail{k}{}Sampler", if normal { "Normal" } else { "Diffuse" })
    }
}

/// Texture parameters holding colour (sampled with sRGB decoding).
pub const COLOUR_TEXTURES: &[&str] =
    &["DiffuseTexture", "DiffuseTexture1", "DiffuseTexture2", "DiffuseTexture3", "DiffuseTexture4", "DiffuseSampler0"];

/// Material parameters with non-zero defaults when a material omits them.
const PARAM_DEFAULTS: &[(&str, [f32; 4])] = &[
    ("DiffuseColor1", [1.0; 4]),
    ("DiffuseColor2", [1.0; 4]),
    ("DiffuseTiling1", [1.0; 4]),
    ("DiffuseTiling2", [1.0; 4]),
    ("NormalTiling1", [1.0; 4]),
    ("NormalTiling2", [1.0; 4]),
    ("SpecularTiling1", [1.0; 4]),
    ("Glossiness", [0.5; 4]),
    // UV tiling with a texture-group index: scale 1, group 0.
    ("DiffuseTiling1AndGroup1", [1.0, 1.0, 0.0, 0.0]),
    ("DiffuseTiling2AndGroup2", [1.0, 1.0, 0.0, 0.0]),
    ("NormalTiling1AndGroup3", [1.0, 1.0, 0.0, 0.0]),
    ("NormalTiling2AndGroup4", [1.0, 1.0, 0.0, 0.0]),
    ("MaskTiling1AndGroup0", [1.0, 1.0, 0.0, 0.0]),
    ("SpecularTiling1AndGroup2", [1.0, 1.0, 0.0, 0.0]),
];

pub const USAGE_POSITION: u8 = 0;
pub const USAGE_BLEND_WEIGHT: u8 = 1;
pub const USAGE_BLEND_INDICES: u8 = 2;
pub const USAGE_NORMAL: u8 = 3;
pub const USAGE_PSIZE: u8 = 4;
pub const USAGE_FOG: u8 = 11;
pub const USAGE_TEXCOORD: u8 = 5;
pub const USAGE_COLOR: u8 = 10;

pub fn h(name: &str) -> u32 {
    crc32(name.as_bytes())
}

/// Whether a material texture is a colour map (sRGB) rather than data.
pub fn is_colour_texture(name_hash: u32, rel: &str, m: &CachedMaterial) -> bool {
    (!m.diffuse.is_empty() && rel == m.diffuse) || COLOUR_TEXTURES.iter().any(|n| h(n) == name_hash)
}

/// Whether a sampler parameter is a material's colour map (`DiffuseTexture`,
/// `DiffuseTexture1`).
pub fn is_colour_map(name_hash: u32) -> bool {
    name_hash == h("DiffuseTexture") || name_hash == h("DiffuseTexture1")
}

/// Material texture parameters used so widely (the diffuse and normal maps and
/// one other) that by frequency they look like engine globals; the pairing
/// still treats their samplers as the material's (`fc3tool tex-params`).
const MATERIAL_SAMPLERS: [u32; 3] = [0xEE34_B08B, 0x8C65_8791, 0xF958_81AF];

/// Material constants read by so many shaders that by frequency they look
/// like engine globals: `DiffuseColor1` (the dye, and the first layer's tint
/// in layered shaders) and an unnamed scalar most `Simple` and `Cloth_FC3`
/// materials set (the normal map's strength). Materials supply them, with 1
/// when they don't (`fc3tool tex-params all` against the shader index).
const MATERIAL_CONSTANTS: [(u32, [f32; 4]); 2] = [(0x0E44_F777, [1.0; 4]), (0x92B2_69B5, [1.0; 4])];

/// Shader metadata plus the derived sets used for pairing.
pub struct ShaderSet {
    pub entries: Vec<ShaderEntry>,
    pub by_hash: HashMap<u64, usize>,
    gbuffer_ps: Vec<usize>,
    static_vs: Vec<usize>,
    /// Skinned-mesh vertex shaders (bone palette plus world matrix).
    skinned_vs: Vec<usize>,
    /// Constant/sampler name hashes provided by the engine rather than materials.
    pub engine: HashSet<u32>,
    global_regs: HashMap<u32, u32>,
    defaults: HashMap<u32, [f32; 4]>,
    /// Per shader family (lower-case name), from the game's material
    /// descriptors (see `add_descriptor`).
    families: HashMap<String, Family>,
}

/// Where the game keeps its material descriptors (one XML file per shader family).
pub const DESCRIPTOR_DIR: &str = r"engine\shaders\materialdescriptors\";

/// What a shader family's material descriptor says about its parameters.
#[derive(Debug, Default, Clone)]
pub struct Family {
    /// Default values of the parameters a material may leave out.
    pub defaults: HashMap<u32, [f32; 4]>,
    /// Constants the engine works out for each texture from the material's
    /// tiling and UV-group parameters (`DiffuseTiling1AndGroup1` and the like):
    /// constant -> (tiling parameter, UV group parameter).
    pub providers: HashMap<u32, (u32, u32)>,
    /// What the engine binds for a texture a material leaves out
    /// (`replaceTexture`: "white", "black", "normal" and the like), by
    /// sampler parameter.
    pub replacements: HashMap<u32, String>,
    /// Samplers every shader of the family reads (no `useDefine` switches
    /// them off) with no stand-in: the engine leaves them unbound when a
    /// material has no texture for them, and an unbound D3D9 sampler reads
    /// opaque black.
    pub unbound: Vec<u32>,
}

/// What an unbound sampler reads as (opaque black, as D3D9 returns): the
/// engine's own black stand-in has exactly that.
pub const UNBOUND_TEXTURE: &str = r"graphics\__fc3_graphics\_common\_textures\generic\mask\blackmask4x4.xbt";

/// The textures the engine binds in place of a texture a material leaves out,
/// by the descriptor's `replaceTexture` name (the engine's own list, in its
/// code); the flat normal is in the game's normal map layout (x in alpha, y
/// in green).
pub const REPLACEMENT_TEXTURES: [(&str, &str); 5] = [
    ("white", r"graphics\__fc3_graphics\_common\_textures\generic\mask\whitemask4x4.xbt"),
    ("black", r"graphics\__fc3_graphics\_common\_textures\generic\mask\blackmask4x4.xbt"),
    ("normal", r"graphics\__fc3_graphics\_common\_textures\generic\mask\normalmask4x4.xbt"),
    ("transparent", r"graphics\__fc3_graphics\_common\_textures\generic\mask\transparentmask4x4.xbt"),
    ("perlinNoise", r"graphics\__fc3_graphics\_common\_textures\generic\mask\perlin.xbt"),
];

impl Family {
    /// Reads a material descriptor (`engine\shaders\materialdescriptors\*.xml`):
    /// its family name, and each parameter's default and texture mapping.
    pub fn parse(xml: &str) -> Option<(String, Family)> {
        let attrs = |tag: &str| -> HashMap<String, String> {
            let mut out = HashMap::new();
            let mut rest = tag;
            while let Some(eq) = rest.find("=\"") {
                let key = rest[..eq].rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").to_string();
                let after = &rest[eq + 2..];
                let Some(end) = after.find('"') else { break };
                out.insert(key, after[..end].to_string());
                rest = &after[end + 1..];
            }
            out
        };
        let head = xml.find("<materialdescriptor")?;
        let head_end = xml[head..].find('>')? + head;
        let name = attrs(&xml[head..head_end]).get("name")?.to_ascii_lowercase();
        let mut family = Family::default();
        let mut rest = xml;
        while let Some(i) = rest.find("<parameter ") {
            let Some(end) = rest[i..].find('>') else { break };
            let a = attrs(&rest[i..i + end]);
            rest = &rest[i + end..];
            let Some(param) = a.get("name") else { continue };
            let ty = a.get("type").map(String::as_str).unwrap_or("");
            if ty == "sampler2D" {
                if let Some(r) = a.get("replaceTexture") {
                    family.replacements.insert(h(param), r.clone());
                } else if !a.contains_key("useDefine") {
                    family.unbound.push(h(param));
                }
                // The constant the engine provides for this texture: named
                // outright, or the tiling's name with the UV group's number.
                let (Some(tiling), Some(uv)) = (a.get("tilingName"), a.get("UvName")) else { continue };
                let provider = match a.get("uvTilingName") {
                    Some(p) => p.clone(),
                    None => match uv.strip_prefix("UVGroupMapChannel") {
                        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => format!("{tiling}AndGroup{n}"),
                        _ => continue,
                    },
                };
                family.providers.insert(h(&provider), (h(tiling), h(uv)));
            } else if let Some(v) = a.get("defaultvalue") {
                let values: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if !values.is_empty() && values.len() <= 4 {
                    let mut d = [0.0; 4];
                    d[..values.len()].copy_from_slice(&values);
                    family.defaults.insert(h(param), d);
                }
            }
        }
        Some((name, family))
    }
}

impl ShaderSet {
    pub fn new(entries: Vec<ShaderEntry>) -> ShaderSet {
        let by_hash = entries.iter().enumerate().map(|(i, e)| (e.hash, i)).collect();
        // Bindings present in many shaders are engine globals; so is anything the
        // engine fills per object.
        let mut freq: HashMap<u32, usize> = HashMap::new();
        for e in &entries {
            let unique: HashSet<u32> = e.bindings.iter().map(|b| b.name_hash).collect();
            for n in unique {
                *freq.entry(n).or_default() += 1;
            }
        }
        let mut engine: HashSet<u32> = freq.iter().filter(|(_, n)| **n * 8 > entries.len()).map(|(h, _)| *h).collect();
        engine.extend([h(names::WORLD_MATRIX), h(names::MESH_DECOMPRESSION), h(names::BLEND_MATRICES)]);
        for (c, _) in MATERIAL_CONSTANTS {
            engine.remove(&c);
        }
        let mut global_regs = HashMap::new();
        for e in &entries {
            for b in &e.bindings {
                if engine.contains(&b.name_hash) && !b.sampler {
                    global_regs.entry(b.name_hash).or_insert(b.register);
                }
            }
        }
        let world = h(names::WORLD_MATRIX);
        // (The game streams the tangent frame as PSIZE (tangent) and FOG
        // (binormal), packed like the normal.)
        let static_inputs = |e: &ShaderEntry| {
            e.inputs.iter().all(|i| match i.usage {
                USAGE_TEXCOORD => i.usage_index < 2,
                USAGE_POSITION | USAGE_NORMAL | USAGE_COLOR | USAGE_PSIZE | USAGE_FOG => i.usage_index == 0,
                _ => false,
            })
        };
        let gbuffer_ps = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                !e.vertex && e.color_outputs == 0xF && e.nonuniform == 0 && e.samplers.iter().all(|(r, t)| *t == 1 && *r < 8)
            })
            .collect();
        let mut static_vs: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                e.vertex
                    && e.samplers.is_empty()
                    && static_inputs(e)
                    && e.inputs.iter().any(|i| i.usage == USAGE_POSITION)
                    && e.outputs.iter().any(|o| o.location.is_none())
                    && e.bindings.iter().any(|b| b.name_hash == world)
            })
            .collect();
        static_vs.sort_by_key(|&i| entries[i].size);
        let palette = h(names::BLEND_MATRICES);
        let skinned_inputs = |e: &ShaderEntry| {
            e.inputs.iter().all(|i| match i.usage {
                USAGE_TEXCOORD => i.usage_index < 2,
                USAGE_POSITION | USAGE_NORMAL | USAGE_COLOR | USAGE_BLEND_WEIGHT | USAGE_BLEND_INDICES | USAGE_FOG | USAGE_PSIZE => {
                    i.usage_index == 0
                }
                _ => false,
            })
        };
        let mut skinned_vs: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                e.vertex
                    && e.samplers.is_empty()
                    && skinned_inputs(e)
                    && e.inputs.iter().any(|i| i.usage == USAGE_BLEND_INDICES)
                    && e.outputs.iter().any(|o| o.location.is_none())
                    && e.bindings.iter().any(|b| b.name_hash == palette)
            })
            .collect();
        skinned_vs.sort_by_key(|&i| entries[i].size);
        let defaults = PARAM_DEFAULTS.iter().map(|(n, v)| (h(n), *v)).chain(MATERIAL_CONSTANTS).collect();
        ShaderSet { entries, by_hash, gbuffer_ps, static_vs, skinned_vs, engine, global_regs, defaults, families: HashMap::new() }
    }

    /// How shiny a material is, from its own parameters (or its family's
    /// defaults): `[perceptual roughness, reflectance, ambient specular, 1]`,
    /// reflectance in Bevy's terms (F0 = 0.16 r²). The game's deferred lighting reads the
    /// glossiness (0..1) from the third G-buffer target's red channel and
    /// lights with a Blinn-Phong exponent of 2^(13 glossiness); `None` for
    /// families without a glossiness.
    pub fn surface(&self, m: &CachedMaterial) -> Option<[f32; 4]> {
        let family = self.families.get(&m.shader.to_ascii_lowercase());
        let value = |name: &str| -> Option<f32> {
            let hash = h(name);
            m.params
                .iter()
                .find(|(p, _)| *p == hash)
                .map(|(_, raw)| param_floats(raw))
                .and_then(|v| v.first().copied())
                .or_else(|| family.and_then(|f| f.defaults.get(&hash)).map(|d| d[0]))
        };
        let gloss = value("Glossiness")?.clamp(0.0, 1.0);
        let f0 = value("SpecularReflectance").unwrap_or(0.03).clamp(0.0, 1.0);
        // How much of the surroundings it reflects (the game lights its
        // ambient specular at this fraction); families without the parameter
        // take the generic props' default.
        let generic = || self.families.get("generic_fc3").and_then(|f| f.defaults.get(&h("AmbientSpecularMultiplier"))).map(|d| d[0]);
        let ambient = value("AmbientSpecularMultiplier").or_else(generic).unwrap_or(1.0).clamp(0.0, 1.0);
        Some([roughness_from_power((13.0 * gloss).exp2()), (f0 / 0.16).sqrt().min(1.0), ambient, 1.0])
    }

    /// Adds a material descriptor (see [`Family::parse`]); returns whether
    /// it could be read.
    pub fn add_descriptor(&mut self, xml: &str) -> bool {
        match Family::parse(xml) {
            Some((name, family)) => {
                self.families.insert(name, family);
                true
            }
            None => false,
        }
    }

    /// What the engine binds for each of its family's textures a material
    /// leaves out: a stand-in (see [`REPLACEMENT_TEXTURES`]), or for a sampler
    /// every shader reads with none, nothing, which reads as black (see
    /// [`UNBOUND_TEXTURE`]). Sampler parameter and game path.
    pub fn replacements(&self, m: &CachedMaterial) -> Vec<(u32, &'static str)> {
        let Some(family) = self.families.get(&m.shader.to_ascii_lowercase()) else { return Vec::new() };
        let missing = |h: &u32| !m.textures.iter().any(|(t, _)| t == h);
        let mut out: Vec<(u32, &'static str)> = family
            .replacements
            .iter()
            .filter(|(h, _)| missing(h))
            .filter_map(|(h, r)| REPLACEMENT_TEXTURES.iter().find(|(n, _)| n == r).map(|(_, p)| (*h, *p)))
            .chain(family.unbound.iter().filter(|h| missing(h)).map(|h| (*h, UNBOUND_TEXTURE)))
            .collect();
        out.sort();
        out
    }

    pub fn entry(&self, hash: u64) -> Option<&ShaderEntry> {
        self.by_hash.get(&hash).map(|&i| &self.entries[i])
    }

    pub fn global_reg(&self, name: &str) -> Option<u32> {
        self.global_regs.get(&h(name)).copied()
    }

    /// The original near-terrain shaders: the smallest G-buffer pixel shader
    /// blending four detail layers with normal maps (without the burn,
    /// sector-shadow and neighbour variants), and the smallest terrain vertex
    /// shader (packed grid position, heights and normal) that feeds it.
    pub fn terrain_pair(&self) -> Option<(&ShaderEntry, &ShaderEntry)> {
        let has = |e: &ShaderEntry, name: &str| e.bindings.iter().any(|b| b.rows > 0 && b.name_hash == h(name));
        let mut required: Vec<String> = (0..4).flat_map(|k| [names::detail_sampler(k, false), names::detail_sampler(k, true)]).collect();
        required.extend(
            [names::MASK_SAMPLER, names::COLOR_SAMPLER, names::MASK_CHANNEL_SELECTORS, names::DETAIL_UV_SCALING_GLOSS].map(String::from),
        );
        let excluded = ["BurnSampler", "NeighborExists", names::SECTOR_OFFSET, "SectorHemiMapSampler"];
        let mut pixel: Vec<&ShaderEntry> = self
            .entries
            .iter()
            .filter(|e| {
                !e.vertex
                    && e.color_outputs == 0xF
                    && e.samplers.iter().all(|(r, t)| *t == 1 && *r < 16)
                    && required.iter().all(|n| has(e, n))
                    && !excluded.iter().any(|n| has(e, n))
            })
            .collect();
        pixel.sort_by_key(|e| e.size);
        let terrain_inputs = |e: &ShaderEntry| {
            e.inputs.len() == 3
                && e.inputs.iter().all(|i| i.usage_index == 0 && matches!(i.usage, USAGE_POSITION | USAGE_PSIZE | USAGE_NORMAL))
        };
        let mut vertex: Vec<&ShaderEntry> = self
            .entries
            .iter()
            .filter(|e| e.vertex && e.samplers.is_empty() && terrain_inputs(e) && has(e, names::SECTOR_UV_TRANSFORM))
            .collect();
        vertex.sort_by_key(|e| e.size);
        pixel.into_iter().find_map(|ps| vertex.iter().find(|vs| compatible(vs, ps)).map(|vs| (*vs, ps)))
    }

    /// Register of a named binding in one shader.
    pub fn register(e: &ShaderEntry, name: &str) -> Option<u32> {
        e.bindings.iter().find(|b| b.name_hash == h(name)).map(|b| b.register)
    }

    /// G-buffer pixel shaders this material can drive, best first.
    pub fn ranked_pixel_shaders(&self, m: &CachedMaterial) -> Vec<&ShaderEntry> {
        let textures: HashSet<u32> = m.textures.iter().map(|(h, _)| *h).collect();
        let params: HashSet<u32> = m.params.iter().map(|(h, _)| *h).chain(textures.iter().copied()).collect();
        let diffuse = m.textures.iter().find(|(_, rel)| !m.diffuse.is_empty() && *rel == m.diffuse).map(|(h, _)| *h);
        let dyed = m
            .params
            .iter()
            .find(|(h, _)| *h == crate::material::param::DIFFUSE_COLOR1)
            .map(|(_, raw)| param_floats(raw))
            .is_some_and(|f| f.len() >= 3 && f[..3].iter().any(|v| (v - 1.0).abs() > 0.02));
        let mut ranked: Vec<(i32, u32, usize)> = Vec::new();
        for &i in &self.gbuffer_ps {
            let e = &self.entries[i];
            let material: Vec<_> =
                e.bindings.iter().filter(|b| !self.engine.contains(&b.name_hash) || (b.sampler && MATERIAL_SAMPLERS.contains(&b.name_hash))).collect();
            let samplers_ok = material.iter().filter(|b| b.sampler).all(|b| textures.contains(&b.name_hash));
            let uses_diffuse = diffuse.is_none_or(|d| e.bindings.iter().any(|b| b.sampler && b.name_hash == d));
            if !samplers_ok || !uses_diffuse || !material.iter().any(|b| b.sampler) {
                continue;
            }
            let hit = material.iter().filter(|b| params.contains(&b.name_hash)).count() as i32;
            let miss = material.len() as i32 - hit;
            // A dyed material wants a shader that applies its dye.
            let dye = if dyed && material.iter().any(|b| b.name_hash == crate::material::param::DIFFUSE_COLOR1) { 12 } else { 0 };
            ranked.push((hit * 4 - miss * 3 + dye, e.size, i));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked.into_iter().map(|(_, _, i)| &self.entries[i]).collect()
    }

    /// Static-mesh (or skinned) vertex shaders that can feed `ps`, smallest first.
    pub fn vertex_shaders_for<'a>(&'a self, ps: &'a ShaderEntry, skinned: bool) -> impl Iterator<Item = &'a ShaderEntry> + 'a {
        let set = if skinned { &self.skinned_vs } else { &self.static_vs };
        set.iter().map(|&i| &self.entries[i]).filter(move |vs| compatible(vs, ps))
    }

    /// A stage's material constants (sparse): parameters by binding name,
    /// defaults for well-known ones, identity mesh decompression.
    pub fn constants(&self, e: &ShaderEntry, m: &CachedMaterial) -> Vec<(u16, [f32; 4])> {
        self.constants_with(e, m, false)
    }

    /// As [`Self::constants`]; with `neutral`, pixel shader parameters the
    /// material doesn't supply (per-object tints the engine sets at run time)
    /// read 1.0 instead of 0.
    pub fn constants_with(&self, e: &ShaderEntry, m: &CachedMaterial, neutral: bool) -> Vec<(u16, [f32; 4])> {
        let decompression = h(names::MESH_DECOMPRESSION);
        let family = self.families.get(&m.shader.to_ascii_lowercase());
        let param = |name: u32| m.params.iter().find(|(h, _)| *h == name).map(|(_, raw)| param_floats(raw));
        let mut regs: HashMap<u16, [f32; 4]> = HashMap::new();
        for b in e.bindings.iter().filter(|b| !b.sampler) {
            let reg = b.register as usize;
            if reg >= 256 {
                continue;
            }
            if b.name_hash == decompression {
                // Positions and UVs are stored decompressed: scale 1, offset 0.
                regs.insert(reg as u16, [0.0, 1.0, 0.0, 1.0]);
                continue;
            }
            // A texture's tiling and UV set, as the engine works them out:
            // the tiling in the half (xy: first UV set, zw: second) the
            // shader reads for the material's UV group.
            if let Some(&(tiling, group)) = family.and_then(|f| f.providers.get(&b.name_hash)) {
                if m.params.iter().all(|(h, _)| *h != b.name_hash) {
                    let fallback = |name: u32| family.and_then(|f| f.defaults.get(&name)).map(|d| d.to_vec());
                    let t = param(tiling).filter(|t| t.len() >= 2).or_else(|| fallback(tiling)).unwrap_or(vec![1.0, 1.0]);
                    let g = param(group).or_else(|| fallback(group)).and_then(|g| g.first().copied()).unwrap_or(0.0);
                    let (tx, ty) = (t[0], t.get(1).copied().unwrap_or(t[0]));
                    regs.insert(reg as u16, if g.round() == 1.0 { [0.0, 0.0, tx, ty] } else { [tx, ty, 0.0, 0.0] });
                    continue;
                }
            }
            if self.engine.contains(&b.name_hash) {
                continue;
            }
            match m.params.iter().find(|(h, _)| *h == b.name_hash) {
                Some((_, raw)) => {
                    for (i, v) in param_floats(raw).iter().enumerate() {
                        let r = (reg + i / 4) as u16;
                        if r < 256 {
                            regs.entry(r).or_insert([0.0; 4])[i % 4] = *v;
                        }
                    }
                }
                None => {
                    if let Some(d) = family.and_then(|f| f.defaults.get(&b.name_hash)).or_else(|| self.defaults.get(&b.name_hash)) {
                        regs.insert(reg as u16, *d);
                    } else if neutral && !e.vertex && b.rows == 1 {
                        regs.insert(reg as u16, [1.0; 4]);
                    }
                }
            }
        }
        let mut out: Vec<_> = regs.into_iter().collect();
        out.sort_by_key(|(r, _)| *r);
        out
    }

    /// Engine globals for a fixed sample view (used only for validation).
    fn sample_globals(&self, c: &mut [Vec4; 256]) {
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        for name in [names::VIEW_ROT_PROJECTION, names::VIEW_PROJECTION, names::PROJECTION, names::VIEW, names::INV_PROJECTION, names::INV_PROJECTION_DEPTH, names::INV_VIEW] {
            if let Some(r) = self.global_reg(name) {
                for (k, row) in identity.iter().enumerate() {
                    if (r as usize + k) < 256 {
                        c[r as usize + k] = *row;
                    }
                }
            }
        }
        let mut put = |name: &str, v: Vec4| {
            if let Some(r) = self.global_reg(name) {
                if (r as usize) < 256 {
                    c[r as usize] = v;
                }
            }
        };
        put(names::CAMERA_POSITION, [0.0, -4.0, 1.5, 1.0]);
        put(names::CAMERA_DIRECTION, [0.0, 0.94, -0.35, 70.0]);
        put(names::CAMERA_RIGHT, [1.0, 0.0, 0.0, 0.0]);
        put(names::CAMERA_UP, [0.0, 0.35, 0.94, 0.0]);
        put(names::VIEWPORT_SIZE, [1600.0, 900.0, 1.0 / 1600.0, 1.0 / 900.0]);
        put(names::CAMERA_DISTANCES, [0.1, 4000.0, 10.0, 1.0 / 4000.0]);
        put(names::AMBIENT_MULTIPLIER, [1.0; 4]);
        put(names::BLOOM_ADAPTATION, [1.0; 4]);
    }
}

/// Whether a vertex shader produces what a pixel shader reads: every input
/// semantic is written, inputs used as texture coordinates come from the
/// vertex UVs (or position), and inputs used as directions from the normal.
pub fn compatible(vs: &ShaderEntry, ps: &ShaderEntry) -> bool {
    let fed_by = |mask: u32, sources: &[(u8, u8)]| {
        vs.inputs.iter().any(|i| i.register < 32 && mask & (1 << i.register) != 0 && sources.contains(&(i.usage, i.usage_index)))
    };
    // A pixel input read only as a texture coordinate must come from the UVs
    // or position; one only normalised, from the normal or tangent frame
    // (PSIZE, FOG). One used both ways (a vertex colour blending layers, say)
    // just has to come from the vertex.
    ps.inputs.iter().all(|i| {
        let Some(o) = vs.outputs.iter().find(|o| (o.usage, o.usage_index) == (i.usage, i.usage_index)) else { return false };
        match i.flags & 3 {
            1 => fed_by(o.flags, &[(USAGE_TEXCOORD, 0), (USAGE_TEXCOORD, 1), (USAGE_POSITION, 0)]),
            2 => fed_by(o.flags, &[(USAGE_NORMAL, 0), (USAGE_PSIZE, 0), (USAGE_FOG, 0)]),
            3 => o.flags != 0,
            _ => true,
        }
    })
}

/// Unlit's installed descriptor requires mesh UVs. Generic compatibility also
/// permits world-position mapping (needed by terrain), which can otherwise
/// pair an Unlit diffuse sampler with a world-position output. Mean-colour
/// probes cannot detect that error.
pub fn material_uv_compatible(shader: &str, vs: &ShaderEntry, ps: &ShaderEntry) -> bool {
    shader != "Unlit" || ps.inputs.iter().filter(|i| i.flags & 3 == 1).all(|i| {
        vs.outputs.iter().find(|o| (o.usage, o.usage_index) == (i.usage, i.usage_index)).is_some_and(|o| {
            vs.inputs.iter().any(|v| v.usage == USAGE_TEXCOORD && v.register < 32 && o.flags & (1 << v.register) != 0)
        })
    })
}

/// A raw material value as floats (small integers stored as u32 become numbers).
pub fn param_floats(raw: &[u8]) -> Vec<f32> {
    match raw.len() {
        1 => vec![raw[0] as f32],
        n if n % 4 == 0 => raw
            .chunks_exact(4)
            .map(|c| {
                let bits = u32::from_le_bytes(c.try_into().unwrap());
                if bits != 0 && bits < 0x0080_0000 {
                    bits as f32
                } else {
                    f32::from_bits(bits)
                }
            })
            .collect(),
        _ => vec![],
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Inputs to a pairing decision for one material.
pub struct MaterialSample<'a> {
    pub material: &'a CachedMaterial,
    /// Average colour (as stored) of each material texture, by parameter hash.
    pub texture_means: &'a HashMap<u32, [f32; 4]>,
}

/// Why a candidate pair was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Reject {
    Inputs,
    VertexRun,
    PixelRun,
    NotFinite,
    NormalLength,
    NormalDirection,
    AlbedoBrightness,
    AlbedoHue,
    /// The colour map is read at a fixed coordinate (or not at all).
    StaticUv,
    /// The pixel shader kills the sample pixel (`texkill`), so the pair
    /// would draw nothing.
    Killed,
}

/// Runs a pair on a sample vertex and checks the G-buffer outputs.
pub fn validate(set: &ShaderSet, vs: &ShaderEntry, vs_obj: &ShaderObject, ps: &ShaderEntry, ps_obj: &ShaderObject, s: &MaterialSample) -> bool {
    check(set, vs, vs_obj, ps, ps_obj, s).is_ok()
}

/// Like [`validate`], with the reason for a rejection.
/// On success, says whether render target 1 decodes to a usable world normal.
pub fn check(set: &ShaderSet, vs: &ShaderEntry, vs_obj: &ShaderObject, ps: &ShaderEntry, ps_obj: &ShaderObject, s: &MaterialSample) -> Result<bool, Reject> {
    check_with(set, vs, vs_obj, ps, ps_obj, s, false)
}

/// One run of a pair on a sample vertex (facing up, white) at a UV and
/// position: the pixel shader's four colour outputs and the coordinates the
/// diffuse map was read at.
#[allow(clippy::too_many_arguments)]
fn run_pair(
    set: &ShaderSet,
    vs: &ShaderEntry,
    vs_obj: &ShaderObject,
    ps: &ShaderEntry,
    ps_obj: &ShaderObject,
    s: &MaterialSample,
    neutral: bool,
    uv: Vec4,
    position: Vec4,
) -> Result<([Vec4; 4], Vec<Vec4>), Reject> {
    let m = s.material;
    let diffuse_hash = m.textures.iter().find(|(_, rel)| !m.diffuse.is_empty() && *rel == m.diffuse).map(|(h, _)| *h);
    let sampler_name: HashMap<u32, u32> = ps.bindings.iter().filter(|b| b.sampler).map(|b| (b.register, b.name_hash)).collect();
    let diffuse_reg = diffuse_hash.and_then(|d| sampler_name.iter().find(|(_, n)| **n == d).map(|(r, _)| *r));

    {
        // Texture reads return the texture's average colour (linear for colour
        // maps, as the GPU decodes them); engine textures read white.
        let reads = std::cell::RefCell::new(Vec::new());
        let sample = |reg: u32, coord: Vec4| -> Vec4 {
            if Some(reg) == diffuse_reg {
                reads.borrow_mut().push(coord);
            }
            let Some(&name) = sampler_name.get(&reg) else { return [1.0; 4] };
            let Some(mean) = s.texture_means.get(&name) else { return [1.0; 4] };
            let rel = m.textures.iter().find(|(h, _)| *h == name).map(|(_, r)| r.as_str()).unwrap_or("");
            if is_colour_texture(name, rel, m) {
                [srgb_to_linear(mean[0]), srgb_to_linear(mean[1]), srgb_to_linear(mean[2]), mean[3]]
            } else {
                *mean
            }
        };
        let no_texture = |_: u32, _: Vec4| -> Vec4 { [1.0; 4] };

        let mut vm = Machine::new(&no_texture);
        set.sample_globals(&mut vm.c);
        for (r, v) in set.constants(vs, m) {
            vm.c[r as usize] = v;
        }
        let rows = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        // Skinned shaders: every bone in the palette at identity.
        if let Some(pal) = vs.bindings.iter().find(|b| b.name_hash == h(names::BLEND_MATRICES)) {
            // Only the palette's own rows: the registers after it hold the
            // world matrix and mesh decompression.
            for r in pal.register as usize..(pal.register as usize + pal.rows.max(3) as usize).min(256) {
                vm.c[r] = rows[(r - pal.register as usize) % 3];
            }
        }
        if let Some(wm) = vs.bindings.iter().find(|b| b.name_hash == h(names::WORLD_MATRIX)) {
            for (k, row) in rows.iter().enumerate().take(wm.rows.max(1) as usize) {
                let r = wm.register as usize + k;
                if r < 256 {
                    vm.c[r] = *row;
                }
            }
        }
        for i in &vs.inputs {
            let r = i.register as usize;
            if r >= 16 {
                return Err(Reject::Inputs);
            }
            vm.v[r] = match i.usage {
                USAGE_POSITION => position,
                USAGE_NORMAL => [0.5, 0.5, 1.0, 1.0],
                USAGE_TEXCOORD => uv,
                USAGE_BLEND_INDICES => [0.0; 4],
                USAGE_BLEND_WEIGHT => [1.0, 0.0, 0.0, 0.0],
                // Tangent along x, binormal along y (packed like the normal).
                USAGE_PSIZE => [1.0, 0.5, 0.5, 1.0],
                USAGE_FOG => [0.5, 1.0, 0.5, 1.0],
                _ => [1.0; 4],
            };
        }
        if d3d9_interp::run(&vs_obj.program, &mut vm).is_err() {
            return Err(Reject::VertexRun);
        }
        let mut pm = Machine::new(&sample);
        set.sample_globals(&mut pm.c);
        for (r, v) in set.constants_with(ps, m, neutral) {
            pm.c[r as usize] = v;
        }
        for i in &ps.inputs {
            let Some(o) = vs.outputs.iter().find(|o| (o.usage, o.usage_index) == (i.usage, i.usage_index)) else {
                return Err(Reject::Inputs);
            };
            if (i.register as usize) < 16 && (o.register as usize) < 16 {
                pm.v[i.register as usize] = vm.o[o.register as usize];
            }
        }
        if let Err(e) = d3d9_interp::run(&ps_obj.program, &mut pm) {
            if std::env::var("FC3_DEBUG_INTERP").is_ok() {
                eprintln!("pixel shader {:016x}: {e:#}", ps.hash);
            }
            return Err(Reject::PixelRun);
        }
        if pm.killed {
            return Err(Reject::Killed);
        }
        let oc = pm.oc;
        drop(pm);
        Ok((oc, reads.into_inner()))
    }
}

/// What a pair writes to its four render targets for the sample vertex (see
/// `check`), with the material's constants.
pub fn targets(set: &ShaderSet, vs: &ShaderEntry, vs_obj: &ShaderObject, ps: &ShaderEntry, ps_obj: &ShaderObject, s: &MaterialSample) -> Result<[Vec4; 4], Reject> {
    run_pair(set, vs, vs_obj, ps, ps_obj, s, false, [0.37, 0.61, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]).map(|(oc, _)| oc)
}


/// [`check`], optionally with neutral unsupplied pixel shader parameters
/// (see [`ShaderSet::constants_with`]).
pub fn check_with(
    set: &ShaderSet,
    vs: &ShaderEntry,
    vs_obj: &ShaderObject,
    ps: &ShaderEntry,
    ps_obj: &ShaderObject,
    s: &MaterialSample,
    neutral: bool,
) -> Result<bool, Reject> {
    let m = s.material;
    let diffuse_hash = m.textures.iter().find(|(_, rel)| !m.diffuse.is_empty() && *rel == m.diffuse).map(|(h, _)| *h);
    let diffuse_reg = diffuse_hash.and_then(|d| ps.bindings.iter().find(|b| b.sampler && b.name_hash == d).map(|b| b.register));
    let run = |uv: Vec4, position: Vec4| run_pair(set, vs, vs_obj, ps, ps_obj, s, neutral, uv, position).map(|(oc, reads)| (oc[0], oc[1], reads));
    let (c0, c1, reads_a) = run([0.37, 0.61, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0])?;
    // The albedo the runtime reads must be finite.
    if c0[..3].iter().any(|x| !x.is_finite()) {
        return Err(Reject::NotFinite);
    }
    // The colour map must be read at coordinates that follow the vertex UVs or position
    // (a pair that reads it at a fixed spot renders one flat colour).
    if diffuse_reg.is_some() {
        // Move the vertex too: some shaders map textures from world position.
        let (_, _, reads_b) = run([0.81, 0.13, 0.0, 1.0], [0.7, -0.4, 0.3, 1.0])?;
        let moved = reads_a.iter().zip(&reads_b).any(|(a, b)| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() > 1e-3);
        if reads_a.is_empty() || !moved {
            if std::env::var("FC3_DEBUG_INTERP").is_ok() {
                eprintln!("static uv: vs {:016x} ps {:016x} reads {:?} vs {:?}", vs.hash, ps.hash, reads_a, reads_b);
            }
            return Err(Reject::StaticUv);
        }
    }
    // Normal: target 1 packs the world normal as n * 0.5 + 0.5; the sample
    // vertex faces up, so the decoded normal should too. Some shader families
    // keep other data in that target; then the runtime lights with the mesh
    // normal instead.
    let n = [c1[0] * 2.0 - 1.0, c1[1] * 2.0 - 1.0, c1[2] * 2.0 - 1.0];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    let normal_ok = len.is_finite() && (0.75..=1.25).contains(&len) && n[2] / len >= 0.5;
    // Albedo: target 0 stores sqrt(linear albedo); compare with the colour map.
    let albedo = [0, 1, 2].map(|k| c0[k].clamp(0.0, 1.0).powi(2));
    if let Some(mean) = diffuse_hash.and_then(|d| s.texture_means.get(&d)) {
        let expected = [0, 1, 2].map(|k| srgb_to_linear(mean[k]));
        let (la, le) = (luma(albedo), luma(expected));
        if le > 0.02 {
            let ratio = la / le;
            if !(0.15..=6.0).contains(&ratio) {
                if std::env::var("FC3_DEBUG_ALBEDO").is_ok() {
                    eprintln!("albedo: ps {:016x} out {:.3?} expected {:.3?} ratio {ratio:.3}", ps.hash, albedo, expected);
                }
                return Err(Reject::AlbedoBrightness);
            }
            let dot: f32 = (0..3).map(|k| albedo[k] * expected[k]).sum();
            let na: f32 = albedo.iter().map(|x| x * x).sum::<f32>().sqrt();
            let ne: f32 = expected.iter().map(|x| x * x).sum::<f32>().sqrt();
            if na > 1e-4 && dot / (na * ne) < 0.85 {
                return Err(Reject::AlbedoHue);
            }
        }
    }
    Ok(normal_ok)
}

/// The material with every colour parameter (an opaque RGB in 0..1) set to white.
fn undyed(m: &CachedMaterial) -> CachedMaterial {
    let mut out = m.clone();
    for (_, raw) in &mut out.params {
        if raw.len() != 16 {
            continue;
        }
        let v = param_floats(raw);
        let colour = v.len() == 4 && (v[3] - 1.0).abs() < 1e-3 && v[..3].iter().all(|x| (0.0..=1.0).contains(x));
        if colour {
            *raw = [1.0f32; 4].iter().flat_map(|f| f.to_le_bytes()).collect();
        }
    }
    out
}

/// Vertex shaders of each kind tried per pixel shader (smallest first). Many
/// small ones place the UV in another register than the pixel shader reads.
const VS_TRIES: usize = 24;
/// Further pixel shaders tried for a pair that works with both kinds of
/// vertex shader, once one works with either.
const PARTIAL_TRIES: usize = 40;

/// One pass of [`pair_material`]; `darker_ok` also takes pairs rejected for
/// brightness that pass with the material's colours whitened.
fn pair_pass(
    set: &ShaderSet,
    s: &MaterialSample,
    object: &dyn Fn(u64) -> Option<std::sync::Arc<ShaderObject>>,
    max_pixel_shaders: usize,
    darker_ok: bool,
) -> Option<OriginalBinding> {
    let m = s.material;
    // A pixel shader that works with both a static and a skinned vertex shader
    // is preferred; failing that, the first that works with either. After a
    // partial match only a few more candidates are tried.
    let mut partial: Option<OriginalBinding> = None;
    let mut budget = usize::MAX;
    for ps in set.ranked_pixel_shaders(m).into_iter().take(max_pixel_shaders) {
        if budget == 0 {
            break;
        }
        budget = budget.saturating_sub(1);
        let Some(ps_obj) = object(ps.hash) else { continue };
        // The first vertex shader of each kind (static, skinned) that passes.
        // Dyed materials (a grey map tinted by colour parameters) change the
        // hue on purpose: a pair rejected only for hue still counts when it
        // passes with those colours whitened. In the last pass, so does one
        // rejected for brightness (a black `DiffuseColor` paints the map
        // black), but only for materials no pair takes as they are.
        let pick = |skinned: bool, neutral: bool| {
            set.vertex_shaders_for(ps, skinned).filter(|vs| material_uv_compatible(&m.shader, vs, ps)).take(VS_TRIES).find_map(|vs| {
                let vs_obj = object(vs.hash)?;
                match check_with(set, vs, &vs_obj, ps, &ps_obj, s, neutral) {
                    Ok(n) => Some((vs, n)),
                    Err(r @ (Reject::AlbedoHue | Reject::AlbedoBrightness)) if r == Reject::AlbedoHue || darker_ok => {
                        let undyed = undyed(s.material);
                        let plain = MaterialSample { material: &undyed, texture_means: s.texture_means };
                        check_with(set, vs, &vs_obj, ps, &ps_obj, &plain, neutral).ok().map(|n| (vs, n))
                    }
                    Err(_) => None,
                }
            })
        };
        let (mut fixed, mut skinned) = (pick(false, false), pick(true, false));
        // Failing that, with neutral tints for parameters the engine would set.
        let neutral = fixed.is_none() && skinned.is_none();
        if neutral {
            (fixed, skinned) = (pick(false, true), pick(true, true));
        }
        if fixed.is_none() && skinned.is_none() {
            continue;
        }
        {
            let use_normal = fixed.or(skinned).map(|(_, n)| n).unwrap_or(false);
            let samplers = ps
                .samplers
                .iter()
                .filter_map(|&(reg, _)| {
                    let name = ps.bindings.iter().find(|b| b.sampler && b.register == reg)?.name_hash;
                    let (_, rel) = m.textures.iter().find(|(h, _)| *h == name)?;
                    Some((reg as u8, rel.clone(), is_colour_texture(name, rel, m)))
                })
                .collect();
            let binding = OriginalBinding {
                vs: fixed.map_or(0, |(vs, _)| vs.hash),
                ps: ps.hash,
                vs_constants: fixed.map(|(vs, _)| set.constants(vs, m)).unwrap_or_default(),
                ps_constants: set.constants_with(ps, m, neutral),
                samplers,
                use_normal,
                skinned_vs: skinned.map_or(0, |(vs, _)| vs.hash),
                skinned_vs_constants: skinned.map(|(vs, _)| set.constants(vs, m)).unwrap_or_default(),
            };
            if fixed.is_some() && skinned.is_some() {
                return Some(binding);
            }
            if partial.is_none() {
                partial = Some(binding);
                budget = budget.min(PARTIAL_TRIES);
            }
        }
    }
    partial
}

/// Chooses and validates a pair for a material, returning its baked binding.
/// `object` loads a parsed shader object by hash.
pub fn pair_material(
    set: &ShaderSet,
    s: &MaterialSample,
    object: &dyn Fn(u64) -> Option<std::sync::Arc<ShaderObject>>,
    max_pixel_shaders: usize,
) -> Option<OriginalBinding> {
    let mut b = pair_pass(set, s, object, max_pixel_shaders, false).or_else(|| pair_pass(set, s, object, max_pixel_shaders, true))?;
    if let Some(surface) = set.surface(s.material) {
        b.ps_constants.push((SURFACE_REGISTER as u16, surface));
    }
    // Vegetation: how much sunlight passes through, as the pair writes it
    // for the light passes (target 1's alpha past one half marks the class,
    // target 2's alpha is the amount).
    if let (Some(vs), Some(ps), Some(vs_obj), Some(ps_obj)) = (set.entry(b.vs), set.entry(b.ps), object(b.vs), object(b.ps)) {
        if let Ok(t) = targets(set, vs, &vs_obj, ps, &ps_obj, s) {
            if t[1][3] > 0.496 && t[2][3].is_finite() && t[2][3] > 0.0 {
                b.ps_constants.push((VEGETATION_SSS_REGISTER as u16, [t[2][3].min(1.0), 0.0, 0.0, 1.0]));
            }
        }
    }
    Some(b)
}

/// As [`pair_material`]; a material no pair takes as it is may still pair
/// once given the textures the engine binds for those it leaves out (a flat
/// normal map, a white mask: see [`ShaderSet::replacements`]), whose average
/// colours `mean` gives by game path. Returns the binding and the textures
/// added (sampler parameter, game path), which the material must then carry.
pub fn pair_material_or_replaced(
    set: &ShaderSet,
    s: &MaterialSample,
    object: &dyn Fn(u64) -> Option<std::sync::Arc<ShaderObject>>,
    max_pixel_shaders: usize,
    mean: &dyn Fn(&str) -> Option<[f32; 4]>,
) -> Option<(OriginalBinding, Vec<(u32, String)>)> {
    if let Some(b) = pair_material(set, s, object, max_pixel_shaders) {
        return Some((b, Vec::new()));
    }
    let added = set.replacements(s.material);
    if added.is_empty() {
        return None;
    }
    let mut material = s.material.clone();
    let mut means = s.texture_means.clone();
    for (h, path) in &added {
        material.textures.push((*h, crate::cache::texture_rel(path)));
        if let Some(m) = mean(path) {
            means.insert(*h, m);
        }
    }
    let sample = MaterialSample { material: &material, texture_means: &means };
    let b = pair_material(set, &sample, object, max_pixel_shaders)?;
    Some((b, added.into_iter().map(|(h, p)| (h, p.to_string())).collect()))
}

/// Pixel shader constant register holding a vegetation material's light
/// through leaves (x; w = 1 when set), beyond what ps_3_0 can address.
pub const VEGETATION_SSS_REGISTER: usize = 253;

/// Pixel shader constant register the runtime glue reads a material's
/// surface from (see [`ShaderSet::surface`]); beyond the 224 a ps_3_0 shader
/// can address, like the dye's (255).
pub const SURFACE_REGISTER: usize = 254;

/// A Blinn-Phong specular exponent as a perceptual roughness (the square
/// root of the GGX roughness with the same highlight width).
pub fn roughness_from_power(power: f32) -> f32 {
    (2.0 / (power.max(1.0) + 2.0)).sqrt().sqrt()
}

/// Parses a shader object (helper for callers holding raw bytes).
pub fn parse_object(bytes: &[u8]) -> Option<ShaderObject> {
    d3d9::parse_object(bytes).ok()
}

/// The rejection reasons for every pair `pair_material` would try (for surveys).
pub fn diagnose(
    set: &ShaderSet,
    s: &MaterialSample,
    object: &dyn Fn(u64) -> Option<std::sync::Arc<ShaderObject>>,
    max_pixel_shaders: usize,
) -> Vec<Reject> {
    let mut out = Vec::new();
    for ps in set.ranked_pixel_shaders(s.material).into_iter().take(max_pixel_shaders) {
        let Some(ps_obj) = object(ps.hash) else { continue };
        for vs in set.vertex_shaders_for(ps, false).take(4).chain(set.vertex_shaders_for(ps, true).take(4)) {
            let Some(vs_obj) = object(vs.hash) else { continue };
            if let Err(r) = check(set, vs, &vs_obj, ps, &ps_obj, s) {
                out.push(r);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn unlit_rejects_world_position_as_diffuse_uv() {
        // Installed greenhouse pair's input/output dependency metadata. Its
        // TEXCOORD4 output depends on POSITION0, not texture coordinates.
        let mut vs = ShaderEntry::from_line("64c1e197728d54dc vs 0 0 0.0:0:0:0,10.0:1:1:0 0.0:0:-:1,5.0:1:0:0,5.1:2:1:2,5.2:3:2:0,5.3:4:3:0,5.4:5:4:1 - - 0 0").unwrap();
        let ps = ShaderEntry::from_line("69067e32118fcf59 ps 0 0 5.0:0:0:0,5.3:1:3:0,5.4:2:4:1 - 0:1 - 15 0").unwrap();
        assert!(compatible(&vs, &ps));
        assert!(!material_uv_compatible("Unlit", &vs, &ps));
        assert!(material_uv_compatible("Terrain", &vs, &ps));
        vs.inputs[1].usage = USAGE_TEXCOORD;
        vs.inputs[1].usage_index = 0;
        vs.outputs.last_mut().unwrap().flags = 2;
        assert!(material_uv_compatible("Unlit", &vs, &ps));
        vs.outputs.pop();
        assert!(!material_uv_compatible("Unlit", &vs, &ps));
    }
    use super::*;

    /// A made-up descriptor in the game's layout.
    const DESCRIPTOR: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<materialdescriptor name="Sample" version="6">
    <parameter name="DiffuseTexture1" type="sampler2D" tilingName="DiffuseTiling1" UvName="UVGroupMapChannel1" optional="0" />
    <parameter name="DiffuseTiling1" type="float2" defaultvalue="1,1" />
    <parameter name="DetailTexture" type="sampler2D" tilingName="DetailTiling" UvName="DetailUV" uvTilingName="DetailUVTiling" />
    <parameter name="Glossiness" type="float2" defaultvalue="0.25, 0.5" />
    <parameter name="UVGroupMapChannel1" type="int" />
</materialdescriptor>"#;

    #[test]
    fn descriptors_give_defaults_and_tiling() {
        let (name, f) = Family::parse(DESCRIPTOR).unwrap();
        assert_eq!(name, "sample");
        assert_eq!(f.defaults.get(&h("Glossiness")), Some(&[0.25, 0.5, 0.0, 0.0]));
        assert_eq!(f.providers.get(&h("DiffuseTiling1AndGroup1")), Some(&(h("DiffuseTiling1"), h("UVGroupMapChannel1"))));
        assert_eq!(f.providers.get(&h("DetailUVTiling")), Some(&(h("DetailTiling"), h("DetailUV"))));
    }

    /// Samplers the engine fills in when a material leaves the texture out
    /// get the engine's stand-in texture.
    #[test]
    fn descriptors_name_stand_in_textures() {
        let xml = r#"<materialdescriptor name="Sample" version="6">
    <parameter name="NormalTexture1" type="sampler2D" tilingName="NormalTiling1" UvName="UVGroupMapChannel3" replaceTexture="normal" />
    <parameter name="MaskTexture" type="sampler2D" UvName="UVGroupMapChannel0" replaceTexture="white" />
    <parameter name="NoiseTexture" type="sampler2D" replaceTexture="perlinNoise" />
    <parameter name="OtherTexture" type="sampler2D" replaceTexture="unknown" />
    <parameter name="DiffuseTexture1" type="sampler2D" tilingName="DiffuseTiling1" UvName="UVGroupMapChannel1" />
    <parameter name="DetailTexture" type="sampler2D" useDefine="DETAIL" />
</materialdescriptor>"#;
        let (_, f) = Family::parse(xml).unwrap();
        assert_eq!(f.replacements.get(&h("NormalTexture1")).map(String::as_str), Some("normal"));
        assert_eq!(f.replacements.get(&h("MaskTexture")).map(String::as_str), Some("white"));
        let mut set = ShaderSet::new(Vec::new());
        assert!(set.add_descriptor(xml));
        // The mask is the material's own; an unknown stand-in has no texture;
        // the colour map, always read, reads black unbound; a switched-off
        // detail map isn't read at all.
        let m = CachedMaterial { shader: "Sample".into(), textures: vec![(h("MaskTexture"), "textures/mask.dds".into())], ..Default::default() };
        let r: std::collections::HashMap<u32, &str> = set.replacements(&m).into_iter().collect();
        assert_eq!(r.len(), 3);
        assert_eq!(r[&h("DiffuseTexture1")], UNBOUND_TEXTURE);
        assert!(r[&h("NormalTexture1")].ends_with(r"\normalmask4x4.xbt"));
        assert!(r[&h("NoiseTexture")].ends_with(r"\perlin.xbt"));
    }

    #[test]
    fn provided_tiling_follows_the_uv_group() {
        let mut set = ShaderSet::new(Vec::new());
        assert!(set.add_descriptor(DESCRIPTOR));
        let entry = ShaderEntry {
            hash: 1,
            vertex: false,
            offset: 0,
            size: 0,
            inputs: vec![],
            outputs: vec![],
            samplers: vec![],
            bindings: vec![crate::cache::ShaderBinding { name_hash: h("DiffuseTiling1AndGroup1"), register: 5, rows: 1, sampler: false }],
            color_outputs: 0,
            nonuniform: 0,
        };
        let floats = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        let mut m = CachedMaterial { shader: "Sample".into(), ..Default::default() };
        assert_eq!(set.constants_with(&entry, &m, false), vec![(5, [1.0, 1.0, 0.0, 0.0])]);
        m.params.push((h("DiffuseTiling1"), floats(&[3.0, 2.0])));
        assert_eq!(set.constants_with(&entry, &m, false), vec![(5, [3.0, 2.0, 0.0, 0.0])]);
        // On the second UV set: the other half.
        m.params.push((h("UVGroupMapChannel1"), 1u32.to_le_bytes().to_vec()));
        assert_eq!(set.constants_with(&entry, &m, false), vec![(5, [0.0, 0.0, 3.0, 2.0])]);
    }

    #[test]
    fn surfaces_from_gloss_and_reflectance() {
        // Glossier is smoother; a matte surface is near fully rough.
        assert!(roughness_from_power(256.0) < roughness_from_power(16.0));
        assert!((roughness_from_power(0.0) - (2.0f32 / 3.0).sqrt().sqrt()).abs() < 1e-6);
        let mut set = ShaderSet::new(Vec::new());
        assert!(set.add_descriptor(DESCRIPTOR));
        let floats = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        // The family's default glossiness (0.25), reflectance 0.03 by default.
        let mut m = CachedMaterial { shader: "Sample".into(), ..Default::default() };
        let s = set.surface(&m).unwrap();
        assert!((s[0] - roughness_from_power(3.25f32.exp2())).abs() < 1e-6);
        assert!((0.16 * s[1] * s[1] - 0.03).abs() < 1e-6);
        assert_eq!(s[3], 1.0);
        // The material's own values win.
        m.params.push((h("Glossiness"), floats(&[1.0, 1.0])));
        m.params.push((h("SpecularReflectance"), floats(&[0.16, 0.16])));
        let s = set.surface(&m).unwrap();
        assert!((s[0] - roughness_from_power(8192.0)).abs() < 1e-6 && (s[1] - 1.0).abs() < 1e-6);
        // No glossiness in the family: nothing.
        assert!(set.surface(&CachedMaterial { shader: "Other".into(), ..Default::default() }).is_none());
    }
}
