//! Encounters: what the world spawns around the player. Wildlife regions
//! (`CWildlifeRegion` shapes in the world sectors) each name spawn settings
//! (`encounterspawnsettings.lib`: how near and far from the player) and a
//! weighted list of prefabs (`encounterprefablist.lib`); a prefab
//! (`generated\encounters\<id>.fcb`, packed: its size, then LZO, around an
//! ordinary FCB) places its animals about its origin. The cache keeps the
//! regions, settings, lists and the prefabs' animals as text
//! ([`WILDLIFE_REL`]).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const WILDLIFE_REL: &str = "world/wildlife.txt";
const MAGIC: &str = "fc3-wildlife 2";
/// The single-player encounter service's settings among the templates.
pub const SERVICE: &str = "encounterservice:settings.fcxsingle";
pub const SPAWN_SETTINGS_LIB: &str = r"generated\databases\generic\encounterspawnsettings.lib";
pub const PREFAB_LIST_LIB: &str = r"generated\databases\generic\encounterprefablist.lib";
/// The class of a wildlife region's entity.
pub const REGION_CLASS: &str = "CWildlifeRegion";

/// A packed prefab's FCB bytes, or `None` if these are not packed.
pub fn unpack(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 8 || bytes.starts_with(b"nbCF") {
        return None;
    }
    let size = u32::from_le_bytes(bytes[..4].try_into().ok()?) as usize;
    if size < bytes.len() || size > 64 << 20 {
        return None;
    }
    let out = crate::lzo::decompress(&bytes[4..], size).ok()?;
    out.starts_with(b"nbCF").then_some(out)
}

/// The archive path of a prefab by its id.
pub fn prefab_path(id: u64) -> String {
    format!(r"generated\encounters\{id}.fcb")
}

/// Unpacks and parses a prefab.
pub fn parse(bytes: &[u8]) -> Result<std::sync::Arc<Object>> {
    let Some(raw) = unpack(bytes) else { bail!("not a packed encounter prefab") };
    crate::fcb::parse(&raw)
}

/// Spawning about the player: between `min` and `max` metres off, within
/// `angle` degrees round him, at least `separation` metres from other
/// wildlife.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpawnSettings {
    pub min: f32,
    pub max: f32,
    pub angle: f32,
    pub separation: f32,
}

/// An animal a prefab places: its library name
/// (`FC3_Quadrupeds/Crocodile.CrocodileJungle_3`), its offset from the
/// prefab's origin (Dunia, metres) and its heading (degrees about Z).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Animal {
    pub archetype: String,
    pub offset: [f32; 3],
    pub yaw: f32,
}

/// A wildlife region: a shape on the map (its corners, Dunia, and the
/// triangles filling it), its spawn settings and prefab list (by key), and
/// whether it spawns on land and in water.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Region {
    pub name: String,
    pub settings: u64,
    pub list: u64,
    pub land: bool,
    pub water: bool,
    pub points: Vec<[f32; 3]>,
    pub triangles: Vec<[u16; 3]>,
}

impl Region {
    /// Whether a map point (Dunia x, y) lies inside it.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let inside = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
            let s = |p: [f32; 3], q: [f32; 3]| (q[0] - p[0]) * (y - p[1]) - (q[1] - p[1]) * (x - p[0]);
            let (d1, d2, d3) = (s(a, b), s(b, c), s(c, a));
            !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
        };
        self.triangles.iter().any(|t| {
            let p = |i: u16| self.points.get(i as usize).copied();
            matches!((p(t[0]), p(t[1]), p(t[2])), (Some(a), Some(b), Some(c)) if inside(a, b, c))
        })
    }

    /// Its extent on the map (min x, min y, max x, max y).
    pub fn bounds(&self) -> [f32; 4] {
        self.points.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])])
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Wildlife {
    pub settings: HashMap<u64, SpawnSettings>,
    /// Prefab lists: (prefab id, weight); a 0 id is a chance of nothing.
    pub lists: HashMap<u64, Vec<(u64, f32)>>,
    /// Prefabs' animals by prefab id, and what each costs of the budget.
    pub prefabs: HashMap<u64, Vec<Animal>>,
    pub costs: HashMap<u64, f32>,
    pub regions: Vec<Region>,
    /// The wildlife encounters' budget (`WildlifeEncounters`: the costs of
    /// the groups out at once) and the most groups out at once.
    pub budget: f32,
    pub max_groups: u32,
}

fn float(o: &Object, name: &str) -> f32 {
    match o.field(name) {
        Some(b) if b.len() >= 4 => f32::from_le_bytes(b[..4].try_into().unwrap()),
        // (A zero is stored as a single byte.)
        Some(b) if b.len() == 1 => b[0] as f32,
        _ => 0.0,
    }
}

