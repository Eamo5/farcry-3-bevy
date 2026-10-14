//! Character progression data: the skill tree, the XP each level needs, and
//! crafting (recipes, what they take, what they give, and where the
//! ingredients come from).
//!
//! The converter reads these from the game's templates and generic
//! databases and writes two text tables to the cache ([`SKILLS_REL`],
//! [`RECIPES_REL`]); the game reads only those.
//!
//! * Abilities live in the `AbilityService` template ([`TEMPLATES`]).
//!   `Single_Player/AbilityList` holds each ability's properties: the points
//!   it takes to master (`iMaxSkillPoint`), localised name and description
//!   ids, a number the description formats (`iDescriptionInt`), and the bonus
//!   plans it applies. `Single_Player/Chapters/ChapterList` arranges them in
//!   chapters (the skill tree's branches); per ability: up to two
//!   prerequisite abilities (`dependantAbility1/2`, CRC-32 of the name),
//!   whether it can be bought (`bCanPurchase`), the mission whose completion
//!   makes it upgradable (`UnlockOnMissionComplete`) or whether scripts do
//!   (`UnlockFromScript`), an optional statistic prerequisite (`preReq`), and
//!   its cell in the menu grid. `Single_Player/XPLevels/LevelsList` gives the
//!   total XP each level needs.
//! * Bonus plans (`BonusService` template) are lists of modifiers: an engine
//!   attribute id, a value, and whether the value adds (`selValueType` 1) or
//!   scales (0).
//! * Recipes are [`RECIPE_LIB`] entries: a type and subtype (by `disId` into
//!   [`RECIPE_TYPE_LIB`] / [`RECIPE_SUBTYPE_LIB`]; type 255 marks merged,
//!   unused entries), ingredients (`Requirements`: a [`SHOP_ITEMS_LIB`] item
//!   id and a count), a prerequisite recipe and story gates. Each points at
//!   an entity-library prototype (`arkArchetype` = its `disLibItemId`), whose
//!   entity name is how scripts name the recipe and whose `CRecipe`
//!   component holds the kind ([`KIND_POTION`] ...), the effects (bonus
//!   plans and a potion's duration) and the name crafting events carry.
//! * Ingredients are shop items: loot items (skins, plant parts) or gadgets
//!   (`selGadgetType`). Loot comes from an entity's
//!   `CFCXPawnLootComponent`, which names loot groups ([`LOOT_GROUPS`]) of
//!   items. The table keeps where every ingredient drops: animals by their
//!   skeleton, other entities (plant pickups) by prototype name.

use std::collections::{BTreeSet, HashMap};

use anyhow::{bail, Result};

use crate::fcb::Object;

/// The skill tree in the cache.
pub const SKILLS_REL: &str = "progression/skills.txt";
/// Crafting in the cache.
pub const RECIPES_REL: &str = "progression/recipes.txt";

/// The engine's object templates (the ability and bonus services).
pub const TEMPLATES: &str = r"generated\nomadobjecttemplates.fcb";
/// The outposts' flag controller (the script that hands out a taken
/// outpost's bonus XP), in the script pack.
pub const FLAG_CONTROLLER: &str = "domino/library/sp/lib_common.flag_controler.lua";

/// A taken outpost's bonus XP from its flag controller: what it sets when
/// the engine says the outpost was taken undetected, and with no alarm
/// raised, as XP rewards (`Outpost/Undetected`, `Outpost/NoAlarm`). Each
/// message's listener goes to a handler whose next step sets the bonus.
pub fn outpost_bonuses(script: &str) -> Vec<(String, f32)> {
    // The body of `function export:<name>()`.
    let body = |name: &str| -> Option<&str> {
        let start = script.find(&format!("function export:{name}()"))?;
        let rest = &script[start..];
        Some(&rest[..rest.find("\nend;").unwrap_or(rest.len())])
    };
    // `<field> = self.<handler>;` in a text, the handler's name.
    let handler = |text: &str, field: &str| -> Option<String> {
        let at = text.find(&format!("{field} = self."))? + field.len() + " = self.".len();
        Some(text[at..].split(';').next()?.trim().to_string())
    };
    let bonus = |message: &str| -> Option<f32> {
        // The listener box the message is given to: `l0 = self[N];` before it.
        let at = script.find(&format!("l0.message = \"{message}\";"))?;
        let before = &script[..at];
        let open = before.rfind("l0 = self[")? + "l0 = self[".len();
        let index = &before[open..open + before[open..].find(']')?];
        // That box's creation, its handler for the message.
        let created = script.find(&format!("self[{index}] = cbox:CreateBox("))?;
        let setup = &script[created..];
        let setup = &setup[..setup[1..].find("\nself[").map_or(setup.len(), |e| e + 1)];
        let received = handler(setup, "l0.Received")?;
        // The handler's next step sets the bonus.
        let next = handler(body(&received)?, "l0.Out")?;
        let set = body(&next)?;
        let at = set.find("l0.Integer = ")? + "l0.Integer = ".len();
        set[at..].split(';').next()?.trim().parse().ok()
    };
    [("Outpost/Undetected", "OutpostUndetected"), ("Outpost/NoAlarm", "OutpostNoAlarmsTriggered")]
        .iter()
        .filter_map(|(name, message)| Some((name.to_string(), bonus(message)?)))
        .collect()
}
pub const RECIPE_LIB: &str = r"generated\databases\generic\craftingrecipe.lib";
pub const RECIPE_TYPE_LIB: &str = r"generated\databases\generic\craftingrecipetype.lib";
pub const RECIPE_SUBTYPE_LIB: &str = r"generated\databases\generic\craftingrecipesubtype.lib";
pub const SHOP_ITEMS_LIB: &str = r"generated\databases\generic\shoppingitems.lib";
pub const LOOT_GROUPS: &str = r"generated\databases\generic\lootinggroups_default.obj";
/// The key prefix of missions' items (`Quest items/...`).
pub const QUEST_ITEMS: &str = "Quest items/";
pub const ENTITY_LIBRARY: &str = r"worlds\fc3_main\generated\entitylibrary_full.fcb";

/// `CRecipe` kinds (`selRecipeType`; also the crafting event's type).
pub const KIND_POTION: u32 = 1;
pub const KIND_GEAR: u32 = 2;
pub const KIND_MEDKIT: u32 = 3;
pub const KIND_ARROW: u32 = 4;

const NONE: u32 = u32::MAX;

/// One modifier of a bonus plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Bonus {
    /// The plan's name.
    pub plan: String,
    /// Engine attribute id (`selAttribute`).
    pub attribute: u32,
    pub value: f32,
    /// The value adds to the attribute (else it scales it).
    pub additive: bool,
}

/// A branch of the skill tree (an ability chapter).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Branch {
    pub number: u32,
    /// Internal chapter name.
    pub key: String,
    pub name: String,
    pub description: String,
}

/// An ability in the skill tree.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ability {
    /// As scripts name it.
    pub id: String,
    /// Branch number.
    pub branch: u32,
    /// Skill points to learn it.
    pub cost: u32,
    /// Abilities that must be learned first.
    pub requires: Vec<String>,
    /// It can be bought (else only scripts grant it).
    pub purchasable: bool,
    /// The player can have it at all.
    pub available: bool,
    /// The mission whose completion makes it upgradable (empty: none).
    pub unlock_mission: String,
    /// Scripts make it upgradable.
    pub unlock_script: bool,
    /// Cell in the branch's menu grid.
    pub row: u32,
    pub column: u32,
    /// The number the description formats (`%d`).
    pub value: i32,
    /// Statistic prerequisite: engine statistic id and target (0: none), with
    /// its text.
    pub stat: u32,
    pub stat_target: u32,
    pub stat_text: String,
    pub name: String,
    pub description: String,
    pub bonuses: Vec<Bonus>,
    /// The tattoo part learning it adds to (empty: none).
    pub tattoo_part: String,
}

/// A part of the player's tattoo: the mask ids it covers, in the order they
/// appear (the k-th learned ability of the part shows the k-th id).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TattooPart {
    pub name: String,
    pub ids: Vec<u8>,
}

/// The tattoo's design and its mask (each pixel's part id).
pub const TATTOO_IMAGE: &str = r"graphics\__fc3_graphics\_common\_textures\characters\firstperson\new_player_tattoo.xbt";
pub const TATTOO_MASK: &str = r"graphics\__fc3_graphics\_common\_textures\characters\firstperson\new_player_tattoo_mask.xbt";
/// The downloadable tribal tattoo the right arm's material names in the same
/// slot (not part of the game without it).
pub const TATTOO_DLC_IMAGE: &str = r"graphics\__fc3_graphics\_common\_textures\characters\firstperson\tribtattoo_dlc.xbt";

/// The skill tree and XP levels.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SkillTree {
    pub branches: Vec<Branch>,
    pub abilities: Vec<Ability>,
    /// Total XP needed for each level after the first.
    pub xp_levels: Vec<u32>,
    /// The tattoo's parts.
    pub tattoo: Vec<TattooPart>,
    /// XP rewards (`XPRewards`), by name: kill multipliers (`Standard/...`,
    /// `Takedown/...`, of the victim's own XP) and flat amounts
    /// (`RadioTower`, `SafeHouse`, `DisableAlarm`).
    pub xp_rewards: Vec<(String, f32)>,
}

