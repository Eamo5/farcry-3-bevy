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
    pub items: Vec<fc3_formats::progression::Item>,
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
        Self { recipes: metadata.recipes.clone(), items: metadata.items.clone(), ..default() }
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

    pub fn available(&self, progress: &Progress, recipe: &Recipe) -> bool {
        !recipe.unlock_script
            && (recipe.unlock_mission.is_empty() || self.completed_missions.contains(&recipe.unlock_mission.to_ascii_lowercase()))
            && (recipe.requires.is_empty() || progress.recipes.contains(&recipe.requires))
    }

    pub fn can_craft(&self, progress: &Progress, stats: &PlayerStats, medkit_cap: u32, loot: &LootCatalog, id: &str) -> bool {
        let Some(recipe) = self.recipe(id) else { return false };
        let supported = match id {
            MEDICINE => stats.medkits < medkit_cap,
            RUCKSACK => !progress.recipes.contains(id),
            _ => false,
        };
        supported && self.available(progress, recipe) && loot.can_consume(progress, &recipe.ingredients)
    }

    /// Validate output capacity, unlocks and *all* ingredients before mutation.
    /// Events describe only committed transactions, with the original CRecipe
    /// kind and sName; species with the same authored item are interchangeable.
    pub fn craft(&self, progress: &mut Progress, stats: &mut PlayerStats, medkit_cap: u32, loot: &LootCatalog, id: &str) -> Option<ItemCrafted> {
        if !self.can_craft(progress, stats, medkit_cap, loot, id) { return None }
        let recipe = self.recipe(id)?;
        if !loot.consume(progress, &recipe.ingredients) { return None }
        match id {
            MEDICINE => stats.medkits += 1,
            RUCKSACK => { progress.recipes.insert(id.to_string()); }
            _ => unreachable!("output validated before consumption"),
        }
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

pub fn loot_count(progress: &Progress) -> u32 {
    progress.plants.iter().chain(progress.skins.values()).fold(0u32, |sum, n| sum.saturating_add(*n))
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
        let mut p = Progress::default();
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
