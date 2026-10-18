//! The handbook's statistics (`M_Handbook.feu`'s statistics tab): the rows
//! of its categories as the engine built them (`CFCXStatsDataProvider`), from
//! the counters the game keeps by the engine's statistic numbers
//! (`enumStatisticType`, `Progress::stats`), counted here as the player
//! plays.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::progression::{Animal, Progress};
use crate::syringes::{Plant, Syringe};

/// The engine's statistics, by number (`enumStatisticType`).
pub mod stat {
    pub const GAME_TIME: u32 = 0;
    pub const MISSIONS: u32 = 1;
    pub const SIDE_MISSIONS: u32 = 2;
    pub const DISTANCE_WALKED: u32 = 11;
    pub const DISTANCE_SWIMMING: u32 = 12;
    pub const DISTANCE_DRIVEN_LAND: u32 = 13;
    pub const DISTANCE_DRIVEN_SEA: u32 = 14;
    pub const DISTANCE_IN_AIR: u32 = 15;
    pub const DISTANCE_ZIPLINING: u32 = 16;
    pub const DAYS_SURVIVED: u32 = 18;
    pub const SUPPLY_DROPS: u32 = 20;
    pub const WANTED_DEAD: u32 = 21;
    pub const HUNTS: u32 = 22;
    pub const KILLS: u32 = 80;
    pub const ANIMAL_KILLS: u32 = 81;
    pub const IRON_SIGHT_KILLS: u32 = 82;
    pub const HIP_FIRE_KILLS: u32 = 83;
    pub const STEALTH_KILLS: u32 = 84;
    pub const HEADSHOT_KILLS: u32 = 85;
    pub const MELEE_KILLS: u32 = 86;
    pub const FIRE_KILLS: u32 = 87;
    pub const EXPLOSIVE_KILLS: u32 = 88;
    pub const RUN_OVER_KILLS: u32 = 89;
    pub const PISTOL_KILLS: u32 = 90;
    pub const SMG_KILLS: u32 = 91;
    pub const SHOTGUN_KILLS: u32 = 92;
    pub const ASSAULT_RIFLE_KILLS: u32 = 93;
    pub const SNIPER_KILLS: u32 = 94;
    pub const LMG_KILLS: u32 = 95;
    pub const RPG_KILLS: u32 = 96;
    pub const GRENADE_LAUNCHER_KILLS: u32 = 97;
    pub const FLARE_GUN_KILLS: u32 = 98;
    pub const ARROW_KILLS: u32 = 99;
    pub const FLAMETHROWER_KILLS: u32 = 100;
    pub const MOUNTED_GUN_KILLS: u32 = 101;
    pub const GRENADE_KILLS: u32 = 102;
    pub const STANDARD_TAKEDOWNS: u32 = 150;
    pub const DRAG_TAKEDOWNS: u32 = 151;
    pub const FROM_ABOVE_TAKEDOWNS: u32 = 152;
    pub const LEDGE_TAKEDOWNS: u32 = 153;
    pub const WATER_TAKEDOWNS: u32 = 154;
    pub const HEAVY_TAKEDOWNS: u32 = 155;
    pub const PISTOL_TAKEDOWNS: u32 = 156;
    pub const GRENADE_TAKEDOWNS: u32 = 157;
    pub const KNIFE_THROW_TAKEDOWNS: u32 = 158;
    pub const CHAIN_TAKEDOWNS: u32 = 159;
    pub const BULLETS_SHOT: u32 = 180;
    pub const BULLETS_HIT: u32 = 181;
    pub const BULLETS_SHOT_IN_COVER: u32 = 182;
    pub const BULLETS_HIT_IN_COVER: u32 = 183;
    pub const ROCKS_THROWN: u32 = 186;
    pub const GRENADES_THROWN: u32 = 187;
    pub const MOLOTOVS_THROWN: u32 = 188;
    /// The flamethrower's own kills (`Weapon_LPO50_Kill`, which the kills
    /// page shows).
    pub const LPO50_KILLS: u32 = 427;
    pub const CASH_EARNED: u32 = 530;
    pub const CASH_SPENT: u32 = 531;
    pub const ANIMALS_SKINNED: u32 = 532;
    pub const PLANTS_HARVESTED: u32 = 533;
    pub const BODIES_LOOTED: u32 = 534;
    pub const MANUAL_HEALS: u32 = 536;
    pub const POKER_PLAYED: u32 = 537;
    pub const POKER_WON: u32 = 538;
    pub const POKER_LOST: u32 = 539;
    pub const POKER_WINNINGS: u32 = 540;
    pub const POKER_LARGEST_WIN: u32 = 541;
    pub const POKER_TIME: u32 = 542;
    pub const RACES_COMPLETED: u32 = 543;
    pub const RACES_TIME: u32 = 544;
    pub const RACES_WINNINGS: u32 = 545;
    pub const SHOOTING_COMPLETED: u32 = 546;
    pub const SHOOTING_TIME: u32 = 547;
    pub const SHOOTING_WINNINGS: u32 = 548;
    pub const KNIVES_WON: u32 = 549;
    pub const KNIVES_TIME: u32 = 550;
    pub const KNIVES_WINNINGS: u32 = 551;
    pub const TRIALS_PLAYED: u32 = 567;
    pub const SYRINGES_CRAFTED: u32 = 830;
    pub const SYRINGES_USED: u32 = 831;
    pub const ARROWS_CRAFTED: u32 = 832;
    pub const GEAR_CRAFTED: u32 = 833;
    pub const MEDICINE_CRAFTED: u32 = 834;
    pub const MEDICINE_USED: u32 = 835;
}