/// The engine's names for the kill XP multipliers, by group.
const XP_STANDARD: [&str; 4] = ["StealthKill", "KillWithHoldBreath", "KillWithHeadShotModifier", "KillWithExplosion"];
const XP_TAKEDOWN: [&str; 12] = [
    "StandardTakedown",
    "HeavyTakedown",
    "TakedownFromAbove",
    "TakedownFromAboveDouble",
    "TakedownFromWater",
    "TakedownFromWaterDouble",
    "TakedownFromLedge",
    "TakedownFromLedgeDouble",
    "TakedownPistolMarksman",
    "TakedownKnifeThrow",
    "TakedownChain",
    "TakedownGrenadePinPull",
];

/// A recipe type or subtype (a crafting menu tab or group).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Category {
    pub id: u32,
    pub sort: u32,
    pub key: String,
    pub name: String,
}

/// A shop item: one a recipe takes, a supply the shop sells, or loot.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub id: u32,
    /// Localized name id used by `GetLootQuantity` and `CPawnLootItemEvent`.
    pub name_id: u32,
    /// Looted (skins, plant parts).
    pub loot: bool,
    pub rare: bool,
    /// Engine gadget type for gadgets (grenades...), 0 for others.
    pub gadget: u32,
    pub key: String,
    pub name: String,
    /// Price in the shop (what loot sells for), in dollars.
    pub cost: u32,
    /// Rounds (or charges) one purchase gives.
    pub amount: u32,
    /// The shop subcategory it is sold under ([`SHOP_AMMO`],
    /// [`SHOP_CONSUMABLES`]...; `u32::MAX` when not sold).
    pub shop: u32,
    /// The engine's item type.
    pub kind: u32,
    pub sort: u32,
    /// The script that unlocks it in the shop (empty: always there).
    pub unlock: String,
    pub description: String,
    /// Armour a body-armour item gives (its pickup's armour value).
    pub armour: f32,
    /// XP for picking it up (collectibles).
    pub xp: u32,
    /// Its icon in the menus (`sShopIconName`): an exported symbol of the
    /// pages that list it (`TX_AnimalHideBoar`), or a shared one
    /// (`AnimalLoot`).
    pub icon: String,
}

/// The shop subcategories supplies are sold under.
pub const SHOP_AMMO: u32 = 82;
pub const SHOP_CONSUMABLES: u32 = 158;
/// The shop category supplies are in, and the one loot is sold from.
const SHOP_CATEGORY_GEAR: u32 = 11;
const SHOP_CATEGORY_LOOT: u32 = 12;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Recipe {
    /// As scripts name it (the archetype's entity name).
    pub id: String,
    /// [`KIND_POTION`], [`KIND_GEAR`], ...
    pub kind: u32,
    pub category: u32,
    pub subcategory: u32,
    pub sort: u32,
    /// Seconds a potion lasts (0 for others).
    pub duration: f32,
    /// Extra health slots a syringe gives (on top, and not regenerating).
    pub health_slots: u32,
    /// (max, increment) per level.
    pub levels: Vec<(u32, u32)>,
    /// (item id, count).
    pub ingredients: Vec<(u32, u32)>,
    /// A recipe that must be crafted first (empty: none).
    pub requires: String,
    pub unlock_mission: String,
    pub unlock_script: bool,
    /// The crafting menu's name.
    pub key: String,
    /// The name crafting events carry (`CRecipe` `sName`).
    pub label: String,
    pub name: String,
    pub description: String,
    pub bonuses: Vec<Bonus>,
}

/// One of the game's loot groups. Everything in it is rolled on its own.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LootGroup {
    pub id: u32,
    /// Percent chance of cash, and its least and most (dollars).
    pub cash: (f32, u32, u32),
    /// (item id, percent chance).
    pub items: Vec<(u32, f32)>,
    /// Ammunition and gadgets, as the shop item that sells them: (item id,
    /// percent chance, least, most).
    pub supplies: Vec<(u32, f32, u32, u32)>,
}

/// Where loot items drop.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// Keyed by skeleton path (animals), else by prototype name.
    pub skeleton: bool,
    /// Lower-case skeleton path, or the prototype name (world entities are
    /// named after it).
    pub key: String,
    /// The prototype's full name.
    pub prototype: String,
    /// (item id, percent chance).
    pub items: Vec<(u32, f32)>,
    /// The loot groups it rolls: (group id, percent chance).
    pub groups: Vec<(u32, f32)>,
    /// The graphic kit its characters wear (0: none).
    pub kit: u64,
    /// XP for killing it (`XpGiven`; 0: none).
    pub xp: u32,
}

/// Everything crafting needs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Crafting {
    pub types: Vec<Category>,
    pub subtypes: Vec<Category>,
    pub items: Vec<Item>,
    pub recipes: Vec<Recipe>,
    pub sources: Vec<Source>,
    /// The loot groups sources draw from.
    pub groups: Vec<LootGroup>,
}

// ---------------------------------------------------------------- fields ---

/// An unsigned field of any width up to 8 bytes (little endian).
fn uint(o: &Object, name: &str) -> Option<u64> {
    let b = o.field(name)?;
    if b.is_empty() || b.len() > 8 {
        return None;
    }
    Some(b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
}

fn u32f(o: &Object, name: &str) -> u32 {
    uint(o, name).map(|v| v as u32).unwrap_or(NONE)
}

/// A count or index field (0 when missing or unset).
fn num(o: &Object, name: &str) -> u32 {
    Some(u32f(o, name)).filter(|&v| v != NONE).unwrap_or(0)
}

fn text(o: &Object, name: &str) -> String {
    o.str(name).unwrap_or_default()
}

/// A field that is either a flag byte or a string (set when not empty).
fn flag(o: &Object, name: &str) -> bool {
    match o.field(name) {
        Some([b]) => *b != 0,
        Some(b) => !crate::fcb::bytes_to_string(b).is_empty(),
        None => false,
    }
}

fn located(strings: &dyn Fn(u32) -> String, id: u32) -> String {
    if id == NONE || id == 0 {
        String::new()
    } else {
        strings(id)
    }
}

/// A template by its `Name` among the templates file's objects.
fn template<'a>(root: &'a Object, name: &str) -> Option<&'a Object> {
    root.children.iter().find(|c| c.str("Name").as_deref() == Some(name)).and_then(|c| c.child("Template")).map(|t| t.as_ref())
}

// ----------------------------------------------------------- bonus plans ---

/// Every bonus plan of the `BonusService` template, by name hash: (name,
/// modifiers).
pub fn bonus_plans(templates: &Object) -> HashMap<u32, (String, Vec<Bonus>)> {
    let mut out = HashMap::new();
    let Some(service) = template(templates, "BonusService") else { return out };
    service.walk(&mut |o, _| {
        let Some(hash) = uint(o, "name").map(|h| h as u32) else { return };
        if !o.is("BonusPlan") || hash == NONE || hash == 0 {
            return;
        }
        let plan = text(o, "text_name");
        let mut bonuses = Vec::new();
        o.walk(&mut |b, _| {
            if !b.is("Bonus") {
                return;
            }
            let attribute = u32f(b, "selAttribute");
            let difficulty = u32f(b, "iDifficultyLevel");
            if attribute == 0 || attribute == NONE || (difficulty != NONE && difficulty != 0) {
                return;
            }
            bonuses.push(Bonus { plan: plan.clone(), attribute, value: b.f32("fValue").unwrap_or(0.0), additive: u32f(b, "selValueType") == 1 });
        });
        out.insert(hash, (plan, bonuses));
    });
    out
}

/// The modifiers of plans named by hash (unknown ones are skipped).
fn plan_bonuses(plans: &HashMap<u32, (String, Vec<Bonus>)>, hashes: &[u32]) -> Vec<Bonus> {
    let mut seen = BTreeSet::new();
    hashes.iter().filter(|h| seen.insert(**h)).filter_map(|h| plans.get(h)).flat_map(|(_, b)| b.iter().cloned()).collect()
}

/// Plan hashes listed under an object (`BonusPlans/BonusPlanEntry`, or a
/// `bonusPlan` field of its own).
fn plan_hashes(o: &Object) -> Vec<u32> {
    let mut out = Vec::new();
    o.walk(&mut |c, _| {
        for name in ["BonusPlan", "bonusPlan"] {
            if let Some(h) = uint(c, name).map(|h| h as u32).filter(|&h| h != NONE && h != 0) {
                out.push(h);
            }
        }
    });
    out
}

// ------------------------------------------------------------ skill tree ---

