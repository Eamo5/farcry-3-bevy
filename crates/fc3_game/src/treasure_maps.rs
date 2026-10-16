//! The maps the shops sell (`fc3_formats::shop::TreasureMap`) and what they
//! show on the world map: in each of their cells, the relics, lost letters,
//! memory cards, plants or loot chests (`reveal`), from an index of every
//! such thing on the island (the cached sectors' entities, read once in the
//! background). A map is for sale once its radio tower is taken (else once
//! its mission is done, or its script group opened; in free roam, all are);
//! bought, it is kept (`Progress::maps`). What has been picked up drops off
//! the map.

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use fc3_formats::shop::{map_cell, reveal, TreasureMap};

use crate::data::CacheInfo;

pub struct TreasureMapsPlugin;

impl Plugin for TreasureMapsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TreasureMaps>()
            .init_resource::<Island>()
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_maps, index_island))
            .add_systems(Update, (finish_index, maps_for_sale));
    }
}

/// The maps, and those for sale now (id and price).
#[derive(Resource, Default)]
pub struct TreasureMaps {
    pub maps: Vec<TreasureMap>,
    pub for_sale: Vec<(u32, u32)>,
}

impl TreasureMaps {
    /// A map's name in the shop: what it shows and where (the game's own
    /// names for most are blank).
    pub fn label(&self, id: u32) -> String {
        let Some(m) = self.maps.iter().find(|m| m.id == id) else { return String::new() };
        let bits = m.reveals.iter().fold(0, |b, (_, r)| b | r);
        let loot = reveal::LOOT_WOOD | reveal::LOOT_METAL | reveal::LOOT_REINFORCED;
        let what = if bits == reveal::PLANT {
            "Plants"
        } else if bits & !loot == 0 {
            "Loot chests"
        } else {
            "Relics, letters, memory cards"
        };
        let mut cells: Vec<&str> = m.reveals.iter().map(|(c, _)| c.as_str()).collect();
        cells.sort();
        cells.dedup();
        format!("{what} map: {}", cells.join(" "))
    }

    /// What the maps bought show: each cell's flags.
    pub fn shown(&self, bought: &std::collections::BTreeSet<u32>) -> std::collections::HashMap<String, u16> {
        let mut cells = std::collections::HashMap::new();
        for m in self.maps.iter().filter(|m| bought.contains(&m.id)) {
            for (cell, bits) in &m.reveals {
                *cells.entry(cell.clone()).or_insert(0) |= bits;
            }
        }
        cells
    }
}

/// A plant's leaf (its colour on the map).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaf {
    Green,
    Crimson,
    Amber,
    White,
    Blue,
}

/// Something a map can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Find {
    /// A relic (its kind's `reveal` bit).
    Relic(u16),
    Letter,
    MemoryCard,
    Plant(Leaf),
    /// A loot chest (its kind's `reveal` bit).
    Chest(u16),
}

impl Find {
    /// The map flag that shows it.
    pub fn reveal(self) -> u16 {
        match self {
            Find::Relic(bit) | Find::Chest(bit) => bit,
            Find::Letter => reveal::LETTER,
            Find::MemoryCard => reveal::SD_CARD,
            Find::Plant(_) => reveal::PLANT,
        }
    }

    /// What a world entity is, by its prototype's name: the relics by
    /// their animal, the letters (the Japanese soldiers' bodies), the memory
    /// cards, the plants by the leaf they give, and the wooden, metal and
    /// reinforced loot chests.
    pub fn of(name: &str) -> Option<Find> {
        let n = name.to_ascii_lowercase();
        let rest = n.strip_prefix("pickups/collectibles/").or_else(|| n.strip_prefix("pickups/plants/"));
        if let Some(c) = rest {
            let relic = [
                ("relicboar", reveal::BOAR_RELIC),
                ("relicheron", reveal::HERON_RELIC),
                ("relicshark", reveal::SHARK_RELIC),
                ("relicspider", reveal::SPIDER_RELIC),
                ("relictribal", reveal::TRIBAL_RELIC),
            ];
            if let Some((_, bit)) = relic.iter().find(|(p, _)| c.starts_with(p)) {
                return Some(Find::Relic(*bit));
            }
            if c.starts_with("japcorpses") {
                return Some(Find::Letter);
            }
            if c.starts_with("sdpickup") {
                return Some(Find::MemoryCard);
            }
            // (The leaves the plants give, as their loot has it.)
            let leaf = [
                ("aloe", Leaf::Green),
                ("goldenbeehive", Leaf::Crimson),
                ("goodluckplant", Leaf::Crimson),
                ("heliconia", Leaf::Amber),
                ("hibiscus", Leaf::Amber),
                ("palani", Leaf::Amber),
                ("goatsfoot", Leaf::White),
                ("tiare", Leaf::White),
                ("redalgae", Leaf::Blue),
                ("sacredlotus", Leaf::Blue),
            ];
            return leaf.iter().find(|(p, _)| c.starts_with(p)).map(|(_, l)| Find::Plant(*l));
        }
        let chest = n.strip_prefix("pickups/loot/")?;
        if chest.starts_with("container_wood") {
            Some(Find::Chest(reveal::LOOT_WOOD))
        } else if chest.starts_with("container_metal") {
            Some(Find::Chest(reveal::LOOT_METAL))
        } else if chest.starts_with("container_reinforced") {
            Some(Find::Chest(reveal::LOOT_REINFORCED))
        } else {
            None
        }
    }
}

/// A thing on the island: its entity, what it is, where (Bevy space) and
/// its cell of the world map.
#[derive(Clone, Debug)]
pub struct Thing {
    pub id: u64,
    pub find: Find,
    pub at: Vec3,
    pub cell: String,
}