/// The weapons kept apart, by the game's (their kill counts: the
/// favourites' on the weapons page). The rest count only by their class.
pub fn weapon_kills(name: &str) -> Option<u32> {
    Some(match name {
        "RPG-7" => 189,
        "GL-94" => 196,
        "1911" => 203,
        "Skorpion" => 210,
        "6P9" => 217,
        "D50" => 238,
        "PKM LMG" => 245,
        "U100" => 252,
        "MKG" => 259,
        "Machete" => 266,
        "DShK" => 273,
        "M249" => 280,
        "AK-74M Rifle" => 294,
        "STG-90" => 301,
        "P416" => 308,
        "F1" => 315,
        "MS16" => 322,
        "ACE" => 329,
        "870 Shotgun" => 343,
        "SPAS-12" => 350,
        "1887" => 357,
        "A2000" => 371,
        "Vector .45 ACP" => 378,
        "PP-19 Bizon" => 385,
        "MP5 SMG" => 392,
        "SVD" => 399,
        "700 Sniper" => 406,
        "Z93" => 413,
        "Flamethrower" => stat::LPO50_KILLS,
        "Recurve Bow" => 434,
        _ => return None,
    })
}

/// A weapon class's kill count. (The engine's own table had assault rifles
/// counted as shotguns and sniper rifles as assault rifles, the snipers'
/// row never moving; here each counts as its row says.)
pub fn class_kills(class: crate::weapons::WeaponClass) -> Option<u32> {
    use crate::weapons::WeaponClass::*;
    Some(match class {
        Pistol => stat::PISTOL_KILLS,
        Smg => stat::SMG_KILLS,
        Rifle => stat::ASSAULT_RIFLE_KILLS,
        Shotgun => stat::SHOTGUN_KILLS,
        Sniper => stat::SNIPER_KILLS,
        Lmg => stat::LMG_KILLS,
        Launcher => stat::RPG_KILLS,
        GrenadeLauncher => stat::GRENADE_LAUNCHER_KILLS,
        Bow => stat::ARROW_KILLS,
        Flamethrower => stat::FLAMETHROWER_KILLS,
        Mounted => stat::MOUNTED_GUN_KILLS,
        Melee => return None,
    })
}

/// An animal's (killed, skinned) counts, as the game kept them by kind
/// (the wild pigs with the pigs, the rabid dogs with the dogs).
pub fn animal_counts(animal: Animal) -> (u32, u32) {
    match animal {
        Animal::Chicken => (601, 600),
        Animal::Pig | Animal::WildPig => (603, 602),
        Animal::Bear => (605, 604),
        Animal::Tapir => (607, 606),
        Animal::Boar => (611, 610),
        Animal::Buffalo => (613, 612),
        Animal::Cassowary => (615, 614),
        Animal::Crocodile => (617, 616),
        Animal::Deer => (619, 618),
        Animal::Dog => (621, 620),
        Animal::Goat => (625, 624),
        Animal::Tortoise => (627, 626),
        Animal::Komodo => (631, 630),
        Animal::Leopard => (633, 632),
        Animal::Lizard => (635, 634),
        Animal::Macaque => (637, 636),
        Animal::Rat => (641, 640),
        Animal::Thylacine => (643, 642),
        Animal::Tiger => (645, 644),
        Animal::Crab => (649, 648),
        Animal::Crow => (651, 650),
        Animal::Shark => (659, 658),
        Animal::JellyFish => (663, 662),
        Animal::MantaRay => (665, 664),
        Animal::Moray => (667, 666),
        Animal::RiverFish => (671, 670),
        Animal::SeaTurtle => (673, 672),
    }
}

/// A plant's harvested count.
pub fn plant_count(plant: Plant) -> u32 {
    match plant {
        Plant::Aloe => 780,
        Plant::Arrowroot => 781,
        Plant::BreezeWakame => 782,
        Plant::GoatsFoot => 785,
        Plant::GoblinsGold => 786,
        Plant::GoldenBeehive => 787,
        Plant::GoodLuckPlant => 788,
        Plant::Heliconia => 789,
        Plant::Hibiscus => 790,
        Plant::Palani => 791,
        Plant::RedAlgae => 792,
        Plant::SacredLotus => 793,
        Plant::ScrewPine => 794,
        Plant::StarRhododendron => 795,
        Plant::StoutKelp => 796,
        Plant::Tiare => 797,
        Plant::TorchGinger => 798,
        Plant::YellowSage => 799,
    }
}

/// A syringe's (crafted, used) counts.
pub fn syringe_counts(s: Syringe) -> (u32, u32) {
    match s {
        Syringe::HealthBoost => (836, 837),
        Syringe::NeverDrown => (838, 839),
        Syringe::SprintBurst => (840, 841),
        Syringe::PlantVision => (844, 845),
        Syringe::WildlifeVision => (846, 847),
        Syringe::AnimalRepellent => (848, 849),
        Syringe::AnimalDamage => (852, 853),
        Syringe::Fireproof => (854, 855),
        Syringe::Steady => (856, 857),
        Syringe::EnemyVision => (860, 861),
        Syringe::Invincible => (862, 863),
        Syringe::OneHitKill => (864, 865),
    }
}

