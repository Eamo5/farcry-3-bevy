//! Character progression: XP and levels, skills, money, hunting skins,
//! crafted gear upgrades, and the shop at liberated outposts.
//!
//! Gameplay systems read the combined effect through [`Perks`] (max health,
//! damage taken, reload speed, carry limits, ...), recomputed whenever skills
//! or upgrades change. The journal (Tab) shows skills, crafting and stats.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::combat::{DamageKind, Died, Health, PlayerStats};
use crate::hud::HudState;
use crate::menu::GameState;
use crate::player::Player;
use crate::weapons::Inventory;

pub struct ProgressionPlugin;

impl Plugin for ProgressionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Progress>()
            .init_resource::<Perks>()
            .init_resource::<Skills>()
            .init_resource::<SkillHover>()
            .add_message::<StatGain>()
            .init_resource::<TakedownRewards>()
            .add_systems(OnExit(GameState::Loading), (load_skills, load_handbook))
            .init_resource::<HandbookData>()
            .init_resource::<HandbookView>()
            .add_systems(Update, (build_handbook, handbook_encounters))
            .add_systems(Update, (count_stats, build_skill_page, build_gear_list, build_potion_list, test_skills, autopilot_skills))
            .init_resource::<ShopTab>()
            .init_resource::<Popups>()
            .add_message::<XpGain>()
            .add_message::<ItemBought>()
            .add_message::<ItemLooted>()
            .init_resource::<Shops>()
            .init_resource::<ShopCatalog>()
            .init_resource::<LootCatalog>()
            .init_resource::<crate::crafting::CraftCatalog>()
            .add_message::<crate::crafting::ItemCrafted>()
            .init_resource::<ShopOverride>()
            .init_resource::<PauseMenu>()
            .add_systems(OnExit(GameState::Loading), (load_shop_catalog, load_loot_catalog))
            .add_systems(Update, find_shops)
            .add_systems(Update, remind_skill_points.run_if(in_state(GameState::Playing)))
            .add_systems(Startup, (setup_journal, setup_shop, setup_popups))
            .add_systems(
                Update,
                (
                    award_kills,
                    award_outposts,
                    apply_xp,
                    update_perks,
                    apply_perks,
                    toggle_journal,
                    journal_automation,
                    journal_buttons,
                    journal_layout,
                    refresh_journal,
                    shop_prompt.before(crate::hud::HudSet).after(crate::turrets::man_turrets),
                    shop_buttons,
                    refresh_shop,
                    update_popups,
                )
                    .chain(),
            );
    }
}

// ------------------------------------------------------------------ data ---

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Animal {
    Boar,
    Deer,
    Goat,
    Dog,
    Tapir,
    Leopard,
    Bear,
    Tiger,
    Komodo,
    Cassowary,
    Buffalo,
    Crocodile,
    Shark,
    Macaque,
    Chicken,
    WildPig,
    Thylacine,
    // The small and the swimming: rats, lizards, crabs, crows, fish and the
    // rest the wildlife regions spawn.
    Pig,
    Rat,
    Lizard,
    Crab,
    Crow,
    RiverFish,
    SeaTurtle,
    Tortoise,
    JellyFish,
    Moray,
    MantaRay,
}

impl Animal {
    pub const ALL: [Animal; 28] = [
        Animal::Boar,
        Animal::Deer,
        Animal::Goat,
        Animal::Dog,
        Animal::Tapir,
        Animal::Leopard,
        Animal::Bear,
        Animal::Tiger,
        Animal::Komodo,
        Animal::Cassowary,
        Animal::Buffalo,
        Animal::Crocodile,
        Animal::Shark,
        Animal::Macaque,
        Animal::Chicken,
        Animal::WildPig,
        Animal::Thylacine,
        Animal::Pig,
        Animal::Rat,
        Animal::Lizard,
        Animal::Crab,
        Animal::Crow,
        Animal::RiverFish,
        Animal::SeaTurtle,
        Animal::Tortoise,
        Animal::JellyFish,
        Animal::Moray,
        Animal::MantaRay,
    ];

    /// The game's animal archetype (its counters: health and what harms
    /// it), `fc3_formats::counters`.
    pub fn archetype(self) -> &'static str {
        match self {
            Animal::Boar => "Boar",
            Animal::Deer => "DeerFC3",
            Animal::Goat => "Goat",
            Animal::Dog => "Dogs_Rapid",
            Animal::Tapir => "AsianTapir",
            Animal::Leopard => "Leopard",
            Animal::Bear => "AsianBlackBear",
            Animal::Tiger => "Tiger",
            Animal::Komodo => "KomodoDragon",
            Animal::Cassowary => "Cassowary",
            Animal::Buffalo => "Buffalo",
            Animal::Crocodile => "Crocodile",
            Animal::Shark => "BullShark",
            Animal::Macaque => "Macaque",
            Animal::Chicken => "Chicken",
            Animal::WildPig => "Pig_Wild",
            Animal::Thylacine => "Thylacine",
            Animal::Pig => "Pig",
            Animal::Rat => "Rat",
            Animal::Lizard => "Lizard",
            Animal::Crab => "Crab",
            Animal::Crow => "Crow",
            Animal::RiverFish => "RiverFish",
            Animal::SeaTurtle => "SeaTurtle",
            Animal::Tortoise => "GTortoise",
            Animal::JellyFish => "JellyFish",
            Animal::Moray => "Moray",
            Animal::MantaRay => "MantaRay",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Animal::Boar => "Boar",
            Animal::Deer => "Deer",
            Animal::Goat => "Goat",
            Animal::Dog => "Dog",
            Animal::Tapir => "Tapir",
            Animal::Leopard => "Leopard",
            Animal::Bear => "Bear",
            Animal::Tiger => "Tiger",
            Animal::Komodo => "Komodo Dragon",
            Animal::Cassowary => "Cassowary",
            Animal::Buffalo => "Water Buffalo",
            Animal::Crocodile => "Crocodile",
            Animal::Shark => "Bull Shark",
            Animal::Macaque => "Macaque",
            Animal::Chicken => "Chicken",
            Animal::WildPig => "Wild Pig",
            Animal::Thylacine => "Thylacine",
            Animal::Pig => "Pig",
            Animal::Rat => "Rat",
            Animal::Lizard => "Lizard",
            Animal::Crab => "Crab",
            Animal::Crow => "Crow",
            Animal::RiverFish => "Fish",
            Animal::SeaTurtle => "Sea Turtle",
            Animal::Tortoise => "Tortoise",
            Animal::JellyFish => "Jellyfish",
            Animal::Moray => "Moray Eel",
            Animal::MantaRay => "Manta Ray",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Animal::Boar => "boar",
            Animal::Deer => "deer",
            Animal::Goat => "goat",
            Animal::Dog => "dog",
            Animal::Tapir => "tapir",
            Animal::Leopard => "leopard",
            Animal::Bear => "bear",
            Animal::Tiger => "tiger",
            Animal::Komodo => "komodo",
            Animal::Cassowary => "cassowary",
            Animal::Buffalo => "buffalo",
            Animal::Crocodile => "crocodile",
            Animal::Shark => "shark",
            Animal::Macaque => "macaque",
            Animal::Chicken => "chicken",
            Animal::WildPig => "wildpig",
            Animal::Thylacine => "thylacine",
            Animal::Pig => "pig",
            Animal::Rat => "rat",
            Animal::Lizard => "lizard",
            Animal::Crab => "crab",
            Animal::Crow => "crow",
            Animal::RiverFish => "riverfish",
            Animal::SeaTurtle => "seaturtle",
            Animal::Tortoise => "tortoise",
            Animal::JellyFish => "jellyfish",
            Animal::Moray => "moray",
            Animal::MantaRay => "mantaray",
        }
    }

    fn from_id(s: &str) -> Option<Animal> {
        Animal::ALL.into_iter().find(|a| a.id() == s)
    }
}

/// The game's skill tree (`fc3_formats::progression`, from the cache):
/// branches, abilities with their costs, prerequisites, story gates and
/// bonuses, and the XP each level needs.
#[derive(Resource, Default)]
pub struct Skills(pub fc3_formats::progression::SkillTree);

impl Skills {
    /// An XP reward from the game's table (`fc3_formats::progression::SkillTree::xp_rewards`).
    pub fn xp_reward(&self, name: &str) -> Option<f32> {
        self.0.xp_rewards.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }

    pub fn ability(&self, id: &str) -> Option<&fc3_formats::progression::Ability> {
        self.0.abilities.iter().find(|a| a.id == id)
    }
}

/// Why an ability can't be learned yet.
#[derive(Clone, Debug, PartialEq)]
pub enum SkillLock {
    /// The game never offers it.
    Unavailable,
    /// Opens with the story (a mission's completion, or the scripts).
    Story,
    /// These must be learned first.
    Requires(Vec<String>),
    /// An engine statistic must reach a count: (stat, have, need).
    Stat(u32, u32, u32),
    /// Not enough skill points.
    Points,
}

/// Where an ability stands for the player.
#[derive(Clone, Debug, PartialEq)]
pub enum SkillState {
    Learned,
    Ready,
    Locked(SkillLock),
}

/// An ability's state: learned, ready to learn, or what holds it back (in
/// the order the game checks: availability, the story, prerequisites, the
/// statistic, then points).
pub fn skill_state(p: &Progress, a: &fc3_formats::progression::Ability) -> SkillState {
    if p.has(&a.id) {
        return SkillState::Learned;
    }
    if !a.available || !a.purchasable {
        return SkillState::Locked(SkillLock::Unavailable);
    }
    let opened = !p.story
        || p.upgradable.contains(&a.id)
        || (!a.unlock_script && (a.unlock_mission.is_empty() || p.missions.contains(&a.unlock_mission.to_ascii_lowercase())));
    if !opened {
        return SkillState::Locked(SkillLock::Story);
    }
    let missing: Vec<String> = a.requires.iter().filter(|r| !p.has(r)).cloned().collect();
    if !missing.is_empty() {
        return SkillState::Locked(SkillLock::Requires(missing));
    }
    let have = p.stat(a.stat);
    if a.stat != 0 && have < a.stat_target {
        return SkillState::Locked(SkillLock::Stat(a.stat, have, a.stat_target));
    }
    if p.skill_points < a.cost {
        return SkillState::Locked(SkillLock::Points);
    }
    SkillState::Ready
}

/// Engine statistics the skill tree asks for (`preReq`), counted as the
/// player plays (see [`StatGain`]). Supply drops, Wanted Dead hunts, mounted
/// guns, looted bodies and body armour aren't counted yet: their activities
/// aren't in the game.
#[allow(dead_code)]
pub mod stat {
    pub const RELICS: u32 = 7;
    pub const SUPPLY_DROPS: u32 = 20;
    pub const WANTED_DEAD: u32 = 21;
    pub const OUTPOSTS_UNDETECTED: u32 = 24;
    pub const MELEE_KILLS: u32 = 86;
    pub const HANDGUN_KILLS: u32 = 90;
    pub const MOUNTED_GUN_KILLS: u32 = 101;
    pub const GRENADE_KILLS: u32 = 102;
    pub const TAKEDOWN_KILLS: u32 = 150;
    pub const TAKEDOWNS_FROM_ABOVE: u32 = 152;
    pub const TAKEDOWNS_FROM_BELOW: u32 = 161;
    pub const ATTACHMENTS_BOUGHT: u32 = 185;
    pub const MONEY_SPENT: u32 = 531;
    pub const BODIES_LOOTED: u32 = 534;
    pub const ARMOR_BOUGHT: u32 = 563;
    pub const BULL_SHARKS_KILLED: u32 = 659;
    pub const MEDICINE_CRAFTED: u32 = 834;
}

/// A statistic counted up.
#[derive(Message, Clone, Copy)]
pub struct StatGain {
    pub stat: u32,
    pub amount: u32,
}

/// Total XP needed to reach `level` (level 1 needs none): the game's table
/// when it is loaded, past its end out of reach.
pub fn xp_needed(level: u32, skills: &Skills) -> u32 {
    let levels = &skills.0.xp_levels;
    if levels.is_empty() {
        return xp_for_level(level);
    }
    match level {
        0 | 1 => 0,
        n => levels.get(n as usize - 2).copied().unwrap_or(u32::MAX),
    }
}

/// Total XP to reach `level` without the game's table (an old cache).
pub fn xp_for_level(level: u32) -> u32 {
    let l = level.saturating_sub(1);
    250 * l * (l + 1)
}

/// The game's abilities an older test name stands for.
fn legacy_skill(name: &str) -> Vec<&'static str> {
    let own = match name {
        "front_takedown" => "SP_Takedown",
        "death_from_above" => "SP_DeathFromAboveSingle",
        "double_takedown" => "SP_DeathFromAboveDouble",
        "death_from_below" => "SP_TakedownFromBelowSingle",
        "heavy_takedown" => "SP_HeavyUnitTakedown",
        "chain_takedown" => "SP_ChainTakedown",
        "pistol_takedown" => "SP_PistolMarksmanTakedown",
        "grenade_takedown" => "SP_GrenadeTakedown",
        "takedown_drag" => "SP_DragBackTakedown",
        _ => return Vec::new(),
    };
    vec!["SP_Takedown", own]
}

#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Progress {
    pub xp: u32,
    pub level: u32,
    pub skill_points: u32,
    /// Learned abilities (the game's ability ids).
    pub skills: HashSet<String>,
    /// Abilities the story has made upgradable (`SetAbilityUpgradable`).
    pub upgradable: HashSet<String>,
    /// Story missions completed (lower case), which open abilities.
    pub missions: HashSet<String>,
    /// Engine statistics counted so far (see [`stat`]).
    pub stats: HashMap<u32, u32>,
    /// The story is being played (its missions open the abilities; in free
    /// roam they are open). Not saved: set as the story starts.
    pub story: bool,
    /// What the crafted wallets hold (0: the free-roam wallets). Not saved:
    /// worked out from the crafted gear.
    pub wallet: u32,
    pub money: u32,
    pub skins: HashMap<Animal, u32>,
    pub upgrades: HashMap<String, u8>,
    /// Authored gear recipe identities; stored separately from legacy upgrades.
    pub recipes: HashSet<String>,
    pub outposts: u32,
    /// Radio towers activated (indices into the world's tower list).
    pub towers: std::collections::BTreeSet<u32>,
    /// Treasure maps bought (their shop ids: `treasure_maps`).
    pub maps: std::collections::BTreeSet<u32>,
    /// Collectibles found: relics, lost letters, memory cards.
    pub relics: u32,
    pub letters: u32,
    pub cards: u32,
    /// Plants gathered (see `syringes`), and syringes on hand by potion
    /// recipe.
    pub plants: [u32; crate::syringes::Plant::ALL.len()],
    pub potions: std::collections::BTreeMap<String, u32>,
    /// Body armour worn: what it has left to absorb.
    pub armour: f32,
    /// Loot items carried to sell, by item id.
    pub loot: std::collections::BTreeMap<u32, u32>,
    /// Script groups the story has opened (`UnlockItemsFromScript`).
    pub unlocks: std::collections::BTreeSet<String>,
    /// Handbook pages opened by meeting what they describe (by page key).
    pub met: std::collections::BTreeSet<String>,
    /// The scripts have shown the whole tattoo (`EnableWholeTattoo`).
    pub whole_tattoo: bool,
    /// Bonus plans the scripts grant for a while (`AddBonusPlan`, lower
    /// case): the abilities with them count as learned.
    pub bonus_plans: std::collections::BTreeSet<String>,
    /// Bonus plans the scripts take away for a while (`RemoveBonusPlan`,
    /// lower case: JAS_190's takedowns, until the cenote's end): the
    /// abilities with them count as not learned.
    pub removed_plans: std::collections::BTreeSet<String>,
}

impl Default for Progress {
    fn default() -> Self {
        Progress {
            xp: 0,
            level: 1,
            skill_points: 1,
            skills: HashSet::new(),
            upgradable: HashSet::new(),
            missions: HashSet::new(),
            stats: HashMap::new(),
            story: false,
            wallet: 0,
            money: 150,
            skins: HashMap::new(),
            upgrades: HashMap::new(),
            recipes: HashSet::new(),
            outposts: 0,
            towers: Default::default(),
            maps: Default::default(),
            relics: 0,
            letters: 0,
            cards: 0,
            plants: [0; crate::syringes::Plant::ALL.len()],
            potions: Default::default(),
            armour: 0.0,
            loot: Default::default(),
            unlocks: Default::default(),
            met: Default::default(),
            whole_tattoo: false,
            bonus_plans: Default::default(),
            removed_plans: Default::default(),
        }
    }
}

impl Progress {
    pub fn has(&self, skill: &str) -> bool {
        // FC3_SKILLS=all (tests): every skill learned; or a list of them (by
        // ability id, or the takedowns' older test names, which bring the
        // Takedown itself with them).
        self.skills.contains(skill)
            || std::env::var("FC3_SKILLS").is_ok_and(|v| v == "all" || v.split(',').any(|s| s == skill || legacy_skill(s).contains(&skill)))
    }


    /// An engine statistic's count (the relics found count themselves).
    pub fn stat(&self, stat: u32) -> u32 {
        match stat {
            stat::RELICS => self.relics,
            _ => self.stats.get(&stat).copied().unwrap_or(0),
        }
    }

    pub fn upgrade(&self, id: &str) -> u8 {
        self.upgrades.get(id).copied().unwrap_or(0)
    }

    pub fn money_cap(&self) -> u32 {
        if self.wallet > 0 {
            return self.wallet;
        }
        [1000, 2000, 5000, 10000][self.upgrade("wallet").min(3) as usize]
    }

    /// Pays at a store (counted for the skill tree).
    pub fn spend(&mut self, price: u32) {
        self.money -= price;
        *self.stats.entry(stat::MONEY_SPENT).or_insert(0) += price;
    }

    pub fn add_money(&mut self, amount: u32) -> u32 {
        let before = self.money;
        self.money = (self.money + amount).min(self.money_cap());
        let added = self.money - before;
        crate::statistics::add(self, crate::statistics::stat::CASH_EARNED, added);
        added
    }

    /// Save-file lines.
    pub fn to_lines(&self) -> String {
        let mut s = format!("progress {} {} {} {} {}\n", self.xp, self.level, self.skill_points, self.money, self.outposts);
        let mut skills: Vec<&String> = self.skills.iter().collect();
        skills.sort();
        for k in skills {
            s += &format!("skill {k}\n");
        }
        let mut open: Vec<&String> = self.upgradable.iter().collect();
        open.sort();
        for k in open {
            s += &format!("upgradable {k}\n");
        }
        let mut missions: Vec<&String> = self.missions.iter().collect();
        missions.sort();
        for m in missions {
            s += &format!("mission {m}\n");
        }
        let mut stats: Vec<(&u32, &u32)> = self.stats.iter().collect();
        stats.sort();
        for (k, v) in stats {
            s += &format!("stat {k} {v}\n");
        }
        let mut ups: Vec<(&String, &u8)> = self.upgrades.iter().collect();
        ups.sort();
        for (k, v) in ups {
            s += &format!("upgrade {k} {v}\n");
        }
        let mut recipes: Vec<&String> = self.recipes.iter().collect();
        recipes.sort();
        for recipe in recipes { s += &format!("recipe {recipe}\n"); }
        for a in Animal::ALL {
            if let Some(n) = self.skins.get(&a).filter(|n| **n > 0) {
                s += &format!("skin {} {n}\n", a.id());
            }
        }
        for t in &self.towers {
            s += &format!("tower {t}\n");
        }
        for m in &self.maps {
            s += &format!("map {m}\n");
        }
        s += &format!("collectibles {} {} {}\n", self.relics, self.letters, self.cards);
        let list = |v: &[u32]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" ");
        s += &format!("plants {}\n", list(&self.plants));
        for (id, n) in self.potions.iter().filter(|(_, n)| **n > 0) {
            s += &format!("potion {n} {id}\n");
        }
        if self.armour > 0.0 {
            s += &format!("armour {}\n", self.armour);
        }
        for (id, n) in self.loot.iter().filter(|(_, n)| **n > 0) {
            s += &format!("loot {id} {n}\n");
        }
        for group in &self.unlocks {
            s += &format!("unlock {group}\n");
        }
        for key in &self.met {
            s += &format!("met {key}\n");
        }
        if self.whole_tattoo {
            s += "wholetattoo\n";
        }
        for plan in &self.bonus_plans {
            s += &format!("bonusplan {plan}\n");
        }
        for plan in &self.removed_plans {
            s += &format!("removedplan {plan}\n");
        }
        s
    }

    /// Reads one save line; returns false if it isn't a progression line.
    pub fn read_line(&mut self, key: &str, rest: &str) -> bool {
        let mut it = rest.split_whitespace();
        match key {
            "progress" => {
                let mut n = || it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                self.xp = n();
                self.level = n().max(1);
                self.skill_points = n();
                self.money = n();
                self.outposts = n();
            }
            "skill" => {
                self.skills.insert(rest.trim().to_string());
            }
            "upgradable" => {
                self.upgradable.insert(rest.trim().to_string());
            }
            "mission" => {
                self.missions.insert(rest.trim().to_ascii_lowercase());
            }
            "stat" => {
                if let (Some(k), Some(v)) = (it.next().and_then(|v| v.parse().ok()), it.next().and_then(|v| v.parse().ok())) {
                    self.stats.insert(k, v);
                }
            }
            "recipe" => { self.recipes.insert(rest.trim().to_string()); }
            "map" => {
                if let Ok(id) = rest.trim().parse() {
                    self.maps.insert(id);
                }
            }
            "upgrade" => {
                if let (Some(k), Some(v)) = (it.next(), it.next().and_then(|v| v.parse().ok())) {
                    self.upgrades.insert(k.to_string(), v);
                }
            }
            "skin" => {
                if let (Some(a), Some(n)) = (it.next().and_then(Animal::from_id), it.next().and_then(|v| v.parse().ok())) {
                    self.skins.insert(a, n);
                }
            }
            "tower" => {
                if let Some(t) = it.next().and_then(|v| v.parse().ok()) {
                    self.towers.insert(t);
                }
            }
            "collectibles" => {
                let mut n = || it.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                self.relics = n();
                self.letters = n();
                self.cards = n();
            }
            "plants" => {
                self.plants.fill(0);
                for (slot, v) in self.plants.iter_mut().zip(it) {
                    *slot = v.parse().unwrap_or(0);
                }
            }
            "armour" => self.armour = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "unlock" => {
                self.unlocks.insert(rest.trim().to_string());
            }
            "met" => {
                self.met.insert(rest.trim().to_string());
            }
            "wholetattoo" => self.whole_tattoo = true,
            "bonusplan" => {
                self.bonus_plans.insert(rest.trim().to_ascii_lowercase());
            }
            "removedplan" => {
                self.removed_plans.insert(rest.trim().to_ascii_lowercase());
            }
            "loot" => {
                if let (Some(id), Some(n)) = (it.next().and_then(|v| v.parse().ok()), it.next().and_then(|v| v.parse().ok())) {
                    self.loot.insert(id, n);
                }
            }
            "potion" => {
                if let Some((n, id)) = rest.trim().split_once(' ').and_then(|(n, id)| Some((n.parse::<u32>().ok()?, id))) {
                    self.potions.insert(id.to_string(), n);
                }
            }
            // Saves from before the game's recipes: their syringes as the
            // recipes that do the same (the fighter's has none).
            "syringes" => {
                const OLD: [&str; 5] = ["Wildlife vision", "", "Super sprint burst", "Never drown", "Fireproof"];
                for (name, v) in OLD.iter().zip(it) {
                    let n: u32 = v.parse().unwrap_or(0);
                    if !name.is_empty() && n > 0 {
                        *self.potions.entry(format!("FC3_Recipes:Potions/{name}")).or_insert(0) += n;
                    }
                }
            }
            _ => return false,
        }
        true
    }
}