/// The spawn settings library's entries by key.
pub fn spawn_settings(lib: &Object) -> HashMap<u64, SpawnSettings> {
    let mut out = HashMap::new();
    for e in &lib.children {
        let Some(key) = e.u64("hidKey") else { continue };
        out.insert(
            key,
            SpawnSettings {
                min: float(e, "fSpawnDistanceMin"),
                max: float(e, "fSpawnDistanceMax"),
                angle: float(e, "fSpawnAngle"),
                separation: float(e, "fWildlifeSeparationDistance"),
            },
        );
    }
    out
}

/// The prefab list library's lists by key.
pub fn prefab_lists(lib: &Object) -> HashMap<u64, Vec<(u64, f32)>> {
    let mut out = HashMap::new();
    for e in &lib.children {
        let Some(key) = e.u64("hidKey") else { continue };
        let Some(list) = e.child("PrefabList") else { continue };
        let items = list.children_named("PrefabInfo").map(|p| (p.u64("pfabPrefab").unwrap_or(0), float(p, "fWeight"))).filter(|(_, w)| *w > 0.0).collect();
        out.insert(key, items);
    }
    out
}

/// A world entity's wildlife region, if it is one.
pub fn region(e: &Object) -> Option<Region> {
    if e.str_h(crate::world::h::CLASS_NAME).as_deref() != Some(REGION_CLASS) {
        return None;
    }
    let shape = e.field("hidShapePoints")?;
    let n = u32::from_le_bytes(shape.get(..4)?.try_into().ok()?) as usize;
    let points: Vec<[f32; 3]> = (0..n)
        .filter_map(|i| {
            let at = 4 + i * 12;
            let f = |k: usize| shape.get(at + k * 4..at + k * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
            Some([f(0)?, f(1)?, f(2)?])
        })
        .collect();
    let tris = e.field("hidTriangleIndices").unwrap_or_default();
    let m = tris.get(..4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let index = |i: usize| tris.get(4 + i * 2..6 + i * 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()));
    let triangles = (0..m / 3).filter_map(|t| Some([index(t * 3)?, index(t * 3 + 1)?, index(t * 3 + 2)?])).collect();
    Some(Region {
        name: e.str("hidName").unwrap_or_default(),
        settings: e.u64("encounterspawnsettingsSpawnSettings").unwrap_or(0),
        list: e.u64("encounterprefablistEncounterPrefabs").unwrap_or(0),
        land: e.bool("bUsedForLandSpawning").unwrap_or(false),
        water: e.bool("bUsedForWaterSpawning").unwrap_or(false),
        points,
        triangles,
    })
    .filter(|r| r.points.len() >= 3)
}

/// What a prefab costs of the budget (`hidAIBudgetValue`).
pub fn prefab_cost(root: &Object) -> f32 {
    root.children
        .iter()
        .find(|c| c.is("Entity"))
        .and_then(|e| e.child("Components"))
        .and_then(|c| c.child("CEncounterAttribsComponent"))
        .and_then(|c| c.child("EncounterAttributes"))
        .map_or(0.0, |a| float(a, "hidAIBudgetValue"))
}

/// The wildlife encounters' budget and most groups (from the templates).
pub fn service_budget(templates: &Object) -> Option<(f32, u32)> {
    let mut out = None;
    templates.walk(&mut |o, _| {
        if out.is_none() && o.str("FullName").as_deref() == Some(SERVICE) {
            out = o.child("WildlifeEncounters").map(|w| (float(w, "fBudget"), w.u32("nMaxPrefabToSpawn").unwrap_or(0)));
        }
    });
    out
}

/// A prefab's animals, placed about its origin.
pub fn prefab_animals(root: &Object) -> Vec<Animal> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    entities
        .iter()
        .skip(1)
        .filter(|e| e.child("Components").is_some_and(|c| c.child("CFCXCountersComponentAnimal").is_some()))
        .filter_map(|e| {
            let at = e.vec3("hidPos")?;
            Some(Animal {
                archetype: e.str("hidName")?,
                offset: [at[0] - origin[0], at[1] - origin[1], at[2] - origin[2]],
                yaw: e.vec3("hidAngles").map_or(0.0, |a| a[2]),
            })
        })
        .collect()
}

pub const MARKERS_REL: &str = "world/encounters.txt";
const MARKERS_MAGIC: &str = "fc3-encounters 15";

/// Whether a cached markers table is this version's.
pub fn markers_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MARKERS_MAGIC)
}