/// What a row shows: a count, a text (the game's string), or a dash.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Figure {
    Count(i64),
    Text(u32),
    Dash,
}

/// A row of a category: its name (the game's string), its figure, how the
/// page shows it (`sType`: `d` a distance in decimetres, `t` seconds, `m`
/// money, `p` a percentage, `a` the animals' columns, else as it is), the
/// progress page's total and milestones, the animals' skinned column.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: u32,
    pub value: Figure,
    pub kind: &'static str,
    pub total: Option<Figure>,
    pub milestones: &'static str,
}

impl Row {
    fn new(name: u32, value: Figure, kind: &'static str) -> Row {
        Row { name, value, kind, total: None, milestones: "" }
    }
}

/// The weapons page's favourites: a kind of weapon's candidates (their
/// names, the game's strings, by their kill counts).
const FAVOURITE_WEAPONS: [(u32, &[(u32, u32)]); 10] = [
    // Handguns (the signature ones after).
    (157408, &[(42881, 203), (42878, 217), (42880, 231), (58340, 238), (146707, 495), (146709, 496)]),
    (157410, &[(42883, 210), (42890, 371), (42893, 392), (58355, 385), (58357, 378), (146711, 497)]),
    (157411, &[(82887, 357), (43044, 343), (58346, 350), (146708, 498)]),
    (157412, &[(42870, 294), (42875, 301), (42872, 315), (42873, 322), (58344, 308), (82886, 329), (146712, 499)]),
    (157413, &[(42885, 245), (42886, 252), (58348, 259), (146710, 500)]),
    (157414, &[(42894, 406), (42895, 399), (58352, 413), (146713, 501)]),
    // Launchers, specials, throwables (thrown, not kills).
    (157415, &[(42877, stat::RPG_KILLS), (42876, 196)]),
    (157416, &[(119080, 441), (42879, 224), (80239, 434), (80240, stat::FLAMETHROWER_KILLS)]),
    (157417, &[(155169, stat::ROCKS_THROWN), (150737, stat::GRENADES_THROWN), (156291, stat::MOLOTOVS_THROWN)]),
    // The world page's favourite syringe (by those used).
    (
        157426,
        &[
            (150959, 837),
            (82663, 839),
            (82660, 841),
            (150960, 845),
            (84555, 847),
            (82662, 849),
            (150961, 853),
            (82657, 855),
            (82658, 857),
            (84554, 861),
            (150963, 863),
            (150964, 865),
        ],
    ),
];

/// The candidate counted most (the first of a tie), or a dash while none is.
fn favourite(p: &Progress, row: u32) -> Figure {
    let Some((_, candidates)) = FAVOURITE_WEAPONS.iter().find(|(r, _)| *r == row) else { return Figure::Dash };
    let mut best: Option<(u32, u32)> = None;
    for &(name, count) in *candidates {
        let n = p.stat(count);
        if n > 0 && best.is_none_or(|(_, b)| n > b) {
            best = Some((name, n));
        }
    }
    best.map_or(Figure::Dash, |(name, _)| Figure::Text(name))
}

/// The progress page's rows: (name, its count, its total, milestones).
const PROGRESS: [(u32, u32, &str); 15] = [
    (93324, 38, "1-a,3-b,5-a,6-b,7-b,9-a,10-b,11-a,13-b,14-b,15-a,16-b,20-a,23-a,25-a,26-a,32-a,35-a,38-a"),
    (93325, 18, "9-a,18-c"),
    (93326, 34, "3-a,17-c,34-a"),
    (93329, 120, "10-c,20-c,30-b,40-b,60-a"),
    (93330, 20, "6-c,20-a"),
    (93331, 20, "10-c,20-a"),
    (93360, 12, "1-a,6-c"),
    (93362, 24, "1-a"),
    (93363, 23, "1-a"),
    (93361, 19, "1-a"),
    (93338, 14, ""),
    (93327, 54, "5-a,54-a"),
    (93328, 38, ""),
    (93332, 43, ""),
    (93334, 12, ""),
];

/// The missions of each type the handbook counts (by the type the
/// scripts register them with), and those of a save from before they were
/// counted (by their names: `missions_done`).
const MISSION_TYPES: [(&str, u32, &[&str]); 5] = [
    ("mission", stat::MISSIONS, &["main"]),
    ("behaviourquest", stat::SIDE_MISSIONS, &["sqc_", "sqh_"]),
    ("speedquest", stat::SUPPLY_DROPS, &["sqs_"]),
    ("wantedquest", stat::WANTED_DEAD, &["sqw_"]),
    ("huntingquest", stat::HUNTS, &["sqa_"]),
];

/// A mission done (before it joins the missions done): counted by its type
/// (a briefing not), its count first made up from the missions done before
/// if it never was.
pub fn mission_done(p: &mut Progress, kind: &str, giver: bool) {
    let Some(&(_, stat, names)) = MISSION_TYPES.iter().find(|(k, _, _)| k.eq_ignore_ascii_case(kind)).filter(|_| !giver) else { return };
    if !p.stats.contains_key(&stat) {
        let before = missions_done(p, names);
        p.stats.insert(stat, before);
    }
    add(p, stat, 1);
}