/// Gameplay modifiers from skills and upgrades.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Perks {
    /// Guns carried (the machete doesn't take a slot).
    pub weapon_slots: usize,
    pub max_health: f32,
    pub damage_taken: f32,
    /// Damage taken from blasts, fire and blows, on top of `damage_taken`.
    pub explosion_taken: f32,
    pub fire_taken: f32,
    pub melee_taken: f32,
    pub reload_speed: f32,
    pub recoil: f32,
    pub headshot: f32,
    pub sprint: f32,
    /// The fast sprint never tires (`SP_LongerSprintBurst`'s endless burst).
    pub endless_sprint: bool,
    pub swim: f32,
    pub fall_damage: f32,
    /// Multiplies how fast enemies spot a crouching player.
    pub crouch_detection: f32,
    /// Melee takedowns at all (the first tattoo).
    pub takedown: bool,
    pub front_takedown: bool,
    pub heavy_takedown: bool,
    pub death_from_above: bool,
    pub chain_takedown: bool,
    pub pistol_takedown: bool,
    pub grenade_takedown: bool,
    pub takedown_drag: bool,
    pub double_takedown: bool,
    pub death_from_below: bool,
    pub tag_range: f32,
    pub medkit_cap: u32,
    pub medkit_full_heal: bool,
    /// Health a medkit gives on top of its own.
    pub medkit_bonus: f32,
    /// Health bars a manual heal (out of medkits) brings back.
    pub manual_heal_bars: f32,
    /// Scales the wait before health comes back.
    pub regen_delay: f32,
    /// Scales how long syringes last.
    pub potion_duration: f32,
    /// Plants and skins gathered twice over; cash from bodies doubled.
    pub double_plants: bool,
    pub double_skins: bool,
    pub double_cash: bool,
    /// Scales what loot sells for.
    pub sale_price: f32,
    /// Sliding at a sprint, cooking grenades, reloading at a sprint.
    pub slide: bool,
    pub cook_grenades: bool,
    pub sprint_reload: bool,
    /// Scales how fast weapons are swapped, and how long a breath lasts.
    pub swap_speed: f32,
    pub breath: f32,
    pub grenade_cap: u32,
    /// Scales how long a breath steadies a scope.
    pub scope_breath: f32,
    /// Pistols and sniper rifles reload without leaving the sights.
    pub aimed_reload: bool,
    /// One-handed guns fire from a zip line.
    pub zip_shoot: bool,
    /// Scales how fast a mounted gun heats up.
    pub gun_heat: f32,
    /// Scales how fast the bow draws (and nocks), and how far its arrows stray.
    pub bow_speed: f32,
    pub bow_spread: f32,
    /// Sprinting while crouched moves faster than a crouch.
    pub crouch_run: bool,
    /// Scales how far guards hear the player's steps.
    pub footstep_noise: f32,
    /// Scales the damage the vehicle the player drives takes.
    pub vehicle_damage: f32,
    /// Scales what worn body armour absorbs.
    pub armour_absorb: f32,
    /// Takedowns search the body as they kill.
    pub loot_takedown: bool,
    /// C4 and mines carried; special arrows of each kind.
    pub explosive_cap: u32,
    pub arrow_cap: u32,
    pub ammo_scale: f32,
    /// The bonus plans in force (lower case), as scripts ask about them
    /// (`HasBonus`: `standardtakedown`, the Takedown's).
    pub plans: std::collections::BTreeSet<String>,
}

impl Default for Perks {
    fn default() -> Self {
        Perks::from_progress(&Progress::default(), &Skills::default())
    }
}

impl Perks {
    /// The perks of what the player has learned and crafted. Ability effects
    /// are by ability id; how strong they are comes from the abilities'
    /// bonus plans in the skill tree.
    pub fn from_progress(p: &Progress, skills: &Skills) -> Perks {
        Perks::with_gear(p, skills, None)
    }

    /// Perks with the crafted gear's capacities (in the story: the recipes'
    /// bonuses on top of the base kit; free roam keeps its own gear).
    pub fn with_gear(p: &Progress, skills: &Skills, crafting: Option<&crate::crafting::CraftCatalog>) -> Perks {
        let mut perks = Perks::skills_only(p, skills);
        if let Some(c) = crafting.filter(|c| !c.recipes.is_empty()) {
            let tiers = |name: &str| c.gear_chains().iter().find(|ch| ch[0].id.contains(name)).map_or(0, |ch| c.tiers(p, ch));
            let crafted = Perks {
                weapon_slots: 1 + c.gear_bonus(p, gear::HOLSTER) as usize,
                medkit_cap: 3 + c.gear_bonus(p, gear::SYRINGE_BAG) as u32,
                grenade_cap: 4 + c.gear_bonus(p, gear::THROWABLES) as u32,
                explosive_cap: crate::explosives::EXPLOSIVE_CAP + c.gear_bonus(p, gear::EXPLOSIVES) as u32,
                arrow_cap: crate::weapons::ARROW_CAP + c.gear_bonus(p, gear::QUIVER) as u32,
                ammo_scale: [1.0, 1.25, 1.5, 2.0, 2.0][tiers("Ammo pouch").min(4)],
                ..perks.clone()
            };
            // The story starts from the base kit; free roam keeps what it
            // carries and gains what is crafted beyond it.
            if p.story {
                perks = crafted;
            } else {
                perks.weapon_slots = perks.weapon_slots.max(crafted.weapon_slots);
                perks.medkit_cap = perks.medkit_cap.max(crafted.medkit_cap);
                perks.grenade_cap = perks.grenade_cap.max(crafted.grenade_cap);
                perks.explosive_cap = perks.explosive_cap.max(crafted.explosive_cap);
                perks.arrow_cap = perks.arrow_cap.max(crafted.arrow_cap);
                perks.ammo_scale = perks.ammo_scale.max(crafted.ammo_scale);
            }
        }
        perks
    }

    fn skills_only(p: &Progress, skills: &Skills) -> Perks {
        // Learned and not taken away for now, or granted for now, by its
        // bonus plan.
        let plan_in = |a: &fc3_formats::progression::Ability, set: &std::collections::BTreeSet<String>| {
            a.bonuses.iter().any(|b| set.contains(&b.plan.to_ascii_lowercase()))
        };
        let has = |a: &fc3_formats::progression::Ability| (p.has(&a.id) && !plan_in(a, &p.removed_plans)) || plan_in(a, &p.bonus_plans);
        // The learned abilities' bonuses on an engine attribute, summed.
        let sum = |attribute: u32| -> f32 {
            skills.0.abilities.iter().filter(|a| has(a)).flat_map(|a| &a.bonuses).filter(|b| b.attribute == attribute).map(|b| b.value).sum()
        };
        // Learned, with each of its bonuses' values (the first, by default).
        let learned = |id: &str| skills.ability(id).map_or(p.has(id), has);
        let value = |id: &str| -> f32 { skills.ability(id).and_then(|a| a.bonuses.first()).map_or(0.0, |b| b.value) };
        // The percentage an ability's description states.
        let percent = |id: &str| -> f32 { if learned(id) { skills.ability(id).map_or(0, |a| a.value) as f32 / 100.0 } else { 0.0 } };
        let scale = |id: &str| if learned(id) { 1.0 - value(id) } else { 1.0 };
        let bars = 4.0 + sum(attr::HEALTH_SLOTS);
        Perks {
            max_health: bars * crate::combat::HEALTH_SEGMENT,
            damage_taken: 1.0,
            explosion_taken: scale("SP_ExplosionDamageResist"),
            fire_taken: scale("SP_FireDamageResistance"),
            melee_taken: scale("SP_MeleeResist"),
            reload_speed: 1.0 + sum(attr::RELOAD),
            recoil: ["SP_ReducedBulletSpreadA", "SP_ReducedBulletSpreadB"].iter().map(|id| scale(id)).product(),
            headshot: 1.0,
            sprint: 1.0 + sum(attr::SPRINT),
            endless_sprint: sum(attr::SPRINT_BURST) > 0.0,
            swim: 1.0 + sum(attr::SWIM),
            fall_damage: 1.0 - sum(attr::FALL_DAMAGE).min(0.9),
            crouch_detection: 1.0,
            takedown: learned("SP_Takedown"),
            front_takedown: learned("SP_Takedown"),
            heavy_takedown: learned("SP_HeavyUnitTakedown"),
            death_from_above: learned("SP_DeathFromAboveSingle"),
            chain_takedown: learned("SP_ChainTakedown"),
            pistol_takedown: learned("SP_PistolMarksmanTakedown"),
            grenade_takedown: learned("SP_GrenadeTakedown"),
            takedown_drag: learned("SP_DragBackTakedown"),
            double_takedown: learned("SP_DeathFromAboveDouble"),
            death_from_below: learned("SP_TakedownFromBelowSingle"),
            tag_range: 1.0,
            medkit_cap: 3 + p.upgrade("syringe_kit") as u32,
            medkit_full_heal: false,
            medkit_bonus: sum(attr::MEDKIT_HEAL) * crate::combat::HEALTH_SEGMENT,
            manual_heal_bars: 2.0 + sum(attr::MANUAL_HEAL),
            regen_delay: ["SP_IncreaseRegenA", "SP_IncreaseRegenB"].iter().filter(|id| learned(id)).map(|id| value(id)).product(),
            potion_duration: 1.0 + sum(attr::POTION_DURATION),
            double_plants: learned("SP_LootDoublePlant"),
            double_skins: learned("SP_LootDoubleAnimal"),
            double_cash: learned("SP_LootDoubleCash"),
            sale_price: 1.0 + sum(attr::SALE_PRICE),
            slide: learned("SP_Slide"),
            cook_grenades: learned("SP_CookThrowables"),
            sprint_reload: learned("SP_SprintReload"),
            swap_speed: 1.0 + sum(attr::SWAP),
            breath: 1.0 + sum(attr::BREATH),
            grenade_cap: 4 + 2 * p.upgrade("grenade_bag") as u32,
            scope_breath: 1.0 + sum(attr::SCOPE_BREATH),
            aimed_reload: learned("SP_FastAimedReload"),
            zip_shoot: learned("SP_ShootOnZipline"),
            gun_heat: scale("SP_MountedGunHandling"),
            bow_speed: 1.0 + percent("SP_FasterBowDraw"),
            bow_spread: scale("SP_BetterBowHandling"),
            crouch_run: learned("SP_CrouchRun"),
            footstep_noise: scale("SP_SilentRun"),
            vehicle_damage: 1.0 - percent("SP_VehicleExtraHealth"),
            armour_absorb: 1.0 + percent("SP_ArmorDefense"),
            loot_takedown: learned("SP_LootTakedown"),
            explosive_cap: crate::explosives::EXPLOSIVE_CAP,
            arrow_cap: crate::weapons::ARROW_CAP,
            weapon_slots: 3 + p.upgrade("holster") as usize,
            ammo_scale: [1.0, 1.25, 1.5, 2.0][p.upgrade("ammo_pouch").min(3) as usize],
            plans: skills
                .0
                .abilities
                .iter()
                .filter(|a| has(a))
                .flat_map(|a| &a.bonuses)
                .map(|b| b.plan.to_ascii_lowercase())
                .chain(p.bonus_plans.iter().cloned())
                .collect(),
        }
    }
}

/// The engine attributes (`selAttribute`) the crafted gear adds to.
mod gear {
    pub const QUIVER: u32 = 52;
    pub const HOLSTER: u32 = 54;
    pub const THROWABLES: u32 = 55;
    pub const EXPLOSIVES: u32 = 56;
    pub const SYRINGE_BAG: u32 = 59;
    pub const WALLET: u32 = 109;
}

/// The engine attributes (`selAttribute`) of the ability bonuses the game
/// applies by amount.
mod attr {
    pub const SWAP: u32 = 11;
    pub const SPRINT: u32 = 18;
    pub const SPRINT_BURST: u32 = 65;
    pub const SCOPE_BREATH: u32 = 37;
    pub const RELOAD: u32 = 13;
    pub const BREATH: u32 = 94;
    pub const SWIM: u32 = 44;
    pub const POTION_DURATION: u32 = 66;
    pub const HEALTH_SLOTS: u32 = 71;
    pub const MANUAL_HEAL: u32 = 73;
    pub const MEDKIT_HEAL: u32 = 74;
    pub const FALL_DAMAGE: u32 = 87;
    pub const SALE_PRICE: u32 = 117;
}

/// XP earned (shown as a popup).
#[derive(Message, Clone)]
pub struct XpGain {
    pub amount: u32,
    pub reason: &'static str,
}

/// A real ingredient added by gathering or skinning, not a mission shortcut.
#[derive(Message, Clone, Copy)]
pub struct ItemLooted(pub LootKind);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LootKind {
    Skin(Animal),
    Plant(crate::syringes::Plant),
    /// A shop item taken from a body or chest (a mission's key or codes).
    Item(u32),
}

/// An animal's prototype in the entity library (under `FC3_Animals:`):
/// its identity, not a shared skeleton or localized name (pigs and rare
/// variants can use a boar's or bear's model too).
pub(crate) fn animal_prototype(animal: Animal) -> Option<&'static str> {
    Some(match animal {
        Animal::Boar => "FC3_Quadrupeds/Boar",
        Animal::Deer => "FC3_Quadrupeds/DeerFC3",
        Animal::Goat => "FC3_Quadrupeds/Goat",
        Animal::Dog => "FC3_Quadrupeds/Dogs_Rapid",
        Animal::Tapir => "FC3_Quadrupeds/AsianTapir",
        Animal::Leopard => "FC3_Quadrupeds/Leopard",
        Animal::Bear => "FC3_Quadrupeds/AsianBlackBear",
        Animal::Tiger => "FC3_Quadrupeds/Tiger",
        Animal::Komodo => "FC3_Quadrupeds/KomodoDragon",
        Animal::Cassowary => "FC3_Quadrupeds/Cassowary",
        Animal::Buffalo => "FC3_Quadrupeds/Buffalo",
        Animal::Shark => "FC3_Aquatic/BullShark",
        Animal::Crocodile => "FC3_Quadrupeds/Crocodile",
        Animal::Macaque => "FC3_Quadrupeds/Macaque",
        Animal::WildPig => "FC3_Quadrupeds/Pig_Wild",
        Animal::Thylacine => "FC3_Quadrupeds/Thylacine",
        Animal::Pig => "FC3_Domestic/Pig",
        Animal::Rat => "FC3_Quadrupeds/Rat",
        Animal::Lizard => "FC3_Quadrupeds/Lizard",
        Animal::Crab => "SimpleAnimal/Crab",
        Animal::Crow => "Winged/Crow",
        Animal::RiverFish => "FC3_Aquatic/RiverFish",
        Animal::SeaTurtle => "FC3_Aquatic/SeaTurtle",
        Animal::Tortoise => "FC3_Quadrupeds/GTortoise",
        Animal::JellyFish => "FC3_Aquatic/JellyFish",
        Animal::Moray => "FC3_Aquatic/Moray",
        Animal::MantaRay => "FC3_Aquatic/MantaRay",
        // No authored identity in the catalog.
        Animal::Chicken => return None,
    })
}

/// The identities of supported ingredients in the installed game. Counts
/// still come from live inventory; crafting rules migrate independently.
#[derive(Resource, Default)]
pub struct LootCatalog(HashMap<LootKind, fc3_formats::progression::Item>);

impl LootCatalog {
    pub(crate) fn from_metadata(metadata: &fc3_formats::progression::Crafting) -> Self {
        let mut out = HashMap::new();
        let dropped_item = |source: &fc3_formats::progression::Source| {
            source.items.iter().filter(|(_, chance)| *chance >= 100.0)
                .find_map(|(id, _)| metadata.items.iter().find(|i| i.id == *id && i.loot && !i.rare && i.name_id != 0))
        };
        for animal in Animal::ALL {
            let Some(prototype) = animal_prototype(animal) else { continue };
            let full = format!("FC3_Animals:{prototype}");
            if let Some(item) = metadata.sources.iter().find(|s| s.prototype.eq_ignore_ascii_case(&full)).and_then(dropped_item) {
                out.insert(LootKind::Skin(animal), item.clone());
            }
        }
        for plant in crate::syringes::Plant::ALL {
            let prefix = format!("Pickups/Plants/{}_", plant.name().replace(['\'', ' '], ""));
            let source = metadata.sources.iter().find(|s| !s.skeleton && s.key.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()));
            let item = source.and_then(dropped_item);
            if let Some(item) = item { out.insert(LootKind::Plant(plant), item.clone()); }
        }
        for item in &metadata.items {
            out.insert(LootKind::Item(item.id), item.clone());
        }
        Self(out)
    }

    pub fn item(&self, kind: LootKind) -> Option<&fc3_formats::progression::Item> {
        self.0.get(&kind)
    }

    /// Nothing read from the game (no cache).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Several plants yield the same leaf item; scripts see the combined
    /// amount actually carried, including after consumption or save loading.
    pub fn quantities(&self, progress: &Progress) -> HashMap<u32, u32> {
        let mut quantities: HashMap<u32, u32> = HashMap::new();
        for (kind, item) in &self.0 {
            let count = match kind {
                LootKind::Skin(animal) => progress.skins.get(animal).copied().unwrap_or(0),
                LootKind::Plant(plant) => progress.plants[plant.index()],
                LootKind::Item(id) => progress.loot.get(id).copied().unwrap_or(0),
            };
            if count > 0 {
                let total = quantities.entry(item.name_id).or_default();
                *total = total.saturating_add(count);
            }
        }
        quantities
    }
}

fn load_loot_catalog(info: Res<crate::data::CacheInfo>, mut catalog: ResMut<LootCatalog>, mut crafting: ResMut<crate::crafting::CraftCatalog>) {
    match std::fs::read_to_string(info.root.join(fc3_formats::progression::RECIPES_REL))
        .map_err(anyhow::Error::from).and_then(|text| fc3_formats::progression::Crafting::from_text(&text)) {
        Ok(metadata) => {
            *catalog = LootCatalog::from_metadata(&metadata);
            *crafting = crate::crafting::CraftCatalog::from_metadata(&metadata);
            info!("loot: {} supported ingredient identities", catalog.0.len());
        }
        Err(e) => warn!("loot: cannot read ingredient catalog: {e:#}"),
    }
}

// ---------------------------------------------------------------- awards ---