/// The world's encounter markers (`CEncounterMarkerComponent`): by the
/// marker entity's id, the prefabs it spawns while the area is hostile and
/// once it is friendly (a 0 is a chance of nothing); and the people each
/// prefab places (`CPawnEntity`: archetype, offset, heading). The cache
/// keeps them as text ([`MARKERS_REL`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Markers {
    pub markers: HashMap<u64, (Vec<u64>, Vec<u64>)>,
    pub prefabs: HashMap<u64, Vec<Animal>>,
    /// The vehicles each prefab parks (by their descriptor's path, under
    /// `vehicles\`).
    pub vehicles: HashMap<u64, Vec<Animal>>,
    /// The guard spots each prefab sets out (by what is done there, as
    /// `crate::cache::guard_spot_kind` names it).
    pub spots: HashMap<u64, Vec<Animal>>,
    /// The props each prefab sets out (by their model's path): sandbags,
    /// crates, sacks, camp fires, cages.
    pub props: HashMap<u64, Vec<Animal>>,
    /// The animals each prefab brings (by archetype): attack dogs with their
    /// handlers, predators on the prowl.
    pub beasts: HashMap<u64, Vec<Animal>>,
    /// The bodies each prefab leaves (by archetype: `Corpses/Corpse.*`):
    /// pirates, rebels and civilians, and animals' carcasses.
    pub corpses: HashMap<u64, Vec<Animal>>,
    /// The fires its props burn (camp fires' flames) and the light they
    /// give.
    pub fires: HashMap<u64, Vec<Fire>>,
    /// The prefabs whose rescued civilians call the player over and reward
    /// him (those with `STP_CivCallPlayerReward*` points), and the cash a
    /// rescue pays per survivor (the encounter library's
    /// `Cash_Base_Reward`).
    pub rewards: std::collections::BTreeSet<u64>,
    pub reward_cash: u32,
    /// The prefabs whose shooters (an executioner, a firing squad) kill
    /// their captives once the player comes near, and how.
    pub executions: HashMap<u64, Execution>,
}

/// An execution, as the prefab's script runs it: once the player is within
/// `near` metres (its proximity listener's `nearZone`), each shooter kills
/// a captive every `interval` seconds (its longest delay), while he lives,
/// firing from the attack point by them (`AttackTarget_*`: offsets from the
/// prefab's origin, Dunia axes).
#[derive(Debug, Clone, PartialEq)]
pub struct Execution {
    pub near: f32,
    pub interval: f32,
    pub points: Vec<[f32; 3]>,
}

/// A prefab's attack points (where its shooters fire from).
pub fn prefab_attack_points(root: &Object) -> Vec<[f32; 3]> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    entities
        .iter()
        .filter(|e| e.str("hidName").is_some_and(|n| n.to_ascii_lowercase().contains("attacktarget")))
        .filter_map(|e| e.vec3("hidPos"))
        .map(|p| [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]])
        .collect()
}

/// A pawn of a prefab that executes its captives (by its archetype).
pub fn is_shooter(archetype: &str) -> bool {
    let a = archetype.to_ascii_lowercase();
    a.contains("executioner") || a.contains("_squad")
}

/// The script a prefab runs (its Domino component's), archive path with
/// forward slashes as the script pack keys it.
pub fn prefab_script(root: &Object) -> Option<String> {
    let mut out = None;
    root.walk(&mut |o, _| {
        if out.is_none() && o.is("ScriptDescriptor") {
            out = o.str("fileScriptPath").map(|p| p.replace('\\', "/").to_ascii_lowercase());
        }
    });
    out
}

/// An execution from a prefab's script text, if its pawns include
/// shooters: the proximity listener's radius and the longest delay.
pub fn execution(script: &str, pawns: &[Animal]) -> Option<Execution> {
    if !pawns.iter().any(|p| is_shooter(&p.archetype)) {
        return None;
    }
    let number = |key: &str| -> Vec<f32> {
        script.match_indices(key).filter_map(|(i, _)| script[i + key.len()..].split(|c: char| !(c.is_ascii_digit() || c == '.')).next()?.parse().ok()).collect()
    };
    let near = *number("nearZone = ").first()?;
    let interval = number("seconds = ").into_iter().fold(0.0f32, f32::max);
    (near > 0.0 && interval > 0.0).then_some(Execution { near, interval, points: Vec::new() })
}

/// Whether a prefab's rescued civilians call the player over for a reward.
pub fn prefab_rewards(root: &Object) -> bool {
    root.children.iter().filter(|c| c.is("Entity")).any(|e| e.str("hidName").is_some_and(|n| n.to_ascii_lowercase().contains("stp_civcallplayerreward")))
}

/// The encounter library's reward graph, and its base cash.
pub const REWARD_SCRIPT: &str = "domino/user/fc3/fc3_encounters/libs/encounter_library.giveplayerreward.lua";

