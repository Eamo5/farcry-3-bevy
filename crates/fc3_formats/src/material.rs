//! `.material.bin` files: a "TAM\0" header followed by an FCB object
//! holding the shader name and a list of named parameters.

use anyhow::{Context, Result};

use crate::fcb;

#[derive(Debug, Clone, Default)]
pub struct Material {
    pub name: String,
    pub shader: String,
    /// (parameter name hash, texture path)
    pub textures: Vec<(u32, String)>,
    /// (parameter name hash, raw value) for non-texture parameters.
    pub params: Vec<(u32, Vec<u8>)>,
}

/// Parameter name hashes (CRC-32 of the name) with known meaning.
pub mod param {
    /// `TwoSided`: drawn without back-face culling.
    pub const TWO_SIDED: u32 = 0x4035_25B2;
    /// `AlphaTestEnabled`: cut out where the colour output's alpha is low.
    pub const ALPHA_TEST_ENABLED: u32 = 0x7ABB_06A6;
    /// `DiffuseColor1`: the colour the (first) diffuse map is dyed with.
    pub const DIFFUSE_COLOR1: u32 = 0x0E44_F777;
    /// `DiffuseColor`: the `Simple` family's dye.
    pub const DIFFUSE_COLOR: u32 = 0x9015_351F;
}

/// A colour parameter as authored (clamped to 0..2).
fn colour_param(params: &[(u32, Vec<u8>)], name: u32) -> Option<[f32; 3]> {
    let (_, raw) = params.iter().find(|(h, _)| *h == name)?;
    if raw.len() < 12 {
        return None;
    }
    Some(std::array::from_fn(|i| f32::from_le_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], raw[i * 4 + 3]]).clamp(0.0, 2.0)))
}

/// A material's `DiffuseColor1` as authored (clamped to 0..2).
pub fn diffuse_color1(params: &[(u32, Vec<u8>)]) -> Option<[f32; 3]> {
    colour_param(params, param::DIFFUSE_COLOR1)
}

/// How a material maps its main colour map: the tiling (`DiffuseTiling1`, or
/// the `Simple` family's `Tiling`) and the UV set (`UVGroupMapChannel1`: 0
/// for the first, 1 for the second), as the material descriptors pair them.
pub fn diffuse_mapping(params: &[(u32, Vec<u8>)]) -> ([f32; 2], u8) {
    const DIFFUSE_TILING1: u32 = 0xE525_F198;
    const TILING: u32 = 0xACA1_F4B8;
    const UV_GROUP1: u32 = 0xFAA1_5E1B;
    let find = |name: u32| params.iter().find(|(h, _)| *h == name).map(|(_, raw)| crate::original::param_floats(raw));
    let tiling = [DIFFUSE_TILING1, TILING]
        .into_iter()
        .find_map(|n| find(n).filter(|f| f.len() >= 2 && f[0] > 0.0 && f[1] > 0.0).map(|f| [f[0], f[1]]))
        .unwrap_or([1.0, 1.0]);
    let uv = find(UV_GROUP1).and_then(|f| f.first().copied()).map_or(0, |g| g.round().clamp(0.0, 3.0) as u8);
    (tiling, uv)
}

/// The colour a material's shader family dyes its diffuse map with:
/// `DiffuseColor` for `Simple`, `DiffuseColor1` for the rest.
pub fn dye_param(shader: &str, params: &[(u32, Vec<u8>)]) -> Option<[f32; 3]> {
    if shader == "Simple" {
        colour_param(params, param::DIFFUSE_COLOR)
    } else {
        diffuse_color1(params)
    }
}

