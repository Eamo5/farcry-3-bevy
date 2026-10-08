//! What the story runtime reads, kept in the cache at conversion.
//!
//! The missions are Domino scripts (see `scripts`); around them the engine
//! keeps world data they rely on: the mission block table, always-loaded
//! ("omni") entities such as the one running the master graph, map data,
//! spawn points, objective markers, the reward and music templates, and the
//! localised strings. Those generated files are copied as they are
//! ([`RAW_FILES`]). From the world itself the story needs the entities scripts
//! name or host, with their components: [`StoryEntity`], gathered from every
//! sector and metasector, every mission layer included.

use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::fcb::Object;

/// Generated game files copied into the cache as they are: (archive path,
/// cache path).
pub const RAW_FILES: [(&str, &str); 11] = [
    (r"worlds\fc3_main\generated\fc3_main.managers.fcb", "story/managers.fcb"),
    (r"worlds\fc3_main\generated\fc3_main.omnis.fcb", "story/omnis.fcb"),
    (r"worlds\fc3_main\generated\fc3_main.mapsdata.fcb", "story/mapsdata.fcb"),
    (r"worlds\fc3_main\generated\fc3_main.mapsdatamain.fcb", "story/mapsdatamain.fcb"),
    (r"worlds\fc3_main\generated\fc3_main.metasectorinfos.fcb", "story/metasectorinfos.fcb"),
    (r"worlds\fc3_main\generated\spawnpointslibrary.fcb", "story/spawnpoints.fcb"),
    (r"worlds\fc3_main\generated\mapobjectivemarkerslibrary.fcb", "story/markers.fcb"),
    (r"worlds\fc3_main\generated\entitylibrary_full.fcb", "story/entitylibrary.fcb"),
    (r"generated\nomadobjecttemplates.fcb", "story/templates.fcb"),
    (r"languages\english\oasisstrings_compressed.bin", "story/strings.bin"),
    (r"languages\english\speechlength.bin", "story/speechlength.bin"),
];

/// The story entities, in the cache.
pub const ENTITIES_REL: &str = "story/entities.bin";

/// What the shop sells, as scripts know it: `model<TAB>archetype<TAB>name
/// id` (a weapon's model path, its library archetype, the string id of its
/// shop name), from the entity library.
pub const SHOP_ITEMS_REL: &str = "story/shop_items.txt";

/// The shop items table from the entity library: weapon archetypes
/// (`weapons:<name>`, whose model is field `C230E653`) joined with their
/// properties (`WeaponProperties:<name>`, `locidDisplayName`).
pub fn shop_items(library: &crate::fcb::Object) -> Vec<(String, String, u32)> {
    use std::collections::HashMap;
    let (mut models, mut names): (HashMap<String, String>, HashMap<String, u32>) = (HashMap::new(), HashMap::new());
    library.walk(&mut |o, _| {
        let Some(h) = o.str("hidName") else { return };
        if let Some(name) = h.strip_prefix("weapons:") {
            let mut model = None;
            o.walk(&mut |c, _| {
                if model.is_none() {
                    model = c.str_h(0xC230_E653).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
                }
            });
            if let Some(m) = model {
                models.entry(name.to_string()).or_insert(m.to_ascii_lowercase());
            }
        } else if let Some(name) = h.strip_prefix("WeaponProperties:") {
            let mut id = None;
            o.walk(&mut |c, _| {
                if id.is_none() {
                    id = c.u32("locidDisplayName").filter(|&i| i != 0 && i != u32::MAX);
                }
            });
            if let Some(i) = id {
                names.entry(name.to_string()).or_insert(i);
            }
        }
    });
    let mut out: Vec<(String, String, u32)> = models.into_iter().filter_map(|(a, m)| names.get(&a).map(|&i| (m, a, i))).collect();
    out.sort();
    out
}

