//! Weapon stock from the installed game's shopping database, joined to its
//! entity-library prototypes. Kept separately from the story's name lookup so
//! the host can honor item/category ids, prices and tutorial shop restrictions.
//! With it go the shop page's tree: its categories (buy, sell) and their
//! subcategories (handguns, ammo, plant parts...), from the libraries the
//! converter copies.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::fcb::Object;

pub const ITEMS_LIB: &str = r"generated\databases\generic\shoppingitems.lib";
pub const WEAPONS_REL: &str = "story/shop_weapons.txt";
const FORMAT: &str = "fc3shop 2";
/// The table before the shop page's fields (read still, without them).
const FORMAT_1: &str = "fc3shop 1";

/// The shop page's categories and subcategories, as the converter copies
/// them.
pub const CATEGORIES_LIB: &str = r"generated\databases\generic\shopcategory.lib";
pub const CATEGORIES_REL: &str = "story/shopcategory.lib";
pub const SUBCATEGORIES_LIB: &str = r"generated\databases\generic\shopsubcategory1.lib";
pub const SUBCATEGORIES_REL: &str = "story/shopsubcategory1.lib";

/// The engine's item types (`shpitpeItemType`) a subcategory lists.
pub mod item_type {
    pub const WEAPON: u32 = 66;
    pub const ATTACHMENT: u32 = 67;
    pub const PAINT: u32 = 68;
    pub const MAP: u32 = 71;
    pub const AMMO: u32 = 73;
    pub const ANIMAL_PART: u32 = 76;
    pub const PLANT_PART: u32 = 77;
    pub const LOOT: u32 = 78;
    pub const HEALTH: u32 = 87;
    pub const ARMOUR: u32 = 88;
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Weapon {
    pub id: u32,
    /// The weapon-family category used by `OverrideShopMenu`.
    pub category: u32,
    pub cost: u32,
    pub sort: u32,
    pub name_id: u32,
    pub unlock_script: String,
    pub unlock_mission: String,
    pub archetype: String,
    pub model: String,
    /// Its icon in the shop page's lists (`sShopIconName`: a frame of the
    /// movie's weapon icons) and its description (a string id).
    pub icon: String,
    pub description_id: u32,
    /// The shop page's bars for it (`WeaponStatistics`: accuracy, damage,
    /// range, fire rate and mobility, out of 5) and its stars.
    pub stats: [f32; 5],
    pub rating: f32,
}

impl Weapon {
    /// Signature/DLC variants use a dotted prototype name and can share the
    /// base weapon's model. A model alone does not make them the same item.
    pub fn is_base(&self) -> bool {
        !self.archetype.contains('.')
    }

    /// The shopping database's `TowerN` script frees this item after N radio
    /// towers, independently of whether it can already be bought for money.
    pub fn tower_requirement(&self) -> Option<usize> {
        let number = self.unlock_script.strip_prefix("Tower")?;
        if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit()) { return None }
        number.parse().ok().filter(|&n| n > 0)
    }
}

fn uint(o: &Object, name: &str) -> Option<u64> {
    let b = o.field(name)?;
    if b.is_empty() || b.len() > 8 { return None }
    Some(b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
}

/// Unset strings are sometimes a single zero byte rather than a string.
fn text(o: &Object, name: &str) -> String {
    o.str(name).filter(|s| !s.is_empty() && s.chars().all(|c| !c.is_control()) && !s.contains('\u{fffd}')).unwrap_or_default()
}

pub fn weapons(items: &Object, library: &Object) -> Vec<Weapon> {
    let mut prototypes = HashMap::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") { return }
        let Some(id) = uint(o, "disLibItemId") else { return };
        let Some(entity) = o.child("Entity") else { return };
        let Some(archetype) = entity.str("hidName").filter(|a| a.starts_with("weapons:")) else { return };
        let mut model = None;
        entity.walk(&mut |c, _| {
            if model.is_none() {
                model = c.str_h(0xC230_E653).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
            }
        });
        if let Some(model) = model {
            prototypes.insert(id, (archetype, model.to_ascii_lowercase()));
        }
    });
    let mut out = Vec::new();
    for item in &items.children {
        let Some((archetype, model)) = uint(item, "arkArchetype").and_then(|id| prototypes.get(&id)) else { continue };
        let (Some(id), Some(category), Some(cost)) = (uint(item, "disId"), uint(item, "shpscataSubCategory1"), uint(item, "iCost")) else { continue };
        if id >= u16::MAX as u64 || category >= u8::MAX as u64 || cost >= u32::MAX as u64 || item.bool("bIsLootItem") == Some(true) { continue }
        let statistics = item.child("WeaponStatistics");
        let stat = |name: &str| statistics.and_then(|s| s.f32(name)).filter(|v| v.is_finite()).unwrap_or(0.0);
        out.push(Weapon {
            id: id as u32, category: category as u32, cost: cost as u32,
            sort: uint(item, "iSortId").unwrap_or(0) as u32,
            name_id: uint(item, "locidItemNameId").unwrap_or(0) as u32,
            unlock_script: text(item, "UnlockFromScript"), unlock_mission: text(item, "UnlockOnMissionComplete"),
            archetype: archetype.clone(), model: model.clone(),
            icon: text(item, "sShopIconName"),
            description_id: uint(item, "locidDescriptionId").unwrap_or(0) as u32,
            stats: [stat("iAccuracy"), stat("iDammage"), stat("iRange"), stat("iFireRate"), stat("iHandling")],
            rating: stat("iStarRating"),
        });
    }
    out.sort_by_key(|w| (w.category, w.sort, w.id));
    out
}