/// A rescue's cash per survivor, from the reward graph's text.
pub fn reward_cash(script: &str) -> Option<u32> {
    let at = script.find("Cash_Base_Reward = ")? + "Cash_Base_Reward = ".len();
    script[at..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// A fire a prefab's prop burns: its world effect (`cache::world_fx_kind`;
/// empty for one the game draws no flames for) and light, about the
/// prefab's origin in Bevy's axes (before the prefab is turned).
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub kind: String,
    pub offset: [f32; 3],
    pub light: Option<crate::lights::WorldLight>,
}

/// An effect a prefab's prop plays on itself (a camp fire's flames): the
/// effect, the bone of the prop's mesh it plays at (empty: the prop's
/// origin) and its scale; the prop's mesh, its offset from the prefab's
/// origin (Dunia axes) and angles.
#[derive(Debug, Clone, PartialEq)]
pub struct PropEffect {
    pub name: String,
    pub bone: String,
    pub scale: f32,
    pub mesh: String,
    pub offset: [f32; 3],
    pub angles: [f32; 3],
}

/// The effects a prefab's props play on themselves.
pub fn prefab_effects(root: &Object) -> Vec<PropEffect> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entities.iter().skip(1) {
        let w = crate::world::parse_entity(e);
        let offset = [w.pos[0] - origin[0], w.pos[1] - origin[1], w.pos[2] - origin[2]];
        let mesh = w.meshes.first().cloned().unwrap_or_default();
        let own = w.effect.iter().map(|n| (n.clone(), String::new(), 1.0));
        for (name, bone, scale) in own.chain(w.attached_effects.iter().map(|a| (a.name.clone(), a.bone.clone(), a.scale))) {
            out.push(PropEffect { name, bone, scale, mesh: mesh.clone(), offset, angles: w.angles });
        }
    }
    out
}

/// A world entity's encounter marker (its id and prefab lists), if it is
/// one.
pub fn marker(e: &Object) -> Option<(u64, Vec<u64>, Vec<u64>)> {
    let m = e.child("Components")?.child("CEncounterMarkerComponent")?;
    let id = e.u64("disEntityId")?;
    let list = |n: &str| m.child(n).map(|l| l.children_named("Prefab").map(|p| p.u64("pfabItems").unwrap_or(0)).collect::<Vec<_>>()).unwrap_or_default();
    Some((id, list("EncounterPrefabs"), list("FriendlyEncounterPrefabs")))
}

/// A prefab's people, placed about its origin.
pub fn prefab_pawns(root: &Object) -> Vec<Animal> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    entities
        .iter()
        .skip(1)
        .filter(|e| e.str_h(crate::world::h::CLASS_NAME).as_deref() == Some("CPawnEntity"))
        .filter_map(|e| {
            let at = e.vec3("hidPos")?;
            Some(Animal {
                archetype: e.str("hidName")?,
                offset: [at[0] - origin[0], at[1] - origin[1], at[2] - origin[2]],
                yaw: e.vec3("hidAngles").map_or(0.0, |a| a[2]),
            })
        })
        .collect()
}

/// A prefab's guard spots (its `SimpleAnimation` points), placed about its
/// origin.
pub fn prefab_spots(root: &Object, role_clip: &dyn Fn(&str, &str) -> Option<String>) -> Vec<Animal> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    // The scenes' loops, where their leads stand (an extra's part is in his
    // lead's sequence).
    let leads: Vec<([f32; 3], String)> =
        entities.iter().filter_map(|e| Some((e.vec3("hidPos")?, scene_role(e).filter(|(r, _)| *r == "leadActor")?.1))).collect();
    entities
        .iter()
        .skip(1)
        .filter_map(|e| {
            let name = e.str("hidName")?;
            // What is done there: a kind we know, or the loop its own move
            // plays (`move:<name>`, for the fighters' others: cleaning a
            // gun, the radio, searching).
            // A synced scene's actor plays his part of its loop
            // (`clip:<path>`: a conversation's speaker and listener).
            let at = e.vec3("hidPos")?;
            let near_lead = || {
                let d = |p: &[f32; 3]| (p[0] - at[0]).hypot(p[1] - at[1]);
                leads.iter().filter(|(p, _)| d(p) < SCENE_REACH).min_by(|a, b| d(&a.0).total_cmp(&d(&b.0))).map(|(_, s)| s.clone())
            };
            let n = name.to_ascii_lowercase();
            let role = scene_role(e).or_else(|| n.contains("syncanimextraactor").then(|| near_lead().map(|s| ("extraActor1", s))).flatten());
            let scene = role.and_then(|(role, looped)| role_clip(&looped, role));
            let kind = match (crate::cache::guard_spot_kind(&name), scene) {
                (Some(k), _) => k.to_string(),
                (None, Some(clip)) if is_fighter_spot(&name) => format!("clip:{clip}"),
                _ => format!("move:{}", fighter_spot_loop(e, &name)?),
            };
            Some(Animal { archetype: kind, offset: [at[0] - origin[0], at[1] - origin[1], at[2] - origin[2]], yaw: e.vec3("hidAngles").map_or(0.0, |a| a[2]) })
        })
        .collect()
}