/// The skill tree from the templates file. `strings` resolves localised
/// string ids ("" when unknown). Chapters without a localised name (tattoo
/// bookkeeping) are left out.
pub fn skill_tree(templates: &Object, strings: &dyn Fn(u32) -> String) -> Result<SkillTree> {
    let Some(sp) = template(templates, "AbilityService").and_then(|t| t.child("Single_Player")) else {
        bail!("no single-player ability service template");
    };
    let plans = bonus_plans(templates);
    // Ability properties by name hash.
    struct Props<'a> {
        id: String,
        props: Option<&'a Object>,
        plans: Vec<u32>,
    }
    let mut props: HashMap<u32, Props> = HashMap::new();
    if let Some(list) = sp.child("AbilityList") {
        for a in list.children_named("Ability") {
            let hash = u32f(a, "name");
            let id = text(a, "text_name");
            if hash == NONE || id.is_empty() {
                continue;
            }
            let p = a.child("Properties").map(|p| p.as_ref());
            props.insert(hash, Props { id, props: p, plans: p.map(plan_hashes).unwrap_or_default() });
        }
    }
    let mut tree = SkillTree::default();
    let chapters = sp.child("Chapters").and_then(|c| c.child("ChapterList"));
    for ch in chapters.iter().flat_map(|c| c.children_named("Chapters")) {
        let number = u32f(ch, "iChapterNumber");
        let name = located(strings, u32f(ch, "locidLocalizedChapterName"));
        if number == NONE || name.is_empty() {
            continue;
        }
        tree.branches.push(Branch { number, key: text(ch, "chapterName"), name, description: located(strings, u32f(ch, "locidLocalizedChapterDescription")) });
        let skills = ch.child("BasicAbilities").and_then(|b| b.child("SkillsList"));
        for s in skills.iter().flat_map(|l| l.children_named("AbilityType")) {
            let hash = u32f(s, "abilityName");
            let Some(p) = props.get(&hash) else { continue };
            let requires = [("dependantAbility1", "text_dependantAbility1"), ("dependantAbility2", "text_dependantAbility2")]
                .iter()
                .filter_map(|(h, t)| {
                    let dep = u32f(s, h);
                    (dep != NONE && dep != 0).then(|| props.get(&dep).map(|d| d.id.clone()).unwrap_or_else(|| text(s, t)))
                })
                .filter(|d| !d.is_empty())
                .collect();
            let stat = s.child("preReq").map(|r| u32f(r, "selStatisticType")).unwrap_or(NONE);
            let pp = p.props;
            let prop = |name: &str| pp.map(|o| u32f(o, name)).unwrap_or(NONE);
            tree.abilities.push(Ability {
                id: p.id.clone(),
                branch: number,
                cost: pp.map(|o| num(o, "iMaxSkillPoint")).unwrap_or(0).clamp(1, 1000),
                requires,
                purchasable: s.bool("bCanPurchase").unwrap_or(true),
                available: s.bool("bAvailable").unwrap_or(true),
                unlock_mission: text(s, "UnlockOnMissionComplete"),
                unlock_script: flag(s, "UnlockFromScript"),
                row: num(s, "iRowIndex"),
                column: num(s, "iColumnIndex"),
                value: pp.and_then(|o| uint(o, "iDescriptionInt")).map(|v| v as u32 as i32).unwrap_or(0),
                stat: if stat == NONE { 0 } else { stat },
                stat_target: if stat == NONE { 0 } else { num(s, "preReqValue") },
                stat_text: if stat == NONE { String::new() } else { located(strings, u32f(s, "locidPreReqStringId")) },
                name: located(strings, prop("locidLocalizedSkillName")),
                description: located(strings, prop("locidLocalizedDescription")),
                bonuses: plan_bonuses(&plans, &p.plans),
                // (The part's name as text; field D1340889.)
                tattoo_part: if s.bool("bIsAddedToTattoo").unwrap_or(false) { s.str_h(0xD134_0889).unwrap_or_default() } else { String::new() },
            });
        }
    }
    // The tattoo's parts (their names as text, field C71C615B), with the
    // list's closing 0 left out.
    sp.walk(&mut |o, _| {
        if !o.is("TattooPart") {
            return;
        }
        let Some(name) = o.str_h(0xC71C_615B).filter(|n| !n.is_empty()) else { return };
        let mut ids = Vec::new();
        o.walk(&mut |i, _| {
            if i.is("Id") {
                if let Some(v) = uint(i, "value").filter(|&v| v > 0 && v < 256) {
                    ids.push(v as u8);
                }
            }
        });
        tree.tattoo.push(TattooPart { name, ids });
    });
    if let Some(levels) = sp.child("XPLevels").and_then(|x| x.child("LevelsList")) {
        tree.xp_levels = levels.children_named("XPLevel").map(|l| u32f(l, "iXPNeeded")).filter(|&x| x != 0 && x != NONE).collect();
    }
    if let Some(rewards) = sp.child("XPRewards") {
        let abilities = rewards.child("ForAbilities");
        for (group, names) in [("Standard", &XP_STANDARD[..]), ("Takedown", &XP_TAKEDOWN[..])] {
            let Some(g) = abilities.and_then(|a| a.child(group)) else { continue };
            for name in names {
                if let Some(v) = g.child(name).and_then(|o| o.f32("fXPBaseMultiplier")) {
                    tree.xp_rewards.push((format!("{group}/{name}"), v));
                }
            }
        }
        for (object, field, name) in [("RadioTower", "UnlockRadioTower", "RadioTower"), ("SafeHouse", "UnlockSafeHouse", "SafeHouse"), ("SafeHouse", "DisableAlarm", "DisableAlarm")] {
            if let Some(v) = rewards.child(object).and_then(|o| uint(o, field)).filter(|&v| v != NONE as u64) {
                tree.xp_rewards.push((name.into(), v as f32));
            }
        }
    }
    Ok(tree)
}

// -------------------------------------------------------------- crafting ---

/// The parsed game files crafting is read from.
pub struct CraftingFiles<'a> {
    pub recipes: &'a Object,
    pub types: &'a Object,
    pub subtypes: &'a Object,
    pub shop_items: &'a Object,
    pub loot_groups: &'a Object,
    pub library: &'a Object,
    pub templates: &'a Object,
}

fn categories(lib: &Object, strings: &dyn Fn(u32) -> String) -> Vec<Category> {
    let mut out: Vec<Category> = lib
        .children
        .iter()
        .filter_map(|e| {
            let id = u32f(e, "disId");
            (id != NONE).then(|| Category { id, sort: num(e, "iSortId"), key: text(e, "text_hidName"), name: located(strings, u32f(e, "locidItemNameId")) })
        })
        .collect();
    out.sort_by_key(|c| (c.sort, c.id));
    out
}