/// Which takedown is killing whom (its `Takedown/...` XP reward), until the
/// death comes in.
#[derive(Resource, Default)]
pub struct TakedownRewards(pub HashMap<Entity, &'static str>);

#[allow(clippy::too_many_arguments)]
fn award_kills(
    mut died: MessageReader<Died>,
    mut hits: MessageReader<crate::combat::DamageEvent>,
    players: Query<(Entity, &Inventory), With<Player>>,
    npcs: Query<&crate::npc::Npc>,
    animals: Query<&crate::wildlife::WildAnimal>,
    kits: Query<&crate::characters::KitId>,
    (skills, crafting, mut takedowns): (Res<Skills>, Option<Res<crate::crafting::CraftCatalog>>, ResMut<TakedownRewards>),
    driving: Res<crate::vehicles::Driving>,
    mut headshots: Local<HashMap<Entity, bool>>,
    mut xp: MessageWriter<XpGain>,
    mut stats: MessageWriter<StatGain>,
) {
    let Ok((player, inv)) = players.single() else { return };
    // Whether the player's last hit on each was to the head.
    if headshots.len() > 512 {
        headshots.clear();
    }
    for h in hits.read().filter(|h| h.instigator == Some(player)) {
        headshots.insert(h.target, h.headshot);
    }
    for d in died.read() {
        let headshot = headshots.remove(&d.entity).unwrap_or(false);
        let takedown = takedowns.0.remove(&d.entity);
        // The kill counts: an animal's by its kind, a man's by how the player
        // killed him (the weapon in hand at the time).
        if d.killer == Some(player) && d.entity != player {
            if let Ok(a) = animals.get(d.entity) {
                for stat in [crate::statistics::stat::ANIMAL_KILLS, crate::statistics::animal_counts(a.animal()).0] {
                    stats.write(StatGain { stat, amount: 1 });
                }
            } else if let Ok(npc) = npcs.get(d.entity) {
                let how = crate::statistics::Kill {
                    kind: d.kind,
                    weapon: inv.slots.get(inv.current).map(|s| &inv.defs[s.def]),
                    aiming: inv.aiming,
                    headshot,
                    takedown: takedown.is_some(),
                    unaware: npc.state == crate::npc::NpcState::Idle,
                    driving: driving.vehicle.is_some(),
                };
                for stat in crate::statistics::kill_stats(&how) {
                    stats.write(StatGain { stat, amount: 1 });
                }
            }
        }
        if d.killer != Some(player) || d.entity == player || !npcs.get(d.entity).is_ok_and(|n| n.faction.hostile_to_player()) {
            continue;
        }
        let Ok(npc) = npcs.get(d.entity) else { continue };
        // The victim's own XP (by the kit he wears), times how he died.
        let kit = npc.rig.and_then(|r| kits.get(r).ok()).map_or(0, |k| k.0);
        let base = crafting.as_ref().map_or(0, |c| c.kill_xp(kit));
        let (reward, reason) = match takedown {
            Some(t) => (format!("Takedown/{t}"), "Takedown"),
            None if d.kind == DamageKind::Explosion => ("Standard/KillWithExplosion".to_string(), "Explosive kill"),
            None if headshot => ("Standard/KillWithHeadShotModifier".to_string(), "Headshot"),
            None if npc.state == crate::npc::NpcState::Idle => ("Standard/StealthKill".to_string(), "Stealth kill"),
            None => (String::new(), "Kill"),
        };
        let amount = (base as f32 * skills.xp_reward(&reward).unwrap_or(1.0)).round() as u32;
        if amount > 0 {
            xp.write(XpGain { amount, reason });
        }
    }
}

/// A taken outpost: the game's reward for a new safe house, and its flag
/// controller's bonus for taking it undetected or with no alarm raised.
fn award_outposts(
    mut liberated: MessageReader<crate::outposts::OutpostLiberated>,
    skills: Res<Skills>,
    mut progress: ResMut<Progress>,
    mut xp: MessageWriter<XpGain>,
) {
    for l in liberated.read() {
        progress.outposts += 1;
        if l.undetected {
            *progress.stats.entry(stat::OUTPOSTS_UNDETECTED).or_insert(0) += 1;
        }
        progress.add_money(500);
        let reward = |name: &str| skills.xp_reward(name).unwrap_or(0.0) as u32;
        xp.write(XpGain { amount: reward("SafeHouse"), reason: "Outpost liberated" });
        let bonus = match (l.undetected, l.alarm) {
            (true, _) => Some((reward("Outpost/Undetected"), "Undetected bonus")),
            (false, false) => Some((reward("Outpost/NoAlarm"), "No alarms bonus")),
            _ => None,
        };
        info!("outpost {} taken: {} XP, bonus {:?}", l.outpost, reward("SafeHouse"), bonus);
        if let Some((amount, reason)) = bonus.filter(|b| b.0 > 0) {
            xp.write(XpGain { amount, reason });
        }
    }
}

fn apply_xp(mut gains: MessageReader<XpGain>, skills: Res<Skills>, mut progress: ResMut<Progress>, mut hud: ResMut<HudState>, mut popups: ResMut<Popups>) {
    for g in gains.read() {
        progress.xp += g.amount;
        popups.0.push((format!("+{} XP  {}", g.amount, g.reason), 2.5));
        while progress.xp >= xp_needed(progress.level + 1, &skills) {
            progress.level += 1;
            progress.skill_points += 1;
            hud.notify(format!("LEVEL {} - skill point earned (Tab)", progress.level));
        }
    }
}

fn update_perks(mut progress: ResMut<Progress>, skills: Res<Skills>, crafting: Res<crate::crafting::CraftCatalog>, mut perks: ResMut<Perks>) {
    if progress.is_changed() || skills.is_changed() || crafting.is_changed() {
        // The crafted wallets' hold ($1000 to begin with).
        let wallet = if crafting.recipes.is_empty() { 0 } else { 1000 + crafting.gear_bonus(&progress, gear::WALLET) as u32 };
        let wallet = if progress.story { wallet } else { wallet.max(Progress { wallet: 0, ..progress.clone() }.money_cap()) };
        if progress.wallet != wallet {
            progress.bypass_change_detection().wallet = wallet;
        }
        let p = Perks::with_gear(&progress, &skills, Some(&crafting));
        if *perks != p {
            *perks = p;
        }
    }
}

/// Keeps the player's health and carry limits in line with the perks.
fn apply_perks(
    perks: Res<Perks>,
    mut boosts: ResMut<crate::syringes::Boosts>,
    mut players: Query<(&mut Health, &mut PlayerStats, &mut Inventory), With<Player>>,
) {
    let Ok((mut h, mut stats, mut inv)) = players.single_mut() else { return };
    let base = perks.max_health;
    // The health boost: its slots on top, filled; they don't regenerate, so
    // what of them is lost is gone.
    if boosts.full_heal && !h.dead {
        boosts.full_heal = false;
        h.max = base + boosts.extra_health;
        h.current = h.max;
    }
    let extra = if h.dead { 0.0 } else { boosts.extra_health.min((h.current - base).max(0.0)) };
    if extra != boosts.extra_health {
        boosts.extra_health = extra;
    }
    let want = base + extra;
    if (h.max - want).abs() > 0.1 {
        let gained = want - h.max;
        h.max = want;
        if gained > 0.0 && !h.dead {
            h.current += gained;
        }
        h.current = h.current.min(h.max);
    }
    stats.medkits = stats.medkits.min(perks.medkit_cap);
    inv.grenades = inv.grenades.min(perks.grenade_cap);
    inv.reserve_scale = perks.ammo_scale;
}

// --------------------------------------------------------------- journal ---

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum JournalPage {
    #[default]
    Crafting,
    Skills,
    /// The survival guide, tutorials and collectibles (`wikiitems.lib`).
    Handbook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CraftTutorial {
    Medicine(u8),
    Rucksack,
    Done,
}

/// Script-owned pause-menu page overrides. Tutorial progress records actual
/// committed crafts, never button presses or pre-existing inventory counts.
#[derive(Resource, Default)]
pub struct PauseMenu {
    pub page: JournalPage,
    pub tutorial: Option<CraftTutorial>,
    /// Keep completion across closing/quitting so re-entering B40's checkpoint
    /// cannot demand another pair of leaves after they were already consumed.
    pub tutorial_completed: bool,
    pub crafting_locked: bool,
    pub skills_locked: bool,
    pub unlock_gates: [Option<String>; 2],
}

impl PauseMenu {
    pub fn from_story(story: &fc3_story::Story) -> Self {
        let mut menu = Self::default();
        if let Some((crafting, skills)) = &story.data().menu_unlocks {
            menu.crafting_locked = true;
            menu.skills_locked = true;
            menu.unlock_gates = [Some(crafting.clone()), Some(skills.clone())];
            menu.update_unlocks(&story.missions().into_iter().filter(|m| m.1 == "Completed").map(|m| m.0.to_ascii_lowercase()).collect());
        }
        menu
    }

    pub fn update_unlocks(&mut self, completed: &HashSet<String>) {
        for (gate, locked) in self.unlock_gates.iter_mut().zip([&mut self.crafting_locked, &mut self.skills_locked]) {
            if gate.as_ref().is_some_and(|mission| completed.contains(mission)) {
                *gate = None;
                *locked = false;
            }
        }
    }

    pub fn can_exit(&self) -> bool {
        matches!(self.tutorial, None | Some(CraftTutorial::Done))
    }

    pub fn allows_page(&self, page: JournalPage) -> bool {
        match page {
            JournalPage::Crafting => !self.crafting_locked,
            JournalPage::Skills => !self.skills_locked && self.can_exit(),
            JournalPage::Handbook => self.can_exit(),
        }
    }

    pub fn allows_recipe(&self, id: &str) -> bool {
        !self.crafting_locked && match self.tutorial {
            Some(CraftTutorial::Medicine(_)) => id == crate::crafting::MEDICINE,
            Some(CraftTutorial::Rucksack) => id == crate::crafting::RUCKSACK,
            Some(CraftTutorial::Done) => false,
            None => true,
        }
    }

    pub fn record_craft(&mut self, id: &str) {
        self.tutorial = match (self.tutorial, id) {
            (Some(CraftTutorial::Medicine(n)), crate::crafting::MEDICINE) if n < 1 => Some(CraftTutorial::Medicine(n + 1)),
            (Some(CraftTutorial::Medicine(_)), crate::crafting::MEDICINE) => Some(CraftTutorial::Rucksack),
            (Some(CraftTutorial::Rucksack), crate::crafting::RUCKSACK) => Some(CraftTutorial::Done),
            (stage, _) => stage,
        };
        if self.tutorial == Some(CraftTutorial::Done) { self.tutorial_completed = true; }
    }

    pub fn override_page(&mut self, crafting: bool, skills: bool, tutorial: bool) {
        if skills && !crafting {
            self.page = JournalPage::Skills;
            self.skills_locked = false;
        } else {
            self.page = JournalPage::Crafting;
            self.crafting_locked = false;
            if tutorial && self.tutorial.is_none() {
                self.tutorial = Some(if self.tutorial_completed { CraftTutorial::Done } else { CraftTutorial::Medicine(0) });
            }
        }
    }
}

#[derive(Component)]
struct JournalRoot;

#[derive(Component)]
struct JournalContent(JournalPage);

#[derive(Component)]
struct JournalOrdinaryCrafting;

#[derive(Component, Clone, Copy)]
enum JournalButton {
    Page(JournalPage),
    Skill(usize),
    Craft(usize),
    Authored(usize),
    /// Mix a syringe, or inject one.
    Syringe(usize),
    Inject(usize),
    /// Tip an arrow.
    Arrow(usize),
    /// The handbook: a subcategory, an entry on the page, a page turn.
    HandbookSub(u32),
    HandbookEntry(usize),
    HandbookPage(i32),
    Close,
}

#[derive(Component)]
struct JournalText(JournalField);

#[derive(Clone, Copy, PartialEq)]
enum JournalField {
    Header,
    Tutorial,
    Close,
    Skill(usize),
    SkillStatus(usize),
    /// What the skill pointed at does, and what it needs.
    SkillDetail,
    Craft(usize),
    Authored(usize),
    Skins,
    Syringe(usize),
    Arrow(usize),
    Plants,
    HandbookEntry(usize),
    HandbookText,
    HandbookPage,
}

const IDLE: Color = Color::srgba(0.1, 0.1, 0.1, 0.85);
const HOVER: Color = Color::srgba(0.75, 0.25, 0.12, 0.9);
const TEXT: Color = Color::srgb(0.95, 0.95, 0.92);
const ACCENT: Color = Color::srgb(0.95, 0.6, 0.3);

fn font(size: f32) -> TextFont {
    TextFont { font_size: bevy::text::FontSize::Px(size), ..default() }
}

fn setup_journal(mut commands: Commands) {
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::top(Val::Px(30.0)),
                row_gap: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.85)),
            Visibility::Hidden,
            GlobalZIndex(11),
            JournalRoot,
        ))
        .id();
    commands.spawn((Text::new(""), font(22.0), TextColor(TEXT), JournalText(JournalField::Header), ChildOf(root)));
    let tabs = commands.spawn((Node { column_gap: Val::Px(12.0), ..default() }, ChildOf(root))).id();
    for (page, label) in [(JournalPage::Crafting, "CRAFTING"), (JournalPage::Skills, "SKILLS"), (JournalPage::Handbook, "HANDBOOK")] {
        commands.spawn((Button, Node { width: Val::Px(220.0), height: Val::Px(40.0), align_items: AlignItems::Center,
            justify_content: JustifyContent::Center, ..default() }, BackgroundColor(IDLE), JournalButton::Page(page), ChildOf(tabs),
            children![(Text::new(label), font(20.0), TextColor(TEXT))]));
    }
    commands.spawn((Text::new(""), font(22.0), TextColor(ACCENT), JournalText(JournalField::Tutorial), ChildOf(root)));
    let columns = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(50.0), ..default() }, ChildOf(root))).id();
    // Skills, by branch (filled in once the skill tree is loaded).
    let skills = commands
        .spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, JournalContent(JournalPage::Skills), SkillPage, ChildOf(columns)))
        .id();
    commands.spawn((Text::new("SKILLS"), font(24.0), TextColor(ACCENT), ChildOf(skills)));
    // Crafting and skins.
    let craft = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(6.0), ..default() }, JournalContent(JournalPage::Crafting), ChildOf(columns))).id();
    commands.spawn((Text::new("CRAFTING"), font(24.0), TextColor(ACCENT), ChildOf(craft)));
    for i in 0..crate::crafting::TUTORIAL_RECIPES.len() {
        let b = commands.spawn((Button,
            Node { width: Val::Px(440.0), min_height: Val::Px(46.0), align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
            BackgroundColor(IDLE), JournalButton::Authored(i), ChildOf(craft))).id();
        commands.spawn((Text::new(""), font(15.0), TextColor(TEXT), JournalText(JournalField::Authored(i)), ChildOf(b)));
    }
    // Gear on the left; syringes, skins and plants on the right.
    let ordinary = commands
        .spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(24.0), ..default() }, JournalOrdinaryCrafting, ChildOf(craft)))
        .id();
    // The gear chains and the syringes go in once the crafting catalog is
    // loaded.
    commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, GearList, ChildOf(ordinary)));
    let right = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, ChildOf(ordinary))).id();
    commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, PotionList, ChildOf(right)));
    commands.spawn((Text::new("SKINS"), font(20.0), TextColor(ACCENT), Node { margin: UiRect::top(Val::Px(12.0)), ..default() }, ChildOf(right)));
    commands.spawn((Text::new(""), font(14.0), TextColor(TEXT), Node { width: Val::Px(440.0), ..default() }, JournalText(JournalField::Skins), ChildOf(right)));
    commands.spawn((
        Text::new(""),
        font(14.0),
        TextColor(Color::srgba(0.9, 0.9, 0.88, 0.85)),
        Node { width: Val::Px(440.0), margin: UiRect::top(Val::Px(8.0)), ..default() },
        JournalText(JournalField::Plants),
        ChildOf(right),
    ));
    // The handbook: subcategories (filled in once it is loaded), a page of
    // entries, and the one picked.
    let book = commands
        .spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(24.0), ..default() }, JournalContent(JournalPage::Handbook), ChildOf(columns)))
        .id();
    commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), width: Val::Px(260.0), ..default() }, HandbookTopics, ChildOf(book)));
    let list = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(2.0), width: Val::Px(330.0), ..default() }, ChildOf(book))).id();
    for i in 0..HANDBOOK_ROWS {
        commands.spawn((
            Button,
            Node { width: Val::Px(330.0), height: Val::Px(26.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(8.0)), flex_shrink: 0.0, ..default() },
            BackgroundColor(IDLE),
            JournalButton::HandbookEntry(i),
            ChildOf(list),
            children![(Text::new(""), font(14.0), TextColor(TEXT), JournalText(JournalField::HandbookEntry(i)))],
        ));
    }
    let turn = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(8.0), margin: UiRect::top(Val::Px(6.0)), ..default() }, ChildOf(list))).id();
    for (step, label) in [(-1, "<"), (1, ">")] {
        commands.spawn((
            Button,
            Node { width: Val::Px(40.0), height: Val::Px(26.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() },
            BackgroundColor(IDLE),
            JournalButton::HandbookPage(step),
            ChildOf(turn),
            children![(Text::new(label), font(15.0), TextColor(TEXT))],
        ));
    }
    commands.spawn((Text::new(""), font(14.0), TextColor(TEXT), JournalText(JournalField::HandbookPage), ChildOf(turn)));
    commands.spawn((Text::new(""), font(15.0), TextColor(TEXT), Node { width: Val::Px(520.0), ..default() }, JournalText(JournalField::HandbookText), ChildOf(book)));
    let close = commands
        .spawn((
            Button,
            Node { width: Val::Px(200.0), height: Val::Px(36.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() },
            BackgroundColor(IDLE),
            JournalButton::Close,
            ChildOf(root),
        ))
        .id();
    commands.spawn((Text::new("CLOSE  (Tab)"), font(17.0), TextColor(TEXT), JournalText(JournalField::Close), ChildOf(close)));
}

fn toggle_journal(
    keys: Res<ButtonInput<KeyCode>>,
    args: Res<crate::Args>,
    mut opened: Local<bool>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut root: Query<&mut Visibility, With<JournalRoot>>,
    mut menu: ResMut<PauseMenu>,
) {
    // --journal (tests): open once in play.
    let test = args.open_journal && !*opened && *state.get() == GameState::Playing;
    if test {
        *opened = true;
        // FC3_JOURNAL_PAGE=skills|handbook (tests): open on the skill tree or
        // the handbook.
        match std::env::var("FC3_JOURNAL_PAGE").as_deref() {
            Ok("skills") => menu.page = JournalPage::Skills,
            Ok("handbook") => menu.page = JournalPage::Handbook,
            _ => {}
        }
    }
    if keys.just_pressed(KeyCode::Tab) || test {
        match state.get() {
            GameState::Playing if menu.allows_page(menu.page) => next.set(GameState::Journal),
            GameState::Journal if menu.can_exit() => {
                menu.tutorial = None;
                next.set(GameState::Playing);
            }
            _ => {}
        }
    }
    if let Ok(mut v) = root.single_mut() {
        *v = if *state.get() == GameState::Journal { Visibility::Inherited } else { Visibility::Hidden };
    }
}

#[allow(clippy::type_complexity)]
fn journal_buttons(
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut buttons: Query<(&Interaction, &JournalButton, &mut BackgroundColor), Changed<Interaction>>,
    mut progress: ResMut<Progress>,
    mut hud: ResMut<HudState>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut boosts: ResMut<crate::syringes::Boosts>,
    crafting: Res<crate::crafting::CraftCatalog>,
    loot: Res<LootCatalog>,
    perks: Res<Perks>,
    mut players: Query<&mut PlayerStats, With<Player>>,
    mut crafted: MessageWriter<crate::crafting::ItemCrafted>,
    mut menu: ResMut<PauseMenu>,
    (skills, mut hover, mut inventories): (Res<Skills>, ResMut<SkillHover>, Query<&mut Inventory, With<Player>>),
    (book, mut view): (Res<HandbookData>, ResMut<HandbookView>),
) {
    if *state.get() != GameState::Journal {
        return;
    }
    for (interaction, kind, mut bg) in &mut buttons {
        match interaction {
            Interaction::Pressed => {
                sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.7 });
                match *kind {
                    JournalButton::Page(page) => {
                        if menu.allows_page(page) { menu.page = page; }
                    }
                    JournalButton::Authored(i) => {
                        let id = crate::crafting::TUTORIAL_RECIPES[i];
                        if menu.page != JournalPage::Crafting || !menu.allows_recipe(id) { continue; }
                        if let Ok(mut stats) = players.single_mut() {
                            if let Some(event) = crafting.craft(&mut progress, &mut stats, perks.medkit_cap, &loot, id) {
                                menu.record_craft(id);
                                if let Some(recipe) = crafting.recipe(id) { hud.notify(format!("Crafted {}", recipe.name)); }
                                crafted.write(event);
                            }
                        }
                    }
                    JournalButton::Skill(i) => {
                        if menu.page != JournalPage::Skills || !menu.allows_page(JournalPage::Skills) { continue; }
                        let Some(a) = skills.0.abilities.get(i) else { continue };
                        if skill_state(&progress, a) == SkillState::Ready {
                            progress.skill_points -= a.cost;
                            progress.skills.insert(a.id.clone());
                            hud.notify(format!("Learned {}", a.name));
                        }
                    }
                    JournalButton::Craft(i) => {
                        if menu.page != JournalPage::Crafting || menu.tutorial.is_some() || menu.crafting_locked { continue; }
                        // The chain's next tier.
                        let chains = crafting.gear_chains();
                        let Some(next) = chains.get(i).and_then(|c| c.iter().find(|r| !progress.recipes.contains(&r.id))) else { continue };
                        let id = next.id.clone();
                        if let Ok(mut stats) = players.single_mut() {
                            if let Some(event) = crafting.craft(&mut progress, &mut stats, perks.medkit_cap, &loot, &id) {
                                if let Some(recipe) = crafting.recipe(&id) { hud.notify(format!("Crafted {}", recipe.name)); }
                                crafted.write(event);
                            }
                        }
                    }
                    JournalButton::Syringe(i) => {
                        if menu.page != JournalPage::Crafting || menu.tutorial.is_some() || menu.crafting_locked { continue; }
                        let Some(id) = potion_recipes(&crafting).get(i).map(|r| r.id.clone()) else { continue };
                        if let Ok(mut stats) = players.single_mut() {
                            if let Some(event) = crafting.craft(&mut progress, &mut stats, perks.medkit_cap, &loot, &id) {
                                if let Some(recipe) = crafting.recipe(&id) { hud.notify(format!("Crafted {}", recipe.name)); }
                                crafted.write(event);
                            }
                        }
                    }
                    JournalButton::Arrow(i) => {
                        if menu.page != JournalPage::Crafting || menu.tutorial.is_some() || menu.crafting_locked { continue; }
                        let Some(id) = crafting.arrow_recipes().get(i).map(|r| r.id.clone()) else { continue };
                        if let Ok(mut inv) = inventories.single_mut() {
                            if let Some(event) = crafting.craft_arrow(&progress, &mut inv, perks.arrow_cap, &id) {
                                if let Some(recipe) = crafting.recipe(&id) { hud.notify(format!("Crafted {}", recipe.name)); }
                                crafted.write(event);
                            }
                        }
                    }
                    JournalButton::Inject(i) => {
                        if menu.page != JournalPage::Crafting || menu.tutorial.is_some() || menu.crafting_locked { continue; }
                        let Some(recipe) = potion_recipes(&crafting).get(i).copied() else { continue };
                        if crate::syringes::inject(&mut progress, &mut boosts, recipe) {
                            hud.notify(format!("{}: {}", recipe.name, recipe.description));
                        }
                    }
                    JournalButton::HandbookSub(sub) => {
                        view.subcategory = Some(sub);
                        view.page = 0;
                        view.entry = None;
                    }
                    JournalButton::HandbookEntry(row) => {
                        let open = book.0.as_ref().and_then(|b| handbook_page(b, &view).get(row).copied()).filter(|e| handbook_open(e, &progress));
                        if let Some(e) = open {
                            view.entry = Some(e.id);
                        }
                    }
                    JournalButton::HandbookPage(step) => {
                        let pages = book.0.as_ref().map_or(1, |b| handbook_entries(b, &view).len().div_ceil(HANDBOOK_ROWS).max(1));
                        view.page = (view.page as i32 + step).clamp(0, pages as i32 - 1) as usize;
                    }
                    JournalButton::Close => {
                        if menu.can_exit() { menu.tutorial = None; next.set(GameState::Playing); }
                    }
                }
            }
            Interaction::Hovered => {
                bg.0 = HOVER;
                if let JournalButton::Skill(i) = *kind {
                    hover.0 = Some(i);
                }
            }
            Interaction::None => bg.0 = IDLE,
        }
    }
}

