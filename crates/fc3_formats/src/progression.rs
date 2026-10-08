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
pub const RECIPE_LIB: &str = r"generated\databases\generic\craftingrecipe.lib";
pub const RECIPE_TYPE_LIB: &str = r"generated\databases\generic\craftingrecipetype.lib";
pub const RECIPE_SUBTYPE_LIB: &str = r"generated\databases\generic\craftingrecipesubtype.lib";
pub const SHOP_ITEMS_LIB: &str = r"generated\databases\generic\shoppingitems.lib";
pub const LOOT_GROUPS: &str = r"generated\databases\generic\lootinggroups_default.obj";
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
}

/// The skill tree and XP levels.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SkillTree {
    pub branches: Vec<Branch>,
    pub abilities: Vec<Ability>,
    /// Total XP needed for each level after the first.
    pub xp_levels: Vec<u32>,
}

/// A recipe type or subtype (a crafting menu tab or group).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Category {
    pub id: u32,
    pub sort: u32,
    pub key: String,
    pub name: String,
}

/// A shop item a recipe takes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub id: u32,
    /// Looted (skins, plant parts).
    pub loot: bool,
    pub rare: bool,
    /// Engine gadget type for gadgets (grenades...), 0 for others.
    pub gadget: u32,
    pub key: String,
    pub name: String,
}

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
}

/// Everything crafting needs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Crafting {
    pub types: Vec<Category>,
    pub subtypes: Vec<Category>,
    pub items: Vec<Item>,
    pub recipes: Vec<Recipe>,
    pub sources: Vec<Source>,
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
            });
        }
    }
    if let Some(levels) = sp.child("XPLevels").and_then(|x| x.child("LevelsList")) {
        tree.xp_levels = levels.children_named("XPLevel").map(|l| u32f(l, "iXPNeeded")).filter(|&x| x != 0 && x != NONE).collect();
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
        let effects: Vec<&Object> = recipe.and_then(|r| r.child("EffectList")).map(|l| l.children_named("Effect").map(|e| e.as_ref()).collect()).unwrap_or_default();
        let hashes = effects.iter().map(|e| plan_hashes(e)).find(|h| !h.is_empty()).unwrap_or_default();
        let duration = effects.iter().filter_map(|e| e.child("Properties").and_then(|p| p.f32("fDuration"))).find(|d| *d > 0.0).unwrap_or(0.0);
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
    // The shop items recipes take.
    let wanted: BTreeSet<u32> = out.recipes.iter().flat_map(|r| r.ingredients.iter().map(|i| i.0)).collect();
    for e in &f.shop_items.children {
        let id = u32f(e, "disId");
        if !wanted.contains(&id) {
            continue;
        }
        let gadget = u32f(e, "selGadgetType");
        out.items.push(Item {
            id,
            loot: e.bool("bIsLootItem").unwrap_or(false),
            rare: e.bool("bIsRareItem").unwrap_or(false),
            gadget: if gadget == NONE { 0 } else { gadget },
            key: text(e, "text_hidName"),
            name: located(strings, u32f(e, "locidItemNameId")),
        });
    }
    out.items.sort_by_key(|i| i.id);
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
        for g in loot.child("LootGroups").iter().flat_map(|l| l.children_named("Group")) {
            let chance = g.f32("percent").unwrap_or(100.0).clamp(0.0, 100.0);
            for &(item, percent) in groups.get(&u32f(g, "lootGroup")).into_iter().flatten() {
                items.push((item, chance * percent.clamp(0.0, 100.0) / 100.0));
            }
        }
        if items.is_empty() {
            continue;
        }
        let skeleton = c.child("CAnimationComponent").map(|a| text(a, "text_fileSkeleton").to_ascii_lowercase()).filter(|s| !s.is_empty());
        out.sources.push(Source { skeleton: skeleton.is_some(), key: skeleton.unwrap_or_else(|| p.name.clone()), prototype: p.entity.clone(), items });
    }
    out.sources.sort_by(|a, b| (a.skeleton, &a.key, &a.prototype).cmp(&(b.skeleton, &b.key, &b.prototype)));
    out
}

// ------------------------------------------------------------ text form ---

