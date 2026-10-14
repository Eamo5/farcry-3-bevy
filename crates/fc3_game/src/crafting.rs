//! Authored crafting transactions. Unsupported outputs never consume loot.
use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use fc3_formats::progression::{Crafting, Recipe};

use crate::combat::PlayerStats;
use crate::progression::{LootCatalog, LootKind, Progress};

pub const MEDICINE: &str = "FC3_Recipes:Potions/Med kit small";
pub const RUCKSACK: &str = "FC3_Recipes:Gear/Loot pouch small";
pub const TUTORIAL_RECIPES: [&str; 2] = [MEDICINE, RUCKSACK];

#[derive(Resource, Default)]
pub struct CraftCatalog {
    pub recipes: Vec<Recipe>,
    /// The crafting menu's tabs and their groups (the original page's).
    #[cfg_attr(not(feature = "original-ui"), allow(dead_code))]
    pub types: Vec<fc3_formats::progression::Category>,
    #[cfg_attr(not(feature = "original-ui"), allow(dead_code))]
    pub subtypes: Vec<fc3_formats::progression::Category>,
    pub items: Vec<fc3_formats::progression::Item>,
    /// Where loot drops, and the loot groups they roll.
    pub sources: Vec<fc3_formats::progression::Source>,
    pub groups: Vec<fc3_formats::progression::LootGroup>,
    /// Refreshed from the campaign, including completed missions on reload.
    pub completed_missions: HashSet<String>,
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct ItemCrafted {
    pub kind: u32,
    pub label: String,
}

impl CraftCatalog {
    pub fn from_metadata(metadata: &Crafting) -> Self {
        Self {
            recipes: metadata.recipes.clone(),
            types: metadata.types.clone(),
            subtypes: metadata.subtypes.clone(),
            items: metadata.items.clone(),
            sources: metadata.sources.clone(),
            groups: metadata.groups.clone(),
            ..default()
        }
    }

    pub fn item(&self, id: u32) -> Option<&fc3_formats::progression::Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Rolls what a body dressed from a graphic kit carries: each of its loot
    /// groups by its chance, then the group's cash, items and supplies each
    /// by theirs. `None` when the game gives the kit no loot.
    pub fn roll_body(&self, kit: u64, rng: &mut crate::weapons::Rng) -> Option<BodyLoot> {
        // The kit's own merc (not a co-op or one-off variant of it).
        let source = self
            .sources
            .iter()
            .filter(|s| s.kit == kit && kit != 0 && !s.groups.is_empty())
            .min_by_key(|s| (s.prototype.matches('.').count(), s.prototype.len()))?;
        Some(self.roll(source, rng))
    }

    /// Rolls a character's own loot groups (`pickups::OwnLoot`).
    pub fn roll_groups(&self, groups: &[(u32, f32)], rng: &mut crate::weapons::Rng) -> BodyLoot {
        let source = fc3_formats::progression::Source {
            skeleton: false,
            key: String::new(),
            prototype: String::new(),
            items: Vec::new(),
            groups: groups.to_vec(),
            kit: 0,
            xp: 0,
        };
        self.roll(&source, rng)
    }

    /// Rolls what a placed container holds, by its name (its prototype's,
    /// with the placement's number after it).
    pub fn roll_container(&self, name: &str, rng: &mut crate::weapons::Rng) -> Option<BodyLoot> {
        let prototype = match name.rsplit_once('_') {
            Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
            _ => name,
        };
        let source = self.sources.iter().find(|s| !s.skeleton && !s.groups.is_empty() && (s.key.eq_ignore_ascii_case(name) || s.key.eq_ignore_ascii_case(prototype)))?;
        Some(self.roll(source, rng))
    }

    fn roll(&self, source: &fc3_formats::progression::Source, rng: &mut crate::weapons::Rng) -> BodyLoot {
        let mut loot = BodyLoot::default();
        let range = |lo: u32, hi: u32, rng: &mut crate::weapons::Rng| lo + ((rng.f32() * (hi.max(lo) - lo + 1) as f32) as u32).min(hi.max(lo) - lo);
        for &(id, chance) in &source.groups {
            if rng.f32() * 100.0 >= chance {
                continue;
            }
            let Some(g) = self.groups.iter().find(|g| g.id == id) else { continue };
            if g.cash.0 > 0.0 && rng.f32() * 100.0 < g.cash.0 {
                loot.cash += range(g.cash.1, g.cash.2, rng);
            }
            for &(item, p) in &g.items {
                if rng.f32() * 100.0 < p {
                    loot.items.push(item);
                }
            }
            for &(item, p, lo, hi) in &g.supplies {
                if rng.f32() * 100.0 < p {
                    loot.supplies.push((item, range(lo, hi, rng).max(1)));
                }
            }
        }
        loot
    }