pub fn to_text(weapons: &[Weapon]) -> String {
    let mut out = format!("{FORMAT}\n");
    for w in weapons {
        let stats = w.stats.map(|v| v.to_string()).join(":");
        out += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{stats}\t{}\n",
            w.id, w.category, w.cost, w.sort, w.name_id, w.unlock_script, w.unlock_mission, w.archetype, w.model, w.icon, w.description_id, w.rating
        );
    }
    out
}

/// Whether a cached table is this version's (an older one still reads,
/// without the shop page's fields).
pub fn is_current(text: &str) -> bool {
    text.lines().next() == Some(FORMAT)
}

pub fn from_text(text: &str) -> Result<Vec<Weapon>> {
    let mut lines = text.lines();
    let columns = match lines.next() {
        Some(FORMAT) => 13,
        Some(FORMAT_1) => 9,
        _ => bail!("not a current weapon shop table"),
    };
    lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let fields: Vec<_> = l.split('\t').collect();
            if fields.len() != columns {
                bail!("malformed weapon shop entry")
            }
            let number = |i: usize| fields[i].parse::<u32>().context("invalid weapon shop number");
            let mut stats = [0.0; 5];
            let mut rating = 0.0;
            if columns == 13 {
                let values: Vec<f32> = fields[11].split(':').map(|v| v.parse().context("invalid weapon shop statistic")).collect::<Result<_>>()?;
                stats = values.try_into().map_err(|_| anyhow::anyhow!("malformed weapon shop statistics"))?;
                rating = fields[12].parse().context("invalid weapon shop rating")?;
            }
            Ok(Weapon {
                id: number(0)?,
                category: number(1)?,
                cost: number(2)?,
                sort: number(3)?,
                name_id: number(4)?,
                unlock_script: fields[5].into(),
                unlock_mission: fields[6].into(),
                archetype: fields[7].into(),
                model: fields[8].into(),
                icon: fields.get(9).map_or(String::new(), |s| s.to_string()),
                description_id: if columns == 13 { number(10)? } else { 0 },
                stats,
                rating,
            })
        })
        .collect()
}

/// A shop page's category (`shopcategory.lib`: buy, sell): its name (a
/// string id), its place among them, its icon (a frame of the page's
/// widget) and the subcategories under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: u32,
    pub name_id: u32,
    pub sort: u32,
    pub icon: String,
    pub subcategories: Vec<u32>,
}

/// A subcategory (`shopsubcategory1.lib`: handguns, ammo, plant parts...):
/// its name (a string id), its place among them, and what it lists: weapon
/// families (`shpscatbSubCategory2`) or items of some types ([`item_type`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubCategory {
    pub id: u32,
    pub name_id: u32,
    pub sort: u32,
    pub families: Vec<u32>,
    pub item_types: Vec<u32>,
}

/// The ids a dependency list links to (`<dep><Link field=id/>...`).
fn links(o: &Object, dep: &str, field: &str) -> Vec<u32> {
    o.child(dep).map(|d| d.children.iter().filter_map(|l| uint(l, field)).map(|v| v as u32).collect()).unwrap_or_default()
}

