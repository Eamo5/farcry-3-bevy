//! Character progression: XP and levels, skills, money, hunting skins,
//! crafted gear upgrades, and the shop at liberated outposts.
//!
//! Gameplay systems read the combined effect through [`Perks`] (max health,
//! damage taken, reload speed, carry limits, ...), recomputed whenever skills
//! or upgrades change. The journal (Tab) shows skills, crafting and stats.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::combat::{Died, DamageKind, Health, PlayerStats};
use crate::hud::HudState;
use crate::menu::GameState;
use crate::player::Player;
use crate::weapons::Inventory;

pub struct ProgressionPlugin;

impl Plugin for ProgressionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Progress>()
            .init_resource::<Perks>()
            .init_resource::<ShopTab>()
            .init_resource::<Popups>()
            .add_message::<XpGain>()
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
                    journal_buttons,
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
}

impl Animal {
    pub const ALL: [Animal; 15] = [
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
    ];

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
        }
    }

    fn from_id(s: &str) -> Option<Animal> {
        Animal::ALL.into_iter().find(|a| a.id() == s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Branch {
    Heron,
    Shark,
    Spider,
}

impl Branch {
    fn title(self) -> &'static str {
        match self {
            Branch::Heron => "THE HERON  (long range, mobility)",
            Branch::Shark => "THE SHARK  (assault, healing)",
            Branch::Spider => "THE SPIDER  (stealth, survival)",
        }
    }
}

pub struct SkillDef {
    pub id: &'static str,
    pub name: &'static str,
    pub branch: Branch,
    pub cost: u32,
    pub requires: Option<&'static str>,
    pub description: &'static str,
}

const fn skill(id: &'static str, name: &'static str, branch: Branch, cost: u32, requires: Option<&'static str>, description: &'static str) -> SkillDef {
    SkillDef { id, name, branch, cost, requires, description }
}

pub const SKILLS: &[SkillDef] = &[
    skill("fleet_foot", "Fleet Foot", Branch::Heron, 1, None, "Sprint 15% faster"),
    skill("steady_aim", "Steady Aim", Branch::Heron, 1, Some("fleet_foot"), "30% less recoil"),
    skill("quick_reload", "Quick Reload", Branch::Heron, 2, Some("steady_aim"), "Reload 30% faster"),
    skill("marksman", "Marksman", Branch::Heron, 2, Some("quick_reload"), "Headshots deal 50% more damage"),
    skill("tough_skin", "Tough Skin", Branch::Shark, 1, None, "One more health bar"),
    skill("field_medic", "Field Medic", Branch::Shark, 1, Some("tough_skin"), "Medkits heal fully; carry 2 more"),
    skill("thick_hide", "Thick Hide", Branch::Shark, 2, Some("field_medic"), "Take 20% less damage"),
    skill("iron_skin", "Iron Skin", Branch::Shark, 2, Some("thick_hide"), "One more health bar"),
    skill("light_step", "Light Step", Branch::Spider, 1, None, "Crouching is 40% harder to spot"),
    skill("scout", "Scout", Branch::Spider, 1, Some("light_step"), "Tag at twice the range"),
    skill("front_takedown", "Frontal Takedown", Branch::Spider, 2, Some("scout"), "Take down unaware enemies from the front"),
    skill("heavy_takedown", "Heavy Takedown", Branch::Spider, 2, Some("front_takedown"), "Take down armoured heavies and flamers"),
    skill("death_from_above", "Death From Above", Branch::Spider, 2, Some("front_takedown"), "Drop onto an enemy below to take him down"),
    skill("chain_takedown", "Chain Takedown", Branch::Spider, 3, Some("death_from_above"), "Follow a takedown with a thrown knife"),
    skill("pistol_takedown", "Pistol Takedown", Branch::Spider, 3, Some("front_takedown"), "Take his pistol and shoot the enemies in view"),
    skill("death_from_below", "Death From Below", Branch::Spider, 2, Some("front_takedown"), "Pull an enemy off a ledge above you, or into the water"),
    skill("double_takedown", "Double Takedown", Branch::Spider, 3, Some("death_from_above"), "Drop onto two enemies standing together"),
    skill("takedown_drag", "Takedown Drag", Branch::Spider, 2, Some("scout"), "Drag the body away after a takedown (E)"),
    skill("grenade_takedown", "Grenade Takedown", Branch::Spider, 3, Some("front_takedown"), "Kick him, grenade live, into his friends"),
    skill("swimmer", "Swimmer", Branch::Spider, 1, None, "Swim 30% faster"),
    skill("hard_landing", "Hard Landing", Branch::Spider, 1, Some("swimmer"), "Half fall damage"),
];

pub struct UpgradeDef {
    pub id: &'static str,
    pub name: &'static str,
    /// Per level: the skins it takes and what it gives.
    pub levels: &'static [(&'static [(Animal, u32)], &'static str)],
}

pub const UPGRADES: &[UpgradeDef] = &[
    UpgradeDef {
        id: "ammo_pouch",
        name: "Ammo Pouch",
        levels: &[
            (&[(Animal::Deer, 2)], "Carry 25% more ammo"),
            (&[(Animal::Boar, 2)], "Carry 50% more ammo"),
            (&[(Animal::Tiger, 1), (Animal::Buffalo, 2)], "Carry double ammo"),
        ],
    },
    UpgradeDef {
        id: "grenade_bag",
        name: "Grenade Bag",
        levels: &[(&[(Animal::Goat, 2)], "Carry 6 grenades"), (&[(Animal::Dog, 2)], "Carry 8 grenades"), (&[(Animal::Cassowary, 2)], "Carry 10 grenades")],
    },
    UpgradeDef {
        id: "syringe_kit",
        name: "Syringe Kit",
        levels: &[(&[(Animal::Boar, 1), (Animal::Goat, 1)], "Carry 4 medkits"), (&[(Animal::Tapir, 2)], "Carry 5 medkits"), (&[(Animal::Crocodile, 1)], "Carry 6 medkits")],
    },
    UpgradeDef {
        id: "holster",
        name: "Weapon Holster",
        levels: &[(&[(Animal::Boar, 1)], "Carry 4 weapons"), (&[(Animal::Tapir, 2)], "Carry 5 weapons"), (&[(Animal::Bear, 1)], "Carry 6 weapons")],
    },
    UpgradeDef {
        id: "wallet",
        name: "Wallet",
        levels: &[(&[(Animal::Deer, 1)], "Hold $2000"), (&[(Animal::Goat, 2)], "Hold $5000"), (&[(Animal::Shark, 1)], "Hold $10000")],
    },
];

/// Total XP needed to reach `level` (level 1 needs none).
pub fn xp_for_level(level: u32) -> u32 {
    let l = level.saturating_sub(1);
    250 * l * (l + 1)
}

#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Progress {
    pub xp: u32,
    pub level: u32,
    pub skill_points: u32,
    pub skills: HashSet<String>,
    pub money: u32,
    pub skins: HashMap<Animal, u32>,
    pub upgrades: HashMap<String, u8>,
    pub outposts: u32,
    /// Radio towers activated (indices into the world's tower list).
    pub towers: std::collections::BTreeSet<u32>,
    /// Collectibles found: relics, lost letters, memory cards.
    pub relics: u32,
    pub letters: u32,
    pub cards: u32,
    /// Plants gathered and syringes mixed (see `syringes`).
    pub plants: [u32; 8],
    pub syringes: [u32; 5],
}