    /// The XP for killing a character dressed from a graphic kit (its own
    /// merc's), or 0.
    pub fn kill_xp(&self, kit: u64) -> u32 {
        self.sources.iter().filter(|s| s.kit == kit && kit != 0 && s.xp > 0).min_by_key(|s| (s.prototype.matches('.').count(), s.prototype.len())).map_or(0, |s| s.xp)
    }

    /// The XP for killing an animal (by its prototype), or 0.
    pub fn animal_xp(&self, animal: crate::progression::Animal) -> u32 {
        let Some(p) = crate::progression::animal_prototype(animal) else { return 0 };
        let full = format!("FC3_Animals:{p}");
        self.sources.iter().find(|s| s.prototype.eq_ignore_ascii_case(&full)).map_or(0, |s| s.xp)
    }

    /// The XP for finding a collectible whose key names it (`relic`,
    /// `letter`, `sd card`), or 0.
    pub fn collectible_xp(&self, what: &str) -> u32 {
        self.items.iter().filter(|i| i.key.starts_with("Collectibles/") && i.key.to_ascii_lowercase().contains(what)).map(|i| i.xp).max().unwrap_or(0)
    }

    /// The armour an armour pickup gives (the shop's body armour's).
    pub fn armour_value(&self) -> f32 {
        self.items.iter().map(|i| i.armour).fold(0.0, f32::max)
    }

    /// What a loot item sells for (none: it doesn't sell).
    pub fn sale_price(&self, id: u32, perks: &crate::progression::Perks) -> u32 {
        self.item(id).map_or(0, |i| (i.cost as f32 * perks.sale_price).round() as u32)
    }

    pub fn recipe(&self, id: &str) -> Option<&Recipe> {
        self.recipes.iter().find(|r| r.id.eq_ignore_ascii_case(id))
    }

    /// Initial rucksack holds 16 items; crafted loot-bag modifiers are the
    /// installed recipe's attribute 62, not an invented upgrade tier.
    pub fn loot_capacity(&self, progress: &Progress) -> u32 {
        let mut capacity = 16.0;
        for recipe in &self.recipes {
            if !progress.recipes.contains(&recipe.id) { continue }
            for bonus in recipe.bonuses.iter().filter(|b| b.attribute == 62) {
                if bonus.additive { capacity += bonus.value; } else { capacity *= bonus.value; }
            }
        }
        capacity.max(0.0) as u32
    }

    /// The story has opened the recipe (in free roam, every one), and its
    /// previous tier is crafted.
    pub fn available(&self, progress: &Progress, recipe: &Recipe) -> bool {
        let opened = !progress.story
            || (!recipe.unlock_script && (recipe.unlock_mission.is_empty() || self.completed_missions.contains(&recipe.unlock_mission.to_ascii_lowercase())));
        opened && (recipe.requires.is_empty() || progress.recipes.contains(&recipe.requires))
    }

    pub fn can_craft(&self, progress: &Progress, stats: &PlayerStats, medkit_cap: u32, loot: &LootCatalog, id: &str) -> bool {
        let Some(recipe) = self.recipe(id) else { return false };
        let supported = match recipe.kind {
            _ if id == MEDICINE => stats.medkits < medkit_cap,
            // Gear is made once, tier by tier.
            fc3_formats::progression::KIND_GEAR => !progress.recipes.contains(&recipe.id),
            // Syringes the game plays, as many as the syringe kit holds.
            fc3_formats::progression::KIND_POTION if crate::syringes::Syringe::of(&recipe.id).is_some() => {
                progress.potions.get(&recipe.id).copied().unwrap_or(0) < medkit_cap
            }
            // The health boost, carried like the syringes.
            fc3_formats::progression::KIND_MEDKIT if recipe.health_slots > 0 => progress.potions.get(&recipe.id).copied().unwrap_or(0) < medkit_cap,
            _ => false,
        };
        supported && self.available(progress, recipe) && loot.can_consume(progress, &recipe.ingredients)
    }