/// The loop move a fighters' smart terrain point plays (its parameters'
/// `..._loop` or `..._wait`), if it is one: not an animal's, a civilian's, a
/// heavy's or an executioner's. A synced scene's actors (a conversation's
/// lead and extra) wait at theirs with the scene's waiting loop.
fn fighter_spot_loop(e: &Object, name: &str) -> Option<String> {
    let params = e.child("Components")?.child("CAIContextualActionComponent")?.child("Parameters")?;
    fighter_spot_move(name, &crate::moves::move_names(params))
}

/// Of a smart terrain point's moves (by its name), the loop it plays, if
/// it is a fighters' one: not an animal's, a civilian's, a heavy's or an
/// executioner's. A synced scene's actors wait at theirs.
pub fn fighter_spot_move(name: &str, moves: &[String]) -> Option<String> {
    is_fighter_spot(name).then(|| moves.iter().find(|m| m.ends_with("_loop") || m.ends_with("_wait")).cloned()).flatten()
}

/// Whether a smart terrain point (by its name) is a fighters' one: not an
/// animal's, a civilian's, a heavy's, an executioner's or a victim's.
pub fn is_fighter_spot(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // (What follows the last mention: `SyncAnimLeadActor.SyncAnimLeadActor_X`.)
    let what =
        ["simpleanimation", "syncanimleadactor", "syncanimextraactor", "syncanimmaster"].iter().find(|k| n.contains(**k)).and_then(|k| n.rsplit(k).next());
    // (Nor a victim's part in a scene: the one beaten.)
    what.is_some_and(|what| !n.contains("animalsimpleanimation") && !["heavy", "beheader", "civ", "extw_", "beaten", "victim"].iter().any(|k| what.contains(k)))
}

/// How far (m) an extra stands from his scene's lead.
const SCENE_REACH: f32 = 5.0;

/// A synced scene's actor point (`SyncAnimLeadActor`, `SyncAnimExtraActor`):
/// its part in the scene's sequences (`leadActor`, `extraActor1`) and the
/// loop sequence it plays (archive path, from its parameters).
pub fn scene_role(e: &Object) -> Option<(&'static str, String)> {
    let n = e.str("hidName")?.to_ascii_lowercase();
    let role = if n.contains("syncanimleadactor") {
        "leadActor"
    } else if n.contains("syncanimextraactor") {
        "extraActor1"
    } else {
        return None;
    };
    let params = e.child("Components")?.child("CAIContextualActionComponent")?.child("Parameters")?;
    let looped = params.fields.iter().filter(|(_, v)| v.last() == Some(&0) && v.len() > 5).map(|(_, v)| crate::fcb::bytes_to_string(v)).find(|s| {
        let l = s.to_ascii_lowercase();
        l.ends_with(".cseq") && l.rsplit(['\\', '/']).next().is_some_and(|f| f.contains("loop"))
    })?;
    Some((role, looped))
}

/// The clip a part plays in a sequence (its first animation clip's source),
/// from the sequence's XML.
pub fn role_clip(cseq_xml: &str, role: &str) -> Option<String> {
    let root = crate::xml::parse(cseq_xml)?;
    let mut out = None;
    root.walk(&mut |e| {
        if out.is_none() && e.name == "CCharacterTrackGroup" && e.attr("sTag") == Some(role) {
            e.walk(&mut |c| {
                if out.is_none() && c.name == "CAnimationClip" {
                    out = c.attr("sAnimationSource").map(str::to_string);
                }
            });
        }
    });
    out
}

/// The moves a prefab's smart terrain points name (for the move table).
pub fn prefab_moves(root: &Object) -> Vec<String> {
    let mut out = Vec::new();
    root.walk(&mut |o, _| {
        if o.is("CAIContextualActionComponent") {
            if let Some(p) = o.child("Parameters") {
                out.extend(crate::moves::move_names(p));
            }
        }
    });
    out
}

/// A prefab's bodies (`Corpses/Corpse.*`), placed about its origin.
pub fn prefab_corpses(root: &Object) -> Vec<Animal> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    entities
        .iter()
        .skip(1)
        .filter_map(|e| {
            let name = e.str("hidName").filter(|n| n.starts_with("Corpses/Corpse"))?;
            let at = e.vec3("hidPos")?;
            Some(Animal { archetype: name, offset: [at[0] - origin[0], at[1] - origin[1], at[2] - origin[2]], yaw: e.vec3("hidAngles").map_or(0.0, |a| a[2]) })
        })
        .collect()
}