#[allow(clippy::type_complexity)]
fn journal_layout(
    menu: Res<PauseMenu>,
    mut nodes: Query<(&mut Node, Option<&JournalContent>, Option<&JournalOrdinaryCrafting>, Option<&JournalButton>)>,
    (book, view): (Res<HandbookData>, Res<HandbookView>),
) {
    let rows = book.0.as_ref().map_or(0, |b| handbook_page(b, &view).len());
    for (mut node, content, ordinary, button) in &mut nodes {
        let shown = if let Some(content) = content { content.0 == menu.page }
            else if let Some(JournalButton::HandbookEntry(i)) = button { *i < rows }
            else if ordinary.is_some() { menu.tutorial.is_none() }
            else if let Some(JournalButton::Authored(i)) = button { menu.allows_recipe(crate::crafting::TUTORIAL_RECIPES[*i]) }
            else if let Some(JournalButton::Page(page)) = button { menu.allows_page(*page) }
            else { continue };
        node.display = if shown { Display::Flex } else { Display::None };
    }
}

/// Smoke runs press the same recipe buttons as a user, at real-time intervals
/// even while game time is paused. Failed crafts do not advance the tutorial.
fn journal_automation(
    state: Res<State<GameState>>, menu: Res<PauseMenu>, time: Res<Time<Real>>,
    mut due: Local<f32>, mut pressed: Local<Option<Entity>>, mut buttons: Query<(Entity, &JournalButton, &mut Interaction)>,
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() { return; }
    if let Some(last) = pressed.take() {
        if let Ok((_, _, mut interaction)) = buttons.get_mut(last) { *interaction = Interaction::None; }
    }
    if *state.get() != GameState::Journal || menu.tutorial.is_none() || time.elapsed_secs() < *due { return; }
    *due = time.elapsed_secs() + 1.0;
    let target = match menu.tutorial {
        Some(CraftTutorial::Medicine(_)) => JournalButton::Authored(0),
        Some(CraftTutorial::Rucksack) => JournalButton::Authored(1),
        Some(CraftTutorial::Done) => JournalButton::Close,
        None => return,
    };
    for (e, kind, mut interaction) in &mut buttons {
        let matches = match (*kind, target) {
            (JournalButton::Authored(a), JournalButton::Authored(b)) => a == b,
            (JournalButton::Close, JournalButton::Close) => true,
            _ => false,
        };
        if matches { *interaction = Interaction::Pressed; *pressed = Some(e); break; }
    }
}

fn refresh_journal(
    state: Res<State<GameState>>,
    progress: Res<Progress>,
    players: Query<&PlayerStats>,
    mut texts: Query<(&JournalText, &mut Text, &mut TextColor)>,
    crafting: Res<crate::crafting::CraftCatalog>,
    loot: Res<LootCatalog>,
    perks: Res<Perks>,
    menu: Res<PauseMenu>,
    host: Option<NonSend<crate::story::StoryHost>>,
    (skills, hover, inventories): (Res<Skills>, Res<SkillHover>, Query<&Inventory, With<Player>>),
    (book, view): (Res<HandbookData>, Res<HandbookView>),
) {
    if *state.get() != GameState::Journal {
        return;
    }
    let kills = players.single().map(|s| s.kills).unwrap_or(0);
    let page: Vec<&fc3_formats::progression::HandbookEntry> = book.0.as_ref().map(|b| handbook_page(b, &view)).unwrap_or_default();
    for (field, mut t, mut c) in &mut texts {
        match field.0 {
            JournalField::Tutorial => {
                let localized = |id, fallback: &str| host.as_ref().and_then(|h| h.story.data().strings.get(id).map(str::to_string))
                    .unwrap_or_else(|| fallback.to_string());
                t.0 = match menu.tutorial {
                    Some(CraftTutorial::Medicine(n)) => format!("{}  {n}/2", localized(125975, "Craft Medicine")),
                    Some(CraftTutorial::Rucksack) => localized(167714, "Craft a Simple Loot Rucksack"),
                    Some(CraftTutorial::Done) => "Crafting complete. Close the menu to continue.".into(),
                    None => String::new(),
                };
            }
            JournalField::Close => {
                t.0 = if menu.can_exit() { "CLOSE  (Tab / Esc)" } else { "Finish crafting to continue" }.into();
                c.0 = if menu.can_exit() { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
            }
            JournalField::Authored(i) => {
                let id = crate::crafting::TUTORIAL_RECIPES[i];
                if let Some(recipe) = crafting.recipe(id) {
                    let available = players.single().is_ok_and(|stats| crafting.can_craft(&progress, stats, perks.medkit_cap, &loot, id));
                    let status = if progress.recipes.contains(id) { "crafted" } else if !crafting.available(&progress, recipe) { "locked" } else { "craft" };
                    let status = if id == crate::crafting::MEDICINE && players.single().is_ok_and(|s| s.medkits >= perks.medkit_cap) { "syringe pouch full" } else { status };
                    let have = loot.quantities(&progress);
                    let amounts = recipe.ingredients.iter().map(|(id, need)| {
                        let item = crafting.items.iter().find(|i| i.id == *id);
                        let count = item.and_then(|i| have.get(&i.name_id)).copied().unwrap_or(0);
                        format!("{}/{} {}", count, need, item.map_or("Unknown ingredient", |i| i.name.as_str()))
                    }).collect::<Vec<_>>().join(", ");
                    t.0 = format!("{} - {status}\n{amounts}", recipe.name);
                    c.0 = if available { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
                } else {
                    t.0 = "Crafting data unavailable".into();
                }
            }
            JournalField::Header => {
                let (lo, hi) = (xp_needed(progress.level, &skills), xp_needed(progress.level + 1, &skills));
                t.0 = format!(
                    "LEVEL {}    XP {} / {}    SKILL POINTS {}    ${} / {}    OUTPOSTS {}    KILLS {}\nRADIO TOWERS {} / 18    RELICS {} / {}    LOST LETTERS {} / {}    MEMORY CARDS {} / {}",
                    progress.level,
                    progress.xp - lo,
                    hi - lo,
                    progress.skill_points,
                    progress.money,
                    progress.money_cap(),
                    progress.outposts,
                    kills,
                    progress.towers.len(),
                    progress.relics,
                    crate::pickups::RELICS,
                    progress.letters,
                    crate::pickups::LETTERS,
                    progress.cards,
                    crate::pickups::MEMORY_CARDS
                );
            }
            JournalField::SkillStatus(i) => {
                let Some(a) = skills.0.abilities.get(i) else { continue };
                t.0 = match skill_state(&progress, a) {
                    SkillState::Learned => "LEARNED".into(),
                    SkillState::Locked(SkillLock::Story) | SkillState::Locked(SkillLock::Unavailable) => "LOCKED".into(),
                    SkillState::Locked(SkillLock::Stat(_, have, need)) => format!("{have}/{need}"),
                    _ => format!("{} PT", a.cost),
                };
            }
            JournalField::Skill(i) => {
                let Some(a) = skills.0.abilities.get(i) else { continue };
                c.0 = match skill_state(&progress, a) {
                    SkillState::Learned => Color::srgb(0.55, 0.85, 0.5),
                    SkillState::Ready => TEXT,
                    _ => Color::srgba(0.8, 0.8, 0.8, 0.55),
                };
            }
            JournalField::SkillDetail => {
                let Some(a) = hover.0.and_then(|i| skills.0.abilities.get(i)) else {
                    t.0 = "Point at a skill for what it does.".into();
                    continue;
                };
                let why = match skill_state(&progress, a) {
                    SkillState::Learned => "Learned.".to_string(),
                    SkillState::Ready => format!("Learn it for {} skill point{} (click).", a.cost, if a.cost == 1 { "" } else { "s" }),
                    SkillState::Locked(SkillLock::Unavailable) => "Not available.".into(),
                    SkillState::Locked(SkillLock::Story) => "Opens as the story goes on.".into(),
                    SkillState::Locked(SkillLock::Requires(r)) => {
                        let names: Vec<&str> = r.iter().map(|id| skills.ability(id).map_or(id.as_str(), |x| x.name.as_str())).collect();
                        format!("Learn {} first.", names.join(" and "))
                    }
                    SkillState::Locked(SkillLock::Stat(_, have, need)) => format!("{}: {have}/{need}", a.stat_text),
                    SkillState::Locked(SkillLock::Points) => format!("Needs {} skill point{}.", a.cost, if a.cost == 1 { "" } else { "s" }),
                };
                t.0 = format!("{}\n{}\n{why}", a.name.to_uppercase(), describe(a));
            }
            JournalField::Craft(i) => {
                let chains = crafting.gear_chains();
                let Some(chain) = chains.get(i) else { continue };
                let done = crafting.tiers(&progress, chain);
                let Some(next) = chain.iter().find(|r| !progress.recipes.contains(&r.id)) else {
                    t.0 = format!("{}  -  complete ({done}/{})", chain.last().map_or("", |r| r.name.as_str()), chain.len());
                    c.0 = Color::srgb(0.55, 0.85, 0.5);
                    continue;
                };
                let have = loot.quantities(&progress);
                let amounts = next.ingredients.iter().map(|(id, need)| {
                    let item = crafting.items.iter().find(|i| i.id == *id);
                    let count = item.and_then(|i| have.get(&i.name_id)).copied().unwrap_or(0);
                    format!("{count}/{need} {}", item.map_or("Unknown ingredient", |i| i.name.as_str()))
                }).collect::<Vec<_>>().join(", ");
                let state = if crafting.available(&progress, next) { "" } else { "  (locked)" };
                t.0 = format!("{}  ({}/{}){state}\n{amounts}", next.name, done + 1, chain.len());
                let ready = players.single().is_ok_and(|stats| crafting.can_craft(&progress, stats, perks.medkit_cap, &loot, &next.id));
                c.0 = if ready { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
            }
            JournalField::Skins => {
                let skins: Vec<String> = Animal::ALL.iter().map(|a| format!("{} {}", a.name(), progress.skins.get(a).copied().unwrap_or(0))).collect();
                // The rucksack: everything gathered, and the loot to sell by
                // name.
                let loot: Vec<String> = progress
                    .loot
                    .iter()
                    .filter(|(_, n)| **n > 0)
                    .map(|(id, n)| format!("{} x{n}", crafting.item(*id).map_or("?", |i| i.name.as_str())))
                    .collect();
                let loot = if loot.is_empty() { String::new() } else { format!("\nLoot: {}", loot.join(", ")) };
                t.0 = format!(
                    "Rucksack {}/{}{loot}\n{}",
                    crate::crafting::loot_count(&progress),
                    crafting.loot_capacity(&progress),
                    skins.join("   ")
                );
            }
            JournalField::Syringe(i) => {
                let Some(recipe) = potion_recipes(&crafting).get(i).copied() else { continue };
                let have = loot.quantities(&progress);
                let needs = recipe.ingredients.iter().map(|(id, need)| {
                    let item = crafting.items.iter().find(|i| i.id == *id);
                    let count = item.and_then(|i| have.get(&i.name_id)).copied().unwrap_or(0);
                    format!("{count}/{need} {}", item.map_or("Unknown ingredient", |i| i.name.as_str()))
                }).collect::<Vec<_>>().join(", ");
                let state = if crafting.available(&progress, recipe) { "" } else { "  (locked)" };
                let on_hand = progress.potions.get(&recipe.id).copied().unwrap_or(0);
                let effect = if recipe.health_slots > 0 { format!("+{} health", recipe.health_slots) } else { format!("{:.0}s", recipe.duration) };
                t.0 = format!("{} x{on_hand}{state}  -  {effect}\n{needs}", recipe.name);
                let ready = players.single().is_ok_and(|stats| crafting.can_craft(&progress, stats, perks.medkit_cap, &loot, &recipe.id));
                c.0 = if ready { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
            }
            JournalField::Arrow(i) => {
                let Some(recipe) = crafting.arrow_recipes().get(i).copied() else { continue };
                let Ok(inv) = inventories.single() else { continue };
                let (grenades, molotovs) = crafting.gadget_cost(recipe).unwrap_or_default();
                let needs = match (grenades, molotovs) {
                    (g, 0) => format!("{}/{g} grenades", inv.grenades),
                    (0, m) => format!("{}/{m} molotovs", inv.molotovs),
                    (g, m) => format!("{}/{g} grenades, {}/{m} molotovs", inv.grenades, inv.molotovs),
                };
                let kind = crate::crafting::arrow_kind(recipe).unwrap_or(1) as usize;
                let state = if crafting.available(&progress, recipe) { "" } else { "  (locked)" };
                t.0 = format!("{} {}/{}{state}\n{needs}", recipe.name, inv.special_arrows[kind - 1], perks.arrow_cap);
                let ready = crafting.can_craft_arrow(&progress, inv, perks.arrow_cap, &recipe.id);
                c.0 = if ready { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
            }
            JournalField::HandbookEntry(row) => {
                let Some(e) = page.get(row) else {
                    t.0 = String::new();
                    continue;
                };
                let open = handbook_open(e, &progress);
                t.0 = if open { e.name.clone() } else { "???".into() };
                let picked = view.entry.or_else(|| page.iter().find(|e| handbook_open(e, &progress)).map(|e| e.id));
                c.0 = if picked == Some(e.id) {
                    ACCENT
                } else if open {
                    TEXT
                } else {
                    Color::srgba(0.8, 0.8, 0.8, 0.45)
                };
            }
            JournalField::HandbookPage => {
                let total = book.0.as_ref().map_or(0, |b| handbook_entries(b, &view).len());
                t.0 = if total > HANDBOOK_ROWS { format!("{}/{}", view.page + 1, total.div_ceil(HANDBOOK_ROWS)) } else { String::new() };
            }
            JournalField::HandbookText => {
                // (The page picked, else the first open one shown.)
                let entry = view
                    .entry
                    .and_then(|id| book.0.as_ref()?.entries.iter().find(|e| e.id == id))
                    .or_else(|| page.iter().copied().find(|e| handbook_open(e, &progress)));
                t.0 = match entry {
                    Some(e) => format!("{}\n\n{}", e.name.to_uppercase(), e.description.replace("\\n", "\n")),
                    None if book.0.is_none() => "The handbook isn't in the cache: run the converter.".into(),
                    None => "Pick a page.".into(),
                };
            }
            JournalField::Plants => {
                let plants: Vec<String> = crate::syringes::Plant::ALL.iter().map(|p| format!("{} {}", p.name(), progress.plants[p.index()])).collect();
                t.0 = format!("Plants: {}", plants.join("   "));
            }
        }
    }
}

/// The skills page's container (filled in from the skill tree).
#[derive(Component)]
struct SkillPage;

/// The skill last pointed at (its details show under the tree).
#[derive(Resource, Default)]
struct SkillHover(Option<usize>);

fn load_skills(info: Res<crate::data::CacheInfo>, mut skills: ResMut<Skills>) {
    match std::fs::read_to_string(info.root.join(fc3_formats::progression::SKILLS_REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| fc3_formats::progression::SkillTree::from_text(&t))
    {
        Ok(tree) => {
            info!("skills: {} branches, {} abilities, {} levels", tree.branches.len(), tree.abilities.len(), tree.xp_levels.len());
            skills.0 = tree;
        }
        Err(e) => warn!("skills: cannot read the skill tree: {e:#}"),
    }
}

/// An ability's description with its number put in (`%d`).
pub(crate) fn describe(a: &fc3_formats::progression::Ability) -> String {
    a.description.replace("%d", &a.value.to_string()).replace("%%", "%")
}

/// Each branch of the skill tree as the game lays it out: its abilities in
/// their rows and columns, with the points they take or what they wait for.
fn build_skill_page(skills: Res<Skills>, pages: Query<Entity, With<SkillPage>>, mut built: Local<bool>, mut commands: Commands) {
    if *built || skills.0.abilities.is_empty() {
        return;
    }
    let Ok(page) = pages.single() else { return };
    *built = true;
    let mut branches = skills.0.branches.clone();
    branches.sort_by_key(|b| b.number);
    for branch in &branches {
        commands.spawn((
            Text::new(branch.name.to_uppercase()),
            font(16.0),
            TextColor(Color::srgb(0.75, 0.8, 0.9)),
            Node { margin: UiRect::top(Val::Px(6.0)), ..default() },
            ChildOf(page),
        ));
        let cells: Vec<(usize, &fc3_formats::progression::Ability)> = skills.0.abilities.iter().enumerate().filter(|(_, a)| a.branch == branch.number).collect();
        let rows = cells.iter().map(|(_, a)| a.row).max().unwrap_or(0);
        let columns = cells.iter().map(|(_, a)| a.column).max().unwrap_or(0);
        for row in 0..=rows {
            let line = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), ..default() }, ChildOf(page))).id();
            for column in 0..=columns {
                let cell = Node { width: Val::Px(146.0), height: Val::Px(44.0), flex_direction: FlexDirection::Column, justify_content: JustifyContent::Center, padding: UiRect::horizontal(Val::Px(6.0)), flex_shrink: 0.0, ..default() };
                match cells.iter().find(|(_, a)| a.row == row && a.column == column) {
                    Some(&(i, a)) => {
                        commands.spawn((Button, cell, BackgroundColor(IDLE), JournalButton::Skill(i), ChildOf(line), children![
                            (Text::new(a.name.clone()), font(13.0), TextColor(TEXT), JournalText(JournalField::Skill(i))),
                            (Text::new(""), font(12.0), TextColor(TEXT), JournalText(JournalField::SkillStatus(i))),
                        ]));
                    }
                    None => {
                        commands.spawn((cell, ChildOf(line)));
                    }
                }
            }
        }
    }
    commands.spawn((
        Text::new(""),
        font(14.0),
        TextColor(Color::srgba(0.9, 0.9, 0.88, 0.9)),
        Node { width: Val::Px(1200.0), min_height: Val::Px(60.0), margin: UiRect::top(Val::Px(8.0)), ..default() },
        JournalText(JournalField::SkillDetail),
        ChildOf(page),
    ));
}

/// The handbook's pages, as converted.
#[derive(Resource, Default)]
pub struct HandbookData(pub Option<fc3_formats::progression::Handbook>);

/// What the handbook shows: a subcategory, its page of entries, the entry
/// read.
#[derive(Resource, Default)]
pub struct HandbookView {
    pub subcategory: Option<u32>,
    pub page: usize,
    pub entry: Option<u32>,
}

/// Where the handbook's subcategories are listed.
#[derive(Component)]
struct HandbookTopics;

/// Entries on a page.
const HANDBOOK_ROWS: usize = 22;

fn load_handbook(info: Res<crate::data::CacheInfo>, mut book: ResMut<HandbookData>) {
    match std::fs::read_to_string(info.root.join(fc3_formats::progression::HANDBOOK_REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| fc3_formats::progression::Handbook::from_text(&t))
    {
        Ok(h) => {
            info!("handbook: {} pages in {} subcategories", h.entries.len(), h.subcategories.len());
            book.0 = Some(h);
        }
        Err(e) => warn!("handbook: {e:#}"),
    }
}

/// Whether the player can read a page: in free roam all of them; in the
/// story, those open from the start, by their mission or by their script
/// group, and those the engine opens as the player meets what they
/// describe: animals killed or skinned, plants gathered, weapons held and
/// vehicles boarded (`Progress::met`), and as many of a kind of collectible
/// as have been found.
pub(crate) fn handbook_open(e: &fc3_formats::progression::HandbookEntry, progress: &Progress) -> bool {
    if !progress.story || e.at_start {
        return true;
    }
    if !e.unlock_mission.is_empty() || !e.unlock_group.is_empty() {
        return (!e.unlock_mission.is_empty() && progress.missions.contains(&e.unlock_mission.to_ascii_lowercase()))
            || (!e.unlock_group.is_empty() && progress.unlocks.contains(&e.unlock_group));
    }
    let key = e.key.to_ascii_lowercase();
    if ["wild life/", "plant life/", "weapons/", "transport/"].iter().any(|s| key.starts_with(s)) {
        return progress.met.contains(&e.key);
    }
    if let Some(rest) = key.strip_prefix("collectibles/") {
        // "Collectibles/Hidden relic 12": the found count of the kind against
        // its number.
        let found = if rest.contains("relic") {
            progress.relics
        } else if rest.contains("letter") {
            progress.letters
        } else {
            progress.cards
        };
        let n: u32 = rest.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(1);
        return n <= found;
    }
    true
}

/// The handbook pages for an animal or a plant met (not the rare or
/// downloadable ones), by name: the page's name within the creature's or
/// the other way round ("Mean dog", "Water Buffalo").
fn met_pages<'a>(book: &'a fc3_formats::progression::Handbook, section: &str, name: &str) -> Vec<&'a str> {
    let name = name.to_ascii_lowercase();
    book.entries
        .iter()
        .filter_map(|e| {
            let page = e.key.strip_prefix(section)?.to_ascii_lowercase();
            (!page.contains(" - ") && (page.contains(&name) || name.contains(&page))).then_some(e.key.as_str())
        })
        .collect()
}

/// A weapon's handbook page, where the handbook names it otherwise
/// (the rest go by their own name).
fn weapon_page(name: &str) -> &str {
    match name {
        "AK-74M Rifle" => "AK47",
        "870 Shotgun" => "M133",
        "700 Sniper" => "R700",
        "PKM LMG" => "PKM",
        "MP5 SMG" => "MP5",
        "Vector .45 ACP" => "ACP",
        "Z93" => "M93",
        _ => name,
    }
}

/// A vehicle's handbook page.
fn vehicle_page(name: &str) -> &str {
    match name {
        "4x4" => "Scavenger",
        "Sedan" => "Darrah",
        "Hatchback" => "Stryus",
        "Truck" => "Cargo truck",
        "Boat" => "RHIB",
        "Hang glider" => "Glider",
        _ => name,
    }
}

/// A handbook page in a section by name, matched on letters and digits
/// alone ("SPAS-12" is "SPAS 12").
fn page_named<'a>(book: &'a fc3_formats::progression::Handbook, section: &str, name: &str) -> Option<&'a str> {
    let plain = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let want = plain(name);
    book.entries.iter().find(|e| e.key.strip_prefix(section).is_some_and(|p| plain(p) == want)).map(|e| e.key.as_str())
}

