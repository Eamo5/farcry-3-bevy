//! The world data the mission system reads, loaded from the cache: the
//! entities scripts name (with every mission layer's), the mission block
//! table, spawn points, objective markers, rewards and strings.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use fc3_formats::fcb::{self, Object};
use fc3_formats::oasis::Strings;
use fc3_formats::story;
use fc3_formats::world::DEFAULT_MISSION_LAYER;

/// Where an entity comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Sector(u32),
    Metasector(u32),
    /// The always-loaded generated files (map data, omnis, safehouses).
    Global,
}

/// An entity the scripts may refer to.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: u64,
    pub name: String,
    pub class: String,
    /// Dunia space (Z up) metres, and degrees.
    pub pos: [f32; 3],
    pub angles: [f32; 3],
    pub layer: u64,
    pub origin: Origin,
    pub components: Arc<Object>,
    /// The entity's own kept fields (a region's shape).
    pub fields: Arc<Object>,
}

impl Entry {
    pub fn component(&self, class: &str) -> Option<&Arc<Object>> {
        self.components.children.iter().find(|c| c.is(class))
    }

    pub fn has(&self, class: &str) -> bool {
        self.component(class).is_some()
    }
}

/// A mission layer: a set of world entities scripts switch on and off.
#[derive(Debug, Clone)]
pub struct Layer {
    pub id: u64,
    pub name: String,
    pub block: u64,
    /// Activated with its block.
    pub on_activate: bool,
    /// Disabled (removed) when its block is deactivated.
    pub disable_on_deactivate: bool,
}

/// A mission block: a mission's checkpoint (or a container of world state
/// layers), its layers and the zone entity whose script plays it.
#[derive(Debug, Clone, Default)]
pub struct Block {
    pub id: u64,
    pub name: String,
    pub layers: Vec<u64>,
    /// The entity with the block's `CMissionZoneComponent` (its script runs
    /// on it); none for blocks that only hold layers.
    pub zone: Option<u64>,
    /// The world sectors the zone covers (leaving them is leaving the zone).
    pub zone_sectors: Vec<u32>,
    pub warn_on_zone_exit: bool,
    pub is_quest: bool,
    pub refs: Vec<u64>,
    /// A base block (`CBaseMissionBlock`): the world's own content (a bar,
    /// an outpost, an activity), not a mission's.
    pub base: bool,
}

#[derive(Debug, Clone)]
pub struct SpawnPoint {
    pub id: u64,
    pub name: String,
    pub layer: u64,
    pub pos: [f32; 3],
    pub angles: [f32; 3],
}

/// A map / objective marker from the marker library.
#[derive(Debug, Clone)]
pub struct Marker {
    pub id: u64,
    pub name: String,
    pub layer: u64,
    pub pos: [f32; 3],
    /// `mapobjectivetype` key (CRC-32 of the type name).
    pub kind: u64,
    pub sub_kind: u64,
    pub enabled: bool,
    pub tooltip: u32,
    pub spawn_point: Option<u64>,
    pub area: Option<u64>,
}

/// A mission's reward, by its main script name (`DOC_020_MAIN`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Reward {
    pub xp: u32,
    pub money: u32,
}

#[derive(Default)]
pub struct StoryData {
    pub entities: HashMap<u64, Entry>,
    /// Library archetypes the engine spawns characters from, by library id
    /// (placed nowhere: their position is the origin).
    pub archetypes: HashMap<u64, Entry>,
    /// Their library names (lower case) -> library id.
    pub archetype_names: HashMap<String, u64>,
    pub blocks: HashMap<u64, Block>,
    pub layers: HashMap<u64, Layer>,
    /// Blocks active at the start of a new game.
    pub active_on_init: Vec<u64>,
    pub spawn_points: HashMap<u64, SpawnPoint>,
    pub markers: HashMap<u64, Marker>,
    /// Lower-case main script name -> reward.
    pub rewards: HashMap<String, Reward>,
    /// Story gates from the single-player PlayerParamsService template.
    pub menu_unlocks: Option<(String, String)>,
    pub strings: Strings,
    /// Spoken line lengths by sound id (seconds).
    pub speech: HashMap<u32, f32>,
    /// Lower-case entity name -> id (names aren't unique; the first wins).
    pub by_name: HashMap<String, u64>,
    /// Block id by its zone entity.
    pub zone_blocks: HashMap<u64, u64>,
    /// The entity running the master graph.
    pub root: Option<u64>,
    /// Group name hash -> members (scripts write a group as `#` and the
    /// hash in hex).
    pub groups: HashMap<u32, Vec<u64>>,
    /// The markups (`CA_Markup1`...) of contextual actions that stage a
    /// sequence, and when they come (s from the action's start), from the
    /// sequence's own event clips.
    pub action_markups: HashMap<u64, Vec<(f64, String)>>,
    pub group_names: HashMap<u32, String>,
    /// The world's water levels by sector id, as its sectors have them
    /// (what `CDominoWaterLevelManager:GetWaterLevel` gives before a script
    /// moves one).
    pub water: HashMap<u32, f64>,
}