/// The main missions done.
pub(crate) fn main_missions_done(p: &Progress) -> u32 {
    missions_of(p, stat::MISSIONS)
}

/// A kind of mission's count: as counted, else (a save from before) by the
/// missions' names.
fn missions_of(p: &Progress, stat: u32) -> u32 {
    let names = MISSION_TYPES.iter().find(|(_, s, _)| *s == stat).map_or(&[][..], |(_, _, n)| *n);
    p.stats.get(&stat).copied().unwrap_or_else(|| missions_done(p, names))
}

/// The missions of a kind done, by their names (`<prefix>_<n>`, once
/// however many of its graphs completed): `main` the story's own
/// (`<region>_<n>_main`, not a mission's optional parts), else those whose
/// name starts so (the side missions `sqc_`, `sqh_`; the hunts `sqa_`, the
/// Wanted Dead `sqw_`, the supply runs `sqs_`), not their briefings.
fn missions_done(p: &Progress, kind: &[&str]) -> u32 {
    let mut done: Vec<String> = p
        .missions
        .iter()
        .map(|m| m.to_ascii_lowercase())
        .filter(|m| {
            if kind == ["main"] {
                m.ends_with("_main") && !m.starts_with("sq") && !m.contains("_opt")
            } else {
                kind.iter().any(|k| m.starts_with(k)) && !m.contains("brief") && !m.contains("giver")
            }
        })
        .map(|m| m.split('_').take(2).collect::<Vec<_>>().join("_"))
        .collect();
    done.sort();
    done.dedup();
    done.len() as u32
}

/// What the handbook shows that the counters don't keep: the crafting
/// recipes the player has, the outposts liberated.
#[derive(Clone, Copy, Default)]
pub struct Extra {
    pub recipes: usize,
    pub outposts: usize,
}

/// What the game's progress counts stand at (the ones kept elsewhere than
/// the counters: the towers, outposts, collectibles, abilities, recipes,
/// weapons and vehicles met).
fn progress_count(p: &Progress, row: u32, extra: Extra) -> u32 {
    let met = |section: &str| p.met.iter().filter(|k| k.to_ascii_lowercase().starts_with(section)).count() as u32;
    match row {
        93324 => missions_of(p, stat::MISSIONS),
        93325 => p.towers.len() as u32,
        93326 => p.outposts.max(extra.outposts as u32),
        93329 => p.relics,
        93330 => p.letters,
        93331 => p.cards,
        93360 => p.stat(stat::TRIALS_PLAYED),
        93362 => missions_of(p, stat::WANTED_DEAD),
        93363 => missions_of(p, stat::HUNTS),
        93361 => missions_of(p, stat::SUPPLY_DROPS),
        93338 => missions_of(p, stat::SIDE_MISSIONS),
        93327 => p.skills.len() as u32,
        93328 => extra.recipes as u32,
        93332 => met("weapons/"),
        93334 => met("transport/"),
        _ => 0,
    }
}