/// Opens the handbook pages of what the player meets: animals and plants,
/// the weapons and gadgets carried, the vehicles boarded and the gear owned.
#[allow(clippy::too_many_arguments)]
fn handbook_encounters(
    book: Res<HandbookData>,
    mut looted: MessageReader<ItemLooted>,
    mut died: MessageReader<Died>,
    players: Query<(Entity, Option<&crate::weapons::Inventory>), With<Player>>,
    animals: Query<&crate::wildlife::WildAnimal>,
    (driving, vehicles, time, mut next): (Res<crate::vehicles::Driving>, Query<&crate::vehicles::Vehicle>, Res<Time>, Local<f32>),
    mut progress: ResMut<Progress>,
) {
    let Some(b) = book.0.as_ref() else {
        looted.clear();
        died.clear();
        return;
    };
    let (player, inventory) = players.single().map(|(e, i)| (Some(e), i)).unwrap_or((None, None));
    let mut met: Vec<&str> = Vec::new();
    // What is carried and ridden, twice a second.
    if time.elapsed_secs() >= *next {
        *next = time.elapsed_secs() + 0.5;
        if let Some(inv) = inventory {
            for s in &inv.slots {
                met.extend(inv.defs.get(s.def).and_then(|d| page_named(b, "Weapons/", weapon_page(d.name))));
            }
            for (have, page) in [(inv.grenades, "Grenade"), (inv.molotovs, "Molotov"), (inv.c4, "C4"), (inv.mines, "Mine")] {
                if have > 0 {
                    met.extend(page_named(b, "Weapons/", page));
                }
            }
        }
        if let Some(v) = driving.vehicle.and_then(|v| vehicles.get(v).ok()) {
            met.extend(page_named(b, "Transport/", vehicle_page(v.name())));
        }
        for (gear, page) in [(crate::freefall::PARACHUTE, "Parachute"), (crate::freefall::WINGSUIT, "Wingsuit")] {
            if progress.upgrade(gear) > 0 {
                met.extend(page_named(b, "Transport/", page));
            }
        }
    }
    for l in looted.read() {
        match l.0 {
            LootKind::Skin(a) => met.extend(met_pages(b, "Wild life/", a.name())),
            LootKind::Plant(p) => met.extend(met_pages(b, "Plant life/", p.name())),
            LootKind::Item(_) => {}
        }
    }
    for d in died.read().filter(|d| d.killer.is_some() && d.killer == player) {
        if let Ok(a) = animals.get(d.entity) {
            met.extend(met_pages(b, "Wild life/", a.animal().name()));
        }
    }
    for key in met {
        if !progress.met.contains(key) {
            progress.met.insert(key.to_string());
        }
    }
}

/// The subcategories with pages, in the menu's order.
fn handbook_topics(book: &fc3_formats::progression::Handbook) -> Vec<(&fc3_formats::progression::HandbookCategory, Vec<&fc3_formats::progression::Category>)> {
    book.categories
        .iter()
        .map(|c| {
            let mut subs: Vec<&fc3_formats::progression::Category> =
                book.subcategories.iter().filter(|s| book.entries.iter().any(|e| e.subcategory == s.id && e.category == c.id)).collect();
            subs.sort_by_key(|s| (s.sort, s.id));
            (c, subs)
        })
        .filter(|(_, subs)| !subs.is_empty())
        .collect()
}

/// The entries of the subcategory shown (the menu's first, by default).
fn handbook_entries<'a>(book: &'a fc3_formats::progression::Handbook, view: &HandbookView) -> Vec<&'a fc3_formats::progression::HandbookEntry> {
    let sub = view.subcategory.or_else(|| handbook_topics(book).first().and_then(|(_, subs)| subs.first()).map(|s| s.id));
    book.entries.iter().filter(|e| Some(e.subcategory) == sub).collect()
}

fn handbook_page<'a>(book: &'a fc3_formats::progression::Handbook, view: &HandbookView) -> Vec<&'a fc3_formats::progression::HandbookEntry> {
    handbook_entries(book, view).into_iter().skip(view.page * HANDBOOK_ROWS).take(HANDBOOK_ROWS).collect()
}

/// A button per subcategory with pages, under its category's name.
fn build_handbook(book: Res<HandbookData>, lists: Query<Entity, With<HandbookTopics>>, mut built: Local<bool>, mut commands: Commands) {
    let Some(b) = book.0.as_ref().filter(|_| !*built) else { return };
    let Ok(list) = lists.single() else { return };
    *built = true;
    for (c, subs) in handbook_topics(b) {
        commands.spawn((Text::new(c.name.to_uppercase()), font(16.0), TextColor(ACCENT), Node { margin: UiRect::top(Val::Px(6.0)), ..default() }, ChildOf(list)));
        for s in subs {
            commands.spawn((
                Button,
                Node { width: Val::Px(250.0), height: Val::Px(24.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(8.0)), flex_shrink: 0.0, ..default() },
                BackgroundColor(IDLE),
                JournalButton::HandbookSub(s.id),
                ChildOf(list),
                children![(Text::new(s.name.clone()), font(14.0), TextColor(TEXT))],
            ));
        }
    }
}

/// Where the gear chains are listed (filled in from the crafting catalog).
#[derive(Component)]
struct GearList;

/// A button per gear chain, offering its next tier.
fn build_gear_list(crafting: Res<crate::crafting::CraftCatalog>, lists: Query<Entity, With<GearList>>, mut built: Local<bool>, mut commands: Commands) {
    if *built || crafting.recipes.is_empty() {
        return;
    }
    let Ok(list) = lists.single() else { return };
    *built = true;
    commands.spawn((Text::new("GEAR"), font(20.0), TextColor(ACCENT), ChildOf(list)));
    for i in 0..crafting.gear_chains().len() {
        commands.spawn((
            Button,
            Node { width: Val::Px(440.0), min_height: Val::Px(46.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
            BackgroundColor(IDLE),
            JournalButton::Craft(i),
            ChildOf(list),
            children![(Text::new(""), font(14.0), TextColor(TEXT), JournalText(JournalField::Craft(i)))],
        ));
    }
}

/// Where the syringes are listed (filled in from the crafting catalog).
#[derive(Component)]
struct PotionList;

/// The syringe recipes the game plays, in menu order.
fn potion_recipes(crafting: &crate::crafting::CraftCatalog) -> Vec<&fc3_formats::progression::Recipe> {
    crafting.recipes.iter().filter(|r| crate::syringes::Syringe::of(&r.id).is_some()).collect()
}

/// A row per syringe: make one (its plants), or inject one on hand.
fn build_potion_list(crafting: Res<crate::crafting::CraftCatalog>, lists: Query<Entity, With<PotionList>>, mut built: Local<bool>, mut commands: Commands) {
    if *built || crafting.recipes.is_empty() {
        return;
    }
    let Ok(list) = lists.single() else { return };
    *built = true;
    commands.spawn((Text::new("SYRINGES"), font(20.0), TextColor(ACCENT), ChildOf(list)));
    for i in 0..potion_recipes(&crafting).len() {
        let row = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(6.0), ..default() }, ChildOf(list))).id();
        commands.spawn((
            Button,
            Node { width: Val::Px(400.0), min_height: Val::Px(42.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
            BackgroundColor(IDLE),
            JournalButton::Syringe(i),
            ChildOf(row),
            children![(Text::new(""), font(13.0), TextColor(TEXT), JournalText(JournalField::Syringe(i)))],
        ));
        commands.spawn((
            Button,
            Node { width: Val::Px(64.0), min_height: Val::Px(42.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, flex_shrink: 0.0, ..default() },
            BackgroundColor(IDLE),
            JournalButton::Inject(i),
            ChildOf(row),
            children![(Text::new("USE"), font(13.0), TextColor(TEXT))],
        ));
    }
    if crafting.arrow_recipes().is_empty() {
        return;
    }
    commands.spawn((Text::new("ARROWS"), font(20.0), TextColor(ACCENT), ChildOf(list)));
    for i in 0..crafting.arrow_recipes().len() {
        commands.spawn((
            Button,
            Node { width: Val::Px(470.0), min_height: Val::Px(42.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
            BackgroundColor(IDLE),
            JournalButton::Arrow(i),
            ChildOf(list),
            children![(Text::new(""), font(13.0), TextColor(TEXT), JournalText(JournalField::Arrow(i)))],
        ));
    }
}

/// The scripted takedown test plays with the Takedown learned.
fn test_skills(args: Res<crate::Args>, mut progress: ResMut<Progress>, mut done: Local<bool>) {
    if !*done && args.takedown_test {
        *done = true;
        progress.skills.insert("SP_Takedown".into());
    }
}

/// The story autopilot spends its skill points as a player would, on what
/// can be learned now: the Takedown first (JAS_190 takes it away for a
/// while and counts on its being there), then the cheapest, in the tree's
/// order.
fn autopilot_skills(skills: Res<Skills>, mut progress: ResMut<Progress>) {
    if progress.skill_points == 0 || std::env::var_os("FC3_STORY_AUTOPILOT").is_none() {
        return;
    }
    let Some(a) = autopilot_pick(&progress, &skills) else { return };
    info!("story autopilot: learned {}", a.id);
    progress.skill_points -= a.cost;
    progress.skills.insert(a.id.clone());
}

fn autopilot_pick<'a>(progress: &Progress, skills: &'a Skills) -> Option<&'a fc3_formats::progression::Ability> {
    skills.0.abilities.iter().filter(|a| skill_state(progress, a) == SkillState::Ready).min_by_key(|a| (a.id != "SP_Takedown", a.cost))
}

/// Statistics counted up as they happen.
fn count_stats(mut gains: MessageReader<StatGain>, mut crafted: MessageReader<crate::crafting::ItemCrafted>, mut progress: ResMut<Progress>) {
    for g in gains.read() {
        *progress.stats.entry(g.stat).or_insert(0) += g.amount;
    }
    for c in crafted.read() {
        if c.kind == fc3_formats::progression::KIND_MEDKIT {
            *progress.stats.entry(stat::MEDICINE_CRAFTED).or_insert(0) += 1;
        }
    }
}

// ------------------------------------------------------------------ shop ---

#[derive(Clone, Copy)]
enum ShopItem {
    AmmoRefill,
    Medkit,
    Grenade,
    Molotov,
    Rockets,
    C4,
    Mine,
    /// Gear kept for good (an upgrade id).
    Gear(&'static str),
    Weapon(usize),
    /// An attachment for a weapon kind.
    Attachment(usize, crate::weapons::Attachment),
    /// A paint job for a weapon kind (`weapons::PAINT_NAMES` index).
    Paint(usize, u8),
    /// Five special arrows (`weapons::ARROW_NAMES` index).
    Arrows(u8),
    /// A supply from the game's shop database: its item id and price.
    Supply(u32, u32),
    /// A treasure map: its id and price (`treasure_maps`).
    Map(u32, u32),
    /// Selling every loot item carried, or one kind (its id).
    SellAll,
    Sell(u32),
}

/// What a supply in the game's shop database does (by its item key).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Supply {
    RefillAll,
    Ammo(crate::weapons::WeaponClass),
    Grenade,
    Molotov,
    C4,
    Mine,
    Armour(f32),
    Medkit,
}

pub(crate) fn supply(item: &fc3_formats::progression::Item) -> Option<Supply> {
    use crate::weapons::WeaponClass as W;
    if item.armour > 0.0 {
        return Some(Supply::Armour(item.armour));
    }
    match item.gadget {
        1 => return Some(Supply::Grenade),
        2 => return Some(Supply::Molotov),
        _ => {}
    }
    let name = item.key.rsplit('/').next()?.to_ascii_lowercase();
    Some(match name.as_str() {
        "refill all" => Supply::RefillAll,
        "handgun" => Supply::Ammo(W::Pistol),
        "smg" => Supply::Ammo(W::Smg),
        "shotgun" => Supply::Ammo(W::Shotgun),
        "assault rifle" => Supply::Ammo(W::Rifle),
        "sniper rifle" => Supply::Ammo(W::Sniper),
        "lmg" => Supply::Ammo(W::Lmg),
        "rpg-7" => Supply::Ammo(W::Launcher),
        "gl-94" => Supply::Ammo(W::GrenadeLauncher),
        "flamethrower" => Supply::Ammo(W::Flamethrower),
        "bow" => Supply::Ammo(W::Bow),
        "c4" => Supply::C4,
        "mine" => Supply::Mine,
        "small health syringe" => Supply::Medkit,
        _ => return None,
    })
}

/// The loot carried that sells, most valuable first, after selling it all.
fn sell_items(progress: &Progress, crafting: &crate::crafting::CraftCatalog) -> Vec<ShopItem> {
    let mut held: Vec<(u32, u32)> = progress.loot.iter().filter(|(id, n)| **n > 0 && crafting.item(**id).is_some_and(|i| i.cost > 0)).map(|(id, n)| (*id, *n)).collect();
    held.sort_by_key(|(id, n)| std::cmp::Reverse(crafting.item(*id).map_or(0, |i| i.cost) * n));
    let mut list = vec![ShopItem::SellAll];
    list.extend(held.into_iter().map(|(id, _)| ShopItem::Sell(id)));
    list.truncate(14);
    list
}

/// What selling a line pays.
fn sale_value(item: ShopItem, progress: &Progress, crafting: &crate::crafting::CraftCatalog, perks: &Perks) -> u32 {
    let stack = |id: u32| crafting.sale_price(id, perks) * progress.loot.get(&id).copied().unwrap_or(0);
    match item {
        ShopItem::SellAll => progress.loot.keys().map(|&id| stack(id)).sum(),
        ShopItem::Sell(id) => stack(id),
        _ => 0,
    }
}

/// Gives `n` of a supply (rounds of ammunition, or gadgets), as far as there
/// is room; returns how many were taken.
pub(crate) fn give_supply(
    item: &fc3_formats::progression::Item,
    n: u32,
    inv: &mut Inventory,
    stats: &mut PlayerStats,
    perks: &Perks,
    progress: &mut Progress,
) -> u32 {
    let add = |have: &mut u32, cap: u32| {
        let took = n.min(cap.saturating_sub(*have));
        *have += took;
        took
    };
    match supply(item) {
        Some(Supply::Ammo(class)) => inv.add_ammo(class, n),
        Some(Supply::Grenade) => add(&mut inv.grenades, perks.grenade_cap),
        Some(Supply::Molotov) => add(&mut inv.molotovs, crate::weapons::MOLOTOV_CAP),
        Some(Supply::C4) => add(&mut inv.c4, perks.explosive_cap),
        Some(Supply::Mine) => add(&mut inv.mines, perks.explosive_cap),
        Some(Supply::Medkit) => add(&mut stats.medkits, perks.medkit_cap),
        Some(Supply::Armour(value)) if progress.armour < value => {
            progress.armour = value;
            1
        }
        _ => 0,
    }
}

/// The supplies the shop sells: consumables, then ammunition, each in the
/// database's order; ammunition only for the guns carried.
fn supply_items(inv: &Inventory, supplies: &[fc3_formats::progression::Item], progress: &Progress) -> Vec<ShopItem> {
    use fc3_formats::progression::{SHOP_AMMO, SHOP_CONSUMABLES};
    let mut list: Vec<&fc3_formats::progression::Item> = supplies
        .iter()
        .filter(|i| i.shop == SHOP_AMMO || i.shop == SHOP_CONSUMABLES)
        // (In the story, what a script opens waits for it.)
        .filter(|i| i.unlock.is_empty() || !progress.story || progress.unlocks.contains(&i.unlock))
        .filter(|i| match supply(i) {
            Some(Supply::Ammo(class)) => inv.slots.iter().any(|s| inv.defs[s.def].class == class),
            Some(Supply::RefillAll) => true,
            Some(_) => i.cost > 0,
            None => false,
        })
        .collect();
    list.sort_by_key(|i| (i.shop != SHOP_CONSUMABLES, supply(i) != Some(Supply::RefillAll), i.sort, i.id));
    list.into_iter().map(|i| ShopItem::Supply(i.id, supply_price(inv, supplies, i))).collect()
}

/// A supply's price; refilling everything costs the ammunition it takes.
fn supply_price(inv: &Inventory, supplies: &[fc3_formats::progression::Item], item: &fc3_formats::progression::Item) -> u32 {
    if supply(item) != Some(Supply::RefillAll) {
        return item.cost;
    }
    inv.slots
        .iter()
        .filter_map(|s| {
            let d = &inv.defs[s.def];
            let missing = ((d.max_reserve as f32 * inv.reserve_scale) as u32).saturating_sub(s.reserve);
            let ammo = supplies.iter().find(|i| supply(i) == Some(Supply::Ammo(d.class)) && i.amount > 0)?;
            Some(missing.div_ceil(ammo.amount) * ammo.cost)
        })
        .sum()
}

impl ShopItem {
    /// Price, with the weapons freed by `towers` radio towers costing nothing.
    fn price(self, inv: &Inventory, towers: usize, catalog: &ShopCatalog) -> u32 {
        if let ShopItem::Weapon(d) = self {
            if let Some(weapon) = catalog.weapon(inv, d) {
                return if weapon.tower_requirement().is_some_and(|n| towers >= n) { 0 } else { weapon.cost };
            }
        }
        match self {
            ShopItem::AmmoRefill => 120,
            ShopItem::Medkit => 80,
            ShopItem::Grenade => 60,
            ShopItem::Molotov => 50,
            ShopItem::Rockets => 150,
            ShopItem::C4 => 250,
            ShopItem::Mine => 200,
            ShopItem::Gear(crate::freefall::WINGSUIT) => 1200,
            ShopItem::Gear(_) => 300,
            // (A sight bought before goes back on for nothing.)
            ShopItem::Attachment(d, a) if inv.owns(d, a) => 0,
            ShopItem::Attachment(d, a) => inv.defs.get(d).map(|d| a.price(d)).unwrap_or(0),
            ShopItem::Paint(..) => PAINT_PRICE,
            ShopItem::Arrows(1) => 150,
            ShopItem::Arrows(_) => 300,
            ShopItem::Supply(_, price) | ShopItem::Map(_, price) => price,
            // (Paid, not charged.)
            ShopItem::SellAll | ShopItem::Sell(_) => 0,
            ShopItem::Weapon(d) => match inv.defs.get(d).map(|d| d.class) {
                Some(crate::weapons::WeaponClass::Sniper) | Some(crate::weapons::WeaponClass::Lmg) => 1200,
                Some(crate::weapons::WeaponClass::Launcher) | Some(crate::weapons::WeaponClass::GrenadeLauncher) => 1500,
                Some(crate::weapons::WeaponClass::Flamethrower) => 1800,
                Some(crate::weapons::WeaponClass::Bow) => 600,
                Some(crate::weapons::WeaponClass::Rifle) | Some(crate::weapons::WeaponClass::Shotgun) => 800,
                _ => 400,
            },
        }
    }

    fn label(self, inv: &Inventory, supplies: &[fc3_formats::progression::Item]) -> String {
        match self {
            ShopItem::SellAll => "Sell all loot".into(),
            // (Named by `TreasureMaps::label`.)
            ShopItem::Map(..) => String::new(),
            ShopItem::Sell(id) => supplies.iter().find(|i| i.id == id).map(|i| i.name.clone()).unwrap_or_default(),
            ShopItem::Supply(id, _) => {
                let Some(item) = supplies.iter().find(|i| i.id == id) else { return String::new() };
                let name = if item.name.is_empty() { item.key.rsplit('/').next().unwrap_or_default() } else { &item.name };
                if item.amount > 1 {
                    format!("{name} x{}", item.amount)
                } else {
                    name.to_string()
                }
            }
            ShopItem::AmmoRefill => "Refill all ammo".into(),
            ShopItem::Medkit => "Medkit".into(),
            ShopItem::Grenade => "Grenade".into(),
            ShopItem::Molotov => "Molotov".into(),
            ShopItem::Rockets => "3 rockets".into(),
            ShopItem::C4 => "C4 charge".into(),
            ShopItem::Mine => "Proximity mine".into(),
            ShopItem::Gear(crate::freefall::WINGSUIT) => "Wingsuit".into(),
            ShopItem::Gear(_) => "Parachute".into(),
            ShopItem::Weapon(d) => inv.defs.get(d).map(|d| d.name.to_string()).unwrap_or_default(),
            ShopItem::Attachment(d, a) => inv.defs.get(d).map(|w| format!("{}: {}", w.name, a.name(w))).unwrap_or_default(),
            ShopItem::Paint(d, p) => inv.defs.get(d).map(|w| format!("{}: {} paint", w.name, crate::weapons::PAINT_NAMES[p as usize])).unwrap_or_default(),
            ShopItem::Arrows(k) => format!("5 {}s", crate::weapons::ARROW_NAMES[k as usize].to_lowercase()),
        }
    }
}

#[derive(Component)]
struct ShopRoot;

#[derive(Component, Clone, Copy)]
enum ShopButton {
    Tab(usize),
    Buy(usize),
    Close,
}

/// Shop categories (tab 0 is supplies).
const SHOP_TABS: [&str; 12] = ["SUPPLIES", "HANDGUNS", "SMG", "RIFLES", "SHOTGUNS", "SNIPER", "HEAVY", "BOWS", "ATTACHMENTS", "PAINT", "MAPS", "SELL"];
/// Treasure maps, and selling loot.
const MAPS_TAB: usize = 10;
const SELL_TAB: usize = 11;
/// The attachments and paint tabs.
const ATTACHMENTS_TAB: usize = 8;
const PAINT_TAB: usize = 9;
/// A paint job's price.
const PAINT_PRICE: u32 = 500;

/// The open shop tab.
#[derive(Resource, Default)]
struct ShopTab(usize);

/// Authored weapon stock, including the ids used by the first-shop tutorial.
#[derive(Resource, Default)]
pub struct ShopCatalog(pub Vec<fc3_formats::shop::Weapon>);

impl ShopCatalog {
    /// Supported base weapons freed by this particular tower, not an
    /// invented one-weapon-per-tower reward list.
    pub fn freed_weapons<'a>(&self, defs: &'a [crate::weapons::WeaponDef], towers: usize) -> Vec<&'a str> {
        self.0.iter().filter(|w| w.is_base() && w.tower_requirement() == Some(towers))
            .filter_map(|w| defs.iter().find(|d| d.model_path().eq_ignore_ascii_case(&w.model)).map(|d| d.name))
            .collect()
    }

    fn weapon(&self, inv: &Inventory, def: usize) -> Option<&fc3_formats::shop::Weapon> {
        let model = inv.defs.get(def)?.model_path();
        self.0.iter().find(|w| w.is_base() && w.model.eq_ignore_ascii_case(&model))
    }

    fn selected_weapon(&self, inv: &Inventory, selection: (u32, u32)) -> Option<usize> {
        let weapon = self.0.iter().find(|w| w.is_base() && (w.category, w.id) == selection)?;
        inv.defs.iter().position(|d| d.model_path().eq_ignore_ascii_case(&weapon.model))
    }
}