/// Crafting from the game files. `strings` resolves localised string ids.
pub fn crafting(f: &CraftingFiles, strings: &dyn Fn(u32) -> String) -> Crafting {
    let plans = bonus_plans(f.templates);
    // Library prototypes: by item id those with a recipe; and those that
    // drop loot.
    struct Proto<'a> {
        name: String,
        entity: String,
        components: Option<&'a Object>,
    }
    let mut by_id: HashMap<u64, Proto> = HashMap::new();
    let mut looters: Vec<Proto> = Vec::new();
    // Armour pickups' values, by item id.
    let mut armour: HashMap<u64, f32> = HashMap::new();
    f.library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let components = e.child("Components").map(|c| c.as_ref());
        let has = |n: &str| components.is_some_and(|c| c.child(n).is_some());
        let p = || Proto { name: text(o, "Name"), entity: text(e, "hidName"), components };
        if has("CRecipe") {
            if let Some(id) = uint(o, "disLibItemId") {
                by_id.insert(id, p());
            }
        }
        if has("CFCXPawnLootComponent") {
            looters.push(p());
        }
        if let (Some(id), Some(value)) = (uint(o, "disLibItemId"), components.and_then(|c| c.child("CPickupArmor")).and_then(|a| a.f32("fArmorValue"))) {
            armour.insert(id, value);
        }
    });
    let mut out = Crafting { types: categories(f.types, strings), subtypes: categories(f.subtypes, strings), ..Default::default() };
    for e in &f.recipes.children {
        let category = u32f(e, "cftrcptpeType");
        let Some(proto) = uint(e, "arkArchetype").and_then(|a| by_id.get(&a)) else { continue };
        if category == 255 || category == NONE || proto.entity.is_empty() {
            continue;
        }
        let recipe = proto.components.and_then(|c| c.child("CRecipe"));
        // The effects: the first effect naming a bonus plan, and a
        // potion's duration.
        let effects: Vec<&Object> =
            recipe.and_then(|r| r.child("EffectList")).map(|l| l.children_named("Effect").map(|e| e.as_ref()).collect()).unwrap_or_default();
        let hashes = effects.iter().map(|e| plan_hashes(e)).find(|h| !h.is_empty()).unwrap_or_default();
        let duration = effects.iter().filter_map(|e| e.child("Properties").and_then(|p| p.f32("fDuration"))).find(|d| *d > 0.0).unwrap_or(0.0);
        let health_slots = effects.iter().filter_map(|e| e.child("Properties").and_then(|p| uint(p, "iNumSpecialHealthSlots"))).find(|&n| n > 0 && n < 64).unwrap_or(0) as u32;
        let ingredients = e
            .child("Requirements")
            .iter()
            .flat_map(|r| r.children_named("Requirement"))
            .map(|r| (u32f(r, "shpLootObject"), u32f(r, "iNeeded")))
            .filter(|&(item, n)| item != 0xFF && item != NONE && n != 0 && n != NONE)
            .collect();
        let levels = e
            .child("Levels")
            .iter()
            .flat_map(|l| l.children_named("Level"))
            .map(|l| (u32f(l, "iMax"), u32f(l, "iIncrement")))
            .filter(|&(m, i)| (m != 0 || i != 0) && m != NONE && i != NONE)
            .collect();
        let requires = uint(e, "arkPreRequisiteRecipe").filter(|&a| a != 0).and_then(|a| by_id.get(&a)).map(|p| p.entity.clone()).unwrap_or_default();
        out.recipes.push(Recipe {
            id: proto.entity.clone(),
            kind: recipe.map(|r| u32f(r, "selRecipeType")).filter(|&k| k != NONE).unwrap_or(0),
            category,
            subcategory: num(e, "cftrcpsubtpeSubType"),
            sort: num(e, "iSortId"),
            duration,
            health_slots,
            levels,
            ingredients,
            requires,
            unlock_mission: text(e, "UnlockOnMissionComplete"),
            unlock_script: flag(e, "UnlockFromScript"),
            key: text(e, "text_hidName"),
            label: recipe.map(|r| text(r, "sName")).unwrap_or_default(),
            name: located(strings, u32f(e, "locidItemNameId")),
            description: located(strings, u32f(e, "locidDescriptionId")),
            bonuses: plan_bonuses(&plans, &hashes),
        });
    }
    out.recipes.sort_by(|a, b| (a.category, a.subcategory, a.sort, &a.id).cmp(&(b.category, b.subcategory, b.sort, &b.id)));
    // The shop items recipes take, the supplies the shop sells, and loot.
    let ingredients: BTreeSet<u32> = out.recipes.iter().flat_map(|r| r.ingredients.iter().map(|i| i.0)).collect();
    let mut wanted = BTreeSet::new();
    for e in &f.shop_items.children {
        let id = u32f(e, "disId");
        let (category, shop) = (u32f(e, "shpcatCategory") & 0xFF, u32f(e, "shpscataSubCategory1") & 0xFF);
        let supply = category == SHOP_CATEGORY_GEAR && (shop == SHOP_AMMO || shop == SHOP_CONSUMABLES);
        let loot = e.bool("bIsLootItem").unwrap_or(false);
        let collectible = text(e, "text_hidName").starts_with("Collectibles/");
        // Missions' items (keys, codes, documents), carried by their
        // characters through mission loot groups.
        let quest = text(e, "text_hidName").starts_with(QUEST_ITEMS);
        if id == NONE || !(ingredients.contains(&id) || supply || collectible || quest || (loot && category == SHOP_CATEGORY_LOOT)) {
            continue;
        }
        wanted.insert(id);
        let gadget = u32f(e, "selGadgetType");
        out.items.push(Item {
            id,
            name_id: num(e, "locidItemNameId"),
            loot,
            rare: e.bool("bIsRareItem").unwrap_or(false),
            gadget: if gadget == NONE { 0 } else { gadget },
            key: text(e, "text_hidName"),
            name: located(strings, u32f(e, "locidItemNameId")),
            cost: num(e, "iCost"),
            amount: num(e, "iAmmoAmount"),
            shop: if supply { shop } else { NONE },
            kind: u32f(e, "shpitpeItemType") & 0xFF,
            sort: num(e, "iSortId"),
            unlock: e.str("UnlockFromScript").unwrap_or_default(),
            description: located(strings, u32f(e, "locidDescriptionId")),
            armour: uint(e, "arkArchetype").and_then(|a| armour.get(&a)).copied().unwrap_or(0.0),
            xp: num(e, "iXPOnLoot"),
            icon: text(e, "sShopIconName"),
        });
    }
    out.items.sort_by_key(|i| i.id);
    // Ammunition by its type, and gadgets by their archetype: the shop items
    // that sell them.
    let mut by_ammo: HashMap<u32, u32> = HashMap::new();
    let mut by_gadget: HashMap<u64, u32> = HashMap::new();
    for e in &f.shop_items.children {
        let id = u32f(e, "disId");
        if !out.items.iter().any(|i| i.id == id && i.shop == SHOP_AMMO) {
            continue;
        }
        let gadget = u32f(e, "selGadgetType");
        if gadget != 0 && gadget != NONE || matches!(text(e, "text_hidName").rsplit('/').next(), Some("C4" | "Mine")) {
            if let Some(a) = uint(e, "arkArchetype").filter(|&a| a != 0) {
                by_gadget.entry(a).or_insert(id);
            }
        } else if num(e, "iAmmoAmount") > 0 {
            by_ammo.entry(u32f(e, "ammoAmmoType")).or_insert(id);
        }
    }
    for g in f.loot_groups.children_named("Groups").flat_map(|g| g.children_named("Group")) {
        let mut group = LootGroup {
            id: u32f(g, "hidID"),
            cash: (g.f32("cashPercent").unwrap_or(0.0), num(g, "cashMin"), num(g, "cashMax")),
            items: g
                .child("Items")
                .iter()
                .flat_map(|i| i.children_named("Item"))
                .map(|i| (u32f(i, "ecoItem"), i.f32("Percent").unwrap_or(0.0)))
                .filter(|(item, p)| *p > 0.0 && wanted.contains(item))
                .collect(),
            supplies: Vec::new(),
        };
        let range = |e: &Object, chance: &str| (e.f32(chance).unwrap_or(0.0), num(e, "iMin"), num(e, "iMax"));
        for a in g.child("Ammunitions").iter().flat_map(|a| a.children_named("AmmoEntry")) {
            let (p, lo, hi) = range(a, "fPercent");
            if let Some(&item) = by_ammo.get(&u32f(a, "ammoAmmoType")).filter(|_| p > 0.0) {
                group.supplies.push((item, p, lo, hi));
            }
        }
        for a in g.child("Gadgets").iter().flat_map(|a| a.children_named("GadgetEntry")) {
            let (p, lo, hi) = range(a, "fPercent");
            if let Some(&item) = uint(a, "arkGadgetType").and_then(|k| by_gadget.get(&k)).filter(|_| p > 0.0) {
                group.supplies.push((item, p, lo, hi));
            }
        }
        if group.id != NONE && (group.cash.0 > 0.0 || !group.items.is_empty() || !group.supplies.is_empty()) {
            out.groups.push(group);
        }
    }
    out.groups.sort_by_key(|g| g.id);
    // Loot groups' items (the ingredients among them), by group id.
    let mut groups: HashMap<u32, Vec<(u32, f32)>> = HashMap::new();
    for g in f.loot_groups.children_named("Groups").flat_map(|g| g.children_named("Group")) {
        let items = g
            .child("Items")
            .iter()
            .flat_map(|i| i.children_named("Item"))
            .map(|i| (u32f(i, "ecoItem"), i.f32("Percent").unwrap_or(0.0)))
            .filter(|(item, _)| wanted.contains(item))
            .collect::<Vec<_>>();
        if !items.is_empty() {
            groups.entry(u32f(g, "hidID")).or_default().extend(items);
        }
    }
    for p in &looters {
        let Some(c) = p.components else { continue };
        let Some(loot) = c.child("CFCXPawnLootComponent") else { continue };
        let mut items: Vec<(u32, f32)> = Vec::new();
        let mut rolled: Vec<(u32, f32)> = Vec::new();
        for g in loot.child("LootGroups").iter().flat_map(|l| l.children_named("Group")) {
            let chance = g.f32("percent").unwrap_or(100.0).clamp(0.0, 100.0);
            let id = u32f(g, "lootGroup");
            for &(item, percent) in groups.get(&id).into_iter().flatten() {
                items.push((item, chance * percent.clamp(0.0, 100.0) / 100.0));
            }
            if chance > 0.0 && out.groups.binary_search_by_key(&id, |g| g.id).is_ok() {
                rolled.push((id, chance));
            }
        }
        if items.is_empty() && rolled.is_empty() {
            continue;
        }
        let skeleton = c.child("CAnimationComponent").map(|a| text(a, "text_fileSkeleton").to_ascii_lowercase()).filter(|s| !s.is_empty());
        let kit = c.child("CGraphicKitComponent").and_then(|k| k.u64("kitidGraphicKit")).filter(|&k| k != 0 && k != u64::MAX).unwrap_or(0);
        out.sources.push(Source {
            skeleton: skeleton.is_some(),
            key: skeleton.unwrap_or_else(|| p.name.clone()),
            prototype: p.entity.clone(),
            items,
            groups: rolled,
            kit,
            // (On the AI's counters component; any component carrying it.)
            xp: c.children.iter().find_map(|k| uint(k, "XpGiven")).filter(|&v| v != NONE as u64).unwrap_or(0) as u32,
        });
    }
    out.sources.sort_by(|a, b| (a.skeleton, &a.key, &a.prototype).cmp(&(b.skeleton, &b.key, &b.prototype)));
    out
}

// -------------------------------------------------------------- handbook ---

/// The handbook (`wikiitems.lib`) and its categories, as the cache keeps it.
pub const HANDBOOK_REL: &str = "progression/handbook.txt";
pub const WIKI_ITEMS: &str = r"generated\databases\generic\wikiitems.lib";
pub const WIKI_CATEGORIES: &str = r"generated\databases\generic\encyclopediacategory.lib";
pub const WIKI_SUBCATEGORIES: &str = r"generated\databases\generic\encyclopediasubcategory.lib";
const HANDBOOK_MAGIC: &str = "fc3-handbook 1";

/// A handbook category and the subcategories under it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HandbookCategory {
    pub id: u32,
    pub sort: u32,
    pub key: String,
    pub name: String,
    pub subcategories: Vec<u32>,
}

/// A handbook page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HandbookEntry {
    pub id: u32,
    pub category: u32,
    pub subcategory: u32,
    pub sort: u32,
    /// Open from the start.
    pub at_start: bool,
    /// Opened by a mission's completion, or a script group (empty: neither;
    /// the engine opens it as the player meets what it describes).
    pub unlock_mission: String,
    pub unlock_group: String,
    pub key: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Handbook {
    pub categories: Vec<HandbookCategory>,
    pub subcategories: Vec<Category>,
    pub entries: Vec<HandbookEntry>,
}