/// The dye a material's diffuse map is multiplied by, when it isn't plain
/// white. Generic, hair and blended materials dye their whole map with
/// `DiffuseColor1`, and `Simple` ones with `DiffuseColor`. `Metal` authors it around 0.5 for none (twice the dye),
/// blended by a mask the pairing doesn't reach. Only its darkening is
/// applied: an AK at 0.49 stays as it is, and a pale camera body dyed 0.008
/// comes out black instead of white.
pub fn single_dye(shader: &str, params: &[(u32, Vec<u8>)]) -> Option<[f32; 3]> {
    let metal = shader == "Metal" || shader == "Metal_Advanced";
    if !(shader.starts_with("Generic_FC3") || shader == "Hair" || shader == "Blended" || shader == "Simple" || shader == "Unlit" || metal) {
        return None;
    }
    let mut dye = dye_param(shader, params)?;
    if metal {
        dye = dye.map(|v| (v * 2.0).min(1.0));
    }
    dye.iter().any(|v| (v - 1.0).abs() > 0.02).then_some(dye)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexRole {
    Diffuse,
    Normal,
    Specular,
    Mask,
    Other,
}

pub fn classify_texture(path: &str) -> TexRole {
    let p = path.to_ascii_lowercase();
    let stem = p.trim_end_matches(".xbt");
    let suffix = stem.rsplit('_').next().unwrap_or("");
    match suffix {
        "d" | "diff" | "diffuse" | "df" | "dif" | "col" | "color" => TexRole::Diffuse,
        "n" | "nm" | "normal" | "nrm" => TexRole::Normal,
        "s" | "sp" | "spec" | "specular" => TexRole::Specular,
        "m" | "mask" => TexRole::Mask,
        _ => {
            if stem.ends_with("_d_a") || stem.contains("_d_") {
                TexRole::Diffuse
            } else if stem.contains("_n_") {
                TexRole::Normal
            } else {
                TexRole::Other
            }
        }
    }
}

pub fn parse(data: &[u8]) -> Result<Material> {
    let start = data
        .windows(4)
        .take(256)
        .position(|w| w == b"nbCF")
        .context("material: embedded FCB not found")?;
    let root = fcb::parse(&data[start..])?;
    let mut m = Material::default();
    let mut strings = root.fields.iter().map(|(_, v)| fcb::bytes_to_string(v));
    m.name = strings.next().unwrap_or_default();
    m.shader = strings.next().unwrap_or_default();
    for child in &root.children {
        // Each parameter: field 0 = parameter name hash, field 1 = value.
        if child.fields.len() < 2 {
            continue;
        }
        let key = child.fields[0].1.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
        let val = &child.fields[1].1;
        let s = fcb::bytes_to_string(val);
        if s.to_ascii_lowercase().ends_with(".xbt") {
            m.textures.push((key, s));
        } else {
            m.params.push((key, val.clone()));
        }
    }
    Ok(m)
}

impl Material {
    /// A boolean parameter (a single non-zero byte); false when absent.
    pub fn flag(&self, hash: u32) -> bool {
        self.params.iter().any(|(k, v)| *k == hash && v.len() == 1 && v[0] != 0)
    }

    pub fn texture(&self, role: TexRole) -> Option<&str> {
        self.textures.iter().find(|(_, p)| classify_texture(p) == role).map(|(_, p)| p.as_str())
    }

    pub fn param_f32(&self, hash: u32) -> Option<f32> {
        self.params
            .iter()
            .find(|(h, v)| *h == hash && v.len() == 4)
            .map(|(_, v)| f32::from_le_bytes(v[..4].try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dyed(v: f32) -> Vec<(u32, Vec<u8>)> {
        let raw: Vec<u8> = [v, v, v, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        vec![(param::DIFFUSE_COLOR1, raw)]
    }

    #[test]
    fn diffuse_mapping_reads_tiling_and_uv_set() {
        let floats = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        assert_eq!(diffuse_mapping(&[]), ([1.0, 1.0], 0));
        let p = vec![(0xE525_F198, floats(&[6.0, 3.0])), (0xFAA1_5E1B, 1u32.to_le_bytes().to_vec())];
        assert_eq!(diffuse_mapping(&p), ([6.0, 3.0], 1));
    }

    #[test]
    fn dyes() {
        // Generic dyes the whole map; white is no dye.
        assert_eq!(single_dye("Generic_FC3", &dyed(0.5)), Some([0.5; 3]));
        // `Simple` dyes with `DiffuseColor` instead.
        let simple = |v: f32| vec![(param::DIFFUSE_COLOR, [v, v, v, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>())];
        assert_eq!(single_dye("Simple", &simple(0.0)), Some([0.0; 3]));
        assert_eq!(single_dye("Simple", &dyed(0.5)), None);
        assert_eq!(single_dye("Generic_FC3", &dyed(1.0)), None);
        // Metal: twice the dye, darkening only.
        assert_eq!(single_dye("Metal", &dyed(0.49)), None);
        assert_eq!(single_dye("Metal", &dyed(1.0)), None);
        assert_eq!(single_dye("Metal", &dyed(0.25)), Some([0.5; 3]));
        // Other families don't dye.
        assert_eq!(single_dye("Terrain", &dyed(0.25)), None);
    }
}