/// Restricts a tutorial shop to the item the mission requested. It does not
/// purchase the item or open the shop without the player's interaction.
#[derive(Resource, Default)]
pub struct ShopOverride(pub Option<(u32, u32)>);

impl ShopOverride {
    pub fn set(&mut self, active: bool, category: u32, item: u32) {
        self.0 = active.then_some((category, item));
    }
}

fn load_shop_catalog(info: Res<crate::data::CacheInfo>, mut catalog: ResMut<ShopCatalog>) {
    match std::fs::read_to_string(info.root.join(fc3_formats::shop::WEAPONS_REL))
        .map_err(anyhow::Error::from).and_then(|t| fc3_formats::shop::from_text(&t)) {
        Ok(stock) => {
            info!("shop: {} authored weapon entries", stock.len());
            catalog.0 = stock;
        }
        Err(e) => warn!("shop: cannot read weapon catalog: {e:#}"),
    }
}

/// Uses the same priced weapon transaction for the UI and campaign smoke
/// tests. A purchase event must describe a gun actually added to inventory.
pub fn purchase_tutorial_weapon(inv: &mut Inventory, progress: &mut Progress, perks: &Perks, catalog: &ShopCatalog, restriction: &ShopOverride) -> Option<ItemBought> {
    let def = catalog.selected_weapon(inv, restriction.0?)?;
    purchase_weapon(inv, progress, perks, catalog, def).map(|(bought, _)| bought)
}

fn purchase_weapon(inv: &mut Inventory, progress: &mut Progress, perks: &Perks, catalog: &ShopCatalog, def: usize) -> Option<(ItemBought, crate::weapons::Granted)> {
    let price = ShopItem::Weapon(def).price(inv, progress.towers.len(), catalog);
    if progress.money < price || inv.slots.iter().any(|s| s.def == def) { return None }
    let name = inv.defs.get(def)?.name;
    let granted = crate::weapons::grant_weapon(inv, name, perks.weapon_slots);
    match granted {
        crate::weapons::Granted::New | crate::weapons::Granted::Swapped(_) => {
            progress.spend(price);
            info!("shop: purchased {name} for ${price}, balance ${}", progress.money);
            Some((ItemBought { model: Some(inv.defs[def].model_path()) }, granted))
        }
        crate::weapons::Granted::Ammo => None,
    }
}

fn shop_tab(class: crate::weapons::WeaponClass) -> usize {
    use crate::weapons::WeaponClass::*;
    match class {
        Melee => 0,
        Pistol => 1,
        Smg => 2,
        Rifle => 3,
        Shotgun => 4,
        Sniper => 5,
        Lmg | Launcher | GrenadeLauncher | Flamethrower => 6,
        Bow => 7,
        // Never sold.
        Mounted => usize::MAX,
    }
}

#[derive(Component)]
struct ShopLine(usize);

/// A shop line's price (right-aligned).
#[derive(Component)]
struct ShopPrice(usize);

#[derive(Component)]
struct ShopHeader;

/// The treasure maps for sale (as many as the tab shows).
fn map_items(maps: &crate::treasure_maps::TreasureMaps) -> Vec<ShopItem> {
    maps.for_sale.iter().map(|&(id, cost)| ShopItem::Map(id, cost)).take(14).collect()
}

/// Shop stock in a tab: supplies, or the weapons of one kind (owned ones
/// show as owned).
fn shop_items(
    inv: &Inventory,
    tab: usize,
    catalog: &ShopCatalog,
    restriction: &ShopOverride,
    supplies: &[fc3_formats::progression::Item],
    progress: &Progress,
) -> Vec<ShopItem> {
    if let Some(selection) = restriction.0 {
        return catalog.selected_weapon(inv, selection).map(ShopItem::Weapon).into_iter().collect();
    }
    if tab == SELL_TAB {
        // (Filled in by `sell_items`, which knows what is carried.)
        return Vec::new();
    }
    // The game's own supplies (and the gear the story doesn't hand out here).
    let from_data = supply_items(inv, supplies, progress);
    if tab == 0 && !from_data.is_empty() {
        let mut list = from_data;
        // (Special arrows are crafted from grenades and molotovs.)
        list.extend([ShopItem::Gear(crate::freefall::PARACHUTE), ShopItem::Gear(crate::freefall::WINGSUIT)]);
        list.truncate(14);
        return list;
    }
    if tab == 0 {
        return vec![
            ShopItem::AmmoRefill,
            ShopItem::Medkit,
            ShopItem::Grenade,
            ShopItem::Molotov,
            ShopItem::Rockets,
            ShopItem::C4,
            ShopItem::Mine,
            ShopItem::Gear(crate::freefall::PARACHUTE),
            ShopItem::Gear(crate::freefall::WINGSUIT),
            ShopItem::Arrows(1),
            ShopItem::Arrows(2),
        ];
    }
    // Paints for the guns carried, the one in hand first.
    if tab == PAINT_TAB {
        let mut guns: Vec<usize> = inv.slots.iter().map(|s| s.def).collect();
        guns.sort_by_key(|&d| d != inv.slot().def);
        guns.dedup();
        return guns
            .into_iter()
            .flat_map(|d| {
                let bits = inv.paint_options.get(d).copied().unwrap_or(0);
                (0..crate::weapons::PAINT_NAMES.len() as u8).filter(move |p| bits & (1 << p) != 0).map(move |p| ShopItem::Paint(d, p))
            })
            .take(14)
            .collect();
    }
    // Attachments for the guns carried, the one in hand first.
    if tab == ATTACHMENTS_TAB {
        let mut guns: Vec<usize> = inv.slots.iter().map(|s| s.def).collect();
        guns.sort_by_key(|&d| d != inv.slot().def);
        return guns
            .into_iter()
            .flat_map(|d| crate::weapons::Attachment::ALL.into_iter().filter(move |a| a.fits(&inv.defs[d])).map(move |a| ShopItem::Attachment(d, a)))
            .take(14)
            .collect();
    }
    (0..inv.defs.len())
        .filter(|&d| inv.defs[d].class != crate::weapons::WeaponClass::Melee && shop_tab(inv.defs[d].class) == tab)
        .map(ShopItem::Weapon)
        .collect()
}