/// A prefab's props: the entities with a model of their own that are no
/// one's body (not people, animals or vehicles) and no guard spot, placed
/// about its origin, by their model's path.
pub fn prefab_props(root: &Object) -> Vec<Animal> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    entities
        .iter()
        .skip(1)
        // (Bodies are dressed from kits and animated: not props.)
        .filter(|e| {
            e.child("Components").is_none_or(|c| {
                ["CFCXAIComponent", "CFCXVehicle", "CGraphicKitComponent", "CAnimationComponent", "CCharacterPhysComponent"]
                    .iter()
                    .all(|n| c.child(n).is_none())
            })
        })
        .filter(|e| e.str("hidName").is_none_or(|n| crate::cache::guard_spot_kind(&n).is_none()))
        .filter_map(|e| {
            let w = crate::world::parse_entity(e);
            if w.class.contains("Pawn") {
                return None;
            }
            let model = w.meshes.first().filter(|m| !m.to_ascii_lowercase().contains(r"\characters\"))?.clone();
            Some(Animal { archetype: model, offset: [w.pos[0] - origin[0], w.pos[1] - origin[1], w.pos[2] - origin[2]], yaw: w.angles[2] })
        })
        .collect()
}

/// A prefab's vehicles (entities described under `vehicles\`), placed about
/// its origin.
pub fn prefab_vehicles(root: &Object) -> Vec<Animal> {
    let entities: Vec<&std::sync::Arc<Object>> = root.children.iter().filter(|c| c.is("Entity")).collect();
    let Some(origin) = entities.first().and_then(|e| e.vec3("hidPos")) else { return Vec::new() };
    entities
        .iter()
        .skip(1)
        .filter_map(|e| {
            let path = e.child("Components")?.child("CFileDescriptorComponent")?.str_h(0x2A7B_CA49)?.to_ascii_lowercase();
            if !path.contains(r"\vehicles\") {
                return None;
            }
            let at = e.vec3("hidPos")?;
            Some(Animal { archetype: path, offset: [at[0] - origin[0], at[1] - origin[1], at[2] - origin[2]], yaw: e.vec3("hidAngles").map_or(0.0, |a| a[2]) })
        })
        .collect()
}

impl Markers {
    pub fn to_text(&self) -> String {
        let ids = |v: &[u64]| v.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(";");
        let mut s = format!("{MARKERS_MAGIC}\n");
        let mut keys: Vec<_> = self.markers.keys().collect();
        keys.sort();
        for k in keys {
            let (h, f) = &self.markers[k];
            s += &format!("marker\t{k}\t{}\t{}\n", ids(h), ids(f));
        }
        let mut keys: Vec<_> = self.prefabs.keys().collect();
        keys.sort();
        for k in keys {
            let pawns: Vec<String> =
                self.prefabs[k].iter().map(|a| format!("{}:{},{},{},{}", a.archetype, a.offset[0], a.offset[1], a.offset[2], a.yaw)).collect();
            s += &format!("prefab\t{k}\t{}\n", pawns.join(";"));
        }
        for (tag, map) in [("vehicles", &self.vehicles), ("spots", &self.spots), ("props", &self.props), ("beasts", &self.beasts), ("corpses", &self.corpses)] {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for k in keys {
                let list: Vec<String> = map[k].iter().map(|a| format!("{}:{},{},{},{}", a.archetype, a.offset[0], a.offset[1], a.offset[2], a.yaw)).collect();
                s += &format!("{tag}\t{k}\t{}\n", list.join(";"));
            }
        }
        let mut keys: Vec<_> = self.executions.keys().collect();
        keys.sort();
        for k in keys {
            let x = &self.executions[k];
            let points: Vec<String> = x.points.iter().map(|p| format!("{},{},{}", p[0], p[1], p[2])).collect();
            s += &format!("execution\t{k}\t{}\t{}\t{}\n", x.near, x.interval, points.join(";"));
        }
        if !self.rewards.is_empty() {
            let ids: Vec<String> = self.rewards.iter().map(|i| i.to_string()).collect();
            s += &format!("rewards\t{}\t{}\n", self.reward_cash, ids.join(";"));
        }
        // A fire per line: its effect and offset, then its light as the
        // lights table writes one.
        let mut keys: Vec<_> = self.fires.keys().collect();
        keys.sort();
        for k in keys {
            for f in &self.fires[k] {
                let light = f.light.map(|l| crate::lights::to_text(&[l]).lines().last().unwrap_or_default().to_string()).unwrap_or_default();
                s += &format!("fire\t{k}\t{}\t{},{},{}\t{light}\n", f.kind, f.offset[0], f.offset[1], f.offset[2]);
            }
        }
        s
    }

    pub fn from_text(text: &str) -> Result<Markers> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(MARKERS_MAGIC) {
            bail!("not an encounter markers table");
        }
        let mut m = Markers::default();
        let ids = |s: &str| s.split(';').filter_map(|i| i.parse::<u64>().ok()).collect::<Vec<_>>();
        for l in lines {
            let f: Vec<&str> = l.split('\t').collect();
            match (f.first().copied(), f.get(1).and_then(|k| k.parse::<u64>().ok())) {
                (Some("marker"), Some(k)) => {
                    m.markers.insert(k, (ids(f.get(2).unwrap_or(&"")), ids(f.get(3).unwrap_or(&""))));
                }
                (Some("execution"), Some(k)) => {
                    let n = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok());
                    if let (Some(near), Some(interval)) = (n(2), n(3)) {
                        let points = f
                            .get(4)
                            .unwrap_or(&"")
                            .split(';')
                            .filter_map(|p| {
                                let v: Vec<f32> = p.split(',').filter_map(|x| x.parse().ok()).collect();
                                (v.len() == 3).then(|| [v[0], v[1], v[2]])
                            })
                            .collect();
                        m.executions.insert(k, Execution { near, interval, points });
                    }
                }
                (Some("rewards"), Some(cash)) => {
                    m.reward_cash = cash as u32;
                    m.rewards = ids(f.get(2).unwrap_or(&"")).into_iter().collect();
                }
                (Some("fire"), Some(k)) => {
                    let v: Vec<f32> = f.get(3).unwrap_or(&"").split(',').filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 3 {
                        let light = crate::lights::from_text(f.get(4).unwrap_or(&"")).into_iter().next();
                        m.fires.entry(k).or_default().push(Fire { kind: f.get(2).unwrap_or(&"").to_string(), offset: [v[0], v[1], v[2]], light });
                    }
                }
                (Some(kind @ ("prefab" | "vehicles" | "spots" | "props" | "beasts" | "corpses")), Some(k)) => {
                    let pawns = f
                        .get(2)
                        .unwrap_or(&"")
                        .split(';')
                        .filter_map(|a| {
                            let (name, v) = a.rsplit_once(':')?;
                            let v: Vec<f32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                            (v.len() == 4).then(|| Animal { archetype: name.to_string(), offset: [v[0], v[1], v[2]], yaw: v[3] })
                        })
                        .collect();
                    match kind {
                        "prefab" => m.prefabs.insert(k, pawns),
                        "vehicles" => m.vehicles.insert(k, pawns),
                        "spots" => m.spots.insert(k, pawns),
                        "props" => m.props.insert(k, pawns),
                        "beasts" => m.beasts.insert(k, pawns),
                        _ => m.corpses.insert(k, pawns),
                    };
                }
                _ => {}
            }
        }
        Ok(m)
    }
}