/// A category's rows (0 the progress page, then time and distance, kills,
/// takedowns, weapons, the world, animals, plants and crafting), in the
/// engine's order (the page sorts some itself).
pub fn rows(category: u32, p: &Progress, extra: Extra) -> Vec<Row> {
    let count = |name: u32, stat: u32, kind: &'static str| Row::new(name, Figure::Count(p.stat(stat) as i64), kind);
    let plain = |pairs: &[(u32, u32)]| pairs.iter().map(|&(name, stat)| count(name, stat, "")).collect::<Vec<_>>();
    // The distances in decimetres (kept in centimetres).
    let distance = |name: u32, stat: u32| Row::new(name, Figure::Count((p.stat(stat) / 10) as i64), "d");
    let percent = |hit: u32, shot: u32| Figure::Count(if p.stat(shot) == 0 { 0 } else { (p.stat(hit) as u64 * 100 / p.stat(shot) as u64) as i64 });
    match category {
        0 => PROGRESS
            .iter()
            .map(|&(name, total, milestones)| {
                let n = progress_count(p, name, extra).min(total);
                Row { name, value: Figure::Count(n as i64), kind: "", total: Some(Figure::Count(total as i64)), milestones }
            })
            .collect(),
        1 => vec![
            count(93369, stat::GAME_TIME, "t"),
            count(93370, stat::DAYS_SURVIVED, ""),
            distance(93321, stat::DISTANCE_WALKED),
            distance(93371, stat::DISTANCE_SWIMMING),
            distance(93382, stat::DISTANCE_DRIVEN_LAND),
            distance(157368, stat::DISTANCE_DRIVEN_SEA),
            distance(157369, stat::DISTANCE_IN_AIR),
            distance(93383, stat::DISTANCE_ZIPLINING),
        ],
        2 => plain(&[
            (93392, stat::KILLS),
            (93398, stat::IRON_SIGHT_KILLS),
            (93399, stat::HIP_FIRE_KILLS),
            (93400, stat::STEALTH_KILLS),
            (93401, stat::HEADSHOT_KILLS),
            (93402, stat::MELEE_KILLS),
            (93403, stat::FIRE_KILLS),
            (93404, stat::EXPLOSIVE_KILLS),
            (93405, stat::RUN_OVER_KILLS),
            (93406, stat::PISTOL_KILLS),
            (93411, stat::SMG_KILLS),
            (93412, stat::SHOTGUN_KILLS),
            (93413, stat::ASSAULT_RIFLE_KILLS),
            (93414, stat::SNIPER_KILLS),
            (93415, stat::LMG_KILLS),
            (93416, stat::RPG_KILLS),
            (93417, stat::GRENADE_LAUNCHER_KILLS),
            (93418, stat::FLARE_GUN_KILLS),
            (93419, stat::ARROW_KILLS),
            (93420, stat::LPO50_KILLS),
            (93421, stat::MOUNTED_GUN_KILLS),
        ]),
        3 => plain(&[
            (157370, stat::STANDARD_TAKEDOWNS),
            (157372, stat::FROM_ABOVE_TAKEDOWNS),
            (157373, stat::LEDGE_TAKEDOWNS),
            (157374, stat::WATER_TAKEDOWNS),
            (157375, stat::HEAVY_TAKEDOWNS),
            (157376, stat::PISTOL_TAKEDOWNS),
            (157377, stat::GRENADE_TAKEDOWNS),
            (157378, stat::KNIFE_THROW_TAKEDOWNS),
            (157379, stat::CHAIN_TAKEDOWNS),
        ]),
        4 => {
            let mut rows = vec![
                count(157405, stat::BULLETS_SHOT, ""),
                Row::new(157406, percent(stat::BULLETS_HIT, stat::BULLETS_SHOT), "p"),
                count(157407, stat::BULLETS_SHOT_IN_COVER, ""),
                Row::new(157409, percent(stat::BULLETS_HIT_IN_COVER, stat::BULLETS_SHOT_IN_COVER), "p"),
            ];
            rows.extend(FAVOURITE_WEAPONS.iter().filter(|(row, _)| *row != 157426).map(|&(row, _)| Row::new(row, favourite(p, row), "")));
            rows
        }
        5 => vec![
            // Money acquired: what the player has and has spent.
            Row::new(157419, Figure::Count(p.money as i64 + p.stat(stat::CASH_SPENT) as i64), "m"),
            count(157420, stat::CASH_SPENT, "m"),
            count(157422, stat::PLANTS_HARVESTED, ""),
            count(157423, stat::BODIES_LOOTED, ""),
            count(157425, stat::MANUAL_HEALS, ""),
            Row::new(157426, favourite(p, 157426), ""),
            count(157427, stat::POKER_PLAYED, ""),
            count(157428, stat::POKER_WON, ""),
            count(157429, stat::POKER_LOST, ""),
            count(157430, stat::POKER_WINNINGS, "m"),
            count(157431, stat::POKER_LARGEST_WIN, "m"),
            count(157432, stat::POKER_TIME, "t"),
            count(157433, stat::RACES_COMPLETED, ""),
            count(157434, stat::RACES_TIME, "t"),
            count(157435, stat::RACES_WINNINGS, "m"),
            count(157517, stat::SHOOTING_COMPLETED, ""),
            count(157518, stat::SHOOTING_TIME, "t"),
            count(157519, stat::SHOOTING_WINNINGS, "m"),
            count(157520, stat::KNIVES_WON, ""),
            count(157521, stat::KNIVES_TIME, "t"),
            count(157522, stat::KNIVES_WINNINGS, "m"),
            count(93360, stat::TRIALS_PLAYED, ""),
        ],
        6 => ANIMALS
            .iter()
            .map(|&(name, killed, skinned)| Row {
                name,
                value: Figure::Count(p.stat(killed) as i64),
                kind: "a",
                total: Some(skinned.map_or(Figure::Dash, |s| Figure::Count(p.stat(s) as i64))),
                milestones: "",
            })
            .collect(),
        7 => plain(&PLANTS),
        8 => plain(&CRAFTING),
        _ => Vec::new(),
    }
}

/// The animals page: (name, killed, skinned) — the total first, then the
/// animals, those that give no skin without a skinned column.
const ANIMALS: [(u32, u32, Option<u32>); 31] = [
    (157587, stat::ANIMAL_KILLS, Some(stat::ANIMALS_SKINNED)),
    (157553, 603, Some(602)),
    (106230, 605, Some(604)),
    (106080, 607, Some(606)),
    (157575, 611, Some(610)),
    (157574, 613, Some(612)),
    (157572, 615, Some(614)),
    (157568, 617, Some(616)),
    (157571, 601, None),
    (157566, 619, Some(618)),
    (157551, 621, Some(620)),
    (157564, 625, Some(624)),
    (157565, 627, Some(626)),
    (157560, 631, Some(630)),
    (157559, 633, Some(632)),
    (157558, 635, None),
    (157557, 637, Some(636)),
    (157550, 641, None),
    (157545, 645, Some(644)),
    (157577, 647, None),
    (157569, 649, None),
    (157567, 651, Some(650)),
    (157547, 653, None),
    (157546, 655, None),
    (157544, 657, Some(656)),
    (157573, 659, Some(658)),
    (157556, 665, None),
    (157555, 667, None),
    (157554, 671, None),
    (157548, 673, Some(672)),
    (157552, 678, Some(677)),
];