impl Default for Progress {
    fn default() -> Self {
        Progress {
            xp: 0,
            level: 1,
            skill_points: 1,
            skills: HashSet::new(),
            money: 150,
            skins: HashMap::new(),
            upgrades: HashMap::new(),
            outposts: 0,
            towers: Default::default(),
            relics: 0,
            letters: 0,
            cards: 0,
            plants: [0; 8],
            syringes: [0; 5],
        }
    }
}

impl Progress {
    pub fn has(&self, skill: &str) -> bool {
        // FC3_SKILLS=all (tests): every skill learned; or a list of them.
        self.skills.contains(skill) || std::env::var("FC3_SKILLS").is_ok_and(|v| v == "all" || v.split(',').any(|s| s == skill))
    }

    pub fn upgrade(&self, id: &str) -> u8 {
        self.upgrades.get(id).copied().unwrap_or(0)
    }

    pub fn money_cap(&self) -> u32 {
        [1000, 2000, 5000, 10000][self.upgrade("wallet").min(3) as usize]
    }

    pub fn add_money(&mut self, amount: u32) -> u32 {
        let before = self.money;
        self.money = (self.money + amount).min(self.money_cap());
        self.money - before
    }

    /// Save-file lines.
    pub fn to_lines(&self) -> String {
        let mut s = format!("progress {} {} {} {} {}\n", self.xp, self.level, self.skill_points, self.money, self.outposts);
        let mut skills: Vec<&String> = self.skills.iter().collect();
        skills.sort();
        for k in skills {
            s += &format!("skill {k}\n");
        }
        let mut ups: Vec<(&String, &u8)> = self.upgrades.iter().collect();
        ups.sort();
        for (k, v) in ups {
            s += &format!("upgrade {k} {v}\n");
        }
        for a in Animal::ALL {
            if let Some(n) = self.skins.get(&a).filter(|n| **n > 0) {
                s += &format!("skin {} {n}\n", a.id());
            }
        }
        for t in &self.towers {
            s += &format!("tower {t}\n");
        }
        s += &format!("collectibles {} {} {}\n", self.relics, self.letters, self.cards);
        let list = |v: &[u32]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" ");
        s += &format!("plants {}\nsyringes {}\n", list(&self.plants), list(&self.syringes));
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
            "plants" | "syringes" => {
                let into: &mut [u32] = if key == "plants" { &mut self.plants } else { &mut self.syringes };
                for (slot, v) in into.iter_mut().zip(it) {
                    *slot = v.parse().unwrap_or(0);
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
    pub reload_speed: f32,
    pub recoil: f32,
    pub headshot: f32,
    pub sprint: f32,
    pub swim: f32,
    pub fall_damage: f32,
    /// Multiplies how fast enemies spot a crouching player.
    pub crouch_detection: f32,
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
    pub grenade_cap: u32,
    pub ammo_scale: f32,
}

impl Default for Perks {
    fn default() -> Self {
        Perks::from_progress(&Progress::default())
    }
}

impl Perks {
    pub fn from_progress(p: &Progress) -> Perks {
        let bars = 4 + u32::from(p.has("tough_skin")) + u32::from(p.has("iron_skin"));
        Perks {
            max_health: bars as f32 * crate::combat::HEALTH_SEGMENT,
            damage_taken: if p.has("thick_hide") { 0.8 } else { 1.0 },
            reload_speed: if p.has("quick_reload") { 1.3 } else { 1.0 },
            recoil: if p.has("steady_aim") { 0.7 } else { 1.0 },
            headshot: if p.has("marksman") { 1.5 } else { 1.0 },
            sprint: if p.has("fleet_foot") { 1.15 } else { 1.0 },
            swim: if p.has("swimmer") { 1.3 } else { 1.0 },
            fall_damage: if p.has("hard_landing") { 0.5 } else { 1.0 },
            crouch_detection: if p.has("light_step") { 0.6 } else { 1.0 },
            front_takedown: p.has("front_takedown"),
            heavy_takedown: p.has("heavy_takedown"),
            death_from_above: p.has("death_from_above"),
            chain_takedown: p.has("chain_takedown"),
            pistol_takedown: p.has("pistol_takedown"),
            grenade_takedown: p.has("grenade_takedown"),
            takedown_drag: p.has("takedown_drag"),
            double_takedown: p.has("double_takedown"),
            death_from_below: p.has("death_from_below"),
            tag_range: if p.has("scout") { 2.0 } else { 1.0 },
            medkit_cap: 3 + p.upgrade("syringe_kit") as u32 + if p.has("field_medic") { 2 } else { 0 },
            medkit_full_heal: p.has("field_medic"),
            grenade_cap: 4 + 2 * p.upgrade("grenade_bag") as u32,
            weapon_slots: 3 + p.upgrade("holster") as usize,
            ammo_scale: [1.0, 1.25, 1.5, 2.0][p.upgrade("ammo_pouch").min(3) as usize],
        }
    }
}

/// XP earned (shown as a popup).
#[derive(Message, Clone)]
pub struct XpGain {
    pub amount: u32,
    pub reason: &'static str,
}

// ---------------------------------------------------------------- awards ---

fn award_kills(mut died: MessageReader<Died>, players: Query<Entity, With<Player>>, npcs: Query<&crate::npc::Npc>, mut xp: MessageWriter<XpGain>) {
    let Ok(player) = players.single() else { return };
    for d in died.read() {
        if d.killer != Some(player) || d.entity == player || !npcs.get(d.entity).is_ok_and(|n| n.faction.hostile_to_player()) {
            continue;
        }
        if d.kind == DamageKind::Melee {
            xp.write(XpGain { amount: 100, reason: "Takedown" });
        } else {
            xp.write(XpGain { amount: 50, reason: "Kill" });
        }
    }
}

fn award_outposts(mut liberated: MessageReader<crate::outposts::OutpostLiberated>, mut progress: ResMut<Progress>, mut xp: MessageWriter<XpGain>) {
    for l in liberated.read() {
        progress.outposts += 1;
        progress.add_money(500);
        xp.write(XpGain { amount: 1500, reason: "Outpost liberated" });
        if l.undetected {
            xp.write(XpGain { amount: 1500, reason: "Undetected bonus" });
        }
        let _ = l.outpost;
    }
}

fn apply_xp(mut gains: MessageReader<XpGain>, mut progress: ResMut<Progress>, mut hud: ResMut<HudState>, mut popups: ResMut<Popups>) {
    for g in gains.read() {
        progress.xp += g.amount;
        popups.0.push((format!("+{} XP  {}", g.amount, g.reason), 2.5));
        while progress.xp >= xp_for_level(progress.level + 1) {
            progress.level += 1;
            progress.skill_points += 1;
            hud.notify(format!("LEVEL {} - skill point earned (Tab)", progress.level));
        }
    }
}

fn update_perks(progress: Res<Progress>, mut perks: ResMut<Perks>) {
    if progress.is_changed() {
        let p = Perks::from_progress(&progress);
        if *perks != p {
            *perks = p;
        }
    }
}

/// Keeps the player's health and carry limits in line with the perks.
fn apply_perks(perks: Res<Perks>, mut players: Query<(&mut Health, &mut PlayerStats, &mut Inventory), With<Player>>) {
    let Ok((mut h, mut stats, mut inv)) = players.single_mut() else { return };
    if (h.max - perks.max_health).abs() > 0.1 {
        let gained = perks.max_health - h.max;
        h.max = perks.max_health;
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

#[derive(Component)]
struct JournalRoot;

#[derive(Component, Clone, Copy)]
enum JournalButton {
    Skill(usize),
    Craft(usize),
    /// Mix a syringe, or inject one.
    Syringe(usize),
    Inject(usize),
    Close,
}

#[derive(Component)]
struct JournalText(JournalField);

#[derive(Clone, Copy, PartialEq)]
enum JournalField {
    Header,
    Skill(usize),
    SkillStatus(usize),
    Craft(usize),
    Skins,
    Syringe(usize),
    Plants,
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
    let columns = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(50.0), ..default() }, ChildOf(root))).id();
    // Skills, by branch.
    let skills = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(4.0), ..default() }, ChildOf(columns))).id();
    commands.spawn((Text::new("SKILLS"), font(24.0), TextColor(ACCENT), ChildOf(skills)));
    for branch in [Branch::Heron, Branch::Shark, Branch::Spider] {
        commands.spawn((Text::new(branch.title()), font(16.0), TextColor(Color::srgb(0.75, 0.8, 0.9)), Node { margin: UiRect::top(Val::Px(8.0)), ..default() }, ChildOf(skills)));
        for (i, s) in SKILLS.iter().enumerate().filter(|(_, s)| s.branch == branch) {
            let b = commands
                .spawn((
                    Button,
                    Node { width: Val::Px(640.0), height: Val::Px(28.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
                    BackgroundColor(IDLE),
                    JournalButton::Skill(i),
                    ChildOf(skills),
                ))
                .id();
            commands.spawn((Text::new(s.name), font(15.0), TextColor(TEXT), Node { width: Val::Px(170.0), ..default() }, JournalText(JournalField::Skill(i)), ChildOf(b)));
            commands.spawn((Text::new(s.description), font(14.0), TextColor(Color::srgba(0.9, 0.9, 0.88, 0.8)), Node { width: Val::Px(370.0), ..default() }, ChildOf(b)));
            commands.spawn((Text::new(""), font(14.0), TextColor(TEXT), JournalText(JournalField::SkillStatus(i)), ChildOf(b)));
        }
    }
    // Crafting and skins.
    let craft = commands.spawn((Node { flex_direction: FlexDirection::Column, row_gap: Val::Px(6.0), ..default() }, ChildOf(columns))).id();
    commands.spawn((Text::new("CRAFTING"), font(24.0), TextColor(ACCENT), ChildOf(craft)));
    for (i, _) in UPGRADES.iter().enumerate() {
        let b = commands
            .spawn((
                Button,
                Node { width: Val::Px(440.0), min_height: Val::Px(46.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
                BackgroundColor(IDLE),
                JournalButton::Craft(i),
                ChildOf(craft),
            ))
            .id();
        commands.spawn((Text::new(""), font(15.0), TextColor(TEXT), JournalText(JournalField::Craft(i)), ChildOf(b)));
    }
    commands.spawn((Text::new("SKINS"), font(20.0), TextColor(ACCENT), Node { margin: UiRect::top(Val::Px(12.0)), ..default() }, ChildOf(craft)));
    commands.spawn((Text::new(""), font(15.0), TextColor(TEXT), Node { width: Val::Px(440.0), ..default() }, JournalText(JournalField::Skins), ChildOf(craft)));
    // Syringes from plants: mix, or inject one on hand.
    commands.spawn((Text::new("SYRINGES"), font(20.0), TextColor(ACCENT), Node { margin: UiRect::top(Val::Px(12.0)), ..default() }, ChildOf(craft)));
    for i in 0..crate::syringes::Syringe::ALL.len() {
        let row = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(6.0), ..default() }, ChildOf(craft))).id();
        let mix = commands
            .spawn((
                Button,
                Node { width: Val::Px(360.0), min_height: Val::Px(40.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(10.0)), flex_shrink: 0.0, ..default() },
                BackgroundColor(IDLE),
                JournalButton::Syringe(i),
                ChildOf(row),
            ))
            .id();
        commands.spawn((Text::new(""), font(14.0), TextColor(TEXT), JournalText(JournalField::Syringe(i)), ChildOf(mix)));
        let inject = commands
            .spawn((
                Button,
                Node { width: Val::Px(74.0), min_height: Val::Px(40.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, flex_shrink: 0.0, ..default() },
                BackgroundColor(IDLE),
                JournalButton::Inject(i),
                ChildOf(row),
            ))
            .id();
        commands.spawn((Text::new("USE"), font(14.0), TextColor(TEXT), ChildOf(inject)));
    }
    commands.spawn((Text::new(""), font(14.0), TextColor(Color::srgba(0.9, 0.9, 0.88, 0.85)), Node { width: Val::Px(440.0), ..default() }, JournalText(JournalField::Plants), ChildOf(craft)));
    let close = commands
        .spawn((
            Button,
            Node { width: Val::Px(200.0), height: Val::Px(36.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, ..default() },
            BackgroundColor(IDLE),
            JournalButton::Close,
            ChildOf(root),
        ))
        .id();
    commands.spawn((Text::new("CLOSE  (Tab)"), font(17.0), TextColor(TEXT), ChildOf(close)));
}

fn toggle_journal(
    keys: Res<ButtonInput<KeyCode>>,
    args: Res<crate::Args>,
    mut opened: Local<bool>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut root: Query<&mut Visibility, With<JournalRoot>>,
) {
    // --journal (tests): open once in play.
    let test = args.open_journal && !*opened && *state.get() == GameState::Playing;
    if test {
        *opened = true;
    }
    if keys.just_pressed(KeyCode::Tab) || test {
        match state.get() {
            GameState::Playing => next.set(GameState::Journal),
            GameState::Journal => next.set(GameState::Playing),
            _ => {}
        }
    }
    if let Ok(mut v) = root.single_mut() {
        *v = if *state.get() == GameState::Journal { Visibility::Inherited } else { Visibility::Hidden };
    }
}

fn can_learn(p: &Progress, s: &SkillDef) -> bool {
    !p.has(s.id) && p.skill_points >= s.cost && s.requires.is_none_or(|r| p.has(r))
}

fn can_craft(p: &Progress, u: &UpgradeDef) -> bool {
    let level = p.upgrade(u.id) as usize;
    u.levels.get(level).is_some_and(|(cost, _)| cost.iter().all(|(a, n)| p.skins.get(a).copied().unwrap_or(0) >= *n))
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
) {
    if *state.get() != GameState::Journal {
        return;
    }
    for (interaction, kind, mut bg) in &mut buttons {
        match interaction {
            Interaction::Pressed => {
                sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.7 });
                match *kind {
                    JournalButton::Skill(i) => {
                        let s = &SKILLS[i];
                        if can_learn(&progress, s) {
                            progress.skill_points -= s.cost;
                            progress.skills.insert(s.id.to_string());
                            hud.notify(format!("Learned {}", s.name));
                        }
                    }
                    JournalButton::Craft(i) => {
                        let u = &UPGRADES[i];
                        if can_craft(&progress, u) {
                            let level = progress.upgrade(u.id) as usize;
                            for (a, n) in u.levels[level].0 {
                                *progress.skins.entry(*a).or_insert(0) -= n;
                            }
                            progress.upgrades.insert(u.id.to_string(), level as u8 + 1);
                            hud.notify(format!("Crafted {} {}", u.name, level + 1));
                        }
                    }
                    JournalButton::Syringe(i) => {
                        let s = crate::syringes::Syringe::ALL[i];
                        if crate::syringes::craft(&mut progress, s) {
                            hud.notify(format!("Mixed a {} syringe", s.name()));
                        }
                    }
                    JournalButton::Inject(i) => {
                        let s = crate::syringes::Syringe::ALL[i];
                        if crate::syringes::inject(&mut progress, &mut boosts, s) {
                            hud.notify(format!("{} syringe: {}", s.name(), s.effect()));
                        }
                    }
                    JournalButton::Close => next.set(GameState::Playing),
                }
            }
            Interaction::Hovered => bg.0 = HOVER,
            Interaction::None => bg.0 = IDLE,
        }
    }
}