/// Everything the maps can show, once indexed.
#[derive(Resource, Default)]
pub struct Island {
    pub things: Vec<Thing>,
    task: Option<Task<Vec<Thing>>>,
}

fn load_maps(info: Option<Res<CacheInfo>>, mut maps: ResMut<TreasureMaps>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::shop::TREASURE_MAPS_REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| fc3_formats::shop::treasure_maps_from_text(&t))
    {
        Ok(list) => {
            info!("{} treasure maps", list.len());
            maps.maps = list;
        }
        Err(e) => warn!("treasure maps: {e:#}; re-run the converter (or `fc3tool treasure-maps write`)"),
    }
}

/// Reads the cached sectors' entities in the background for the things
/// the maps show (the plants' two variants at a spot count once).
fn index_island(info: Option<Res<CacheInfo>>, mut island: ResMut<Island>) {
    let Some(info) = info else { return };
    let dir = info.root.join("sectors");
    island.task = Some(AsyncComputeTaskPool::get().spawn(async move {
        let mut things = Vec::new();
        let mut spots = std::collections::HashSet::new();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Ok(bytes) = std::fs::read(entry.path()) else { continue };
            let Ok(sector) = fc3_formats::cache::CachedSector::from_bytes(&bytes) else { continue };
            for e in &sector.entities {
                let Some(find) = Find::of(&e.name) else { continue };
                let at = Vec3::from_array(e.translation);
                if matches!(find, Find::Plant(_)) && !spots.insert((at.x.round() as i32, at.z.round() as i32)) {
                    continue;
                }
                let d = crate::data::to_dunia(at);
                let Some(cell) = map_cell(d[0], d[1]) else { continue };
                things.push(Thing { id: e.id, find, at, cell });
            }
        }
        things
    }));
}

fn finish_index(mut island: ResMut<Island>) {
    let Some(things) = island.task.as_mut().and_then(|t| block_on(future::poll_once(t))) else { return };
    island.task = None;
    info!("treasure maps: {} things to show on the island", things.len());
    island.things = things;
}

/// The maps for sale, a few times a second: not yet bought, their tower
/// taken (or their mission done, or script group opened).
fn maps_for_sale(
    time: Res<Time>,
    mut next: Local<f32>,
    mut maps: ResMut<TreasureMaps>,
    progress: Res<crate::progression::Progress>,
    host: Option<NonSend<crate::story::StoryHost>>,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 0.5;
    let open = |m: &TreasureMap| match host.as_ref() {
        // (Free roam: every map.)
        None => true,
        Some(h) if m.tower != 0 => h.story.radio_tower_unlocked(m.tower),
        Some(_) => {
            (!m.unlock_mission.is_empty() && progress.missions.contains(&m.unlock_mission.to_ascii_lowercase()))
                || (!m.unlock_script.is_empty() && progress.unlocks.contains(&m.unlock_script))
        }
    };
    let for_sale: Vec<(u32, u32)> = maps.maps.iter().filter(|m| !progress.maps.contains(&m.id) && open(m)).map(|m| (m.id, m.cost)).collect();
    if for_sale != maps.for_sale {
        maps.for_sale = for_sale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The things by their prototypes' names; the rest is nothing.
    #[test]
    fn finds_by_prototype_name() {
        assert_eq!(Find::of("Pickups/Collectibles/RelicShark_12_0"), Some(Find::Relic(reveal::SHARK_RELIC)));
        assert_eq!(Find::of("Pickups/Collectibles/JapCorpses_05"), Some(Find::Letter));
        assert_eq!(Find::of("Pickups/Collectibles/SDPickup04_01_0"), Some(Find::MemoryCard));
        assert_eq!(Find::of("Pickups/Collectibles/Tiare_02_0"), Some(Find::Plant(Leaf::White)));
        assert_eq!(Find::of("Pickups/Loot/Container_Reinforcedl01"), Some(Find::Chest(reveal::LOOT_REINFORCED)));
        assert_eq!(Find::of("Pickups/Loot/Container_Wood01.Temple_container"), Some(Find::Chest(reveal::LOOT_WOOD)));
        assert_eq!(Find::of("Pickups/Loot/MoneyLoot_Small"), None);
        assert_eq!(Find::of("Pickups/Ammo/Rifle"), None);
    }

    /// A map's shop name says what it shows and where; the maps bought
    /// together show the union of their flags per cell.
    #[test]
    fn labels_and_cells_shown() {
        let map = |id, reveals: Vec<(&str, u16)>| TreasureMap {
            id,
            name_id: 0,
            description_id: 0,
            cost: 100,
            tower: 0,
            unlock_mission: String::new(),
            unlock_script: String::new(),
            reveals: reveals.into_iter().map(|(c, b)| (c.to_string(), b)).collect(),
        };
        let maps = TreasureMaps {
            maps: vec![
                map(1, vec![("C3", reveal::PLANT)]),
                map(2, vec![("C3", reveal::SD_CARD | reveal::BOAR_RELIC)]),
                map(3, vec![("G2", reveal::LOOT_WOOD), ("G1", reveal::LOOT_METAL)]),
            ],
            for_sale: Vec::new(),
        };
        assert_eq!(maps.label(1), "Plants map: C3");
        assert_eq!(maps.label(2), "Relics, letters, memory cards map: C3");
        assert_eq!(maps.label(3), "Loot chests map: G1 G2");
        let shown = maps.shown(&[1, 2].into_iter().collect());
        assert_eq!(shown.get("C3"), Some(&(reveal::PLANT | reveal::SD_CARD | reveal::BOAR_RELIC)));
        assert!(!shown.contains_key("G2"));
    }
}