pub fn to_text(w: &Wildlife) -> String {
    let mut s = format!("{MAGIC}\nbudget\t{}\t{}\n", w.budget, w.max_groups);
    let mut keys: Vec<_> = w.settings.keys().collect();
    keys.sort();
    for k in keys {
        let v = w.settings[k];
        s += &format!("settings\t{k}\t{},{},{},{}\n", v.min, v.max, v.angle, v.separation);
    }
    let mut keys: Vec<_> = w.lists.keys().collect();
    keys.sort();
    for k in keys {
        s += &format!("list\t{k}\t{}\n", w.lists[k].iter().map(|(p, wt)| format!("{p}:{wt}")).collect::<Vec<_>>().join(";"));
    }
    let mut keys: Vec<_> = w.prefabs.keys().collect();
    keys.sort();
    for k in keys {
        let animals: Vec<String> = w.prefabs[k].iter().map(|a| format!("{}:{},{},{},{}", a.archetype, a.offset[0], a.offset[1], a.offset[2], a.yaw)).collect();
        s += &format!("prefab\t{k}\t{}\t{}\n", animals.join(";"), w.costs.get(k).copied().unwrap_or(0.0));
    }
    for r in &w.regions {
        let points: Vec<String> = r.points.iter().map(|p| format!("{},{},{}", p[0], p[1], p[2])).collect();
        let tris: Vec<String> = r.triangles.iter().map(|t| format!("{},{},{}", t[0], t[1], t[2])).collect();
        s += &format!("region\t{}\t{}\t{}\t{}{}\t{}\t{}\n", r.name, r.settings, r.list, u8::from(r.land), u8::from(r.water), points.join(";"), tris.join(";"));
    }
    s
}