fn setup_shop(mut commands: Commands) {
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: Val::Px(6.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.85)),
            Visibility::Hidden,
            GlobalZIndex(11),
            ShopRoot,
        ))
        .id();
    commands.spawn((Text::new("SHOP"), font(36.0), TextColor(TEXT), ChildOf(root)));
    commands.spawn((Text::new(""), font(18.0), TextColor(ACCENT), ShopHeader, ChildOf(root)));
    let tabs = commands
        .spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), margin: UiRect::vertical(Val::Px(6.0)), ..default() }, ChildOf(root)))
        .id();
    for (i, name) in SHOP_TABS.iter().enumerate() {
        let b = commands
            .spawn((Button, Node { padding: UiRect::axes(Val::Px(7.0), Val::Px(6.0)), ..default() }, BackgroundColor(IDLE), ShopButton::Tab(i), ChildOf(tabs)))
            .id();
        commands.spawn((Text::new(*name), font(13.0), TextColor(TEXT), ChildOf(b)));
    }
    for i in 0..14 {
        let b = commands
            .spawn((
                Button,
                Node {
                    width: Val::Px(560.0),
                    height: Val::Px(30.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::horizontal(Val::Px(12.0)),
                    flex_shrink: 0.0,
                    ..default()
                },
                BackgroundColor(IDLE),
                ShopButton::Buy(i),
                ChildOf(root),
            ))
            .id();
        commands.spawn((Text::new(""), font(16.0), TextColor(TEXT), ShopLine(i), ChildOf(b)));
        commands.spawn((Text::new(""), font(16.0), TextColor(TEXT), ShopPrice(i), ChildOf(b)));
    }
    let close = commands
        .spawn((
            Button,
            Node {
                width: Val::Px(200.0),
                height: Val::Px(36.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::top(Val::Px(10.0)),
                ..default()
            },
            BackgroundColor(IDLE),
            ShopButton::Close,
            ChildOf(root),
        ))
        .id();
    commands.spawn((Text::new("CLOSE"), font(17.0), TextColor(TEXT), ChildOf(close)));
}

/// Something bought at the shop: a weapon's model path (the story hears of
/// it as the game's item).
#[derive(Message, Clone)]
pub struct ItemBought {
    pub model: Option<String>,
}

/// Where the world's shopkeepers stand (Bevy space): each keeps a shop.
#[derive(Resource, Default)]
pub struct Shops(pub Vec<Vec3>);

/// Shopkeepers as their sectors load.
/// Unspent skill points brought up now and then, as often as the game's
/// HUD reminds of them.
fn remind_skill_points(
    time: Res<Time>,
    mut waited: Local<f32>,
    params: Res<crate::map::HudParams>,
    progress: Res<Progress>,
    mut hud: ResMut<crate::hud::HudState>,
) {
    let every = params.0.skill_reminder;
    if every <= 0.0 || progress.skill_points == 0 {
        *waited = 0.0;
        return;
    }
    *waited += time.delta_secs();
    if *waited >= every {
        *waited = 0.0;
        let n = progress.skill_points;
        hud.notify(format!("{n} skill point{} to spend (Tab)", if n == 1 { "" } else { "s" }));
    }
}

fn find_shops(
    sectors: Res<Assets<crate::cache_assets::Fc3Sector>>,
    added: Query<&crate::objects::SectorObjects, Added<crate::objects::SectorObjects>>,
    mut shops: ResMut<Shops>,
) {
    for so in &added {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        for e in sector.entities.iter().filter(|e| e.name.to_ascii_lowercase().contains("shopkeeper")) {
            let at = Vec3::from_array(e.translation);
            if !shops.0.iter().any(|s| s.distance(at) < 1.0) {
                shops.0.push(at);
            }
        }
    }
}

/// Liberated outposts have a shop at their centre; shopkeepers keep one
/// where they stand.
#[allow(clippy::too_many_arguments)]
fn shop_prompt(
    args: Res<crate::Args>,
    mut opened: Local<bool>,
    mut tab: ResMut<ShopTab>,
    state: Res<State<GameState>>,
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    outposts: Res<crate::outposts::Outposts>,
    players: Query<(&Transform, &Health, &Inventory), With<Player>>,
    mut hud: ResMut<HudState>,
    mut next: ResMut<NextState<GameState>>,
    shops: Res<Shops>,
    catalog: Res<ShopCatalog>,
    restriction: Res<ShopOverride>,
) {
    if *state.get() != GameState::Playing || hud.prompt.is_some() {
        return;
    }
    // --shop <tab> (tests): open once in play.
    if let (Some(t), false) = (args.open_shop, *opened) {
        *opened = true;
        tab.0 = t.min(SHOP_TABS.len() - 1);
        next.set(GameState::Shop);
        return;
    }
    let Ok((t, h, inv)) = players.single() else { return };
    if h.dead {
        return;
    }
    let near = outposts.list.iter().any(|o| o.liberated && Vec2::new(t.translation.x - o.centre.x, t.translation.z - o.centre.z).length() < 10.0)
        || shops.0.iter().any(|s| s.distance(t.translation) < 3.5);
    if !near {
        return;
    }
    hud.prompt = Some(format!("{}  shop", actions.label(crate::controls::Action::Interact, &settings)));
    if actions.just_pressed(crate::controls::Action::Interact) {
        if let Some(def) = restriction.0.and_then(|s| catalog.selected_weapon(inv, s)) {
            tab.0 = shop_tab(inv.defs[def].class);
        }
        next.set(GameState::Shop);
    }
}

#[allow(clippy::type_complexity)]
fn shop_buttons(
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut buttons: Query<(&Interaction, &ShopButton, &mut BackgroundColor), Changed<Interaction>>,
    mut progress: ResMut<Progress>,
    perks: Res<Perks>,
    mut players: Query<(&mut Inventory, &mut PlayerStats), With<Player>>,
    mut hud: ResMut<HudState>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut tab: ResMut<ShopTab>,
    mut bought_items: MessageWriter<ItemBought>,
    (catalog, restriction, crafting, maps): (Res<ShopCatalog>, Res<ShopOverride>, Res<crate::crafting::CraftCatalog>, Res<crate::treasure_maps::TreasureMaps>),
) {
    if *state.get() != GameState::Shop {
        return;
    }
    let Ok((mut inv, mut stats)) = players.single_mut() else { return };
    let items = match tab.0 {
        SELL_TAB if restriction.0.is_none() => sell_items(&progress, &crafting),
        MAPS_TAB if restriction.0.is_none() => map_items(&maps),
        _ => shop_items(&inv, tab.0, &catalog, &restriction, &crafting.items, &progress),
    };
    for (interaction, kind, mut bg) in &mut buttons {
        match interaction {
            Interaction::Pressed => {
                sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.7 });
                let i = match *kind {
                    ShopButton::Buy(i) => i,
                    ShopButton::Tab(t) => {
                        if restriction.0.is_none() { tab.0 = t; }
                        continue;
                    }
                    ShopButton::Close => {
                        next.set(GameState::Playing);
                        continue;
                    }
                };
                let Some(&item) = items.get(i) else { continue };
                if matches!(item, ShopItem::SellAll | ShopItem::Sell(_)) {
                    let value = sale_value(item, &progress, &crafting, &perks);
                    let sold: Vec<u32> = match item {
                        ShopItem::Sell(id) => vec![id],
                        _ => progress.loot.keys().copied().filter(|&id| crafting.sale_price(id, &perks) > 0).collect(),
                    };
                    if sold.is_empty() || value == 0 {
                        hud.notify("Nothing to sell");
                        continue;
                    }
                    for id in sold {
                        progress.loot.remove(&id);
                    }
                    let paid = progress.add_money(value);
                    hud.notify(&if paid < value { format!("Sold for ${value} (wallet full: ${paid} kept)") } else { format!("Sold for ${value}") });
                    continue;
                }
                let price = item.price(&inv, progress.towers.len(), &catalog);
                if progress.money < price {
                    hud.notify("Not enough money");
                    continue;
                }
                if let ShopItem::Weapon(def) = item {
                    if let Some((bought, granted)) = purchase_weapon(&mut inv, &mut progress, &perks, &catalog, def) {
                        if let crate::weapons::Granted::Swapped(old) = granted {
                            hud.notify(&format!("Swapped the {old} for the {}", inv.defs[def].name));
                        }
                        bought_items.write(bought);
                    } else {
                        hud.notify("Already owned");
                    }
                    continue;
                }
                let bought = match item {
                    ShopItem::AmmoRefill => {
                        let mut any = false;
                        let scale = inv.reserve_scale;
                        let defs = inv.defs.clone();
                        for s in &mut inv.slots {
                            let cap = (defs[s.def].max_reserve as f32 * scale) as u32;
                            if s.reserve < cap {
                                s.reserve = cap;
                                any = true;
                            }
                        }
                        any
                    }
                    ShopItem::Medkit if stats.medkits < perks.medkit_cap => {
                        stats.medkits += 1;
                        true
                    }
                    ShopItem::Grenade if inv.grenades < perks.grenade_cap => {
                        inv.grenades += 1;
                        true
                    }
                    ShopItem::Molotov if inv.molotovs < crate::weapons::MOLOTOV_CAP => {
                        inv.molotovs += 1;
                        true
                    }
                    ShopItem::Rockets => inv.add_ammo(crate::weapons::WeaponClass::Launcher, 3) > 0,
                    ShopItem::C4 if inv.c4 < perks.explosive_cap => {
                        inv.c4 += 1;
                        true
                    }
                    ShopItem::Mine if inv.mines < perks.explosive_cap => {
                        inv.mines += 1;
                        true
                    }
                    ShopItem::Attachment(d, a) => inv.attach(d, a),
                    ShopItem::Arrows(k) if inv.special_arrows[k as usize - 1] < perks.arrow_cap => {
                        let n = &mut inv.special_arrows[k as usize - 1];
                        *n = (*n + 5).min(perks.arrow_cap);
                        true
                    }
                    ShopItem::Paint(d, p) => {
                        let fresh = inv.paints.get(d).copied().flatten() != Some(p);
                        if fresh {
                            if inv.paints.len() <= d {
                                inv.paints.resize(d + 1, None);
                            }
                            inv.paints[d] = Some(p);
                        }
                        fresh
                    }
                    ShopItem::Map(id, _) => {
                        let fresh = progress.maps.insert(id);
                        if fresh {
                            hud.notify("Map bought: its finds are on the world map");
                        }
                        fresh
                    }
                    ShopItem::Gear(id) if progress.upgrade(id) == 0 => {
                        progress.upgrades.insert(id.to_string(), 1);
                        true
                    }
                    ShopItem::Supply(id, _) => {
                        let item = crafting.items.iter().find(|i| i.id == id);
                        match item.and_then(supply) {
                            Some(Supply::RefillAll) => {
                                let scale = inv.reserve_scale;
                                let defs = inv.defs.clone();
                                let mut any = false;
                                for s in &mut inv.slots {
                                    let cap = (defs[s.def].max_reserve as f32 * scale) as u32;
                                    any |= s.reserve < cap;
                                    s.reserve = s.reserve.max(cap);
                                }
                                any
                            }
                            Some(Supply::Ammo(class)) => inv.add_ammo(class, item.map_or(0, |i| i.amount)) > 0,
                            Some(Supply::Grenade) if inv.grenades < perks.grenade_cap => {
                                inv.grenades += 1;
                                true
                            }
                            Some(Supply::Molotov) if inv.molotovs < crate::weapons::MOLOTOV_CAP => {
                                inv.molotovs += 1;
                                true
                            }
                            Some(Supply::C4) if inv.c4 < perks.explosive_cap => {
                                inv.c4 += 1;
                                true
                            }
                            Some(Supply::Mine) if inv.mines < perks.explosive_cap => {
                                inv.mines += 1;
                                true
                            }
                            Some(Supply::Medkit) if stats.medkits < perks.medkit_cap => {
                                stats.medkits += 1;
                                true
                            }
                            // Fresh armour, counted for the skill tree.
                            Some(Supply::Armour(value)) if progress.armour < value => {
                                progress.armour = value;
                                *progress.stats.entry(stat::ARMOR_BOUGHT).or_insert(0) += 1;
                                true
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if bought {
                    progress.spend(price);
                    if let ShopItem::Attachment(..) = item {
                        *progress.stats.entry(stat::ATTACHMENTS_BOUGHT).or_insert(0) += 1;
                    }
                    let model = match item {
                        ShopItem::Weapon(d) => Some(inv.defs[d].model_path()),
                        _ => None,
                    };
                    bought_items.write(ItemBought { model });
                } else {
                    hud.notify("Already full");
                }
            }
            Interaction::Hovered => bg.0 = HOVER,
            Interaction::None => bg.0 = IDLE,
        }
    }
}

#[allow(clippy::type_complexity)]
fn refresh_shop(
    state: Res<State<GameState>>,
    progress: Res<Progress>,
    players: Query<&Inventory, With<Player>>,
    mut root: Query<&mut Visibility, With<ShopRoot>>,
    mut header: Query<&mut Text, (With<ShopHeader>, Without<ShopLine>)>,
    mut lines: Query<(&ShopLine, &mut Text, &mut TextColor, &ChildOf), (Without<ShopHeader>, Without<ShopPrice>)>,
    mut prices: Query<(&ShopPrice, &mut Text, &mut TextColor), (Without<ShopHeader>, Without<ShopLine>)>,
    mut buttons: Query<&mut Node, With<ShopButton>>,
    mut tabs: Query<(&ShopButton, &Interaction, &mut BackgroundColor)>,
    tab: Res<ShopTab>,
    perks: Res<Perks>,
    (catalog, restriction, crafting, maps): (Res<ShopCatalog>, Res<ShopOverride>, Res<crate::crafting::CraftCatalog>, Res<crate::treasure_maps::TreasureMaps>),
) {
    let open = *state.get() == GameState::Shop;
    if let Ok(mut v) = root.single_mut() {
        *v = if open { Visibility::Inherited } else { Visibility::Hidden };
    }
    if !open {
        return;
    }
    let Ok(inv) = players.single() else { return };
    let items = match tab.0 {
        SELL_TAB if restriction.0.is_none() => sell_items(&progress, &crafting),
        MAPS_TAB if restriction.0.is_none() => map_items(&maps),
        _ => shop_items(inv, tab.0, &catalog, &restriction, &crafting.items, &progress),
    };
    if let Ok(mut t) = header.single_mut() {
        let guns = inv.slots.iter().filter(|s| inv.defs[s.def].class != crate::weapons::WeaponClass::Melee).count();
        t.0 = format!("${} / {}      weapons carried {} / {}", progress.money, progress.money_cap(), guns, perks.weapon_slots);
    }
    for (b, interaction, mut bg) in &mut tabs {
        if let ShopButton::Tab(i) = b {
            if *interaction == Interaction::None {
                bg.0 = if restriction.0.is_some() { Color::srgba(0.15, 0.15, 0.15, 0.7) }
                    else if *i == tab.0 { Color::srgba(0.85, 0.55, 0.15, 0.9) } else { IDLE };
            }
        }
    }
    for (line, mut t, mut c, parent) in &mut lines {
        let item = items.get(line.0);
        if let Ok(mut node) = buttons.get_mut(parent.parent()) {
            node.display = if item.is_some() { Display::Flex } else { Display::None };
        }
        let Some(&item) = item else { continue };
        let owned = match item {
            ShopItem::Weapon(d) => inv.slots.iter().any(|s| s.def == d),
            ShopItem::Gear(id) => progress.upgrade(id) > 0,
            ShopItem::Attachment(d, a) => inv.has_attachment(d, a),
            ShopItem::Paint(d, p) => inv.paints.get(d).copied().flatten() == Some(p),
            _ => false,
        };
        let price = item.price(inv, progress.towers.len(), &catalog);
        t.0 = match item {
            ShopItem::Map(id, _) => maps.label(id),
            _ => item.label(inv, &crafting.items),
        };
        if let ShopItem::Sell(id) = item {
            t.0 = format!("{}  x{}", t.0, progress.loot.get(&id).copied().unwrap_or(0));
        }
        let selling = matches!(item, ShopItem::SellAll | ShopItem::Sell(_));
        c.0 = if !selling && (owned || progress.money < price) { Color::srgba(0.8, 0.8, 0.8, 0.5) } else { TEXT };
    }
    for (line, mut t, mut c) in &mut prices {
        let Some(&item) = items.get(line.0) else { continue };
        let owned = match item {
            ShopItem::Weapon(d) => inv.slots.iter().any(|s| s.def == d),
            ShopItem::Gear(id) => progress.upgrade(id) > 0,
            ShopItem::Attachment(d, a) => inv.has_attachment(d, a),
            ShopItem::Paint(d, p) => inv.paints.get(d).copied().flatten() == Some(p),
            _ => false,
        };
        if matches!(item, ShopItem::SellAll | ShopItem::Sell(_)) {
            t.0 = format!("+${}", sale_value(item, &progress, &crafting, &perks));
            c.0 = ACCENT;
            continue;
        }
        let price = item.price(inv, progress.towers.len(), &catalog);
        let swap = matches!(item, ShopItem::Attachment(d, a) if inv.owns(d, a));
        t.0 = if owned {
            "fitted".into()
        } else if swap {
            "fit".into()
        } else {
            format!("${price}")
        };
        c.0 = if owned || progress.money < price { Color::srgba(0.8, 0.8, 0.8, 0.5) } else { ACCENT };
    }
}

#[cfg(test)]
mod skill_tests {
    use super::*;
    use fc3_formats::progression::{Ability, Bonus, SkillTree};

    fn ability(id: &str, requires: &[&str], mission: &str) -> Ability {
        Ability { id: id.into(), branch: 1, cost: 1, requires: requires.iter().map(|r| r.to_string()).collect(), purchasable: true, available: true, unlock_mission: mission.into(), ..Default::default() }
    }

    #[test]
    fn a_granted_bonus_plan_lends_its_ability() {
        let mut takedown = ability("SP_Takedown", &[], "");
        takedown.bonuses = vec![Bonus { plan: "standardtakedown".into(), attribute: 102, value: 1.0, additive: true }];
        let skills = Skills(SkillTree { abilities: vec![takedown], ..Default::default() });
        let mut p = Progress::default();
        assert!(!Perks::from_progress(&p, &skills).takedown);
        p.bonus_plans.insert("standardtakedown".into());
        assert!(Perks::from_progress(&p, &skills).takedown);
        assert!(!p.has("SP_Takedown"));
    }

    #[test]
    fn the_autopilot_learns_the_takedown_first() {
        let skills = Skills(SkillTree { abilities: vec![ability("SP_A", &[], ""), ability("SP_Takedown", &[], "")], ..Default::default() });
        let p = Progress { skill_points: 2, ..Progress::default() };
        assert_eq!(autopilot_pick(&p, &skills).map(|a| a.id.as_str()), Some("SP_Takedown"));
        let mut p = p;
        p.skills.insert("SP_Takedown".into());
        assert_eq!(autopilot_pick(&p, &skills).map(|a| a.id.as_str()), Some("SP_A"));
    }

    #[test]
    fn a_removed_bonus_plan_takes_its_ability_away_for_a_while() {
        let mut takedown = ability("SP_Takedown", &[], "");
        takedown.bonuses = vec![Bonus { plan: "StandardTakedown".into(), attribute: 102, value: 1.0, additive: true }];
        let skills = Skills(SkillTree { abilities: vec![takedown], ..Default::default() });
        let mut p = Progress::default();
        p.skills.insert("SP_Takedown".into());
        let perks = Perks::from_progress(&p, &skills);
        assert!(perks.takedown && perks.plans.contains("standardtakedown"));
        p.removed_plans.insert("standardtakedown".into());
        let perks = Perks::from_progress(&p, &skills);
        assert!(!perks.takedown && !perks.plans.contains("standardtakedown"));
        // Kept over a save.
        let mut back = Progress::default();
        for line in p.to_lines().lines() {
            let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
            back.read_line(key, rest);
        }
        assert!(back.removed_plans.contains("standardtakedown"));
    }

    #[test]
    fn handbook_pages_open_by_start_mission_group_or_encounter() {
        use fc3_formats::progression::HandbookEntry;
        let page = |at_start: bool, mission: &str, group: &str| HandbookEntry {
            at_start,
            unlock_mission: mission.into(),
            unlock_group: group.into(),
            ..Default::default()
        };
        let mut p = Progress { story: true, ..Default::default() };
        assert!(handbook_open(&page(true, "", ""), &p));
        assert!(handbook_open(&page(false, "", ""), &p));
        assert!(!handbook_open(&page(false, "JAS_020_main", ""), &p));
        assert!(!handbook_open(&page(false, "", "CHAR_X"), &p));
        p.missions.insert("jas_020_main".into());
        p.unlocks.insert("CHAR_X".into());
        assert!(handbook_open(&page(false, "JAS_020_main", ""), &p));
        assert!(handbook_open(&page(false, "", "CHAR_X"), &p));
        // Met: an animal or plant once killed, skinned or gathered; the
        // collectibles found so far.
        let named = |key: &str| HandbookEntry { key: key.into(), ..Default::default() };
        assert!(!handbook_open(&named("Wild life/Mean dog"), &p));
        let book = fc3_formats::progression::Handbook {
            entries: vec![named("Wild life/Mean dog"), named("Wild life/Buffalo"), named("Wild life/Big buffalo - Rare - X")],
            ..Default::default()
        };
        assert_eq!(met_pages(&book, "Wild life/", Animal::Dog.name()), vec!["Wild life/Mean dog"]);
        assert_eq!(met_pages(&book, "Wild life/", Animal::Buffalo.name()), vec!["Wild life/Buffalo"]);
        p.met.insert("Wild life/Mean dog".into());
        assert!(handbook_open(&named("Wild life/Mean dog"), &p));
        p.relics = 2;
        assert!(handbook_open(&named("Collectibles/Hidden relic 2"), &p) && !handbook_open(&named("Collectibles/Hidden relic 3"), &p));
        // Weapons held and vehicles boarded, by the handbook's names for them.
        let book = fc3_formats::progression::Handbook {
            entries: vec![named("Weapons/SPAS 12"), named("Weapons/AK47"), named("Transport/Scavenger"), named("Transport/Cargo truck")],
            ..Default::default()
        };
        assert_eq!(page_named(&book, "Weapons/", weapon_page("SPAS-12")), Some("Weapons/SPAS 12"));
        assert_eq!(page_named(&book, "Weapons/", weapon_page("AK-74M Rifle")), Some("Weapons/AK47"));
        assert_eq!(page_named(&book, "Transport/", vehicle_page("4x4")), Some("Transport/Scavenger"));
        assert_eq!(page_named(&book, "Transport/", vehicle_page("Truck")), Some("Transport/Cargo truck"));
        assert!(!handbook_open(&named("Weapons/AK47"), &p));
        p.met.insert("Weapons/AK47".into());
        assert!(handbook_open(&named("Weapons/AK47"), &p));
        // Free roam: everything.
        assert!(handbook_open(&page(false, "JAS_999_main", ""), &Progress::default()));
    }

    #[test]
    fn kills_are_worth_the_victims_xp_times_how_they_died() {
        use crate::combat::{DamageEvent, DamageKind, Died};
        let mut app = App::new();
        app.add_message::<Died>().add_message::<DamageEvent>().add_message::<XpGain>().add_message::<StatGain>();
        app.init_resource::<TakedownRewards>().init_resource::<crate::vehicles::Driving>();
        app.insert_resource(Skills(SkillTree {
            xp_rewards: vec![("Takedown/StandardTakedown".into(), 3.0), ("Standard/KillWithHeadShotModifier".into(), 2.5)],
            ..Default::default()
        }));
        let merc = fc3_formats::progression::Source {
            skeleton: true,
            key: "merc".into(),
            prototype: "NPC:Merc/Base.Pirate".into(),
            items: Vec::new(),
            groups: Vec::new(),
            kit: 7,
            xp: 10,
        };
        app.insert_resource(crate::crafting::CraftCatalog { sources: vec![merc], ..Default::default() });
        app.add_systems(Update, award_kills);
        let player = app.world_mut().spawn((crate::player::test_player(), crate::weapons::test_inventory())).id();
        let mut kill = |app: &mut App, takedown: bool, headshot: bool| -> u32 {
            let rig = app.world_mut().spawn(crate::characters::KitId(7)).id();
            let mut npc = crate::npc::new_npc(crate::npc::Faction::Pirate, crate::npc::Role::Assault, Vec3::ZERO, 0.0, 1, &mut crate::weapons::Rng::seeded(1));
            npc.rig = Some(rig);
            npc.state = crate::npc::NpcState::Combat;
            let victim = app.world_mut().spawn(npc).id();
            if takedown {
                app.world_mut().resource_mut::<TakedownRewards>().0.insert(victim, "StandardTakedown");
            }
            let kind = if takedown { DamageKind::Melee } else { DamageKind::Bullet };
            app.world_mut().write_message(DamageEvent {
                target: victim,
                amount: 500.0,
                kind,
                source: None,
                instigator: Some(player),
                headshot,
                hit: crate::combat::Hit::Whole,
            });
            app.world_mut().write_message(Died { entity: victim, killer: Some(player), kind });
            app.update();
            app.world_mut().resource_mut::<Messages<XpGain>>().drain().map(|g| g.amount).sum()
        };
        assert_eq!(kill(&mut app, false, false), 10);
        assert_eq!(kill(&mut app, false, true), 25);
        assert_eq!(kill(&mut app, true, false), 30);
    }

    #[test]
    fn the_health_boost_adds_bars_that_are_lost_not_regenerated() {
        use crate::combat::HEALTH_SEGMENT;
        let mut app = App::new();
        app.init_resource::<Perks>().insert_resource(crate::syringes::Boosts { extra_health: 4.0 * HEALTH_SEGMENT, full_heal: true, ..Default::default() });
        app.add_systems(Update, apply_perks);
        let base = app.world().resource::<Perks>().max_health;
        let stats = PlayerStats {
            medkits: 0,
            kills: 0,
            deaths: 0,
            respawn_timer: 0.0,
            spawn_point: Vec3::ZERO,
            hurt_dir: None,
            burning: 0.0,
            last_hurt: crate::combat::DamageKind::Bullet,
        };
        let mut h = Health::new(base);
        h.current = base * 0.5;
        let player = app.world_mut().spawn((crate::player::test_player(), h, stats, crate::weapons::test_inventory())).id();
        app.update();
        let health = |app: &App| app.world().get::<Health>(player).map(|h| (h.current, h.max)).unwrap();
        // Healed full, four bars on top.
        assert_eq!(health(&app), (base + 4.0 * HEALTH_SEGMENT, base + 4.0 * HEALTH_SEGMENT));
        // Hurt by a bar and a half: those are gone for good.
        app.world_mut().get_mut::<Health>(player).unwrap().current -= 1.5 * HEALTH_SEGMENT;
        app.update();
        assert_eq!(health(&app).1, base + 2.5 * HEALTH_SEGMENT);
        // Below the player's own: back to them.
        app.world_mut().get_mut::<Health>(player).unwrap().current = base - 10.0;
        app.update();
        assert_eq!(health(&app), (base - 10.0, base));
        assert_eq!(app.world().resource::<crate::syringes::Boosts>().extra_health, 0.0);
    }

    #[test]
    fn gates_in_order() {
        let mut p = Progress { story: true, skill_points: 0, ..Default::default() };
        let mut a = ability("SP_B", &["SP_A"], "M1_main");
        a.stat = 150;
        a.stat_target = 2;
        assert_eq!(skill_state(&p, &a), SkillState::Locked(SkillLock::Story));
        p.missions.insert("m1_main".into());
        assert_eq!(skill_state(&p, &a), SkillState::Locked(SkillLock::Requires(vec!["SP_A".into()])));
        p.skills.insert("SP_A".into());
        assert_eq!(skill_state(&p, &a), SkillState::Locked(SkillLock::Stat(150, 0, 2)));
        p.stats.insert(150, 2);
        assert_eq!(skill_state(&p, &a), SkillState::Locked(SkillLock::Points));
        p.skill_points = 1;
        assert_eq!(skill_state(&p, &a), SkillState::Ready);
        p.skills.insert("SP_B".into());
        assert_eq!(skill_state(&p, &a), SkillState::Learned);
    }

    #[test]
    fn scripts_and_free_roam_open_abilities() {
        let mut a = ability("SP_C", &[], "");
        a.unlock_script = true;
        let mut p = Progress { story: true, ..Default::default() };
        assert_eq!(skill_state(&p, &a), SkillState::Locked(SkillLock::Story));
        p.upgradable.insert("SP_C".into());
        assert_eq!(skill_state(&p, &a), SkillState::Ready);
        let free = Progress { story: false, ..Default::default() };
        assert_eq!(skill_state(&free, &a), SkillState::Ready);
    }

    #[test]
    fn perks_follow_learned_bonuses() {
        let bonus = |attribute, value| Bonus { plan: String::new(), attribute, value, additive: false };
        let mut slot = ability("SP_AddHealthSlotA", &[], "");
        slot.bonuses = vec![bonus(attr::HEALTH_SLOTS, 1.0)];
        let mut fire = ability("SP_FireDamageResistance", &[], "");
        fire.bonuses = vec![bonus(42, 0.3)];
        let mut take = ability("SP_Takedown", &[], "");
        take.bonuses = vec![bonus(102, 1.0)];
        let skills = Skills(SkillTree { abilities: vec![slot, fire, take], ..Default::default() });
        let none = Perks::from_progress(&Progress::default(), &skills);
        assert!(!none.takedown);
        assert_eq!(none.fire_taken, 1.0);
        let mut p = Progress::default();
        p.skills.extend(["SP_AddHealthSlotA".to_string(), "SP_FireDamageResistance".into(), "SP_Takedown".into()]);
        let perks = Perks::from_progress(&p, &skills);
        assert!(perks.takedown);
        assert!((perks.fire_taken - 0.7).abs() < 1e-6);
        assert!((perks.max_health - none.max_health - crate::combat::HEALTH_SEGMENT).abs() < 1e-3);
    }

    #[test]
    fn progress_lines_keep_story_unlocks_and_stats() {
        let mut p = Progress::default();
        p.upgradable.insert("SP_X".into());
        p.missions.insert("jas_060_main".into());
        p.stats.insert(150, 3);
        let mut back = Progress::default();
        for line in p.to_lines().lines() {
            let (k, rest) = line.split_once(' ').unwrap_or((line, ""));
            back.read_line(k, rest);
        }
        assert!(back.upgradable.contains("SP_X") && back.missions.contains("jas_060_main"));
        assert_eq!(back.stat(150), 3);
    }
}

#[cfg(test)]
mod journal_tests {
    use super::*;
    use crate::crafting::{CraftCatalog, ItemCrafted, MEDICINE, RUCKSACK};
    use crate::syringes::Plant;
    use fc3_formats::progression::Recipe;

    fn app() -> (App, Entity, [Entity; 3]) {
        let mut metadata = super::loot_tests::metadata();
        metadata.recipes = vec![
            Recipe { id: MEDICINE.into(), kind: 3, label: "Medkit small".into(), ingredients: vec![(11, 1)], ..default() },
            Recipe { id: RUCKSACK.into(), kind: 2, label: "Loot pouch small".into(), ingredients: vec![(10, 2)], ..default() },
        ];
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(State::new(GameState::Journal))
            .init_resource::<NextState<GameState>>()
            .init_resource::<Progress>()
            .init_resource::<HudState>()
            .init_resource::<crate::syringes::Boosts>()
            .insert_resource(CraftCatalog::from_metadata(&metadata))
            .insert_resource(LootCatalog::from_metadata(&metadata))
            .init_resource::<Perks>()
            .init_resource::<Skills>()
            .init_resource::<SkillHover>()
            .init_resource::<HandbookData>()
            .init_resource::<HandbookView>()
            .insert_resource(PauseMenu { tutorial: Some(CraftTutorial::Medicine(0)), ..default() })
            .add_message::<crate::sound::PlaySfx>()
            .add_message::<ItemCrafted>()
            .add_systems(Update, journal_buttons);
        let player = app
            .world_mut()
            .spawn((
                Player {
                    velocity: Vec3::ZERO,
                    yaw: 0.0,
                    pitch: 0.0,
                    grounded: true,
                    crouching: false,
                    swimming: false,
                    noclip: false,
                    spawning: false,
                    keep_height: false,
                    last_landing_speed: 0.0,
                    sprinting: false,
                    spawned_at: 0.0,
                    driving: false,
                    armed_passenger: false,
                    eye: 0.0,
                    air: crate::player::AirMode::None,
                    mantle: None,
                    mounted: None,
                    sliding: None,
                    dragging: false,
                },
                PlayerStats {
                    medkits: 0,
                    kills: 0,
                    deaths: 0,
                    respawn_timer: 0.0,
                    spawn_point: Vec3::ZERO,
                    hurt_dir: None,
                    burning: 0.0,
                    last_hurt: DamageKind::Bullet,
                },
            ))
            .id();
        let buttons = [JournalButton::Authored(0), JournalButton::Authored(1), JournalButton::Close]
            .map(|b| app.world_mut().spawn((b, Interaction::None, BackgroundColor(IDLE))).id());
        (app, player, buttons)
    }

    fn press(app: &mut App, button: Entity) {
        *app.world_mut().get_mut::<Interaction>(button).unwrap() = Interaction::None;
        app.update();
        *app.world_mut().get_mut::<Interaction>(button).unwrap() = Interaction::Pressed;
        app.update();
    }

    #[test]
    fn tutorial_buttons_require_two_real_medicine_crafts_then_a_rucksack_before_close() {
        let (mut app, player, [medicine, rucksack, close]) = app();
        {
            let mut progress = app.world_mut().resource_mut::<Progress>();
            progress.plants[Plant::Arrowroot.index()] = 2;
            progress.skins.insert(Animal::Boar, 2);
        }
        press(&mut app, close);
        assert!(matches!(app.world().resource::<NextState<GameState>>(), NextState::Unchanged));
        press(&mut app, rucksack);
        assert!(!app.world().resource::<Progress>().recipes.contains(RUCKSACK));
        press(&mut app, medicine);
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, Some(CraftTutorial::Medicine(1)));
        assert_eq!(app.world().get::<PlayerStats>(player).unwrap().medkits, 1);
        press(&mut app, close);
        assert!(!app.world().resource::<PauseMenu>().can_exit());
        press(&mut app, medicine);
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, Some(CraftTutorial::Rucksack));
        assert_eq!(app.world().get::<PlayerStats>(player).unwrap().medkits, 2);
        press(&mut app, medicine);
        assert_eq!(app.world().get::<PlayerStats>(player).unwrap().medkits, 2);
        press(&mut app, rucksack);
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, Some(CraftTutorial::Done));
        assert_eq!(crate::crafting::loot_count(app.world().resource::<Progress>()), 0);
        assert!(app.world().resource::<Progress>().recipes.contains(RUCKSACK));
        press(&mut app, close);
        assert!(matches!(app.world().resource::<NextState<GameState>>(), NextState::Pending(GameState::Playing)));
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, None);
        let mut menu = app.world_mut().resource_mut::<PauseMenu>();
        assert!(menu.tutorial_completed);
        menu.override_page(false, false, true);
        assert_eq!(menu.tutorial, Some(CraftTutorial::Done));
        assert!(!menu.allows_recipe(MEDICINE));
        assert!(menu.can_exit());
    }

    #[test]
    fn missing_ingredients_full_capacity_and_held_button_never_advance_the_tutorial() {
        let (mut app, player, [medicine, _, _]) = app();
        press(&mut app, medicine);
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, Some(CraftTutorial::Medicine(0)));
        app.world_mut().resource_mut::<Progress>().plants[Plant::Arrowroot.index()] = 2;
        app.world_mut().get_mut::<PlayerStats>(player).unwrap().medkits = app.world().resource::<Perks>().medkit_cap;
        press(&mut app, medicine);
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, Some(CraftTutorial::Medicine(0)));
        assert_eq!(app.world().resource::<Progress>().plants[Plant::Arrowroot.index()], 2);
        app.world_mut().get_mut::<PlayerStats>(player).unwrap().medkits = 0;
        press(&mut app, medicine);
        for _ in 0..5 { app.update(); }
        assert_eq!(app.world().resource::<PauseMenu>().tutorial, Some(CraftTutorial::Medicine(1)));
        assert_eq!(app.world().get::<PlayerStats>(player).unwrap().medkits, 1);
    }

    #[test]
    fn override_unlocks_only_its_page_and_tutorial_rejects_skills_and_other_recipes() {
        let mut menu = PauseMenu { crafting_locked: true, skills_locked: true, ..default() };
        menu.override_page(true, false, true);
        assert_eq!(menu.page, JournalPage::Crafting);
        assert!(menu.allows_page(JournalPage::Crafting));
        assert!(!menu.allows_page(JournalPage::Skills));
        assert!(menu.skills_locked);
        assert!(!menu.allows_recipe(RUCKSACK));
        menu.record_craft("Wrong recipe");
        assert_eq!(menu.tutorial, Some(CraftTutorial::Medicine(0)));
        menu.record_craft(MEDICINE);
        menu.record_craft(MEDICINE);
        menu.record_craft(RUCKSACK);
        assert!(menu.can_exit());
    }

    #[test]
    fn mission_menu_gates_unlock_once_and_do_not_undo_later_explicit_locks() {
        let mut menu = PauseMenu { crafting_locked: true, skills_locked: true,
            unlock_gates: [Some("jas_080_main".into()), Some("jas_080_main".into())], ..default() };
        menu.update_unlocks(&HashSet::new());
        assert!(!menu.allows_page(JournalPage::Crafting));
        menu.override_page(true, false, true);
        assert!(menu.allows_page(JournalPage::Crafting));
        assert!(!menu.allows_page(JournalPage::Skills));
        menu.tutorial = None;
        let completed = HashSet::from(["jas_080_main".into()]);
        menu.update_unlocks(&completed);
        assert!(menu.allows_page(JournalPage::Skills));
        menu.skills_locked = true;
        menu.update_unlocks(&completed);
        assert!(!menu.allows_page(JournalPage::Skills));
    }
}