    /// The gear chains (a pouch, holster or wallet and its larger tiers), each
    /// in tier order, the chains in menu order.
    pub fn gear_chains(&self) -> Vec<Vec<&Recipe>> {
        let gear: Vec<&Recipe> = self.recipes.iter().filter(|r| r.kind == fc3_formats::progression::KIND_GEAR).collect();
        let mut chains: Vec<Vec<&Recipe>> = Vec::new();
        for first in gear.iter().filter(|r| r.requires.is_empty() || !gear.iter().any(|g| g.id.eq_ignore_ascii_case(&r.requires))) {
            let mut chain = vec![*first];
            while let Some(next) = gear.iter().find(|g| g.requires.eq_ignore_ascii_case(&chain.last().unwrap().id)) {
                if chain.len() > 16 {
                    break;
                }
                chain.push(next);
            }
            chains.push(chain);
        }
        chains.sort_by_key(|c| (c[0].category, c[0].subcategory, c[0].sort));
        chains
    }

    /// What the crafted gear adds to an engine attribute.
    pub fn gear_bonus(&self, progress: &Progress, attribute: u32) -> f32 {
        self.recipes.iter().filter(|r| progress.recipes.contains(&r.id)).flat_map(|r| &r.bonuses).filter(|b| b.attribute == attribute && b.additive).map(|b| b.value).sum()
    }

    /// Gear tiers crafted in a chain.
    pub fn tiers(&self, progress: &Progress, chain: &[&Recipe]) -> usize {
        chain.iter().filter(|r| progress.recipes.contains(&r.id)).count()
    }

    /// Validate output capacity, unlocks and *all* ingredients before mutation.
    /// Events describe only committed transactions, with the original CRecipe
    /// kind and sName; species with the same authored item are interchangeable.
    pub fn craft(&self, progress: &mut Progress, stats: &mut PlayerStats, medkit_cap: u32, loot: &LootCatalog, id: &str) -> Option<ItemCrafted> {
        if !self.can_craft(progress, stats, medkit_cap, loot, id) { return None }
        let recipe = self.recipe(id)?;
        if !loot.consume(progress, &recipe.ingredients) { return None }
        if id == MEDICINE {
            stats.medkits += 1;
        } else if recipe.kind == fc3_formats::progression::KIND_POTION || recipe.health_slots > 0 {
            *progress.potions.entry(recipe.id.clone()).or_insert(0) += 1;
        } else {
            // (Gear, validated above.)
            progress.recipes.insert(recipe.id.clone());
        }
        info!("crafting: {} ({}, {})", recipe.name, recipe.kind, recipe.label);
        Some(ItemCrafted { kind: recipe.kind, label: recipe.label.clone() })
    }

    /// The arrow-tip recipes, in menu order.
    pub fn arrow_recipes(&self) -> Vec<&Recipe> {
        self.recipes.iter().filter(|r| arrow_kind(r).is_some()).collect()
    }

    /// The grenades and molotovs an arrow recipe takes (its ingredients are
    /// gadgets); `None` if it takes anything else.
    pub fn gadget_cost(&self, recipe: &Recipe) -> Option<(u32, u32)> {
        let mut cost = (0, 0);
        for &(id, n) in &recipe.ingredients {
            match self.item(id)?.gadget {
                1 => cost.0 += n,
                2 => cost.1 += n,
                _ => return None,
            }
        }
        Some(cost)
    }

    pub fn can_craft_arrow(&self, progress: &Progress, inv: &crate::weapons::Inventory, cap: u32, id: &str) -> bool {
        let Some(recipe) = self.recipe(id) else { return false };
        let (Some(kind), Some((grenades, molotovs))) = (arrow_kind(recipe), self.gadget_cost(recipe)) else { return false };
        self.available(progress, recipe) && inv.special_arrows[kind as usize - 1] < cap && inv.grenades >= grenades && inv.molotovs >= molotovs
    }