pub fn categories(lib: &Object) -> Vec<Category> {
    let mut out: Vec<Category> = lib
        .children
        .iter()
        .filter_map(|c| {
            Some(Category {
                id: uint(c, "disId")? as u32,
                name_id: uint(c, "locidDescriptionId").unwrap_or(0) as u32,
                sort: uint(c, "iSortId").unwrap_or(0) as u32,
                icon: text(c, "sIconName"),
                subcategories: links(c, "SubCategory1Dep", "disshpscataId"),
            })
        })
        .collect();
    out.sort_by_key(|c| (c.sort, c.id));
    out
}

pub fn subcategories(lib: &Object) -> Vec<SubCategory> {
    let mut out: Vec<SubCategory> = lib
        .children
        .iter()
        .filter_map(|c| {
            Some(SubCategory {
                id: uint(c, "disId")? as u32,
                name_id: uint(c, "locidDescriptionId").unwrap_or(0) as u32,
                sort: uint(c, "iSortId").unwrap_or(0) as u32,
                families: links(c, "SubCategory2Dep", "disshpscatbId"),
                item_types: links(c, "ItemTypeDep", "disshpitpeId"),
            })
        })
        .collect();
    out.sort_by_key(|c| (c.sort, c.id));
    out
}

/// The shop page's tree from the two libraries' bytes, as cached.
pub fn tree_from_libs(categories_lib: &[u8], subcategories_lib: &[u8]) -> Result<(Vec<Category>, Vec<SubCategory>)> {
    let parse = |b: &[u8]| -> Result<std::sync::Arc<Object>> {
        let at = b.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
        crate::fcb::parse(&b[at..])
    };
    Ok((categories(&*parse(categories_lib)?), subcategories(&*parse(subcategories_lib)?)))
}

pub const TREASURE_MAPS_REL: &str = "story/treasure_maps.txt";
const MAPS_FORMAT: &str = "fc3maps 1";

/// What a treasure map shows in a cell of the world map
/// (`MarkersRevealed`), a bit each.
pub mod reveal {
    pub const PLANT: u16 = 1 << 0;
    pub const SD_CARD: u16 = 1 << 1;
    pub const LETTER: u16 = 1 << 2;
    pub const TRIBAL_RELIC: u16 = 1 << 3;
    pub const BOAR_RELIC: u16 = 1 << 4;
    pub const SPIDER_RELIC: u16 = 1 << 5;
    pub const SHARK_RELIC: u16 = 1 << 6;
    pub const HERON_RELIC: u16 = 1 << 7;
    pub const PHOTO: u16 = 1 << 8;
    pub const LOOT_WOOD: u16 = 1 << 9;
    pub const LOOT_METAL: u16 = 1 << 10;
    pub const LOOT_REINFORCED: u16 = 1 << 11;
    /// The flags' fields.
    pub const FIELDS: [(&str, u16); 12] = [
        ("bPlant", PLANT),
        ("bSDCard", SD_CARD),
        ("bJapLetter", LETTER),
        ("bTribalRelic", TRIBAL_RELIC),
        ("bBoarRelic", BOAR_RELIC),
        ("bSpiderRelic", SPIDER_RELIC),
        ("bSharkRelic", SHARK_RELIC),
        ("bHeronRelic", HERON_RELIC),
        ("bPhoto", PHOTO),
        ("bLootWood", LOOT_WOOD),
        ("bLootMetal", LOOT_METAL),
        ("bLootReinforced", LOOT_REINFORCED),
    ];
}

/// A map the shops sell (`bIsTreasureMap`): its name and description
/// (string ids), price, and what opens it for sale (a radio tower's panel
/// taken, else a mission completed or a script's unlock); what it shows in
/// which cells of the world map (`C3`: the row's letter from the north, the
/// column's number from the west, a kilometre and 24 m each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreasureMap {
    pub id: u32,
    pub name_id: u32,
    pub description_id: u32,
    pub cost: u32,
    pub tower: u64,
    pub unlock_mission: String,
    pub unlock_script: String,
    pub reveals: Vec<(String, u16)>,
}