/// Water levels by sector id from the cache's `world/water_planes.txt`
/// (`<sector> <level>` a line).
pub fn water_levels(text: &str) -> HashMap<u32, f64> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

/// A counted array of u64 (u32 count, then the values).
pub fn u64_list(b: &[u8]) -> Vec<u64> {
    if b.len() < 4 {
        return Vec::new();
    }
    let n = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
    b[4..].chunks_exact(8).take(n).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
}

/// A counted array of u32.
pub fn u32_list(b: &[u8]) -> Vec<u32> {
    if b.len() < 4 {
        return Vec::new();
    }
    let n = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
    b[4..].chunks_exact(4).take(n).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn read_fcb(root: &Path, rel: &str) -> Result<Arc<Object>> {
    let bytes = std::fs::read(root.join(rel)).with_context(|| rel.to_string())?;
    let start = bytes.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    fcb::parse(&bytes[start..]).with_context(|| rel.to_string())
}

/// The name a field holds whose name the engine hashes but we only know by
/// that hash (`text_*` duplicates of names, e.g. a spawn point's name).
const TEXT_NAME: u32 = 0xF96F_AF0D;
const BLOCK_NAME: u32 = 0x6C91_FB7A;
const CLASS_NAME: u32 = fc3_formats::world::h::CLASS_NAME;

fn none_if_invalid(id: Option<u64>) -> Option<u64> {
    id.filter(|&i| i != u64::MAX && i != 0)
}

impl StoryData {
    /// Loads everything from the cache's `story/` files.
    pub fn load(root: &Path) -> Result<StoryData> {
        let mut d = StoryData::default();
        let entities = story::from_bytes(&std::fs::read(root.join(story::ENTITIES_REL)).context("story entities")?)?;
        // Contextual actions that stage a sequence: their markups' times.
        for e in &entities {
            let Some(ca) = e.components.children.iter().find(|c| c.name_hash == fc3_formats::hash::name_hash("CAIContextualActionComponent")) else {
                continue;
            };
            let mut paths = Vec::new();
            ca.walk(&mut |o, _| {
                for (_, bytes) in &o.fields {
                    let text = String::from_utf8_lossy(bytes.strip_suffix(&[0]).unwrap_or(bytes)).to_ascii_lowercase();
                    if text.ends_with(".cseq") && !paths.contains(&text) {
                        paths.push(text);
                    }
                }
            });
            let mut markups: Vec<(f64, String)> = Vec::new();
            for path in paths {
                let Some(seq) = std::fs::read_to_string(root.join(fc3_formats::cinema::sequence_rel(&path)))
                    .ok()
                    .and_then(|t| fc3_formats::cinema::Sequence::from_text(&t))
                else {
                    continue;
                };
                for clip in seq.groups.iter().flat_map(|g| &g.clips) {
                    if let fc3_formats::cinema::ClipKind::Event { name } = &clip.kind {
                        if name.starts_with("CA_Markup") && !markups.iter().any(|m| m.1 == *name) {
                            markups.push((clip.start as f64, name.clone()));
                        }
                    }
                }
            }
            if !markups.is_empty() {
                markups.sort_by(|a, b| a.0.total_cmp(&b.0));
                d.action_markups.insert(e.id, markups);
            }
        }
        for e in entities {
            let origin = match e.source {
                story::Source::Sector(id) => Origin::Sector(id),
                story::Source::Metasector(id) => Origin::Metasector(id),
            };
            d.add(Entry {
                id: e.id,
                name: e.name,
                class: e.class,
                pos: e.pos,
                angles: e.angles,
                layer: e.layer,
                origin,
                components: e.components,
                fields: e.fields,
            });
        }
        if let Ok(b) = std::fs::read(root.join(story::ARCHETYPES_REL)) {
            match story::from_bytes(&b) {
                Ok(list) => {
                    for e in list {
                        d.archetype_names.insert(e.name.to_ascii_lowercase(), e.id);
                        d.archetypes.insert(
                            e.id,
                            Entry {
                                id: e.id,
                                name: e.name,
                                class: e.class,
                                pos: e.pos,
                                angles: e.angles,
                                layer: e.layer,
                                origin: Origin::Global,
                                components: e.components,
                                fields: e.fields,
                            },
                        );
                    }
                }
                Err(e) => eprintln!("story: archetypes: {e:#}"),
            }
        }
        // The always-loaded entities: map data (race checkpoints, some
        // zones), safehouses, the master graph's host.
        for rel in ["story/mapsdata.fcb", "story/mapsdatamain.fcb", "story/omnis.fcb"] {
            match read_fcb(root, rel) {
                Ok(o) => d.add_global(&o, DEFAULT_MISSION_LAYER),
                Err(e) => eprintln!("story: {e:#}"),
            }
        }
        let managers = read_fcb(root, "story/managers.fcb")?;
        d.read_blocks(&managers);
        d.add_global(&managers, DEFAULT_MISSION_LAYER);
        if let Ok(o) = read_fcb(root, "story/spawnpoints.fcb") {
            for s in &o.children {
                let Some(id) = s.u64("SpwanPointId") else { continue };
                let sp = SpawnPoint {
                    id,
                    name: s.str_h(TEXT_NAME).unwrap_or_default(),
                    layer: s.u64("EntityLayerId").unwrap_or(DEFAULT_MISSION_LAYER),
                    pos: s.vec3("Pos").unwrap_or_default(),
                    angles: s.vec3("Orientation").unwrap_or_default(),
                };
                d.by_name.entry(sp.name.to_ascii_lowercase()).or_insert(id);
                d.spawn_points.insert(id, sp);
            }
        }
        if let Ok(o) = read_fcb(root, "story/markers.fcb") {
            for m in &o.children {
                let Some(id) = m.u64("GameElementId") else { continue };
                let mk = Marker {
                    id,
                    name: m.str_h(TEXT_NAME).unwrap_or_default(),
                    layer: m.u64("EntityLayerId").unwrap_or(DEFAULT_MISSION_LAYER),
                    pos: m.vec3("Pos").unwrap_or_default(),
                    kind: m.u64("mapobjtypeMapObjectiveType").unwrap_or(0),
                    sub_kind: m.u64("MapObjectiveSubType").unwrap_or(0),
                    enabled: m.bool("bEnable").unwrap_or(false),
                    tooltip: m.u32("locidmapObjectiveToolTipId").unwrap_or(u32::MAX),
                    spawn_point: none_if_invalid(m.u64("entSpawnPointEntity")),
                    area: none_if_invalid(m.u64("entObjectiveAreaEntity")),
                };
                d.by_name.entry(mk.name.to_ascii_lowercase()).or_insert(id);
                d.markers.insert(id, mk);
            }
        }
        if let Ok(o) = read_fcb(root, "story/templates.fcb") {
            o.walk(&mut |obj, _| {
                if obj.str("FullName").as_deref() == Some("playerparamsservice:players.playerparamsservice") {
                    if let Some(unlocks) = obj.children.iter().find(|c| c.is("MissionUnlocks")) {
                        if let (Some(crafting), Some(skills)) = (unlocks.str("sCraftingMenu"), unlocks.str("sSkillsMenu")) {
                            d.menu_unlocks = Some((crafting.to_ascii_lowercase(), skills.to_ascii_lowercase()));
                        }
                    }
                }
                if obj.is("MissionRewards") {
                    for r in &obj.children {
                        if let Some(name) = r.str("sName") {
                            d.rewards.entry(name.to_ascii_lowercase()).or_insert(Reward { xp: r.u32("iXP").unwrap_or(0), money: r.u32("iMoney").unwrap_or(0) });
                        }
                    }
                }
            });
        }
        d.water = water_levels(&std::fs::read_to_string(root.join(fc3_formats::cache::WATER_PLANES_REL)).unwrap_or_default());
        match std::fs::read(root.join("story/strings.bin")).map_err(anyhow::Error::from).and_then(|b| Strings::from_bytes(&b)) {
            Ok(s) => d.strings = s,
            Err(e) => eprintln!("story: strings: {e:#}"),
        }
        match std::fs::read(root.join("story/speechlength.bin")).map_err(anyhow::Error::from).and_then(|b| fc3_formats::oasis::speech_lengths(&b)) {
            Ok(s) => d.speech = s,
            Err(e) => eprintln!("story: speech lengths: {e:#}"),
        }
        // Zones: the entity whose mission zone component names a block.
        let zones: Vec<(u64, u64, Vec<u32>)> = d
            .entities
            .values()
            .filter_map(|e| {
                let z = e.component("CMissionZoneComponent")?;
                Some((z.u64("hidMissionBlockId")?, e.id, z.field("hidSectorsList").map(u32_list).unwrap_or_default()))
            })
            .collect();
        for (block, entity, sectors) in zones {
            d.zone_blocks.insert(entity, block);
            if let Some(b) = d.blocks.get_mut(&block) {
                b.zone = Some(entity);
                b.zone_sectors = sectors;
            }
        }
        d.root = d
            .entities
            .values()
            .find(|e| {
                e.component("CDominoComponent")
                    .and_then(|c| script_path(c))
                    .is_some_and(|p| p.to_ascii_lowercase().ends_with(r"fc3_master\fc3_master.fc3_master.lua"))
            })
            .map(|e| e.id);
        Ok(d)
    }

    fn add(&mut self, e: Entry) {
        if self.entities.contains_key(&e.id) {
            return;
        }
        for (hash, name) in story::groups(&e.components) {
            self.groups.entry(hash).or_default().push(e.id);
            if !name.is_empty() {
                self.group_names.entry(hash).or_insert(name);
            }
        }
        // An alarm box is in the group its alarm names (its own
        // `CGroupComponent` is usually empty).
        if let Some(hash) = e.component("CAlarmPoint").and_then(|a| a.u32("AlarmGroup")).filter(|&h| h != 0 && h != u32::MAX) {
            self.groups.entry(hash).or_default().push(e.id);
        }
        self.by_name.entry(e.name.to_ascii_lowercase()).or_insert(e.id);
        // Sector copies and global copies of one entity: keep the first.
        self.entities.entry(e.id).or_insert(e);
    }

    /// Adds the entities of a generated file (any `Entity` under its
    /// mission layers).
    fn add_global(&mut self, o: &Arc<Object>, layer: u64) {
        let layer = o.u64("missionLayerId").unwrap_or(layer);
        for c in &o.children {
            if c.is("Entity") {
                if let Some(id) = c.u64("disEntityId") {
                    self.add(Entry {
                        id,
                        name: c.str("hidName").unwrap_or_default(),
                        class: c.str_h(CLASS_NAME).unwrap_or_default(),
                        pos: c.vec3("hidPos_precise").or_else(|| c.vec3("hidPos")).unwrap_or_default(),
                        angles: c.vec3("hidAngles").unwrap_or_default(),
                        layer,
                        origin: Origin::Global,
                        components: c.child("Components").cloned().unwrap_or_default(),
                        fields: story::kept_fields(c),
                    });
                }
            } else if !c.is("Components") {
                self.add_global(c, layer);
            }
        }
    }

    fn read_blocks(&mut self, managers: &Arc<Object>) {
        let mut found: Option<&Object> = None;
        managers.walk(&mut |o, _| {
            if found.is_none() && o.is("MissionBlockManager") {
                found = Some(o);
            }
        });
        let Some(m) = found else { return };
        self.active_on_init = m.field("activeOnInitMBlocksList").map(u64_list).unwrap_or_default();
        let mut blocks = Vec::new();
        m.walk(&mut |o, _| {
            if o.is("MissionBlock") {
                blocks.push(o);
            }
        });
        for b in blocks {
            let Some(id) = b.u64("mblockId") else { continue };
            let mut block = Block {
                id,
                name: b.str_h(BLOCK_NAME).unwrap_or_default(),
                warn_on_zone_exit: b.bool("bWarnOnZoneExit").unwrap_or(false),
                is_quest: b.bool("bIsQuest").unwrap_or(false),
                refs: b.field("refMBlocksList").map(u64_list).unwrap_or_default(),
                base: b.str_h(0xCF68_E402).as_deref() == Some("CBaseMissionBlock"),
                ..Block::default()
            };
            if let Some(list) = b.child("MissionBlockLayersList") {
                for l in &list.children {
                    let Some(lid) = l.u64("layerId") else { continue };
                    block.layers.push(lid);
                    self.layers.insert(
                        lid,
                        Layer {
                            id: lid,
                            name: l.str("layerName").unwrap_or_default(),
                            block: id,
                            on_activate: l.bool("isActivatedOnMBlockActivate").unwrap_or(false),
                            disable_on_deactivate: l.bool("isDisabledOnMBlockDeactivate").unwrap_or(false),
                        },
                    );
                }
            }
            self.by_name.entry(block.name.to_ascii_lowercase()).or_insert(id);
            self.blocks.insert(id, block);
        }
    }

    /// A block by name (`jas_020_b10`).
    pub fn block_named(&self, name: &str) -> Option<&Block> {
        let n = name.to_ascii_lowercase();
        self.blocks.values().find(|b| b.name.to_ascii_lowercase() == n)
    }

    /// A layer by name within a block (or anywhere).
    pub fn layer_named(&self, block: Option<u64>, name: &str) -> Option<&Layer> {
        let n = name.to_ascii_lowercase();
        self.layers.values().find(|l| block.is_none_or(|b| l.block == b) && l.name.to_ascii_lowercase() == n)
    }

    /// Where something with this id stands (entity, spawn point or marker).
    pub fn position(&self, id: u64) -> Option<[f32; 3]> {
        self.entities.get(&id).map(|e| e.pos).or_else(|| self.spawn_points.get(&id).map(|s| s.pos)).or_else(|| self.markers.get(&id).map(|m| m.pos))
    }

    /// The layer something with this id belongs to.
    pub fn layer_of(&self, id: u64) -> Option<u64> {
        self.entities.get(&id).map(|e| e.layer).or_else(|| self.spawn_points.get(&id).map(|s| s.layer)).or_else(|| self.markers.get(&id).map(|m| m.layer))
    }

    /// A library archetype by the name scripts spawn it under.
    pub fn archetype_named(&self, name: &str) -> Option<&Entry> {
        self.archetype_names.get(&name.to_ascii_lowercase()).and_then(|id| self.archetypes.get(id))
    }

    pub fn name_of(&self, id: u64) -> Option<&str> {
        self.entities
            .get(&id)
            .map(|e| e.name.as_str())
            .or_else(|| self.spawn_points.get(&id).map(|s| s.name.as_str()))
            .or_else(|| self.markers.get(&id).map(|m| m.name.as_str()))
    }
}

/// The script a Domino component runs.
pub fn script_path(component: &Object) -> Option<String> {
    component.child("ScriptDescriptor").and_then(|s| s.str("fileScriptPath")).filter(|s| !s.is_empty())
}

/// A Domino component's inputs, as the engine writes them into the graph
/// before its `Init`.
#[derive(Debug, Clone, PartialEq)]
pub enum InputValue {
    Number(f64),
    Text(String),
    Bool(bool),
}

pub fn script_inputs(component: &Object) -> Vec<(String, InputValue)> {
    let Some(inputs) = component.child("ScriptDescriptor").and_then(|s| s.child("DominoInputs")) else { return Vec::new() };
    let mut out = Vec::new();
    for i in &inputs.children {
        let Some(name) = i.str("Name") else { continue };
        let class = i.str_h(0xCF68_E402).unwrap_or_default();
        let raw = i.field("Value").unwrap_or_default();
        let u32v = || raw.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let value = match class.as_str() {
            "CDominoInputEntityId" | "CDominoInputunsigned long long" => {
                InputValue::Text(raw.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(u64::MAX).to_string())
            }
            // A group's name hash (none when 0).
            "CDominoInputEntityGroup" => match u32v().filter(|&h| h != 0) {
                Some(h) => InputValue::Text(format!("#{h:08X}")),
                None => continue,
            },
            "CDominoInputfloat" => InputValue::Number(raw.get(..4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0.0) as f64),
            "CDominoInputunsigned long" => InputValue::Number(u32v().unwrap_or(0) as f64),
            "CDominoInputbool" => InputValue::Bool(raw.first().is_some_and(|&b| b != 0)),
            "CDominoSoundInput" => InputValue::Text(format!("0x{:08X}", u32v().unwrap_or(u32::MAX))),
            _ => InputValue::Text(fcb::bytes_to_string(raw)),
        };
        out.push((name, value));
    }
    out
}