    /// Tips an arrow with a grenade or a molotov, as its recipe says.
    pub fn craft_arrow(&self, progress: &Progress, inv: &mut crate::weapons::Inventory, cap: u32, id: &str) -> Option<ItemCrafted> {
        if !self.can_craft_arrow(progress, inv, cap, id) {
            return None;
        }
        let recipe = self.recipe(id)?;
        let (grenades, molotovs) = self.gadget_cost(recipe)?;
        inv.grenades -= grenades;
        inv.molotovs -= molotovs;
        inv.special_arrows[arrow_kind(recipe)? as usize - 1] += 1;
        info!("crafting: {} ({}, {})", recipe.name, recipe.kind, recipe.label);
        Some(ItemCrafted { kind: recipe.kind, label: recipe.label.clone() })
    }

    pub fn ingredient_text(&self, recipe: &Recipe) -> String {
        recipe.ingredients.iter().map(|(id, count)| {
            let name = self.items.iter().find(|i| i.id == *id).map(|i| i.name.as_str()).unwrap_or("Unknown ingredient");
            format!("{count} {name}")
        }).collect::<Vec<_>>().join(", ")
    }
}

impl LootCatalog {
    fn quantities_by_item(&self, progress: &Progress) -> HashMap<u32, u32> {
        let mut quantities: HashMap<u32, u32> = HashMap::new();
        for animal in crate::progression::Animal::ALL {
            let kind = LootKind::Skin(animal);
            if let Some(item) = self.item(kind) {
                let n = progress.skins.get(&animal).copied().unwrap_or(0);
                let count = quantities.entry(item.id).or_default();
                *count = count.saturating_add(n);
            }
        }
        for plant in crate::syringes::Plant::ALL {
            if let Some(item) = self.item(LootKind::Plant(plant)) {
                let count = quantities.entry(item.id).or_default();
                *count = count.saturating_add(progress.plants[plant.index()]);
            }
        }
        quantities
    }

    fn totals(ingredients: &[(u32, u32)]) -> Option<HashMap<u32, u32>> {
        let mut totals: HashMap<u32, u32> = HashMap::new();
        for &(id, count) in ingredients {
            let total = totals.entry(id).or_default();
            *total = total.checked_add(count)?;
        }
        Some(totals)
    }

    /// How many of an ingredient (by its item) the player carries.
    #[cfg_attr(not(feature = "original-ui"), allow(dead_code))]
    pub fn held(&self, progress: &Progress, item: u32) -> u32 {
        self.quantities_by_item(progress).get(&item).copied().unwrap_or(0)
    }

    pub fn can_consume(&self, progress: &Progress, ingredients: &[(u32, u32)]) -> bool {
        let Some(totals) = Self::totals(ingredients) else { return false };
        let have = self.quantities_by_item(progress);
        totals.iter().all(|(id, n)| have.get(id).copied().unwrap_or(0) >= *n)
    }

    pub fn consume(&self, progress: &mut Progress, ingredients: &[(u32, u32)]) -> bool {
        if !self.can_consume(progress, ingredients) { return false }
        let mut remaining = Self::totals(ingredients).expect("validated ingredient totals");
        for animal in crate::progression::Animal::ALL {
            let Some(item) = self.item(LootKind::Skin(animal)) else { continue };
            let Some(need) = remaining.get_mut(&item.id) else { continue };
            if let Some(have) = progress.skins.get_mut(&animal) {
                let used = (*have).min(*need);
                *have -= used;
                *need -= used;
            }
        }
        for plant in crate::syringes::Plant::ALL {
            let Some(item) = self.item(LootKind::Plant(plant)) else { continue };
            let Some(need) = remaining.get_mut(&item.id) else { continue };
            let have = &mut progress.plants[plant.index()];
            let used = (*have).min(*need);
            *have -= used;
            *need -= used;
        }
        true
    }
}

/// The special arrow an arrow-tip recipe makes (`weapons::ARROW_NAMES`
/// index), by its name.
pub fn arrow_kind(recipe: &Recipe) -> Option<u8> {
    if recipe.kind != fc3_formats::progression::KIND_ARROW {
        return None;
    }
    let name = recipe.id.rsplit('/').next()?.to_ascii_lowercase();
    if name.contains("fire") {
        Some(1)
    } else if name.contains("explosive") {
        Some(2)
    } else {
        None
    }
}

pub fn loot_count(progress: &Progress) -> u32 {
    progress.plants.iter().chain(progress.skins.values()).chain(progress.loot.values()).fold(0u32, |sum, n| sum.saturating_add(*n))
}

/// What a searched body gave: cash, loot items (by id), and ammunition or
/// gadgets (shop item id, count).
#[derive(Default, Debug, Clone, PartialEq)]
pub struct BodyLoot {
    pub cash: u32,
    pub items: Vec<u32>,
    pub supplies: Vec<(u32, u32)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progression::{Animal, loot_tests::metadata};
    use crate::syringes::Plant;