pub fn treasure_maps(items: &Object) -> Vec<TreasureMap> {
    let mut out = Vec::new();
    for item in items.children.iter().filter(|i| i.bool("bIsTreasureMap") == Some(true)) {
        let (Some(id), Some(cost)) = (uint(item, "disId"), uint(item, "iCost")) else { continue };
        let mut reveals = Vec::new();
        for info in item.child("RevealInfos").map(|r| r.children.as_slice()).unwrap_or_default() {
            let cell = text(info, "sMapName");
            let shown =
                info.child("MarkersRevealed").map_or(0, |m| reveal::FIELDS.iter().filter(|(f, _)| m.bool(f) == Some(true)).fold(0, |bits, (_, b)| bits | b));
            if !cell.is_empty() && shown != 0 {
                reveals.push((cell, shown));
            }
        }
        out.push(TreasureMap {
            id: id as u32,
            name_id: uint(item, "locidItemNameId").unwrap_or(0) as u32,
            description_id: uint(item, "locidDescriptionId").unwrap_or(0) as u32,
            cost: cost as u32,
            tower: uint(item, "RadioTowerEntityId").unwrap_or(0),
            unlock_mission: text(item, "UnlockOnMissionComplete"),
            unlock_script: text(item, "UnlockFromScript"),
            reveals,
        });
    }
    out.sort_by_key(|m| (m.cost, m.id));
    out
}

/// A world map cell's name (`C3`) at a place (Dunia x, y): the rows are
/// lettered from the north, the columns numbered from the west, a kilometre
/// and 24 m each.
pub fn map_cell(x: f32, y: f32) -> Option<String> {
    let half = crate::worldmap::HALF_EXTENT;
    let (row, col) = (((half - y) / 1024.0).floor(), ((x + half) / 1024.0).floor());
    ((0.0..10.0).contains(&row) && (0.0..10.0).contains(&col)).then(|| format!("{}{}", (b'A' + row as u8) as char, col as u8))
}

pub fn treasure_maps_to_text(maps: &[TreasureMap]) -> String {
    let mut out = format!("{MAPS_FORMAT}\n");
    for m in maps {
        let reveals: Vec<String> = m.reveals.iter().map(|(c, b)| format!("{c}:{b}")).collect();
        out += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            m.id,
            m.name_id,
            m.description_id,
            m.cost,
            m.tower,
            m.unlock_mission,
            m.unlock_script,
            reveals.join(" ")
        );
    }
    out
}