/// The handbook from its databases. Pages without a category of their own
/// (the tutorials) go under the tutorial subcategory their name gives; the
/// manual's pages (`bIsEmanual`) are left out.
pub fn handbook(items: &Object, categories: &Object, subcategories: &Object, strings: &dyn Fn(u32) -> String) -> Handbook {
    let mut out = Handbook { subcategories: self::categories(subcategories, strings), ..Default::default() };
    for e in &categories.children {
        let id = u32f(e, "disId");
        if id == NONE {
            continue;
        }
        let subcategories = e
            .child("SubCategoryDep")
            .iter()
            .flat_map(|d| d.children_named("Link"))
            .map(|l| u32f(l, "disencysubcatId"))
            .filter(|&s| s != NONE && s != 0xFF)
            .collect();
        out.categories.push(HandbookCategory {
            id,
            sort: num(e, "iSortId"),
            key: text(e, "text_hidName"),
            name: located(strings, u32f(e, "locidItemNameId")),
            subcategories,
        });
    }
    out.categories.sort_by_key(|c| (c.sort, c.id));
    // A subcategory's category: the one that lists it, else the one its key
    // starts like ("Tutorials/Combat" under "Tutorials/...").
    let parent = |sub: u32| {
        let prefix = |key: &str| key.split('/').next().unwrap_or_default().to_ascii_lowercase();
        out.categories.iter().find(|c| c.subcategories.contains(&sub)).map(|c| c.id).or_else(|| {
            let key = out.subcategories.iter().find(|s| s.id == sub).map(|s| prefix(&s.key))?;
            out.categories.iter().find(|c| prefix(&c.key) == key).map(|c| c.id)
        })
    };
    for e in &items.children {
        if e.bool("bIsEmanual") == Some(true) {
            continue;
        }
        let key = text(e, "text_hidName");
        let (mut category, mut subcategory) = (u32f(e, "encycatEncyclopediaCategory") & 0xFF, u32f(e, "encysubcatEncyclopediaSubCategory") & 0xFF);
        if subcategory == 0xFF {
            // "Tutorials 03 - Combat/..." under "Tutorials/Combat".
            let group = key.split('/').next().unwrap_or_default();
            let Some(topic) = group.strip_prefix("Tutorials").and_then(|g| g.split_once(" - ")).map(|(_, t)| t.trim().to_ascii_lowercase()) else { continue };
            let Some(sub) = out.subcategories.iter().find(|s| s.key.to_ascii_lowercase() == format!("tutorials/{topic}")) else { continue };
            subcategory = sub.id;
            category = parent(sub.id).unwrap_or(0xFF);
        }
        if category == 0xFF {
            category = parent(subcategory).unwrap_or(0xFF);
        }
        let name = located(strings, u32f(e, "locidItemNameId"));
        if name.is_empty() {
            continue;
        }
        out.entries.push(HandbookEntry {
            id: u32f(e, "disId"),
            category,
            subcategory,
            sort: num(e, "iSortId"),
            at_start: e.bool("bUnlockedAtStart").unwrap_or(false),
            unlock_mission: text(e, "sUnlockOnMissionComplete"),
            unlock_group: text(e, "sUnlockFromScriptGroup"),
            key,
            name,
            description: located(strings, u32f(e, "locidDescriptionId")),
        });
    }
    out.entries.sort_by(|a, b| (a.category, a.subcategory, a.sort, &a.name).cmp(&(b.category, b.subcategory, b.sort, &b.name)));
    out
}

impl Handbook {
    pub fn to_text(&self) -> String {
        let mut s = format!("{HANDBOOK_MAGIC}\n");
        for c in &self.categories {
            let subs = c.subcategories.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");
            s += &format!("cat\t{}\t{}\t{}\t{}\t{subs}\n", c.id, c.sort, esc(&c.key), esc(&c.name));
        }
        for c in &self.subcategories {
            s += &format!("sub\t{}\t{}\t{}\t{}\n", c.id, c.sort, esc(&c.key), esc(&c.name));
        }
        for e in &self.entries {
            s += &format!(
                "entry\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                e.id,
                e.category,
                e.subcategory,
                e.sort,
                u8::from(e.at_start),
                esc(&e.unlock_mission),
                esc(&e.unlock_group),
                esc(&e.key),
                esc(&e.name),
                esc(&e.description)
            );
        }
        s
    }

    pub fn from_text(text: &str) -> Result<Handbook> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(HANDBOOK_MAGIC) {
            bail!("not a handbook table");
        }
        let mut h = Handbook::default();
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            let n = |i: usize| f.get(i).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            match f[0] {
                "cat" if f.len() >= 6 => h.categories.push(HandbookCategory {
                    id: n(1),
                    sort: n(2),
                    key: unesc(f[3]),
                    name: unesc(f[4]),
                    subcategories: f[5].split(',').filter_map(|v| v.parse().ok()).collect(),
                }),
                "sub" if f.len() >= 5 => h.subcategories.push(Category { id: n(1), sort: n(2), key: unesc(f[3]), name: unesc(f[4]) }),
                "entry" if f.len() >= 11 => h.entries.push(HandbookEntry {
                    id: n(1),
                    category: n(2),
                    subcategory: n(3),
                    sort: n(4),
                    at_start: f[5] == "1",
                    unlock_mission: unesc(f[6]),
                    unlock_group: unesc(f[7]),
                    key: unesc(f[8]),
                    name: unesc(f[9]),
                    description: unesc(f[10]),
                }),
                _ => {}
            }
        }
        Ok(h)
    }
}

// ------------------------------------------------------------ text form ---

const SKILLS_MAGIC: &str = "fc3-skills 1";
const RECIPES_MAGIC: &str = "fc3-recipes 8";

/// A string as one tab-free, newline-free field.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c => o.push(c),
        }
    }
    o
}

fn unesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('t') => o.push('\t'),
            Some('n') => o.push('\n'),
            Some(c) => o.push(c),
            None => o.push('\\'),
        }
    }
    o
}

fn bonus_line(owner: &str, b: &Bonus) -> String {
    format!("bonus\t{}\t{}\t{}\t{}\t{}\n", esc(owner), esc(&b.plan), b.attribute, b.value, u8::from(b.additive))
}

fn parse_bonus(f: &[&str]) -> Option<(String, Bonus)> {
    Some((unesc(f.first()?), Bonus { plan: unesc(f.get(1)?), attribute: f.get(2)?.parse().ok()?, value: f.get(3)?.parse().ok()?, additive: *f.get(4)? == "1" }))
}

impl SkillTree {
    pub fn to_text(&self) -> String {
        let mut s = format!("{SKILLS_MAGIC}\n");
        s += &format!("xp\t{}\n", self.xp_levels.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" "));
        for b in &self.branches {
            s += &format!("branch\t{}\t{}\t{}\t{}\n", b.number, esc(&b.key), esc(&b.name), esc(&b.description));
        }
        for a in &self.abilities {
            s += &format!(
                "ability\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                esc(&a.id),
                a.branch,
                a.cost,
                a.requires.iter().map(|r| esc(r)).collect::<Vec<_>>().join(","),
                u8::from(a.purchasable),
                u8::from(a.available),
                esc(&a.unlock_mission),
                u8::from(a.unlock_script),
                a.row,
                a.column,
                a.value,
                a.stat,
                a.stat_target,
                esc(&a.stat_text),
                esc(&a.name),
                esc(&a.description)
            );
            if !a.tattoo_part.is_empty() {
                s += &format!("tattoopart\t{}\t{}\n", esc(&a.id), esc(&a.tattoo_part));
            }
            for b in &a.bonuses {
                s += &bonus_line(&a.id, b);
            }
        }
        for t in &self.tattoo {
            s += &format!("tattoo\t{}\t{}\n", esc(&t.name), t.ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","));
        }
        for (name, v) in &self.xp_rewards {
            s += &format!("xpreward\t{}\t{v}\n", esc(name));
        }
        s
    }

    pub fn from_text(text: &str) -> Result<SkillTree> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(SKILLS_MAGIC) {
            bail!("not a skills table");
        }
        let mut t = SkillTree::default();
        let mut index: HashMap<String, usize> = HashMap::new();
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            match f[0] {
                "xp" => t.xp_levels = f.get(1).map(|v| v.split_whitespace().filter_map(|x| x.parse().ok()).collect()).unwrap_or_default(),
                "xpreward" if f.len() >= 3 => {
                    if let Ok(v) = f[2].parse() {
                        t.xp_rewards.push((unesc(f[1]), v));
                    }
                }
                "branch" if f.len() >= 5 => {
                    t.branches.push(Branch { number: f[1].parse().unwrap_or(0), key: unesc(f[2]), name: unesc(f[3]), description: unesc(f[4]) })
                }
                "ability" if f.len() >= 17 => {
                    let n = |i: usize| f[i].parse::<u32>().unwrap_or(0);
                    let a = Ability {
                        id: unesc(f[1]),
                        branch: n(2),
                        cost: n(3),
                        requires: f[4].split(',').filter(|r| !r.is_empty()).map(unesc).collect(),
                        purchasable: f[5] == "1",
                        available: f[6] == "1",
                        unlock_mission: unesc(f[7]),
                        unlock_script: f[8] == "1",
                        row: n(9),
                        column: n(10),
                        value: f[11].parse().unwrap_or(0),
                        stat: n(12),
                        stat_target: n(13),
                        stat_text: unesc(f[14]),
                        name: unesc(f[15]),
                        description: unesc(f[16]),
                        bonuses: Vec::new(),
                        tattoo_part: String::new(),
                    };
                    index.insert(a.id.clone(), t.abilities.len());
                    t.abilities.push(a);
                }
                "bonus" => {
                    if let Some((owner, b)) = parse_bonus(&f[1..]) {
                        if let Some(&i) = index.get(&owner) {
                            t.abilities[i].bonuses.push(b);
                        }
                    }
                }
                "tattoopart" if f.len() >= 3 => {
                    if let Some(&i) = index.get(&unesc(f[1])) {
                        t.abilities[i].tattoo_part = unesc(f[2]);
                    }
                }
                "tattoo" if f.len() >= 3 => {
                    t.tattoo.push(TattooPart { name: unesc(f[1]), ids: f[2].split(',').filter_map(|v| v.parse().ok()).collect() });
                }
                _ => {}
            }
        }
        Ok(t)
    }
}