#[cfg(test)]
pub(crate) mod loot_tests {
    use super::*;
    use crate::syringes::Plant;

    pub(crate) fn metadata() -> fc3_formats::progression::Crafting {
        fc3_formats::progression::Crafting::from_text(concat!(
            "fc3-recipes 8\n",
            "item\t10\t1\t0\t0\tBoar hide\tHide\t900\t0\t0\t4294967295\t0\t0\t\t\t0\n",
            "item\t11\t1\t0\t0\tGreen leaf\tLeaf\t901\t0\t0\t4294967295\t0\t0\t\t\t0\n",
            "item\t12\t1\t0\t0\tCrimson leaf\tLeaf\t902\t0\t0\t4294967295\t0\t0\t\t\t0\n",
            "item\t13\t1\t1\t0\tRare hide\tRare\t903\t0\t0\t4294967295\t0\t0\t\t\t0\n",
            "item\t14\t1\t0\t0\tPig hide\tPig\t904\t0\t0\t4294967295\t0\t0\t\t\t0\n",
            "source\tskeleton\tboar.skeleton\tFC3_Animals:FC3_Domestic/Pig\t14:100\n",
            "source\tskeleton\tboar.skeleton\tFC3_Animals:FC3_Quadrupeds/Boar.Rare\t13:100\n",
            "source\tskeleton\tboar.skeleton\tFC3_Animals:FC3_Quadrupeds/Boar\t10:100\n",
            "source\tentity\tPickups/Plants/Arrowroot_01\tArrowroot\t11:100\n",
            "source\tentity\tPickups/Plants/ScrewPine_01\tScrewPine\t11:100\n",
            "source\tentity\tPickups/Plants/TorchGinger_01\tTorchGinger\t12:100\n",
            "source\tentity\tPickups/Plants/StarRhododendron_01\tStarRhododendron\t12:100\n",
        )).unwrap()
    }

    #[test]
    fn plant_slots_preserve_old_save_counts_and_round_trip_added_species() {
        let mut p = Progress::default();
        p.plants.fill(99);
        assert!(p.read_line("plants", "1 2 3 4 5 6 7 8"));
        for (i, plant) in Plant::ALL.into_iter().take(8).enumerate() {
            assert_eq!(p.plants[plant.index()], i as u32 + 1);
        }
        assert!(p.plants[8..].iter().all(|n| *n == 0));
        p.plants[Plant::GoodLuckPlant.index()] = 3;
        p.plants[Plant::SacredLotus.index()] = 2;
        let mut loaded = Progress::default();
        for line in p.to_lines().lines() {
            let (key, rest) = line.split_once(' ').unwrap();
            assert!(loaded.read_line(key, rest));
        }
        assert_eq!(loaded, p);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_plant_sources_map_all_eighteen_species_to_their_authored_leaves() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let metadata = fc3_formats::progression::Crafting::from_text(&std::fs::read_to_string(cache.join("progression/recipes.txt")).unwrap()).unwrap();
        let catalog = LootCatalog::from_metadata(&metadata);
        let mut p = Progress::default();
        for (species, id) in [
            (Plant::Arrowroot, 104580), (Plant::Aloe, 104580), (Plant::ScrewPine, 104580), (Plant::GoblinsGold, 104580),
            (Plant::TorchGinger, 104581), (Plant::StarRhododendron, 104581), (Plant::GoldenBeehive, 104581), (Plant::GoodLuckPlant, 104581),
            (Plant::StoutKelp, 142502), (Plant::BreezeWakame, 142502), (Plant::RedAlgae, 142502), (Plant::SacredLotus, 142502),
            (Plant::YellowSage, 104579), (Plant::Heliconia, 104579), (Plant::Hibiscus, 104579), (Plant::Palani, 104579),
            (Plant::GoatsFoot, 142503), (Plant::Tiare, 142503),
        ] {
            assert_eq!(catalog.item(LootKind::Plant(species)).unwrap().name_id, id, "{species:?}");
            p.plants[species.index()] = 1;
        }
        assert_eq!(catalog.quantities(&p), HashMap::from([(104580, 4), (104581, 4), (142502, 4), (104579, 4), (142503, 2)]));
    }

    #[test]
    fn ingredient_identity_comes_from_authored_sources_not_shared_models_or_name_guesses() {
        let mut metadata = metadata();
        let catalog = LootCatalog::from_metadata(&metadata);
        assert_eq!(catalog.item(LootKind::Skin(Animal::Boar)).unwrap().name_id, 900);
        assert_eq!(catalog.item(LootKind::Plant(Plant::Arrowroot)).unwrap().name_id, 901);
        assert_eq!(catalog.item(LootKind::Plant(Plant::ScrewPine)).unwrap().name_id, 901);
        assert_eq!(catalog.item(LootKind::Plant(Plant::TorchGinger)).unwrap().name_id, 902);
        assert!(catalog.item(LootKind::Skin(Animal::Chicken)).is_none());
        assert!(catalog.item(LootKind::Plant(Plant::StoutKelp)).is_none());
        metadata.sources.retain(|s| s.prototype != "FC3_Animals:FC3_Quadrupeds/Boar");
        assert!(LootCatalog::from_metadata(&metadata).item(LootKind::Skin(Animal::Boar)).is_none());
    }

    #[test]
    fn loot_quantities_combine_leaf_sources_follow_consumption_and_survive_save_loading() {
        let catalog = LootCatalog::from_metadata(&metadata());
        let mut progress = Progress::default();
        assert!(catalog.quantities(&progress).is_empty());
        progress.skins.insert(Animal::Boar, 2);
        progress.plants[Plant::Arrowroot.index()] = 2;
        progress.plants[Plant::ScrewPine.index()] = 3;
        progress.plants[Plant::TorchGinger.index()] = 1;
        progress.plants[Plant::StarRhododendron.index()] = 4;
        assert_eq!(catalog.quantities(&progress), HashMap::from([(900, 2), (901, 5), (902, 5)]));
        let mut loaded = Progress::default();
        for line in progress.to_lines().lines() {
            let (key, rest) = line.split_once(' ').unwrap();
            assert!(loaded.read_line(key, rest));
        }
        assert_eq!(catalog.quantities(&loaded), catalog.quantities(&progress));
        loaded.skins.clear();
        loaded.plants[Plant::Arrowroot.index()] = 0;
        loaded.plants[Plant::ScrewPine.index()] = 0;
        loaded.plants[Plant::TorchGinger.index()] -= 1;
        assert_eq!(catalog.quantities(&loaded), HashMap::from([(902, 4)]));
    }
}

#[cfg(test)]
mod shop_tests {
    use super::*;

    fn inventory() -> Inventory {
        let defs = crate::weapons::weapon_defs();
        let unarmed = defs.iter().position(|d| d.name == crate::weapons::UNARMED).unwrap();
        Inventory {
            defs, models: Vec::new(), slots: vec![crate::weapons::WeaponSlot { def: unarmed, mag: 0, reserve: 0 }], current: 0,
            grenades: 0, molotovs: 0, c4: 0, mines: 0,
            cooldown: 0.0, reloading: 0.0, switching: 0.0, aiming: false,
            aim_blend: 0.0, recoil_kick: 0.0, spread_bloom: 0.0, shots_fired: 0,
            recoil_offset: 0.0, since_shot: 0.0, reserve_scale: 1.0,
            attachments: Vec::new(), attachment_models: Vec::new(), mounted_from: None,
            auto_tail: 0, arrow_kind: 0, special_arrows: [0; 2],
            paints: Vec::new(), paint_options: Vec::new(),
        }
    }

    fn catalog(inv: &Inventory) -> (ShopCatalog, usize) {
        let def = inv.defs.iter().position(|d| d.name == "1911").unwrap();
        (ShopCatalog(vec![fc3_formats::shop::Weapon {
            id: 42, category: 7, cost: 75, sort: 0, name_id: 999,
            unlock_script: String::new(), unlock_mission: String::new(),
            archetype: "weapons:TestPistol".into(), model: inv.defs[def].model_path(),
        }]), def)
    }

    #[test]
    fn loot_sells_by_the_stack_at_the_skill_scaled_price() {
        use fc3_formats::progression::Item;
        let item = |id: u32, cost: u32| Item { id, cost, loot: true, ..Default::default() };
        let crafting = crate::crafting::CraftCatalog { items: vec![item(1, 10), item(2, 45), item(3, 0)], ..Default::default() };
        let mut progress = Progress::default();
        progress.loot.extend([(1, 3), (2, 1), (3, 5)]);
        let mut perks = Perks::default();
        // The biggest stack's worth first; what doesn't sell isn't listed.
        let list = sell_items(&progress, &crafting);
        assert!(matches!(list.as_slice(), [ShopItem::SellAll, ShopItem::Sell(2), ShopItem::Sell(1)]));
        assert_eq!(sale_value(ShopItem::SellAll, &progress, &crafting, &perks), 75);
        perks.sale_price = 1.25;
        assert_eq!(sale_value(ShopItem::Sell(1), &progress, &crafting, &perks), 39);
    }

    #[test]
    fn supplies_come_from_the_database_and_refills_cost_the_ammunition_they_take() {
        use fc3_formats::progression::{Item, SHOP_AMMO, SHOP_CONSUMABLES};
        let mut inv = inventory();
        let pistol = inv.defs.iter().position(|d| d.name == "1911").unwrap();
        let cap = inv.defs[pistol].max_reserve;
        inv.slots.push(crate::weapons::WeaponSlot { def: pistol, mag: 0, reserve: cap - 30 });
        let item = |id: u32, key: &str, shop: u32, cost: u32, amount: u32, armour: f32| Item { id, key: key.into(), shop, cost, amount, armour, ..Default::default() };
        let supplies = vec![
            item(1, "Ammo/Handgun", SHOP_AMMO, 20, 24, 0.0),
            item(2, "Ammo/Sniper rifle", SHOP_AMMO, 100, 10, 0.0),
            item(3, "Ammo/Refill all", SHOP_AMMO, 0, 0, 0.0),
            item(4, "Consumables/Body armor A", SHOP_CONSUMABLES, 200, 0, 100.0),
            item(5, "Loot items/Pirate 1", u32::MAX, 12, 0, 0.0),
        ];
        assert_eq!(supply(&supplies[3]), Some(Supply::Armour(100.0)));
        // Consumables first, then the refill, then ammunition for the guns carried.
        let list = supply_items(&inv, &supplies, &Progress::default());
        let ids: Vec<u32> = list.iter().map(|i| if let ShopItem::Supply(id, _) = i { *id } else { 0 }).collect();
        assert_eq!(ids, vec![4, 3, 1]);
        // 30 rounds missing at 24 a box: two boxes.
        assert!(matches!(list[1], ShopItem::Supply(3, 40)));
        assert!(matches!(list[2], ShopItem::Supply(1, 20)));
    }

    #[test]
    fn tutorial_purchase_grants_and_equips_the_selected_weapon_and_charges_its_authored_price_once() {
        let mut inv = inventory();
        let (catalog, def) = catalog(&inv);
        let mut progress = Progress::default();
        let perks = Perks::default();
        let restriction = ShopOverride(Some((7, 42)));
        let bought = purchase_tutorial_weapon(&mut inv, &mut progress, &perks, &catalog, &restriction).unwrap();
        assert_eq!(bought.model, Some(inv.defs[def].model_path()));
        assert_eq!(progress.money, 75);
        assert_eq!(inv.slot().def, def);
        assert_eq!(inv.slot().mag, inv.defs[def].magazine);
        let ammo = (inv.slot().mag, inv.slot().reserve);
        assert!(purchase_tutorial_weapon(&mut inv, &mut progress, &perks, &catalog, &restriction).is_none());
        assert_eq!(progress.money, 75);
        assert_eq!((inv.slot().mag, inv.slot().reserve), ammo);
    }

    #[test]
    fn insufficient_funds_or_an_unknown_tutorial_item_do_not_generate_a_purchase() {
        let mut inv = inventory();
        let (catalog, _) = catalog(&inv);
        let mut progress = Progress { money: 74, ..default() };
        let perks = Perks::default();
        assert!(purchase_tutorial_weapon(&mut inv, &mut progress, &perks, &catalog, &ShopOverride(Some((7, 42)))).is_none());
        assert_eq!(progress.money, 74);
        assert_eq!(inv.slots.len(), 1);
        assert!(purchase_tutorial_weapon(&mut inv, &mut progress, &perks, &catalog, &ShopOverride(Some((7, 43)))).is_none());
        assert_eq!(progress.money, 74);
    }

    #[test]
    fn tutorial_stock_is_restricted_until_the_mission_clears_its_override() {
        let inv = inventory();
        let (catalog, def) = catalog(&inv);
        let mut restriction = ShopOverride::default();
        restriction.set(true, 7, 42);
        for tab in 0..SHOP_TABS.len() {
            assert!(matches!(shop_items(&inv, tab, &catalog, &restriction, &[], &Progress::default()).as_slice(), [ShopItem::Weapon(d)] if *d == def));
        }
        restriction.set(true, 7, 43);
        assert!(shop_items(&inv, 0, &catalog, &restriction, &[], &Progress::default()).is_empty());
        restriction.set(false, 0, 0);
        assert!(shop_items(&inv, 0, &catalog, &restriction, &[], &Progress::default()).len() > 1);
    }

    #[test]
    fn radio_tower_thresholds_override_the_catalog_price_not_a_sandbox_list() {
        let inv = inventory();
        let (mut catalog, def) = catalog(&inv);
        // Even the 1911 follows the authored threshold, not its hard-coded
        // position in an old reward list.
        catalog.0[0].unlock_script = "Tower2".into();
        assert_eq!(ShopItem::Weapon(def).price(&inv, 1, &catalog), 75);
        assert_eq!(ShopItem::Weapon(def).price(&inv, 2, &catalog), 0);
        assert_eq!(ShopItem::Weapon(def).price(&inv, 18, &catalog), 0);
        catalog.0[0].unlock_script.clear();
        assert_eq!(ShopItem::Weapon(def).price(&inv, 18, &catalog), 75);
    }

    #[test]
    fn a_tower_can_free_multiple_weapons_without_granting_or_rebuying_them() {
        let mut inv = inventory();
        let mut catalog = ShopCatalog::default();
        let index = |name| inv.defs.iter().position(|d| d.name == name).unwrap();
        let defs = [index("1911"), index("6P9"), index("STG-90"), index("AK-74M Rifle")];
        for (id, def) in defs.into_iter().enumerate() {
            catalog.0.push(fc3_formats::shop::Weapon {
                id: id as u32, category: 7, cost: 600, sort: 0, name_id: 999,
                unlock_script: format!("Tower{}", if id == 3 { 2 } else { 1 }), unlock_mission: String::new(),
                archetype: format!("weapons:Test{id}"), model: inv.defs[def].model_path(),
            });
            assert_eq!(ShopItem::Weapon(def).price(&inv, 0, &catalog), 600);
            assert_eq!(ShopItem::Weapon(def).price(&inv, 1, &catalog), if id == 3 { 600 } else { 0 });
        }
        let mut variant = catalog.0[0].clone();
        variant.archetype.push_str(".Special");
        catalog.0.insert(0, variant);
        assert_eq!(catalog.freed_weapons(&inv.defs, 0), Vec::<&str>::new());
        assert_eq!(catalog.freed_weapons(&inv.defs, 1), vec!["1911", "6P9", "STG-90"]);
        assert_eq!(catalog.freed_weapons(&inv.defs, 2), vec!["AK-74M Rifle"]);
        assert!(catalog.freed_weapons(&inv.defs, 18).is_empty());
        assert_eq!(inv.slots.len(), 1); // A tower frees stock, not carried guns.
        let mut progress = Progress { money: 0, ..default() };
        progress.towers.insert(0);
        let perks = Perks::default();
        let bought = purchase_weapon(&mut inv, &mut progress, &perks, &catalog, defs[1]).unwrap();
        assert_eq!(bought.0.model, Some(inv.defs[defs[1]].model_path()));
        assert_eq!(progress.money, 0);
        assert_eq!(inv.slot().def, defs[1]);
        assert!(purchase_weapon(&mut inv, &mut progress, &perks, &catalog, defs[1]).is_none());
    }

    #[test]
    fn a_shared_model_does_not_sell_a_variant_as_its_base_weapon() {
        let inv = inventory();
        let (mut catalog, def) = catalog(&inv);
        let mut variant = catalog.0[0].clone();
        variant.id = 43;
        variant.cost = 0;
        variant.archetype.push_str(".Special");
        catalog.0.insert(0, variant);
        assert_eq!(ShopItem::Weapon(def).price(&inv, 0, &catalog), 75);
        assert_eq!(catalog.selected_weapon(&inv, (7, 42)), Some(def));
        assert_eq!(catalog.selected_weapon(&inv, (7, 43)), None);
    }

    #[test]
    fn pressing_the_shop_button_uses_the_real_transaction_and_emits_one_purchase() {
        let inv = inventory();
        let (catalog, def) = catalog(&inv);
        let stats = PlayerStats {
            medkits: 0, kills: 0, deaths: 0, respawn_timer: 0.0,
            spawn_point: Vec3::ZERO, hurt_dir: None, burning: 0.0, last_hurt: DamageKind::Bullet,
        };
        let mut app = App::new();
        app.set_error_handler(|error, context| panic!("{error:?}: {context:?}"));
        app.add_plugins(MinimalPlugins)
            .insert_resource(State::new(GameState::Shop))
            .init_resource::<NextState<GameState>>()
            .init_resource::<Progress>()
            .init_resource::<Perks>()
            .init_resource::<HudState>()
            .init_resource::<ShopTab>()
            .insert_resource(catalog)
            .insert_resource(ShopOverride(Some((7, 42))))
            .init_resource::<crate::crafting::CraftCatalog>()
            .init_resource::<crate::treasure_maps::TreasureMaps>()
            .add_message::<ItemBought>()
            .add_message::<crate::sound::PlaySfx>()
            .add_systems(Update, shop_buttons);
        let player = app
            .world_mut()
            .spawn((
                inv,
                stats,
                Player {
                    velocity: Vec3::ZERO,
                    yaw: 0.0,
                    pitch: 0.0,
                    grounded: true,
                    crouching: false,
                    swimming: false,
                    noclip: false,
                    spawning: false,
                    keep_height: false,
                    last_landing_speed: 0.0,
                    sprinting: false,
                    spawned_at: 0.0,
                    driving: false,
                    armed_passenger: false,
                    eye: crate::player::EYE_HEIGHT,
                    air: crate::player::AirMode::None,
                    mantle: None,
                    mounted: None,
                    sliding: None,
                    dragging: false,
                },
            ))
            .id();
        app.update();
        let button = app.world_mut().spawn((Interaction::Pressed, ShopButton::Buy(0), BackgroundColor(IDLE))).id();
        assert_eq!(*app.world().resource::<State<GameState>>().get(), GameState::Shop);
        assert_eq!(app.world().get::<Interaction>(button), Some(&Interaction::Pressed));
        app.update();
        assert_eq!(app.world().resource::<Progress>().money, 75);
        assert_eq!(app.world().get::<Inventory>(player).unwrap().slot().def, def);
        let messages = app.world().resource::<Messages<ItemBought>>();
        let mut cursor = messages.get_cursor();
        let bought: Vec<_> = cursor.read(messages).collect();
        assert_eq!(bought.len(), 1);
        assert_eq!(bought[0].model.as_deref(), Some(app.world().get::<Inventory>(player).unwrap().defs[def].model_path().as_str()));
        app.update();
        assert_eq!(app.world().resource::<Progress>().money, 75);
        assert!(cursor.read(app.world().resource::<Messages<ItemBought>>()).next().is_none());
    }
}

// ---------------------------------------------------------------- popups ---

/// Recent XP gains shown to the right of the crosshair.
#[derive(Resource, Default)]
struct Popups(Vec<(String, f32)>);

#[derive(Component)]
struct PopupText;

fn setup_popups(mut commands: Commands) {
    commands.spawn((
        Node { position_type: PositionType::Absolute, left: Val::Percent(55.0), top: Val::Percent(44.0), ..default() },
        Text::new(""),
        font(18.0),
        TextColor(Color::srgb(1.0, 0.85, 0.4)),
        GlobalZIndex(1),
        PopupText,
    ));
}

fn update_popups(
    time: Res<Time<Real>>,
    popups: Option<ResMut<Popups>>,
    mut text: Query<&mut Text, With<PopupText>>,
    original: Option<Res<crate::hud::OriginalHud>>,
) {
    let Some(mut popups) = popups else { return };
    for p in &mut popups.0 {
        p.1 -= time.delta_secs();
    }
    popups.0.retain(|p| p.1 > 0.0);
    if let Ok(mut t) = text.single_mut() {
        // (The original HUD's XP kicker shows them instead.)
        let own = !original.is_some_and(|o| o.0);
        let s = popups.0.iter().rev().take(4).filter(|_| own).map(|p| p.0.as_str()).collect::<Vec<_>>().join("\n");
        if t.0 != s {
            t.0 = s;
        }
    }
}
