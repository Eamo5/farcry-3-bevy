//! The game's Domino scripts (Lua mission graphs and boxes), packed into the
//! cache at conversion: every script under `domino\` the file lists name,
//! plus the ones they reach through references the lists don't name.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

/// The packed scripts, in the cache.
pub const DOMINO_PACK_REL: &str = "scripts/domino.pak";

const MAGIC: &[u8; 4] = b"FCDP";
const VERSION: u32 = 1;

/// The id graphs use for a script path (`GetPathID`): CRC-32 of the path in
/// lower case with forward slashes.
pub fn path_id(path: &str) -> u32 {
    crate::hash::crc32(normalise(path).as_bytes())
}

/// A script path as graphs write it, in lower case with forward slashes.
pub fn normalise(path: &str) -> String {
    path.chars().map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() }).collect()
}

/// The archive path of a script a graph names (`domino\system\door.lua`).
pub fn archive_path(path: &str) -> String {
    normalise(path).replace('/', "\\")
}

/// Script paths a script refers to (boxes, libraries, graphs), as written.
pub fn references(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find(".lua\"") {
        let end = at + i + 4;
        // Back to the opening quote.
        if let Some(start) = lower[..end].rfind('"') {
            let p = &text[start + 1..end];
            if p.to_ascii_lowercase().starts_with("domino") && !p.contains('\n') {
                out.push(p.to_string());
            }
        }
        at = end + 1;
    }
    out
}

/// Builds the pack from (path, text) pairs.
pub fn pack(scripts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut index = Vec::new();
    let mut blobs = Vec::new();
    for (path, text) in scripts {
        let blob = crate::cache::compress(text);
        index.push((normalise(path), blobs.len() as u32, blob.len() as u32, text.len() as u32));
        blobs.extend_from_slice(&blob);
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(index.len() as u32).to_le_bytes());
    for (path, offset, size, raw) in &index {
        out.extend_from_slice(&(path.len() as u16).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&raw.to_le_bytes());
    }
    out.extend_from_slice(&blobs);
    out
}

/// The packed scripts, read back.
pub struct ScriptPack {
    blobs: Vec<u8>,
    /// Path id -> (path, offset, size).
    index: HashMap<u32, (String, u32, u32)>,
}

impl ScriptPack {
    pub fn from_bytes(data: &[u8]) -> Result<ScriptPack> {
        if data.len() < 12 || &data[..4] != MAGIC {
            bail!("not a script pack");
        }
        let u32_at = |o: usize| -> Result<u32> { Ok(u32::from_le_bytes(data.get(o..o + 4).context("truncated script pack")?.try_into()?)) };
        if u32_at(4)? != VERSION {
            bail!("script pack version {}", u32_at(4)?);
        }
        let count = u32_at(8)? as usize;
        let mut at = 12;
        let mut index = HashMap::with_capacity(count);
        for _ in 0..count {
            let len = u16::from_le_bytes(data.get(at..at + 2).context("truncated script pack")?.try_into()?) as usize;
            let path = String::from_utf8_lossy(data.get(at + 2..at + 2 + len).context("truncated script pack")?).into_owned();
            at += 2 + len;
            let (offset, size) = (u32_at(at)?, u32_at(at + 4)?);
            at += 12;
            index.insert(crate::hash::crc32(path.as_bytes()), (path, offset, size));
        }
        Ok(ScriptPack { blobs: data[at..].to_vec(), index })
    }

    /// A script's text by the path a graph names it with.
    pub fn get(&self, path: &str) -> Option<String> {
        let (_, offset, size) = self.index.get(&path_id(path))?;
        let blob = self.blobs.get(*offset as usize..(*offset + *size) as usize)?;
        crate::cache::decompress(blob).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Every packed script's path.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.index.values().map(|(p, _, _)| p.as_str())
    }

    /// The particle effects the scripts load to play on characters
    /// (`LoadResource([[name]], "CParticlesSystemParamResource")`, for
    /// `CAPI_Pawn.SpawnParticle`), each once, sorted.
    pub fn particle_resources(&self) -> Vec<String> {
        let mut names = std::collections::BTreeSet::new();
        for path in self.paths() {
            let Some(text) = self.get(path) else { continue };
            names.extend(particle_resources_in(&text));
        }
        names.into_iter().collect()
    }
}

/// The particle effects one script loads (see `ScriptPack::particle_resources`).
pub fn particle_resources_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("LoadResource([[") {
        let rest = &text[at + "LoadResource([[".len()..];
        let Some(end) = rest.find("]]") else { continue };
        if rest[end..].trim_start_matches([']', ',', ' ']).starts_with("\"CParticlesSystemParamResource\"") {
            out.push(rest[..end].to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_name_the_particles_they_load() {
        let text = r#"cbox:LoadResource([[FC3_Gameplay_Water.Player.ScreenDropplets]], "CParticlesSystemParamResource");
cbox:LoadResource([[weapons:FC3/AK47]], "CEntityArchetypeResource");"#;
        assert_eq!(particle_resources_in(text), vec!["FC3_Gameplay_Water.Player.ScreenDropplets".to_string()]);
    }

    #[test]
    fn pack_roundtrip() {
        let scripts = vec![
            ("domino\\system\\a.lua".to_string(), b"export = {}".to_vec()),
            ("Domino/User/B.lua".to_string(), b"-- b".to_vec()),
        ];
        let p = ScriptPack::from_bytes(&pack(&scripts)).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p.get("Domino/System/A.lua").as_deref(), Some("export = {}"));
        assert_eq!(p.get("domino\\user\\b.lua").as_deref(), Some("-- b"));
        assert!(p.get("domino/none.lua").is_none());
    }

    #[test]
    fn finds_referenced_scripts() {
        let text = r#"cbox:RegisterBox("Domino/System/Door.lua"); x = Boxes[GetPathID("Domino/System/Print_v2.lua")]; y = "not.lua""#;
        assert_eq!(references(text), vec!["Domino/System/Door.lua", "Domino/System/Print_v2.lua"]);
    }
}