/// The plants page: (name, harvested).
const PLANTS: [(u32, u32); 17] = [
    (157543, 780),
    (157542, 781),
    (157541, 782),
    (157540, 785),
    (157539, 787),
    (157538, 788),
    (157537, 789),
    (157536, 790),
    (157535, 791),
    (157534, 792),
    (157533, 793),
    (157532, 794),
    (157531, 795),
    (157530, 796),
    (157529, 797),
    (157584, 798),
    (157585, 799),
];

/// The crafting page: (name, count). The engine had the one-hit-kill
/// syringe's crafted and used names the other way round; here each names
/// its own.
const CRAFTING: [(u32, u32); 26] = [
    (157589, stat::SYRINGES_CRAFTED),
    (157590, stat::SYRINGES_USED),
    (157591, stat::ARROWS_CRAFTED),
    (157592, stat::GEAR_CRAFTED),
    (157594, stat::MEDICINE_CRAFTED),
    (157593, stat::MEDICINE_USED),
    (157597, 836),
    (157598, 837),
    (157600, 838),
    (157599, 839),
    (157595, 840),
    (157596, 841),
    (157612, 844),
    (157613, 845),
    (157634, 846),
    (157635, 847),
    (157614, 848),
    (157615, 849),
    (157640, 852),
    (157641, 853),
    (167689, 860),
    (167690, 861),
    (157648, 862),
    (157649, 863),
    (157646, 864),
    (157647, 865),
];

/// Counts a statistic up.
pub fn add(p: &mut Progress, stat: u32, amount: u32) {
    if amount > 0 {
        let n = p.stats.entry(stat).or_insert(0);
        *n = n.saturating_add(amount);
    }
}

/// Keeps a statistic at its highest.
pub fn at_least(p: &mut Progress, stat: u32, value: u32) {
    let n = p.stats.entry(stat).or_insert(0);
    *n = (*n).max(value);
}

/// How the player killed someone: what hurt him last, the weapon in hand,
/// and how.
pub struct Kill<'a> {
    pub kind: crate::combat::DamageKind,
    pub weapon: Option<&'a crate::weapons::WeaponDef>,
    pub aiming: bool,
    pub headshot: bool,
    pub takedown: bool,
    /// He hadn't noticed the player.
    pub unaware: bool,
    /// The player was driving (a run-over comes as an explosion).
    pub driving: bool,
}

/// The kill counts a kill of a man adds to.
pub fn kill_stats(k: &Kill) -> Vec<u32> {
    use crate::combat::DamageKind;
    use crate::weapons::WeaponClass;
    let mut out = vec![stat::KILLS];
    let class = k.weapon.map(|w| w.class);
    let launched = matches!(class, Some(WeaponClass::Launcher | WeaponClass::GrenadeLauncher));
    let by_weapon = |out: &mut Vec<u32>| {
        out.extend(class.and_then(class_kills));
        out.extend(k.weapon.and_then(|w| weapon_kills(w.name)));
    };
    match k.kind {
        DamageKind::Melee if !k.takedown => {
            out.push(stat::MELEE_KILLS);
            out.extend(k.weapon.filter(|w| w.class == WeaponClass::Melee).and_then(|w| weapon_kills(w.name)));
        }
        DamageKind::Bullet => {
            by_weapon(&mut out);
            out.push(if k.aiming { stat::IRON_SIGHT_KILLS } else { stat::HIP_FIRE_KILLS });
        }
        DamageKind::Explosion if k.driving && !launched => out.push(stat::RUN_OVER_KILLS),
        DamageKind::Explosion => {
            out.push(stat::EXPLOSIVE_KILLS);
            if launched {
                by_weapon(&mut out);
            } else {
                out.push(stat::GRENADE_KILLS);
            }
        }
        DamageKind::Fire => {
            out.push(stat::FIRE_KILLS);
            if class == Some(WeaponClass::Flamethrower) {
                by_weapon(&mut out);
            }
        }
        _ => {}
    }
    if k.headshot {
        out.push(stat::HEADSHOT_KILLS);
    }
    if k.unaware && !k.takedown {
        out.push(stat::STEALTH_KILLS);
    }
    out
}

/// A poker game left, won or lost, and what it won over the buy-in.
pub fn poker_game(p: &mut Progress, won: bool, winnings: u32) {
    add(p, if won { stat::POKER_WON } else { stat::POKER_LOST }, 1);
    add(p, stat::POKER_WINNINGS, winnings);
    at_least(p, stat::POKER_LARGEST_WIN, winnings);
}

pub struct StatisticsPlugin;

impl Plugin for StatisticsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, count_play.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// What `count_play` carries from frame to frame.
#[derive(Default)]
struct Tracker {
    /// Time and distances not yet whole (seconds; centimetres), by
    /// statistic.
    seconds: HashMap<u32, f32>,
    centimetres: HashMap<u32, f32>,
    at: Option<Vec3>,
    hours: Option<f32>,
    shots: Option<u32>,
    /// Seconds since the counts were last logged.
    logged: f32,
}

/// A step longer than this in a frame is a jump (fast travel, a script's
/// teleport), not travel.
const JUMP: f32 = 30.0;