const SKILLS_MAGIC: &str = "fc3-skills 1";
const RECIPES_MAGIC: &str = "fc3-recipes 1";

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
            for b in &a.bonuses {
                s += &bonus_line(&a.id, b);
            }
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
            s += &format!("item\t{}\t{}\t{}\t{}\t{}\t{}\n", i.id, u8::from(i.loot), u8::from(i.rare), i.gadget, esc(&i.key), esc(&i.name));
        }
        for r in &self.recipes {
            s += &format!(
                "recipe\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
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
                esc(&r.description)
            );
            for b in &r.bonuses {
                s += &bonus_line(&r.id, b);
            }
        }
        for src in &self.sources {
            s += &format!(
                "source\t{}\t{}\t{}\t{}\n",
                if src.skeleton { "skeleton" } else { "entity" },
                esc(&src.key),
                esc(&src.prototype),
                src.items.iter().map(|(i, p)| format!("{i}:{p}")).collect::<Vec<_>>().join(",")
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
        let pairs = |s: &str| -> Vec<(String, String)> {
            s.split(',').filter_map(|p| p.split_once([':', '/'])).map(|(a, b)| (a.to_string(), b.to_string())).collect()
        };
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
                "item" if f.len() >= 7 => {
                    c.items.push(Item { id: n(1), loot: f[2] == "1", rare: f[3] == "1", gadget: n(4), key: unesc(f[5]), name: unesc(f[6]) })
                }
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
            obj("Bonus", vec![("selAttribute", V::U32(attr)), ("fValue", V::F(value)), ("iDifficultyLevel", V::U32(NONE)), ("selValueType", V::U32(add))], vec![])
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
                            chapter(1, 200, vec![skill("A_First", None, "", NONE), skill("A_Second", Some("A_First"), "M_010_main", 0x96), skill("Missing", None, "", NONE)]),
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
                    obj("Levels", vec![], vec![obj("Level", vec![("iMax", V::U32(1)), ("iIncrement", V::U32(1))], vec![]), obj("Level", vec![("iMax", V::U32(0)), ("iIncrement", V::U32(0))], vec![])]),
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
            obj("Entry", vec![("disId", V::U16(id)), ("text_hidName", V::S(key)), ("bIsLootItem", V::U8(1)), ("bIsRareItem", V::U8(rare)), ("selGadgetType", V::U32(0))], vec![])
        };
        let shop = obj("lib", vec![], vec![item(1280, "Parts/Skin", 0), item(1281, "Parts/Rare skin", 1), item(7, "Other", 0)]);
        let group = |id: u32, item: u32| {
            obj(
                "Group",
                vec![("hidID", V::U32(id))],
                vec![obj("Items", vec![], vec![obj("Item", vec![("ecoItem", V::U32(item)), ("Percent", V::F(50.0))], vec![]), obj("Item", vec![("ecoItem", V::U32(0))], vec![])])],
            )
        };
        let loot = obj("obj", vec![], vec![obj("Groups", vec![], vec![group(5, 1280), group(6, 7)])]);
        let effect = |plan: &str| {
            obj("Effect", vec![], vec![obj("Properties", vec![("bonusPlan", V::U32(name_hash(plan)))], vec![])])
        };
        let crecipe = |label: &'static str| {
            obj("CRecipe", vec![("sName", V::S(label)), ("selRecipeType", V::U32(KIND_GEAR))], vec![obj("EffectList", vec![], vec![obj("Effect", vec![], vec![]), effect("slots")])])
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
                ],
            )],
        );
        let tpl = templates();
        let files = CraftingFiles {
            recipes: &recipes,
            types: &types,
            subtypes: &subtypes,
            shop_items: &shop,
            loot_groups: &loot,
            library: &library,
            templates: &tpl,
        };
        let c = crafting(&files, &strings);
        assert_eq!(c.types, vec![Category { id: 2, sort: 0, key: "Type/Gear".into(), name: "s12".into() }]);
        assert_eq!(c.recipes.len(), 2);
        let (a, b) = (&c.recipes[0], &c.recipes[1]);
        assert_eq!((a.id.as_str(), a.kind, a.category, a.subcategory, a.label.as_str()), ("Recipes:Gear/Holster 1", KIND_GEAR, 2, 3, "Holster 1"));
        assert_eq!((a.ingredients.clone(), a.levels.clone(), a.unlock_mission.as_str(), a.requires.as_str()), (vec![(1280, 2)], vec![(1, 1)], "M_010_main", ""));
        assert_eq!(a.bonuses, vec![Bonus { plan: "slots".into(), attribute: 7, value: 1.0, additive: true }]);
        assert_eq!((b.requires.as_str(), b.name.as_str(), b.description.as_str()), ("Recipes:Gear/Holster 1", "s50", ""));
        assert_eq!(c.items.iter().map(|i| (i.id, i.rare)).collect::<Vec<_>>(), vec![(1280, false), (1281, true)]);
        assert_eq!(c.sources.len(), 2);
        assert_eq!((c.sources[0].skeleton, c.sources[0].key.as_str(), c.sources[0].items.clone()), (false, "Pickups/Plant_01", vec![(1280, 50.0)]));
        assert_eq!((c.sources[1].skeleton, c.sources[1].key.as_str()), (true, r"graphics\goat.skeleton"));
        assert_eq!(Crafting::from_text(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn text_fields_escape() {
        let t = SkillTree {
            branches: vec![Branch { number: 1, key: "k".into(), name: "a\tb".into(), description: "line\nnext \\ done".into() }],
            abilities: vec![Ability { id: "X".into(), requires: vec!["Y".into(), "Z".into()], description: "50%%\n[Run]".into(), ..Default::default() }],
            xp_levels: vec![1, 2, 3],
        };
        assert_eq!(SkillTree::from_text(&t.to_text()).unwrap(), t);
        assert!(SkillTree::from_text("other").is_err());
        assert!(Crafting::from_text("fc3-skills 1\n").is_err());
    }
}