    fn stats() -> PlayerStats {
        PlayerStats { medkits: 0, kills: 0, deaths: 0, respawn_timer: 0.0, spawn_point: Vec3::ZERO,
            hurt_dir: None, burning: 0.0, last_hurt: crate::combat::DamageKind::Bullet }
    }

    fn catalog() -> (CraftCatalog, LootCatalog) {
        let mut metadata = metadata();
        metadata.recipes = vec![
            Recipe { id: MEDICINE.into(), kind: 3, label: "Medkit small".into(), ingredients: vec![(11, 1)], unlock_mission: "JAS_060_main".into(), ..default() },
            Recipe { id: RUCKSACK.into(), kind: 2, label: "Loot pouch small".into(), ingredients: vec![(10, 2)], unlock_mission: "JAS_060_main".into(), ..default() },
        ];
        let mut catalog = CraftCatalog::from_metadata(&metadata);
        catalog.completed_missions.insert("jas_060_main".into());
        (catalog, LootCatalog::from_metadata(&metadata))
    }

    #[test]
    fn arrow_tips_take_a_grenade_or_molotov_from_the_inventory() {
        use fc3_formats::progression::{Item, KIND_ARROW};
        let catalog = CraftCatalog {
            items: vec![Item { id: 1, gadget: 1, ..default() }, Item { id: 2, gadget: 2, ..default() }],
            recipes: vec![
                Recipe { id: "FC3_Recipes:Arrow tips/Explosive tip".into(), kind: KIND_ARROW, ingredients: vec![(1, 1)], ..default() },
                Recipe { id: "FC3_Recipes:Arrow tips/Fire tip".into(), kind: KIND_ARROW, ingredients: vec![(2, 1)], ..default() },
            ],
            ..default()
        };
        assert_eq!(catalog.arrow_recipes().len(), 2);
        let mut inv = crate::weapons::test_inventory();
        inv.grenades = 1;
        let p = Progress::default();
        assert!(catalog.craft_arrow(&p, &mut inv, 10, "FC3_Recipes:Arrow tips/Fire tip").is_none());
        assert!(catalog.craft_arrow(&p, &mut inv, 10, "FC3_Recipes:Arrow tips/Explosive tip").is_some());
        assert_eq!((inv.grenades, inv.special_arrows), (0, [0, 1]));
        // Full quivers take no more.
        inv.molotovs = 3;
        inv.special_arrows[0] = 10;
        assert!(catalog.craft_arrow(&p, &mut inv, 10, "FC3_Recipes:Arrow tips/Fire tip").is_none());
        assert_eq!(inv.molotovs, 3);
    }

    #[test]
    fn bodies_roll_their_kits_loot_groups() {
        use fc3_formats::progression::{LootGroup, Source};
        let source = |prototype: &str, groups: Vec<(u32, f32)>| Source {
            skeleton: false,
            key: prototype.into(),
            prototype: prototype.into(),
            items: Vec::new(),
            groups,
            kit: 7,
            xp: 10,
        };
        let catalog = CraftCatalog {
            sources: vec![source("Merc/Base.Pirate.Coop", vec![(2, 100.0)]), source("Merc/Base.Pirate", vec![(1, 100.0), (2, 0.0)])],
            groups: vec![
                LootGroup { id: 1, cash: (100.0, 3, 20), items: vec![(50, 100.0), (51, 0.0)], supplies: vec![(60, 100.0, 4, 12)] },
                LootGroup { id: 2, cash: (100.0, 500, 500), ..default() },
            ],
            ..default()
        };
        let mut rng = crate::weapons::Rng::seeded(0x5EED);
        for _ in 0..50 {
            let loot = catalog.roll_body(7, &mut rng).unwrap();
            // The kit's own merc, not its co-op variant (whose group pays 500).
            assert!((3..=20).contains(&loot.cash), "{loot:?}");
            assert_eq!(loot.items, vec![50]);
            assert!(matches!(loot.supplies.as_slice(), [(60, n)] if (4..=12).contains(n)), "{loot:?}");
        }
        assert!(catalog.roll_body(8, &mut rng).is_none());
        // Containers by their placed name.
        let mut chests = catalog;
        chests.sources.push(Source { skeleton: false, key: "Pickups/Loot/Container_Wood01".into(), kit: 0, ..source("", vec![(2, 100.0)]) });
        assert_eq!(chests.roll_container("Pickups/Loot/Container_Wood01_9", &mut rng).map(|l| l.cash), Some(500));
        assert!(chests.roll_container("Pickups/Loot/Container_Wood02_9", &mut rng).is_none());
    }