pub fn from_text(text: &str) -> Result<Wildlife> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a wildlife table");
    }
    let mut w = Wildlife::default();
    let nums = |s: &str| s.split(',').filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<_>>();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.first() == Some(&"budget") {
            w.budget = f.get(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
            w.max_groups = f.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
            continue;
        }
        match (f.first().copied(), f.get(1).and_then(|k| k.parse::<u64>().ok())) {
            (Some("settings"), Some(k)) => {
                let v = nums(f.get(2).unwrap_or(&""));
                if v.len() == 4 {
                    w.settings.insert(k, SpawnSettings { min: v[0], max: v[1], angle: v[2], separation: v[3] });
                }
            }
            (Some("list"), Some(k)) => {
                let items = f
                    .get(2)
                    .unwrap_or(&"")
                    .split(';')
                    .filter_map(|i| {
                        let (p, wt) = i.split_once(':')?;
                        Some((p.parse().ok()?, wt.parse().ok()?))
                    })
                    .collect();
                w.lists.insert(k, items);
            }
            (Some("prefab"), Some(k)) => {
                let animals = f
                    .get(2)
                    .unwrap_or(&"")
                    .split(';')
                    .filter_map(|a| {
                        let (name, v) = a.rsplit_once(':')?;
                        let v = nums(v);
                        (v.len() == 4).then(|| Animal { archetype: name.to_string(), offset: [v[0], v[1], v[2]], yaw: v[3] })
                    })
                    .collect();
                w.prefabs.insert(k, animals);
                if let Some(c) = f.get(3).and_then(|c| c.parse().ok()) {
                    w.costs.insert(k, c);
                }
            }
            (Some("region"), _) if f.len() >= 7 => {
                let flags = f[4].as_bytes();
                w.regions.push(Region {
                    name: f[1].to_string(),
                    settings: f[2].parse().unwrap_or(0),
                    list: f[3].parse().unwrap_or(0),
                    land: flags.first() == Some(&b'1'),
                    water: flags.get(1) == Some(&b'1'),
                    points: f[5].split(';').map(nums).filter(|v| v.len() == 3).map(|v| [v[0], v[1], v[2]]).collect(),
                    triangles: f[6]
                        .split(';')
                        .filter_map(|t| {
                            let v: Vec<u16> = t.split(',').filter_map(|x| x.parse().ok()).collect();
                            (v.len() == 3).then(|| [v[0], v[1], v[2]])
                        })
                        .collect(),
                });
            }
            _ => {}
        }
    }
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_finds_points_inside() {
        let mut w = Wildlife::default();
        w.settings.insert(7, SpawnSettings { min: 64.0, max: 80.0, angle: 180.0, separation: 10.0 });
        w.lists.insert(9, vec![(123, 50.0), (0, 1.0)]);
        w.prefabs.insert(123, vec![Animal { archetype: "FC3_Quadrupeds/Boar.Boar_1".into(), offset: [1.0, -2.0, 0.5], yaw: 90.0 }]);
        w.costs.insert(123, 0.75);
        w.budget = 3.0;
        w.max_groups = 5;
        w.regions.push(Region {
            name: "WildlifeRegionShape_Boar_1".into(),
            settings: 7,
            list: 9,
            land: true,
            water: false,
            points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        });
        assert_eq!(from_text(&to_text(&w)).unwrap(), w);
        let r = &w.regions[0];
        assert!(r.contains(5.0, 5.0) && r.contains(1.0, 9.0) && !r.contains(11.0, 5.0) && !r.contains(-1.0, 5.0));
        assert_eq!(r.bounds(), [0.0, 0.0, 10.0, 10.0]);
        let mut m = Markers::default();
        m.markers.insert(42, (vec![7, 0], vec![8]));
        m.prefabs.insert(7, vec![Animal { archetype: "Merc/BaseMerc_FC3.Privateer_Defender_23".into(), offset: [2.0, 1.0, 0.0], yaw: -30.0 }]);
        m.vehicles.insert(7, vec![Animal { archetype: r"graphics\_common\vehicles\land\4x4\4x4.xml".into(), offset: [-3.0, 0.5, 0.2], yaw: 90.0 }]);
        m.spots.insert(7, vec![Animal { archetype: "smoke".into(), offset: [1.0, 1.0, 0.0], yaw: 180.0 }]);
        m.props.insert(7, vec![Animal { archetype: r"graphics\props\sandbag.xbg".into(), offset: [0.5, -2.0, 0.0], yaw: 45.0 }]);
        m.beasts.insert(7, vec![Animal { archetype: "FC3_Quadrupeds/AttackDog_FC3_0".into(), offset: [1.5, 0.0, 0.0], yaw: 10.0 }]);
        assert_eq!(Markers::from_text(&m.to_text()).unwrap(), m);
    }
}