const MAGIC: &[u8; 4] = b"FCSE";
/// Bumped when what the table keeps changes (2: grouped entities, 3: the
/// entity's own fields).
const VERSION: u32 = 3;

/// Whether a stored table is this version's (an older one is rewritten).
pub fn is_current(data: &[u8]) -> bool {
    data.len() >= 8 && &data[..4] == MAGIC && data[4..8] == VERSION.to_le_bytes()
}

/// The groups an entity belongs to: (name hash, name) from its
/// `CGroupComponent` (scripts write a group as `#` and the hash in hex).
pub fn groups(components: &Object) -> Vec<(u32, String)> {
    let Some(c) = components.children.iter().find(|c| c.is("CGroupComponent")) else { return Vec::new() };
    let Some(list) = c.child("Groups") else { return Vec::new() };
    list.children
        .iter()
        .filter_map(|g| {
            let hash = g.u32("group").filter(|&h| h != u32::MAX && h != 0)?;
            Some((hash, g.str_h(0x3B74_FE98).unwrap_or_default()))
        })
        .collect()
}

/// Where an entity was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A world sector (by id).
    Sector(u32),
    /// A metasector (large entities: zones, regions, mission zones).
    Metasector(u32),
}

/// An entity the story runtime may need: one scripts name, host or switch
/// on with a mission layer, with its components as the game stores them.
#[derive(Debug, Clone)]
pub struct StoryEntity {
    pub id: u64,
    pub layer: u64,
    pub source: Source,
    pub name: String,
    pub class: String,
    /// Dunia space (Z up) metres, and degrees.
    pub pos: [f32; 3],
    pub angles: [f32; 3],
    /// The entity's `Components` object (empty when it has none).
    pub components: Arc<Object>,
    /// The entity's own fields besides name, class and placement (a
    /// region's shape points and height, bounds), as an object without
    /// children.
    pub fields: Arc<Object>,
}

/// Entity fields the table keeps (the rest is name, class and placement,
/// stored apart, or editor bookkeeping).
pub const KEPT_FIELDS: [&str; 4] = ["hidShapePoints", "fHeight", "hidBBoxMin", "hidBBoxMax"];

/// An entity's kept fields as an object.
pub fn kept_fields(entity: &Object) -> Arc<Object> {
    let hashes: Vec<u32> = KEPT_FIELDS.iter().map(|n| crate::hash::name_hash(n)).collect();
    Arc::new(Object { name_hash: entity.name_hash, fields: entity.fields.iter().filter(|(h, _)| hashes.contains(h)).cloned().collect(), children: Vec::new() })
}

/// Component classes that make an entity matter to the story whatever its
/// layer: scripts, mission zones, triggers, cinematic scenes, markers, and
/// group membership (scripts address most characters by group).
pub const STORY_COMPONENTS: [&str; 9] = [
    "CDominoComponent",
    "CGroupComponent",
    "CMissionZoneComponent",
    "CTriggerComponent",
    "CProximityTriggerComponent",
    "CTrigger2DComponent",
    "CCinematicSceneComponent",
    "CMapObjectiveComponent",
    "CSafeHouseComponent",
];

/// Whether an entity's components include one of [`STORY_COMPONENTS`].
pub fn has_story_component(components: &Object) -> bool {
    components.children.iter().any(|c| STORY_COMPONENTS.iter().any(|n| c.is(n)))
}

/// Entity ids written in a script (the graphs carry them as decimal
/// strings of the 64-bit `disEntityId`).
pub fn script_entity_ids(text: &str) -> Vec<u64> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j < b.len() && b[j] == b'"' && j - start >= 12 {
                if let Ok(id) = text[start..j].parse::<u64>() {
                    out.push(id);
                }
            }
            i = j.max(start);
        } else {
            i += 1;
        }
    }
    out
}