    #[test]
    fn authored_medicine_consumes_one_green_leaf_and_no_wrong_color_or_rare_substitute() {
        let (catalog, loot) = catalog();
        let mut p = Progress::default();
        let mut stats = stats();
        stats.medkits = 0;
        p.plants[Plant::TorchGinger.index()] = 2;
        assert!(catalog.craft(&mut p, &mut stats, 3, &loot, MEDICINE).is_none());
        p.plants[Plant::ScrewPine.index()] = 2;
        assert_eq!(catalog.craft(&mut p, &mut stats, 3, &loot, MEDICINE), Some(ItemCrafted { kind: 3, label: "Medkit small".into() }));
        assert_eq!(stats.medkits, 1);
        assert_eq!(p.plants[Plant::ScrewPine.index()], 1);
        assert_eq!(p.plants[Plant::TorchGinger.index()], 2);
        let green = loot.item(LootKind::Plant(Plant::ScrewPine)).unwrap().name_id;
        assert_eq!(loot.quantities(&p).get(&green), Some(&1));
    }

    #[test]
    fn crafting_is_atomic_for_capacity_unlocks_duplicate_gear_and_missing_ingredients() {
        let (mut catalog, loot) = catalog();
        // (In the story, where missions open recipes.)
        let mut p = Progress { story: true, ..Default::default() };
        let mut stats = stats();
        p.plants[Plant::Arrowroot.index()] = 2;
        let before = p.clone();
        assert!(catalog.craft(&mut p, &mut stats, 0, &loot, MEDICINE).is_none());
        assert_eq!(before, p);
        assert!(!loot.consume(&mut p, &[(11, 1), (10, 2)]));
        assert!(!loot.consume(&mut p, &[(11, 2), (11, 1)]));
        assert_eq!(before, p);
        p.skins.insert(Animal::Boar, 2);
        catalog.completed_missions.clear();
        let before = p.clone();
        assert!(catalog.craft(&mut p, &mut stats, 3, &loot, RUCKSACK).is_none());
        assert_eq!(before, p);
        catalog.completed_missions.insert("jas_060_main".into());
        assert_eq!(catalog.craft(&mut p, &mut stats, 3, &loot, RUCKSACK), Some(ItemCrafted { kind: 2, label: "Loot pouch small".into() }));
        p.skins.insert(Animal::Boar, 2);
        let before = p.clone();
        assert!(catalog.craft(&mut p, &mut stats, 3, &loot, RUCKSACK).is_none());
        assert_eq!(before, p);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_tutorial_recipes_consume_authored_items_and_apply_rucksack_capacity() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let metadata = Crafting::from_text(&std::fs::read_to_string(cache.join("progression/recipes.txt")).unwrap()).unwrap();
        let loot = LootCatalog::from_metadata(&metadata);
        let mut catalog = CraftCatalog::from_metadata(&metadata);
        catalog.completed_missions.insert("jas_060_main".into());
        let mut p = Progress::default();
        let mut stats = stats();
        p.plants[Plant::Aloe.index()] = 1;
        p.plants[Plant::GoblinsGold.index()] = 1;
        p.skins.insert(Animal::Boar, 2);
        assert_eq!(catalog.loot_capacity(&p), 16);
        for _ in 0..2 {
            assert_eq!(catalog.craft(&mut p, &mut stats, 3, &loot, MEDICINE), Some(ItemCrafted { kind: 3, label: "Medkit small".into() }));
        }
        assert_eq!(stats.medkits, 2);
        assert_eq!(loot.quantities(&p).get(&104580), None);
        assert_eq!(catalog.craft(&mut p, &mut stats, 3, &loot, RUCKSACK), Some(ItemCrafted { kind: 2, label: "Loot pouch small".into() }));
        assert_eq!(loot_count(&p), 0);
        assert_eq!(catalog.loot_capacity(&p), 32);
    }
}