fn refresh_journal(
    state: Res<State<GameState>>,
    progress: Res<Progress>,
    players: Query<&PlayerStats>,
    mut texts: Query<(&JournalText, &mut Text, &mut TextColor)>,
) {
    if *state.get() != GameState::Journal || !(progress.is_changed() || state.is_changed()) {
        return;
    }
    let kills = players.single().map(|s| s.kills).unwrap_or(0);
    for (field, mut t, mut c) in &mut texts {
        match field.0 {
            JournalField::Header => {
                let (lo, hi) = (xp_for_level(progress.level), xp_for_level(progress.level + 1));
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
                let s = &SKILLS[i];
                t.0 = if progress.has(s.id) {
                    "learned".to_string()
                } else if s.requires.is_some_and(|r| !progress.has(r)) {
                    "locked".into()
                } else {
                    format!("{} pt", s.cost)
                };
            }
            JournalField::Skill(i) => {
                let s = &SKILLS[i];
                c.0 = if progress.has(s.id) {
                    Color::srgb(0.55, 0.85, 0.5)
                } else if can_learn(&progress, s) {
                    TEXT
                } else {
                    Color::srgba(0.8, 0.8, 0.8, 0.55)
                };
            }
            JournalField::Craft(i) => {
                let u = &UPGRADES[i];
                let level = progress.upgrade(u.id) as usize;
                t.0 = match u.levels.get(level) {
                    Some((cost, effect)) => {
                        let needs: Vec<String> = cost.iter().map(|(a, n)| format!("{n} {}", a.name())).collect();
                        format!("{} {}  -  {effect}\nneeds {}", u.name, level + 1, needs.join(", "))
                    }
                    None => format!("{}  -  fully upgraded", u.name),
                };
                c.0 = if can_craft(&progress, u) { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
            }
            JournalField::Skins => {
                let skins: Vec<String> = Animal::ALL.iter().map(|a| format!("{} {}", a.name(), progress.skins.get(a).copied().unwrap_or(0))).collect();
                t.0 = skins.join("   ");
            }
            JournalField::Syringe(i) => {
                use crate::syringes::{can_craft, Syringe};
                let s = Syringe::ALL[i];
                let needs: Vec<String> = s.recipe().iter().map(|(p, n)| format!("{n} {}", p.name())).collect();
                t.0 = format!("{} x{}  -  {}\nneeds {}", s.name(), progress.syringes[i], s.effect(), needs.join(", "));
                c.0 = if can_craft(&progress, s) { TEXT } else { Color::srgba(0.8, 0.8, 0.8, 0.55) };
            }
            JournalField::Plants => {
                let plants: Vec<String> = crate::syringes::Plant::ALL.iter().map(|p| format!("{} {}", p.name(), progress.plants[p.index()])).collect();
                t.0 = format!("Plants: {}", plants.join("   "));
            }
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
}

impl ShopItem {
    /// Price, with the weapons freed by `towers` radio towers costing nothing.
    fn price(self, inv: &Inventory, towers: usize) -> u32 {
        if let ShopItem::Weapon(d) = self {
            if inv.defs.get(d).is_some_and(|d| crate::traversal::FREE_WEAPONS[..towers.min(crate::traversal::FREE_WEAPONS.len())].contains(&d.name)) {
                return 0;
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
            ShopItem::Attachment(d, a) => inv.defs.get(d).map(|d| a.price(d)).unwrap_or(0),
            ShopItem::Paint(..) => PAINT_PRICE,
            ShopItem::Arrows(1) => 150,
            ShopItem::Arrows(_) => 300,
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

    fn label(self, inv: &Inventory) -> String {
        match self {
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
const SHOP_TABS: [&str; 10] = ["SUPPLIES", "HANDGUNS", "SMG", "RIFLES", "SHOTGUNS", "SNIPER", "HEAVY", "BOWS", "ATTACHMENTS", "PAINT"];
/// The attachments and paint tabs.
const ATTACHMENTS_TAB: usize = 8;
const PAINT_TAB: usize = 9;
/// A paint job's price.
const PAINT_PRICE: u32 = 500;

/// The open shop tab.
#[derive(Resource, Default)]
struct ShopTab(usize);

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

#[derive(Component)]
struct ShopHeader;

/// Shop stock in a tab: supplies, or the weapons of one kind (owned ones
/// show as owned).
fn shop_items(inv: &Inventory, tab: usize) -> Vec<ShopItem> {
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
    (0..inv.defs.len()).filter(|&d| inv.defs[d].class != crate::weapons::WeaponClass::Melee && shop_tab(inv.defs[d].class) == tab).map(ShopItem::Weapon).collect()
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
    let tabs = commands.spawn((Node { flex_direction: FlexDirection::Row, column_gap: Val::Px(4.0), margin: UiRect::vertical(Val::Px(6.0)), ..default() }, ChildOf(root))).id();
    for (i, name) in SHOP_TABS.iter().enumerate() {
        let b = commands
            .spawn((
                Button,
                Node { padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)), ..default() },
                BackgroundColor(IDLE),
                ShopButton::Tab(i),
                ChildOf(tabs),
            ))
            .id();
        commands.spawn((Text::new(*name), font(14.0), TextColor(TEXT), ChildOf(b)));
    }
    for i in 0..14 {
        let b = commands
            .spawn((
                Button,
                Node { width: Val::Px(560.0), height: Val::Px(30.0), align_items: AlignItems::Center, padding: UiRect::horizontal(Val::Px(12.0)), flex_shrink: 0.0, ..default() },
                BackgroundColor(IDLE),
                ShopButton::Buy(i),
                ChildOf(root),
            ))
            .id();
        commands.spawn((Text::new(""), font(16.0), TextColor(TEXT), ShopLine(i), ChildOf(b)));
    }
    let close = commands
        .spawn((
            Button,
            Node { width: Val::Px(200.0), height: Val::Px(36.0), justify_content: JustifyContent::Center, align_items: AlignItems::Center, margin: UiRect::top(Val::Px(10.0)), ..default() },
            BackgroundColor(IDLE),
            ShopButton::Close,
            ChildOf(root),
        ))
        .id();
    commands.spawn((Text::new("CLOSE"), font(17.0), TextColor(TEXT), ChildOf(close)));
}

/// Liberated outposts have a shop at their centre.
#[allow(clippy::too_many_arguments)]
fn shop_prompt(
    args: Res<crate::Args>,
    mut opened: Local<bool>,
    mut tab: ResMut<ShopTab>,
    state: Res<State<GameState>>,
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    outposts: Res<crate::outposts::Outposts>,
    players: Query<(&Transform, &Health), With<Player>>,
    mut hud: ResMut<HudState>,
    mut next: ResMut<NextState<GameState>>,
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
    let Ok((t, h)) = players.single() else { return };
    if h.dead {
        return;
    }
    let near = outposts.list.iter().any(|o| o.liberated && Vec2::new(t.translation.x - o.centre.x, t.translation.z - o.centre.z).length() < 10.0);
    if !near {
        return;
    }
    hud.prompt = Some(format!("{}  shop", actions.label(crate::controls::Action::Interact, &settings)));
    if actions.just_pressed(crate::controls::Action::Interact) {
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
) {
    if *state.get() != GameState::Shop {
        return;
    }
    let Ok((mut inv, mut stats)) = players.single_mut() else { return };
    let items = shop_items(&inv, tab.0);
    for (interaction, kind, mut bg) in &mut buttons {
        match interaction {
            Interaction::Pressed => {
                sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.7 });
                let i = match *kind {
                    ShopButton::Buy(i) => i,
                    ShopButton::Tab(t) => {
                        tab.0 = t;
                        continue;
                    }
                    ShopButton::Close => {
                        next.set(GameState::Playing);
                        continue;
                    }
                };
                let Some(&item) = items.get(i) else { continue };
                let price = item.price(&inv, progress.towers.len());
                if progress.money < price {
                    hud.notify("Not enough money");
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
                    ShopItem::C4 if inv.c4 < crate::explosives::EXPLOSIVE_CAP => {
                        inv.c4 += 1;
                        true
                    }
                    ShopItem::Mine if inv.mines < crate::explosives::EXPLOSIVE_CAP => {
                        inv.mines += 1;
                        true
                    }
                    ShopItem::Attachment(d, a) => inv.attach(d, a),
                    ShopItem::Arrows(k) if inv.special_arrows[k as usize - 1] < crate::weapons::ARROW_CAP => {
                        let n = &mut inv.special_arrows[k as usize - 1];
                        *n = (*n + 5).min(crate::weapons::ARROW_CAP);
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
                    ShopItem::Gear(id) if progress.upgrade(id) == 0 => {
                        progress.upgrades.insert(id.to_string(), 1);
                        true
                    }
                    ShopItem::Weapon(d) => {
                        let name = inv.defs[d].name;
                        match crate::weapons::grant_weapon(&mut inv, name, perks.weapon_slots) {
                            crate::weapons::Granted::New => true,
                            crate::weapons::Granted::Swapped(old) => {
                                hud.notify(&format!("Swapped the {old} for the {name}"));
                                true
                            }
                            crate::weapons::Granted::Ammo => false,
                        }
                    }
                    _ => false,
                };
                if bought {
                    progress.money -= price;
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
    mut lines: Query<(&ShopLine, &mut Text, &mut TextColor, &ChildOf), Without<ShopHeader>>,
    mut buttons: Query<&mut Node, With<ShopButton>>,
    mut tabs: Query<(&ShopButton, &Interaction, &mut BackgroundColor)>,
    tab: Res<ShopTab>,
    perks: Res<Perks>,
) {
    let open = *state.get() == GameState::Shop;
    if let Ok(mut v) = root.single_mut() {
        *v = if open { Visibility::Inherited } else { Visibility::Hidden };
    }
    if !open {
        return;
    }
    let Ok(inv) = players.single() else { return };
    let items = shop_items(inv, tab.0);
    if let Ok(mut t) = header.single_mut() {
        let guns = inv.slots.iter().filter(|s| inv.defs[s.def].class != crate::weapons::WeaponClass::Melee).count();
        t.0 = format!("${} / {}      weapons carried {} / {}", progress.money, progress.money_cap(), guns, perks.weapon_slots);
    }
    for (b, interaction, mut bg) in &mut tabs {
        if let ShopButton::Tab(i) = b {
            if *interaction == Interaction::None {
                bg.0 = if *i == tab.0 { Color::srgba(0.85, 0.55, 0.15, 0.9) } else { IDLE };
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
        let price = item.price(inv, progress.towers.len());
        t.0 = if owned { format!("{:<38} owned", item.label(inv)) } else { format!("{:<38} ${price}", item.label(inv)) };
        c.0 = if owned || progress.money < price { Color::srgba(0.8, 0.8, 0.8, 0.5) } else { TEXT };
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

fn update_popups(time: Res<Time<Real>>, popups: Option<ResMut<Popups>>, mut text: Query<&mut Text, With<PopupText>>) {
    let Some(mut popups) = popups else { return };
    for p in &mut popups.0 {
        p.1 -= time.delta_secs();
    }
    popups.0.retain(|p| p.1 > 0.0);
    if let Ok(mut t) = text.single_mut() {
        let s = popups.0.iter().rev().take(4).map(|p| p.0.as_str()).collect::<Vec<_>>().join("\n");
        if t.0 != s {
            t.0 = s;
        }
    }
}