pub fn to_bytes(entities: &[StoryEntity]) -> Vec<u8> {
    let mut body = Vec::new();
    let s = |out: &mut Vec<u8>, text: &str| {
        out.extend_from_slice(&(text.len() as u32).to_le_bytes());
        out.extend_from_slice(text.as_bytes());
    };
    body.extend_from_slice(&(entities.len() as u32).to_le_bytes());
    for e in entities {
        body.extend_from_slice(&e.id.to_le_bytes());
        body.extend_from_slice(&e.layer.to_le_bytes());
        let (kind, id) = match e.source {
            Source::Sector(id) => (0u8, id),
            Source::Metasector(id) => (1u8, id),
        };
        body.push(kind);
        body.extend_from_slice(&id.to_le_bytes());
        s(&mut body, &e.name);
        s(&mut body, &e.class);
        for v in e.pos.iter().chain(&e.angles) {
            body.extend_from_slice(&v.to_le_bytes());
        }
        for o in [&e.components, &e.fields] {
            let c = crate::fcb::encode(o);
            body.extend_from_slice(&(c.len() as u32).to_le_bytes());
            body.extend_from_slice(&c);
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&crate::cache::compress(&body));
    out
}

pub fn from_bytes(data: &[u8]) -> Result<Vec<StoryEntity>> {
    if data.len() < 8 || &data[..4] != MAGIC {
        bail!("not a story entity table");
    }
    if data[4..8] != VERSION.to_le_bytes() {
        bail!("story entity table version");
    }
    let body = crate::cache::decompress(&data[8..])?;
    let mut r = crate::reader::Reader::new(&body);
    let n = r.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    let text = |r: &mut crate::reader::Reader| -> Result<String> {
        let len = r.u32()? as usize;
        Ok(String::from_utf8_lossy(r.bytes(len)?).into_owned())
    };
    for _ in 0..n {
        let id = r.u64()?;
        let layer = r.u64()?;
        let kind = r.u8()?;
        let source_id = r.u32()?;
        let source = if kind == 0 { Source::Sector(source_id) } else { Source::Metasector(source_id) };
        let name = text(&mut r)?;
        let class = text(&mut r)?;
        let mut v = [0f32; 6];
        for x in &mut v {
            *x = r.f32()?;
        }
        let len = r.u32()? as usize;
        let components = crate::fcb::decode(r.bytes(len)?).context("story entity components")?;
        let len = r.u32()? as usize;
        let fields = crate::fcb::decode(r.bytes(len)?).context("story entity fields")?;
        out.push(StoryEntity { id, layer, source, name, class, pos: [v[0], v[1], v[2]], angles: [v[3], v[4], v[5]], components, fields });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_roundtrip() {
        let comp = Arc::new(Object {
            name_hash: crate::hash::name_hash("Components"),
            fields: vec![],
            children: vec![Arc::new(Object { name_hash: crate::hash::name_hash("CDominoComponent"), fields: vec![(1, vec![1])], children: vec![] })],
        });
        let e = StoryEntity {
            id: 2074483636964429852,
            layer: 0x64,
            source: Source::Metasector(270583456),
            name: "Zone".into(),
            class: "CEntity".into(),
            pos: [1.0, 2.0, 3.0],
            angles: [0.0, 0.0, 90.0],
            components: comp,
            fields: Arc::new(Object { name_hash: 0, fields: vec![(crate::hash::name_hash("fHeight"), (-1.0f32).to_le_bytes().to_vec())], children: vec![] }),
        };
        let back = from_bytes(&to_bytes(&[e.clone()])).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].id, back[0].source, back[0].name.as_str(), back[0].angles), (e.id, e.source, "Zone", e.angles));
        assert!(has_story_component(&back[0].components));
    }

    #[test]
    fn finds_entity_ids_in_scripts() {
        let text = r##"l0.Door = "2074866577666216923"; l0.Count = "12"; x = "abc"; y = "#70B1A04C""##;
        assert_eq!(script_entity_ids(text), vec![2074866577666216923]);
    }
}