pub fn treasure_maps_from_text(text: &str) -> Result<Vec<TreasureMap>> {
    let mut lines = text.lines();
    if lines.next() != Some(MAPS_FORMAT) {
        bail!("not a current treasure map table")
    }
    lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<_> = l.split('\t').collect();
            if f.len() != 8 {
                bail!("malformed treasure map entry")
            }
            let n = |i: usize| f[i].parse::<u32>().context("invalid treasure map number");
            let reveals = f[7]
                .split_whitespace()
                .map(|r| {
                    let (cell, bits) = r.split_once(':').context("malformed reveal")?;
                    Ok((cell.to_string(), bits.parse::<u16>()?))
                })
                .collect::<Result<_>>()?;
            Ok(TreasureMap {
                id: n(0)?,
                name_id: n(1)?,
                description_id: n(2)?,
                cost: n(3)?,
                tower: f[4].parse().context("invalid tower")?,
                unlock_mission: f[5].into(),
                unlock_script: f[6].into(),
                reveals,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A treasure map item's price, tower and cells, through the table.
    #[test]
    fn treasure_maps_keep_their_cells_and_unlocks() {
        let flags = object("MarkersRevealed", vec![("bSDCard", vec![1]), ("bBoarRelic", vec![1]), ("bPlant", vec![0])], Vec::new());
        let info = object("RevealInfo", vec![("sMapName", b"C3".to_vec())], vec![flags]);
        let blank = object("RevealInfo", vec![("sMapName", vec![0])], vec![object("MarkersRevealed", Vec::new(), Vec::new())]);
        let item = object(
            "Item",
            vec![
                ("bIsTreasureMap", vec![1]),
                ("disId", 900u16.to_le_bytes().to_vec()),
                ("iCost", 500u32.to_le_bytes().to_vec()),
                ("locidItemNameId", 118992u32.to_le_bytes().to_vec()),
                ("RadioTowerEntityId", 2073378669121767367u64.to_le_bytes().to_vec()),
                ("UnlockOnMissionComplete", vec![0]),
            ],
            vec![object("RevealInfos", Vec::new(), vec![info, blank])],
        );
        let weapon = object("Item", vec![("bIsTreasureMap", vec![0])], Vec::new());
        let maps = treasure_maps(&object("lib", Vec::new(), vec![item, weapon]));
        assert_eq!(maps.len(), 1);
        assert_eq!((maps[0].id, maps[0].cost, maps[0].tower), (900, 500, 2073378669121767367));
        assert_eq!(maps[0].reveals, vec![("C3".to_string(), reveal::SD_CARD | reveal::BOAR_RELIC)]);
        assert_eq!(treasure_maps_from_text(&treasure_maps_to_text(&maps)).unwrap(), maps);
        assert!(treasure_maps_from_text("fc3maps 0\n").is_err());
    }

    /// The cells by place: C3's bar and radio tower, F2's and G4's towers.
    #[test]
    fn map_cells_are_lettered_from_the_north_and_numbered_from_the_west() {
        assert_eq!(map_cell(-1176.0, 2849.0).as_deref(), Some("C3"));
        assert_eq!(map_cell(-1538.0, 2506.0).as_deref(), Some("C3"));
        assert_eq!(map_cell(-2529.0, -690.0).as_deref(), Some("F2"));
        assert_eq!(map_cell(-579.0, -1501.0).as_deref(), Some("G4"));
        assert_eq!(map_cell(1554.0, 2460.0).as_deref(), Some("C6"));
    }

    fn object(name: &str, fields: Vec<(&str, Vec<u8>)>, children: Vec<Object>) -> Object {
        Object { name_hash: crate::hash::name_hash(name), fields: fields.into_iter().map(|(n, b)| (crate::hash::name_hash(n), b)).collect(),
            children: children.into_iter().map(Arc::new).collect() }
    }

    #[test]
    fn shopping_ids_and_prices_join_to_weapon_prototypes() {
        let model = "graphics\\test\\pistol.xbg";
        let mut component = Object::default();
        component.fields.push((0xC230_E653, model.as_bytes().to_vec()));
        let entity = object("Entity", vec![("hidName", b"weapons:TestPistol".to_vec())], vec![component]);
        let prototype = object("EntityPrototype", vec![("disLibItemId", 1234u64.to_le_bytes().to_vec())], vec![entity]);
        let statistics = object(
            "WeaponStatistics",
            vec![
                ("iAccuracy", 4f32.to_le_bytes().to_vec()),
                ("iDammage", 3f32.to_le_bytes().to_vec()),
                ("iRange", vec![0; 4]),
                ("iFireRate", 5f32.to_le_bytes().to_vec()),
                ("iHandling", 2f32.to_le_bytes().to_vec()),
                ("iStarRating", 2.5f32.to_le_bytes().to_vec()),
            ],
            Vec::new(),
        );
        let item = object(
            "Item",
            vec![
                ("arkArchetype", 1234u64.to_le_bytes().to_vec()),
                ("disId", 42u16.to_le_bytes().to_vec()),
                ("shpscataSubCategory1", vec![7]),
                ("iCost", 75u32.to_le_bytes().to_vec()),
                ("locidItemNameId", 999u32.to_le_bytes().to_vec()),
                ("UnlockFromScript", b"TestTower".to_vec()),
                ("UnlockOnMissionComplete", vec![0]),
                ("sShopIconName", b"TestIcon\0".to_vec()),
                ("locidDescriptionId", 1001u32.to_le_bytes().to_vec()),
            ],
            vec![statistics],
        );
        let mut invalid = item.clone();
        invalid.fields.retain(|(h, _)| *h != crate::hash::name_hash("arkArchetype"));
        let stock = weapons(&object("lib", Vec::new(), vec![item, invalid]), &object("lib", Vec::new(), vec![prototype]));
        assert_eq!(stock.len(), 1);
        assert_eq!((stock[0].id, stock[0].category, stock[0].cost), (42, 7, 75));
        assert_eq!((stock[0].archetype.as_str(), stock[0].model.as_str()), ("weapons:TestPistol", model));
        assert_eq!(stock[0].unlock_script, "TestTower");
        assert!(stock[0].unlock_mission.is_empty());
        assert_eq!((stock[0].icon.as_str(), stock[0].description_id), ("TestIcon", 1001));
        assert_eq!((stock[0].stats, stock[0].rating), ([4.0, 3.0, 0.0, 5.0, 2.0], 2.5));
        let text = to_text(&stock);
        assert!(is_current(&text));
        assert_eq!(from_text(&text).unwrap(), stock);
    }

    #[test]
    fn malformed_or_outdated_catalogs_are_rejected() {
        assert!(from_text("fc3shop 0\n").is_err());
        assert!(from_text("fc3shop 1\n1\t2\n").is_err());
        assert!(from_text("fc3shop 1\na\t2\t3\t4\t5\t\t\tweapon\tmodel\n").is_err());
        assert!(from_text("fc3shop 1\n").unwrap().is_empty());
        assert!(from_text("fc3shop 2\n1\t2\t3\t4\t5\t\t\tweapon\tmodel\ticon\t6\t1:2:3\t1\n").is_err());
        // (The older table reads still, without the page's fields.)
        let old = from_text("fc3shop 1\n1\t2\t3\t4\t5\t\t\tweapon\tmodel\n").unwrap();
        assert_eq!((old[0].id, old[0].icon.as_str(), old[0].stats), (1, "", [0.0; 5]));
        assert!(!is_current("fc3shop 1\n"));
    }

    /// The page's tree: buy and sell by their order, each with its
    /// subcategories; a subcategory's weapon families or item types.
    #[test]
    fn the_shop_tree_links_categories_to_subcategories() {
        let link = |field: &str, id: u8| object("Link", vec![(field, vec![id])], Vec::new());
        let sell = object(
            "Category",
            vec![
                ("disId", vec![12]),
                ("locidDescriptionId", 80223u32.to_le_bytes().to_vec()),
                ("iSortId", 1u32.to_le_bytes().to_vec()),
                ("sIconName", b"TX_Sell\0".to_vec()),
            ],
            vec![object("SubCategory1Dep", Vec::new(), vec![link("disshpscataId", 161)])],
        );
        let buy = object(
            "Category",
            vec![("disId", vec![11]), ("locidDescriptionId", 80222u32.to_le_bytes().to_vec()), ("iSortId", vec![0; 4]), ("sIconName", b"TX_Buy\0".to_vec())],
            vec![object("SubCategory1Dep", Vec::new(), vec![link("disshpscataId", 82), link("disshpscataId", 151)])],
        );
        let categories = categories(&object("lib", Vec::new(), vec![sell, buy]));
        assert_eq!(
            categories.iter().map(|c| (c.id, c.icon.as_str(), c.subcategories.clone())).collect::<Vec<_>>(),
            vec![(11, "TX_Buy", vec![82, 151]), (12, "TX_Sell", vec![161])]
        );
        let rifles = object(
            "Sub",
            vec![("disId", vec![151]), ("iSortId", 4u32.to_le_bytes().to_vec())],
            vec![
                object("SubCategory2Dep", Vec::new(), vec![link("disshpscatbId", 83), link("disshpscatbId", 85)]),
                object("ItemTypeDep", Vec::new(), Vec::new()),
            ],
        );
        let ammo = object(
            "Sub",
            vec![("disId", vec![82]), ("iSortId", vec![0; 4])],
            vec![object("SubCategory2Dep", Vec::new(), Vec::new()), object("ItemTypeDep", Vec::new(), vec![link("disshpitpeId", 73)])],
        );
        let subs = subcategories(&object("lib", Vec::new(), vec![rifles, ammo]));
        assert_eq!(
            subs.iter().map(|s| (s.id, s.families.clone(), s.item_types.clone())).collect::<Vec<_>>(),
            vec![(82, vec![], vec![item_type::AMMO]), (151, vec![83, 85], vec![])]
        );
    }

    /// The installed game's tree: buy first, its subcategories ammo and the
    /// guns; sell's the parts and loot.
    #[test]
    #[ignore = "reads the installed game's data"]
    fn installed_shop_tree() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let (categories, subs) = tree_from_libs(&vfs.read(CATEGORIES_LIB).unwrap(), &vfs.read(SUBCATEGORIES_LIB).unwrap()).unwrap();
        assert_eq!(categories.iter().map(|c| c.id).collect::<Vec<_>>(), vec![11, 12]);
        assert!(categories[0].subcategories.contains(&82) && categories[0].subcategories.contains(&151));
        assert!(subs.iter().any(|s| s.id == 151 && s.families.len() > 3));
        assert!(subs.iter().any(|s| s.id == 161 && s.item_types == vec![item_type::ANIMAL_PART]));
    }

    #[test]
    fn only_positive_tower_scripts_define_a_free_weapon_threshold() {
        let mut weapon = from_text("fc3shop 1\n1\t2\t3\t4\t5\t\t\tweapon\tmodel\n").unwrap().remove(0);
        for (script, requirement) in [("Tower1", Some(1)), ("Tower15", Some(15)), ("Tower99", Some(99)),
            ("", None), ("Tower", None), ("Tower0", None), ("Tower-1", None), ("Tower+1", None), ("Tower1_extra", None), ("Mission1", None)] {
            weapon.unlock_script = script.into();
            assert_eq!(weapon.tower_requirement(), requirement, "{script}");
        }
    }
}