impl Crafting {
    pub fn to_text(&self) -> String {
        let mut s = format!("{RECIPES_MAGIC}\n");
        for (kind, list) in [("type", &self.types), ("subtype", &self.subtypes)] {
            for c in list {
                s += &format!("{kind}\t{}\t{}\t{}\t{}\n", c.id, c.sort, esc(&c.key), esc(&c.name));
            }
        }
        for i in &self.items {
            s += &format!(
                "item\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                i.id,
                u8::from(i.loot),
                u8::from(i.rare),
                i.gadget,
                esc(&i.key),
                esc(&i.name),
                i.name_id,
                i.cost,
                i.amount,
                i.shop,
                i.kind,
                i.sort,
                esc(&i.unlock),
                esc(&i.description),
                i.armour,
                i.xp,
                esc(&i.icon)
            );
        }
        for r in &self.recipes {
            s += &format!(
                "recipe\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                esc(&r.id),
                r.kind,
                r.category,
                r.subcategory,
                r.sort,
                r.duration,
                r.levels.iter().map(|(m, i)| format!("{m}/{i}")).collect::<Vec<_>>().join(","),
                r.ingredients.iter().map(|(i, n)| format!("{i}:{n}")).collect::<Vec<_>>().join(","),
                esc(&r.requires),
                esc(&r.unlock_mission),
                u8::from(r.unlock_script),
                esc(&r.key),
                esc(&r.label),
                esc(&r.name),
                esc(&r.description),
                r.health_slots
            );
            for b in &r.bonuses {
                s += &bonus_line(&r.id, b);
            }
        }
        for g in &self.groups {
            s += &format!(
                "group\t{}\t{}\t{}\t{}\t{}\t{}\n",
                g.id,
                g.cash.0,
                g.cash.1,
                g.cash.2,
                g.items.iter().map(|(i, p)| format!("{i}:{p}")).collect::<Vec<_>>().join(","),
                g.supplies.iter().map(|(i, p, lo, hi)| format!("{i}:{p}:{lo}:{hi}")).collect::<Vec<_>>().join(",")
            );
        }
        for src in &self.sources {
            s += &format!(
                "source\t{}\t{}\t{}\t{}\t{}\t{:x}\t{}\n",
                if src.skeleton { "skeleton" } else { "entity" },
                esc(&src.key),
                esc(&src.prototype),
                src.items.iter().map(|(i, p)| format!("{i}:{p}")).collect::<Vec<_>>().join(","),
                src.groups.iter().map(|(g, p)| format!("{g}:{p}")).collect::<Vec<_>>().join(","),
                src.kit,
                src.xp
            );
        }
        s
    }