/// Counts what goes on as the player plays: the time played (and in each
/// game of chance or skill), the days gone by, how far the player goes and
/// how (on foot, swimming, driving on land or water, gliding, on zip lines),
/// the rounds fired and those that struck someone, the heals, and what is
/// crafted.
#[allow(clippy::too_many_arguments)]
fn count_play(
    time: Res<Time>,
    (sky, driving, mount): (Option<Res<crate::sky::TimeOfDay>>, Res<crate::vehicles::Driving>, Res<crate::traversal::Mount>),
    (races, knives, gallery, poker): (
        Option<Res<crate::races::Races>>,
        Option<Res<crate::knives::KnifeGame>>,
        Option<Res<crate::shooting_gallery::Gallery>>,
        Option<Res<crate::poker::PokerTable>>,
    ),
    players: Query<(Entity, &Transform, &crate::player::Player, &crate::weapons::Inventory)>,
    vehicles: Query<(&GlobalTransform, &crate::vehicles::Vehicle)>,
    struck: Query<(), Or<(With<crate::npc::Npc>, With<crate::wildlife::WildAnimal>)>>,
    (mut hits, mut actions, mut crafted): (
        MessageReader<crate::combat::DamageEvent>,
        MessageReader<crate::fp::FpAction>,
        MessageReader<crate::crafting::ItemCrafted>,
    ),
    catalog: Option<Res<crate::crafting::CraftCatalog>>,
    mut progress: ResMut<Progress>,
    mut t: Local<Tracker>,
) {
    use crate::traversal::Mount;
    use crate::vehicles::Kind;
    let p = &mut *progress;
    let dt = time.delta_secs();
    let tick = |t: &mut Tracker, p: &mut Progress, stat: u32| {
        let s = t.seconds.entry(stat).or_insert(0.0);
        *s += dt;
        if *s >= 1.0 {
            add(p, stat, *s as u32);
            *s = s.fract();
        }
    };
    tick(&mut t, p, stat::GAME_TIME);
    // (The counts in the log every ten seconds, at debug.)
    t.logged += dt;
    if t.logged > 10.0 {
        t.logged = 0.0;
        let mut counts: Vec<(u32, u32)> = p.stats.iter().map(|(k, v)| (*k, *v)).filter(|(_, v)| *v > 0).collect();
        counts.sort();
        debug!("statistics: {counts:?}");
    }
    for (on, stat) in [
        (races.as_ref().is_some_and(|r| r.holds_player()), stat::RACES_TIME),
        (knives.as_ref().is_some_and(|k| k.holds_player()), stat::KNIVES_TIME),
        (gallery.as_ref().is_some_and(|g| g.holds_player()), stat::SHOOTING_TIME),
        (poker.as_ref().is_some_and(|g| g.holds_player()), stat::POKER_TIME),
    ] {
        if on {
            tick(&mut t, p, stat);
        }
    }
    // A day survived each time the clock goes past midnight.
    if let Some(sky) = sky {
        if t.hours.is_some_and(|h| sky.hours + 12.0 < h) {
            add(p, stat::DAYS_SURVIVED, 1);
        }
        t.hours = Some(sky.hours);
    }
    let Ok((me, tf, player, inv)) = players.single() else { return };
    // How far, and how: the vehicle's way while driving one.
    let ride = driving.vehicle.and_then(|v| vehicles.get(v).ok());
    let at = ride.map_or(tf.translation, |(g, _)| g.translation());
    let how = match (ride.map(|(_, v)| v.kind()), *mount) {
        (_, Mount::Zip { .. }) => Some(stat::DISTANCE_ZIPLINING),
        (Some(Kind::Car), _) => Some(stat::DISTANCE_DRIVEN_LAND),
        (Some(Kind::Boat), _) => Some(stat::DISTANCE_DRIVEN_SEA),
        (Some(Kind::Glider), _) => Some(stat::DISTANCE_IN_AIR),
        _ if player.air != crate::player::AirMode::None => Some(stat::DISTANCE_IN_AIR),
        _ if player.noclip || player.driving => None,
        _ if player.swimming => Some(stat::DISTANCE_SWIMMING),
        _ => Some(stat::DISTANCE_WALKED),
    };
    if let (Some(stat), Some(was)) = (how, t.at) {
        let step = at.distance(was);
        if step < JUMP {
            let cm = t.centimetres.entry(stat).or_insert(0.0);
            *cm += step * 100.0;
            if *cm >= 1.0 {
                add(p, stat, *cm as u32);
                *cm = cm.fract();
            }
        }
    }
    t.at = Some(at);
    // The rounds fired (not the blade's swings or the flamethrower's
    // bursts), and as many as struck someone.
    let fired = t.shots.map_or(0, |s| inv.shots_fired.saturating_sub(s));
    t.shots = Some(inv.shots_fired);
    let gun = inv
        .slots
        .get(inv.current)
        .is_some_and(|s| !matches!(inv.defs[s.def].class, crate::weapons::WeaponClass::Melee | crate::weapons::WeaponClass::Flamethrower));
    if gun {
        add(p, stat::BULLETS_SHOT, fired);
    }
    let mut struck_now: Vec<Entity> = Vec::new();
    for h in hits.read() {
        if h.instigator == Some(me) && h.kind == crate::combat::DamageKind::Bullet && struck.contains(h.target) && !struck_now.contains(&h.target) {
            struck_now.push(h.target);
        }
    }
    let unmatched = p.stat(stat::BULLETS_SHOT).saturating_sub(p.stat(stat::BULLETS_HIT));
    add(p, stat::BULLETS_HIT, (struck_now.len() as u32).min(unmatched));
    for a in actions.read() {
        match a {
            crate::fp::FpAction::Syringe { .. } => {
                add(p, stat::SYRINGES_USED, 1);
                add(p, stat::MEDICINE_USED, 1);
            }
            crate::fp::FpAction::ManualHeal { .. } => add(p, stat::MANUAL_HEALS, 1),
            _ => {}
        }
    }
    for c in crafted.read() {
        use fc3_formats::progression::{KIND_ARROW, KIND_GEAR, KIND_MEDKIT, KIND_POTION};
        match c.kind {
            KIND_ARROW => add(p, stat::ARROWS_CRAFTED, 1),
            KIND_GEAR => add(p, stat::GEAR_CRAFTED, 1),
            KIND_POTION | KIND_MEDKIT => {
                add(p, stat::SYRINGES_CRAFTED, 1);
                // (The medicine's own count is `progression`'s.)
                let recipe = catalog.as_ref().and_then(|cat| cat.recipes.iter().find(|r| r.kind == c.kind && r.label == c.label));
                if let Some(s) = recipe.and_then(|r| Syringe::of(&r.id)) {
                    add(p, syringe_counts(s).0, 1);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_from_the_counters() {
        let mut p = Progress::default();
        add(&mut p, stat::BULLETS_SHOT, 8);
        add(&mut p, stat::BULLETS_HIT, 3);
        add(&mut p, 294, 4);
        add(&mut p, 301, 6);
        p.missions.extend(["doc_020_main", "doc_020_brief", "doc_040_opt20_main", "sqc_010", "sqc_010_brief", "sqa_d5", "sqa_d5_main"].map(String::from));
        let weapons = rows(4, &p, Extra::default());
        assert_eq!(weapons[1].value, Figure::Count(37));
        // The assault rifles' favourite: the STG-90 (6 kills over 4).
        assert_eq!(weapons.iter().find(|r| r.name == 157412).map(|r| r.value), Some(Figure::Text(42875)));
        assert_eq!(weapons.iter().find(|r| r.name == 157411).map(|r| r.value), Some(Figure::Dash));
        let progress = rows(0, &p, Extra { recipes: 3, outposts: 2 });
        assert_eq!(progress[0].value, Figure::Count(1));
        assert_eq!(progress[8].value, Figure::Count(1));
        assert_eq!(progress[10].value, Figure::Count(1));
        assert_eq!(progress[12].value, Figure::Count(3));
        assert_eq!(progress[2].value, Figure::Count(2));
        // Counted by type from then on, made up from the names first.
        mission_done(&mut p, "mission", false);
        mission_done(&mut p, "mission", true);
        mission_done(&mut p, "behaviourquest", false);
        assert_eq!(p.stat(stat::MISSIONS), 2);
        assert_eq!(p.stat(stat::SIDE_MISSIONS), 2);
        // Distances go in decimetres; the animals that give no skin have a
        // dash for it.
        add(&mut p, stat::DISTANCE_WALKED, 123_456);
        assert_eq!(rows(1, &p, Extra::default())[2].value, Figure::Count(12_345));
        assert_eq!(rows(6, &p, Extra::default())[8].total, Some(Figure::Dash));
        assert_eq!(rows(8, &p, Extra::default()).len(), 26);
    }

    #[test]
    fn kills_count_by_how() {
        use crate::combat::DamageKind;
        let defs = crate::weapons::weapon_defs();
        let def = |name: &str| defs.iter().find(|d| d.name == name);
        let kill = |kind, weapon, aiming, takedown, driving| Kill { kind, weapon, aiming, headshot: false, takedown, unaware: false, driving };
        // An aimed rifle shot: the rifles' count and the AK's own.
        let mut rifle = kill_stats(&kill(DamageKind::Bullet, def("AK-74M Rifle"), true, false, false));
        rifle.sort();
        assert_eq!(rifle, vec![stat::KILLS, stat::IRON_SIGHT_KILLS, stat::ASSAULT_RIFLE_KILLS, 294]);
        // Sniper rifles count as their own row.
        assert!(kill_stats(&kill(DamageKind::Bullet, def("SVD"), false, false, false)).contains(&stat::SNIPER_KILLS));
        // A takedown isn't a melee kill; a machete blow is.
        assert_eq!(kill_stats(&kill(DamageKind::Melee, def("Machete"), false, true, false)), vec![stat::KILLS]);
        assert!(kill_stats(&kill(DamageKind::Melee, def("Machete"), false, false, false)).contains(&266));
        // Run over while driving; a grenade otherwise; the RPG's own.
        assert!(kill_stats(&kill(DamageKind::Explosion, def("AK-74M Rifle"), false, false, true)).contains(&stat::RUN_OVER_KILLS));
        assert!(kill_stats(&kill(DamageKind::Explosion, def("AK-74M Rifle"), false, false, false)).contains(&stat::GRENADE_KILLS));
        let rpg = kill_stats(&kill(DamageKind::Explosion, def("RPG-7"), false, false, false));
        assert!(rpg.contains(&stat::RPG_KILLS) && rpg.contains(&189) && !rpg.contains(&stat::GRENADE_KILLS));
        // The flamethrower's fire: its row on the kills page.
        assert!(kill_stats(&kill(DamageKind::Fire, def("Flamethrower"), false, false, false)).contains(&stat::LPO50_KILLS));
    }
}