    pub fn from_text(text: &str) -> Result<Crafting> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(RECIPES_MAGIC) {
            bail!("not a recipes table");
        }
        let mut c = Crafting::default();
        let mut index: HashMap<String, usize> = HashMap::new();
        let pairs =
            |s: &str| -> Vec<(String, String)> { s.split(',').filter_map(|p| p.split_once([':', '/'])).map(|(a, b)| (a.to_string(), b.to_string())).collect() };
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            let n = |i: usize| f.get(i).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            match f[0] {
                "type" | "subtype" if f.len() >= 5 => {
                    let cat = Category { id: n(1), sort: n(2), key: unesc(f[3]), name: unesc(f[4]) };
                    if f[0] == "type" {
                        c.types.push(cat);
                    } else {
                        c.subtypes.push(cat);
                    }
                }
                "item" if f.len() >= 16 => c.items.push(Item {
                    id: n(1),
                    name_id: n(7),
                    loot: f[2] == "1",
                    rare: f[3] == "1",
                    gadget: n(4),
                    key: unesc(f[5]),
                    name: unesc(f[6]),
                    cost: n(8),
                    amount: n(9),
                    shop: n(10),
                    kind: n(11),
                    sort: n(12),
                    unlock: unesc(f[13]),
                    description: unesc(f[14]),
                    armour: f[15].parse().unwrap_or(0.0),
                    xp: f.get(16).and_then(|v| v.parse().ok()).unwrap_or(0),
                    icon: f.get(17).map(|v| unesc(v)).unwrap_or_default(),
                }),
                "item" => bail!("item entry lacks its shop fields"),
                "recipe" if f.len() >= 16 => {
                    let r = Recipe {
                        id: unesc(f[1]),
                        kind: n(2),
                        category: n(3),
                        subcategory: n(4),
                        sort: n(5),
                        duration: f[6].parse().unwrap_or(0.0),
                        levels: pairs(f[7]).into_iter().filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).collect(),
                        ingredients: pairs(f[8]).into_iter().filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).collect(),
                        requires: unesc(f[9]),
                        unlock_mission: unesc(f[10]),
                        unlock_script: f[11] == "1",
                        key: unesc(f[12]),
                        label: unesc(f[13]),
                        name: unesc(f[14]),
                        description: unesc(f[15]),
                        health_slots: f.get(16).and_then(|v| v.parse().ok()).unwrap_or(0),
                        bonuses: Vec::new(),
                    };
                    index.insert(r.id.clone(), c.recipes.len());
                    c.recipes.push(r);
                }
                "bonus" => {
                    if let Some((owner, b)) = parse_bonus(&f[1..]) {
                        if let Some(&i) = index.get(&owner) {
                            c.recipes[i].bonuses.push(b);
                        }
                    }
                }
                "source" if f.len() >= 5 => c.sources.push(Source {
                    skeleton: f[1] == "skeleton",
                    key: unesc(f[2]),
                    prototype: unesc(f[3]),
                    items: f[4].split(',').filter_map(|p| p.split_once(':')).filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).collect(),
                    groups: f
                        .get(5)
                        .map(|g| g.split(',').filter_map(|p| p.split_once(':')).filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).collect())
                        .unwrap_or_default(),
                    kit: f.get(6).and_then(|k| u64::from_str_radix(k, 16).ok()).unwrap_or(0),
                    xp: f.get(7).and_then(|v| v.parse().ok()).unwrap_or(0),
                }),
                "group" if f.len() >= 7 => c.groups.push(LootGroup {
                    id: n(1),
                    cash: (f[2].parse().unwrap_or(0.0), n(3), n(4)),
                    items: f[5].split(',').filter_map(|p| p.split_once(':')).filter_map(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).collect(),
                    supplies: f[6]
                        .split(',')
                        .filter_map(|p| {
                            let v: Vec<&str> = p.split(':').collect();
                            Some((v.first()?.parse().ok()?, v.get(1)?.parse().ok()?, v.get(2)?.parse().ok()?, v.get(3)?.parse().ok()?))
                        })
                        .collect(),
                }),
                _ => {}
            }
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hash::name_hash;

    /// A field value: text (NUL-terminated), or raw bytes.
    enum V {
        S(&'static str),
        U8(u8),
        U16(u16),
        U32(u32),
        U64(u64),
        F(f32),
    }

    fn obj(name: &str, fields: Vec<(&str, V)>, children: Vec<Arc<Object>>) -> Arc<Object> {
        let fields = fields
            .into_iter()
            .map(|(k, v)| {
                let b = match v {
                    V::S(s) => {
                        let mut b = s.as_bytes().to_vec();
                        b.push(0);
                        b
                    }
                    V::U8(x) => vec![x],
                    V::U16(x) => x.to_le_bytes().to_vec(),
                    V::U32(x) => x.to_le_bytes().to_vec(),
                    V::U64(x) => x.to_le_bytes().to_vec(),
                    V::F(x) => x.to_le_bytes().to_vec(),
                };
                (name_hash(k), b)
            })
            .collect();
        Arc::new(Object { name_hash: name_hash(name), fields, children })
    }

    fn strings(id: u32) -> String {
        format!("s{id}")
    }

    fn bonus_service() -> Arc<Object> {
        let bonus = |attr: u32, value: f32, add: u32| {
            obj(
                "Bonus",
                vec![("selAttribute", V::U32(attr)), ("fValue", V::F(value)), ("iDifficultyLevel", V::U32(NONE)), ("selValueType", V::U32(add))],
                vec![],
            )
        };
        let plan = |name: &'static str, bonuses: Vec<Arc<Object>>| {
            obj(
                "BonusPlan",
                vec![("text_name", V::S(name)), ("name", V::U32(name_hash(name)))],
                vec![obj("BonusPlan", vec![], vec![obj("Bonuses", vec![], bonuses)])],
            )
        };
        let plans = obj("BonusPlans", vec![], vec![plan("slots", vec![bonus(7, 1.0, 1), bonus(0, 0.0, 0)]), plan("faster", vec![bonus(9, 0.25, 0)])]);
        obj(
            "Service",
            vec![("Name", V::S("BonusService"))],
            vec![obj("Template", vec![], vec![obj("Categories", vec![], vec![obj("Category", vec![("sName", V::S("Skills"))], vec![plans])])])],
        )
    }

    fn templates() -> Arc<Object> {
        let entry = |plan: &str| obj("BonusPlanEntry", vec![("BonusPlan", V::U32(name_hash(plan)))], vec![]);
        let ability = |id: &'static str, points: u32, plan: &str| {
            obj(
                "Ability",
                vec![("text_name", V::S(id)), ("name", V::U32(name_hash(id)))],
                vec![obj(
                    "Properties",
                    vec![
                        ("iMaxSkillPoint", V::U32(points)),
                        ("locidLocalizedSkillName", V::U32(100)),
                        ("locidLocalizedDescription", V::U32(NONE)),
                        ("iDescriptionInt", V::U32(25)),
                    ],
                    vec![obj("BonusPlans", vec![], vec![entry(plan)])],
                )],
            )
        };
        let skill = |id: &str, dep: Option<&str>, mission: &'static str, stat: u32| {
            obj(
                "AbilityType",
                vec![
                    ("abilityName", V::U32(name_hash(id))),
                    ("dependantAbility1", V::U32(dep.map(name_hash).unwrap_or(NONE))),
                    ("dependantAbility2", V::U32(NONE)),
                    ("bCanPurchase", V::U8(1)),
                    ("bAvailable", V::U8(1)),
                    ("UnlockOnMissionComplete", if mission.is_empty() { V::U8(0) } else { V::S(mission) }),
                    ("UnlockFromScript", V::U8(0)),
                    ("iRowIndex", V::U32(1)),
                    ("iColumnIndex", V::U32(2)),
                    ("preReqValue", V::U32(5)),
                    ("locidPreReqStringId", V::U32(300)),
                ],
                vec![obj("preReq", vec![("selStatisticType", V::U32(stat))], vec![])],
            )
        };
        let chapter = |number: u32, name_id: u32, skills: Vec<Arc<Object>>| {
            obj(
                "Chapters",
                vec![
                    ("iChapterNumber", V::U32(number)),
                    ("chapterName", V::S("Internal")),
                    ("locidLocalizedChapterName", V::U32(name_id)),
                    ("locidLocalizedChapterDescription", V::U32(201)),
                ],
                vec![obj("BasicAbilities", vec![], vec![obj("SkillsList", vec![], skills)])],
            )
        };
        let sp = obj(
            "Single_Player",
            vec![],
            vec![
                obj("AbilityList", vec![], vec![ability("A_First", 0, "slots"), ability("A_Second", 2, "faster")]),
                obj(
                    "Chapters",
                    vec![],
                    vec![obj(
                        "ChapterList",
                        vec![],
                        vec![
                            chapter(
                                1,
                                200,
                                vec![
                                    skill("A_First", None, "", NONE),
                                    skill("A_Second", Some("A_First"), "M_010_main", 0x96),
                                    skill("Missing", None, "", NONE),
                                ],
                            ),
                            chapter(4, NONE, vec![skill("A_First", None, "", NONE)]),
                        ],
                    )],
                ),
                obj(
                    "XPLevels",
                    vec![],
                    vec![obj(
                        "LevelsList",
                        vec![],
                        vec![obj("XPLevel", vec![("iXPNeeded", V::U32(1000))], vec![]), obj("XPLevel", vec![("iXPNeeded", V::U32(0))], vec![])],
                    )],
                ),
            ],
        );
        let ability_service = obj("Service", vec![("Name", V::S("AbilityService"))], vec![obj("Template", vec![], vec![sp])]);
        obj("Root", vec![], vec![ability_service, bonus_service()])
    }

    #[test]
    fn reads_outpost_bonuses_from_the_flag_controller() {
        let script = "function export:Init(cbox)
self[50] = cbox:CreateBox(\"Domino/System/MessageListener_v3.lua\");
l0 = self[50];
l0.Received = self.f_50_Received;
self[52] = cbox:CreateBox(\"Domino/System/MessageListener_v3.lua\");
l0 = self[52];
l0.Received = self.f_52_Received;
end;
function export:f_1()
l0 = self[50];
l0.message = \"OutpostUndetected\";
l0 = self[52];
l0.message = \"OutpostNoAlarmsTriggered\";
end;
function export:f_50_Received()
l0.Out = self.f_55_Out;
end;
function export:f_52_Received()
l0.Out = self.f_45_Out;
end;
function export:f_55_Out()
l0.Integer = 1000;
end;
function export:f_45_Out()
l0.Integer = 50;
end;
";
        assert_eq!(outpost_bonuses(script), vec![("Outpost/Undetected".to_string(), 1000.0), ("Outpost/NoAlarm".to_string(), 50.0)]);
    }

    #[test]
    fn reads_the_skill_tree() {
        let t = skill_tree(&templates(), &strings).unwrap();
        assert_eq!(t.branches, vec![Branch { number: 1, key: "Internal".into(), name: "s200".into(), description: "s201".into() }]);
        assert_eq!(t.abilities.len(), 2);
        let (a, b) = (&t.abilities[0], &t.abilities[1]);
        assert_eq!((a.id.as_str(), a.cost, a.requires.len(), a.unlock_mission.as_str(), a.stat), ("A_First", 1, 0, "", 0));
        assert_eq!(a.bonuses, vec![Bonus { plan: "slots".into(), attribute: 7, value: 1.0, additive: true }]);
        assert_eq!((b.cost, b.requires.clone(), b.unlock_mission.as_str()), (2, vec!["A_First".to_string()], "M_010_main"));
        assert_eq!((b.stat, b.stat_target, b.stat_text.as_str(), b.row, b.column, b.value), (0x96, 5, "s300", 1, 2, 25));
        assert_eq!((b.name.as_str(), b.description.as_str()), ("s100", ""));
        assert_eq!(t.xp_levels, vec![1000]);
        assert_eq!(SkillTree::from_text(&t.to_text()).unwrap(), t);
    }

    #[test]
    fn reads_crafting() {
        let lib_entry = |id: u8, key: &'static str| {
            obj("Entry", vec![("disId", V::U8(id)), ("text_hidName", V::S(key)), ("locidItemNameId", V::U32(10 + id as u32)), ("iSortId", V::U32(0))], vec![])
        };
        let types = obj("lib", vec![], vec![lib_entry(2, "Type/Gear")]);
        let subtypes = obj("lib", vec![], vec![lib_entry(3, "Gear/Holsters")]);
        let recipe = |key: &'static str, ark: u64, pre: u64, mission: &'static str, item: u16, n: u32| {
            obj(
                "Entry",
                vec![
                    ("text_hidName", V::S(key)),
                    ("cftrcptpeType", V::U8(2)),
                    ("cftrcpsubtpeSubType", V::U8(3)),
                    ("iSortId", V::U32(1)),
                    ("arkArchetype", V::U64(ark)),
                    ("arkPreRequisiteRecipe", V::U64(pre)),
                    ("UnlockOnMissionComplete", if mission.is_empty() { V::U8(0) } else { V::S(mission) }),
                    ("UnlockFromScript", V::U8(0)),
                    ("locidItemNameId", V::U32(50)),
                    ("locidDescriptionId", V::U32(NONE)),
                ],
                vec![
                    obj(
                        "Requirements",
                        vec![],
                        vec![
                            obj("Requirement", vec![("shpLootObject", V::U16(item)), ("iNeeded", V::U32(n))], vec![]),
                            obj("Requirement", vec![("shpLootObject", V::U16(0xFF)), ("iNeeded", V::U32(0))], vec![]),
                        ],
                    ),
                    obj(
                        "Levels",
                        vec![],
                        vec![
                            obj("Level", vec![("iMax", V::U32(1)), ("iIncrement", V::U32(1))], vec![]),
                            obj("Level", vec![("iMax", V::U32(0)), ("iIncrement", V::U32(0))], vec![]),
                        ],
                    ),
                ],
            )
        };
        let recipes = obj(
            "lib",
            vec![],
            vec![
                recipe("Gear/Holster 1", 0x11, 0, "M_010_main", 1280, 2),
                recipe("Gear/Holster 2", 0x12, 0x11, "", 1281, 1),
                recipe("Gear/Unused", 0x99, 0, "", 1280, 1),
            ],
        );
        let item = |id: u16, key: &'static str, rare: u8| {
            obj(
                "Entry",
                vec![
                    ("disId", V::U16(id)),
                    ("locidItemNameId", V::U32(id as u32 + 1000)),
                    ("text_hidName", V::S(key)),
                    ("bIsLootItem", V::U8(1)),
                    ("bIsRareItem", V::U8(rare)),
                    ("selGadgetType", V::U32(0)),
                ],
                vec![],
            )
        };
        let supply = |id: u16, key: &'static str, sub: u8, cost: u32, amount: u32, ark: u64| {
            obj(
                "Entry",
                vec![
                    ("disId", V::U16(id)),
                    ("text_hidName", V::S(key)),
                    ("shpcatCategory", V::U8(11)),
                    ("shpscataSubCategory1", V::U8(sub)),
                    ("iCost", V::U32(cost)),
                    ("iAmmoAmount", V::U32(amount)),
                    ("arkArchetype", V::U64(ark)),
                    ("ammoAmmoType", V::U32(0xA770)),
                    ("UnlockFromScript", V::S("UNLOCK_Test")),
                ],
                vec![],
            )
        };
        let shop = obj(
            "lib",
            vec![],
            vec![
                item(1280, "Parts/Skin", 0),
                item(1281, "Parts/Rare skin", 1),
                item(7, "Other", 0),
                supply(40, "Ammo/Handgun", SHOP_AMMO as u8, 20, 24, 0),
                supply(41, "Consumables/Body armor A", SHOP_CONSUMABLES as u8, 200, 0, 0x30),
            ],
        );
        let group = |id: u32, item: u32| {
            obj(
                "Group",
                vec![("hidID", V::U32(id))],
                vec![obj(
                    "Items",
                    vec![],
                    vec![obj("Item", vec![("ecoItem", V::U32(item)), ("Percent", V::F(50.0))], vec![]), obj("Item", vec![("ecoItem", V::U32(0))], vec![])],
                )],
            )
        };
        let cash_and_ammo = obj(
            "Group",
            vec![("hidID", V::U32(9)), ("cashPercent", V::F(100.0)), ("cashMin", V::U32(3)), ("cashMax", V::U32(20))],
            vec![obj(
                "Ammunitions",
                vec![],
                vec![obj("AmmoEntry", vec![("ammoAmmoType", V::U32(0xA770)), ("fPercent", V::F(33.0)), ("iMin", V::U32(4)), ("iMax", V::U32(12))], vec![])],
            )],
        );
        let loot = obj("obj", vec![], vec![obj("Groups", vec![], vec![group(5, 1280), group(6, 7), cash_and_ammo])]);
        let effect = |plan: &str| obj("Effect", vec![], vec![obj("Properties", vec![("bonusPlan", V::U32(name_hash(plan)))], vec![])]);
        let crecipe = |label: &'static str| {
            obj(
                "CRecipe",
                vec![("sName", V::S(label)), ("selRecipeType", V::U32(KIND_GEAR))],
                vec![obj("EffectList", vec![], vec![obj("Effect", vec![], vec![]), effect("slots")])],
            )
        };
        let proto = |id: u64, name: &'static str, entity: &'static str, components: Vec<Arc<Object>>| {
            obj(
                "EntityPrototype",
                vec![("disLibItemId", V::U64(id)), ("Name", V::S(name))],
                vec![obj("Entity", vec![("hidName", V::S(entity))], vec![obj("Components", vec![], components)])],
            )
        };
        let looter = |groups: u32, skeleton: &'static str| {
            let mut c = vec![obj(
                "CFCXPawnLootComponent",
                vec![],
                vec![obj("LootGroups", vec![], vec![obj("Group", vec![("percent", V::F(100.0)), ("lootGroup", V::U32(groups))], vec![])])],
            )];
            if !skeleton.is_empty() {
                c.push(obj("CAnimationComponent", vec![("text_fileSkeleton", V::S(skeleton))], vec![]));
            }
            c
        };
        let library = obj(
            "EntityLibraries",
            vec![],
            vec![obj(
                "EntityLibrary",
                vec![],
                vec![
                    proto(0x11, "Gear/Holster 1", "Recipes:Gear/Holster 1", vec![crecipe("Holster 1")]),
                    proto(0x12, "Gear/Holster 2", "Recipes:Gear/Holster 2", vec![crecipe("Holster 2")]),
                    proto(0x20, "Quadrupeds/Goat", "Animals:Quadrupeds/Goat", looter(5, r"Graphics\Goat.skeleton")),
                    proto(0x21, "Pickups/Plant_01", "Interactive:Pickups/Plant_01", looter(5, "")),
                    proto(0x22, "Pickups/Rock", "Interactive:Pickups/Rock", looter(6, "")),
                    proto(0x30, "Pickups/Armor", "Interactive:Pickups/Armor", vec![obj("CPickupArmor", vec![("fArmorValue", V::F(100.0))], vec![])]),
                    proto(0x31, "Merc/Pirate", "NPC:Merc/Pirate", {
                        let mut c = looter(9, "");
                        c.push(obj("CGraphicKitComponent", vec![("kitidGraphicKit", V::U64(0xA773_2017_0000_0700))], vec![]));
                        c
                    }),
                ],
            )],
        );
        let tpl = templates();
        let files =
            CraftingFiles { recipes: &recipes, types: &types, subtypes: &subtypes, shop_items: &shop, loot_groups: &loot, library: &library, templates: &tpl };
        let c = crafting(&files, &strings);
        assert_eq!(c.types, vec![Category { id: 2, sort: 0, key: "Type/Gear".into(), name: "s12".into() }]);
        assert_eq!(c.recipes.len(), 2);
        let (a, b) = (&c.recipes[0], &c.recipes[1]);
        assert_eq!((a.id.as_str(), a.kind, a.category, a.subcategory, a.label.as_str()), ("Recipes:Gear/Holster 1", KIND_GEAR, 2, 3, "Holster 1"));
        assert_eq!(
            (a.ingredients.clone(), a.levels.clone(), a.unlock_mission.as_str(), a.requires.as_str()),
            (vec![(1280, 2)], vec![(1, 1)], "M_010_main", "")
        );
        assert_eq!(a.bonuses, vec![Bonus { plan: "slots".into(), attribute: 7, value: 1.0, additive: true }]);
        assert_eq!((b.requires.as_str(), b.name.as_str(), b.description.as_str()), ("Recipes:Gear/Holster 1", "s50", ""));
        assert_eq!(c.items.iter().map(|i| (i.id, i.rare)).collect::<Vec<_>>(), vec![(40, false), (41, false), (1280, false), (1281, true)]);
        let (ammo, armour) = (&c.items[0], &c.items[1]);
        assert_eq!((ammo.shop, ammo.cost, ammo.amount, ammo.armour, ammo.unlock.as_str()), (SHOP_AMMO, 20, 24, 0.0, "UNLOCK_Test"));
        assert_eq!((armour.shop, armour.cost, armour.armour), (SHOP_CONSUMABLES, 200, 100.0));
        assert_eq!(c.items[2].shop, NONE);
        assert_eq!(c.items[2].name_id, 2280);
        assert_eq!(c.sources.len(), 3);
        assert_eq!((c.sources[0].key.as_str(), c.sources[0].kit, c.sources[0].groups.clone()), ("Merc/Pirate", 0xA773_2017_0000_0700, vec![(9, 100.0)]));
        assert_eq!((c.sources[1].skeleton, c.sources[1].key.as_str(), c.sources[1].items.clone()), (false, "Pickups/Plant_01", vec![(1280, 50.0)]));
        assert_eq!((c.sources[2].skeleton, c.sources[2].key.as_str()), (true, r"graphics\goat.skeleton"));
        let g = c.groups.iter().find(|g| g.id == 9).unwrap();
        assert_eq!((g.cash, g.supplies.clone()), ((100.0, 3, 20), vec![(40, 33.0, 4, 12)]));
        assert_eq!(Crafting::from_text(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn handbook_round_trips() {
        let h = Handbook {
            categories: vec![HandbookCategory { id: 5, sort: 1, key: "Vice guide/Vice guide".into(), name: "Guide".into(), subcategories: vec![12, 20] }],
            subcategories: vec![Category { id: 12, sort: 2, key: "ViceGuide/Characters".into(), name: "People".into() }],
            entries: vec![HandbookEntry {
                id: 7,
                category: 5,
                subcategory: 12,
                sort: 3,
                at_start: false,
                unlock_mission: "JAS_020_main".into(),
                unlock_group: "CHAR_X".into(),
                key: "Characters/X".into(),
                name: "X".into(),
                description: "Line one\nline\ttwo".into(),
            }],
        };
        assert_eq!(Handbook::from_text(&h.to_text()).unwrap(), h);
        assert!(Handbook::from_text("fc3-recipes 6\n").is_err());
    }

    #[test]
    fn text_fields_escape() {
        let t = SkillTree {
            branches: vec![Branch { number: 1, key: "k".into(), name: "a\tb".into(), description: "line\nnext \\ done".into() }],
            abilities: vec![Ability {
                id: "X".into(),
                requires: vec!["Y".into(), "Z".into()],
                description: "50%%\n[Run]".into(),
                tattoo_part: "Heron1".into(),
                ..Default::default()
            }],
            xp_levels: vec![1, 2, 3],
            tattoo: vec![TattooPart { name: "Heron1".into(), ids: vec![3, 4] }, TattooPart { name: "Relics".into(), ids: vec![] }],
            xp_rewards: vec![("Takedown/StandardTakedown".into(), 3.0), ("RadioTower".into(), 500.0)],
        };
        assert_eq!(SkillTree::from_text(&t.to_text()).unwrap(), t);
        assert!(SkillTree::from_text("other").is_err());
        assert!(Crafting::from_text("fc3-skills 1\n").is_err());
        assert!(Crafting::from_text("fc3-recipes 1\n").is_err());
        assert!(Crafting::from_text("fc3-recipes 2\n").is_err());
        assert!(Crafting::from_text("fc3-recipes 6\nitem\t1\t1\t0\t0\tkey\tname\t5\n").is_err());
    }
}
