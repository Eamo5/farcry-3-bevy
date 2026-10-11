//! How much damage characters and animals take, from the game's data: the
//! counters service's health table (`nomadobjecttemplates.fcb`,
//! `CountersService`: a character's whole health and the share of it each
//! of the head, torso and legs can take), and each enemy archetype's and
//! animal's counters component (`CFCXCountersComponentAI` /
//! `CFCXCountersComponentAnimal`: armour on each body part against bullets,
//! a multiplier for each kind of harm, an animal's own health and head
//! multiplier), with an animal's attack from its AI personality
//! (`AttackDamage`). The cache keeps them as text ([`DAMAGE_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const DAMAGE_REL: &str = "characters/damage.txt";
const MAGIC: &str = "fc3-damage 4";
const SERVICE: &str = "countersservice:counters.countersservice";

/// A character's health and how much of it each body part can lose before
/// it is dead (a head shot past `head` kills outright).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthTable {
    pub global: f32,
    pub head: f32,
    pub torso: f32,
    pub legs: f32,
}

impl Default for HealthTable {
    fn default() -> Self {
        HealthTable { global: 100.0, head: 10.0, torso: 60.0, legs: 100.0 }
    }
}

/// The player's health by difficulty (casual, normal, hardcore, infamous):
/// how much each health bar holds against the normal level
/// (`HealthDifficultySettings`' `fHealthTierValueMultiplier`), and the
/// seconds after a hurt before it regenerates (`HealthRegenerationDelays`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerHealth {
    pub bar_scale: [f32; 4],
    pub regen_delay: [f32; 4],
}

impl Default for PlayerHealth {
    fn default() -> Self {
        PlayerHealth { bar_scale: [1.0; 4], regen_delay: [5.0; 4] }
    }
}

/// The kinds of harm the game multiplies separately (its stims).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harm {
    /// Bullets.
    Pierce,
    /// Blades.
    Cut,
    /// Blows, falls and being run over.
    Crush,
    Burn,
    Explosion,
    Grenade,
    Rocket,
    Ied,
    Splash,
    Arrow,
}

const HARMS: [(Harm, &str); 10] = [
    (Harm::Pierce, "fPierceStimModifier"),
    (Harm::Cut, "fCutStimModifier"),
    (Harm::Crush, "fCrushStimModifier"),
    (Harm::Burn, "fBurnStimModifier"),
    (Harm::Explosion, "fExplosionStimModifier"),
    (Harm::Grenade, "fExplosionGrenadesStimModifier"),
    (Harm::Rocket, "fExplosionRPGStimModifier"),
    (Harm::Ied, "fExplosionIEDStimModifier"),
    (Harm::Splash, "fExplosionSplashStimModifier"),
    (Harm::Arrow, "fArrowStimModifier"),
];

/// One archetype's (or animal's) counters.
#[derive(Debug, Clone, PartialEq)]
pub struct Vulnerability {
    /// The prototype's name: an enemy's after `BaseMerc_FC3.`
    /// ("Pirate_HeavyGunner"), an animal's after its group
    /// ("Tiger").
    pub name: String,
    /// An animal (no body parts' shares, only a head multiplier).
    pub animal: bool,
    /// Its own health, where it has one (animals, overrides).
    pub health: Option<f32>,
    /// Multiplies all damage taken.
    pub body: f32,
    /// Bullets into the head, torso and legs (armour), or for animals the
    /// head's multiplier (torso and legs 1).
    pub head: f32,
    pub torso: f32,
    pub legs: f32,
    /// Each kind of harm, in [`Harm`] order.
    pub harms: [f32; 10],
    /// An animal's attack.
    pub attack: Option<Attack>,
}

/// An animal's bite or charge (its personality's `AttackDamage`): between
/// `min` and `max` to characters; to the player, where `percent` is set,
/// that share of the player's health, less `per_level` for each health bar
/// the player has gained.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Attack {
    pub min: f32,
    pub max: f32,
    pub percent: f32,
    pub per_level: f32,
}

impl Attack {
    /// The damage to the player with `extra_bars` health bars gained, or
    /// `None` when it is dealt as to anyone else.
    pub fn to_player(&self, max_health: f32, extra_bars: f32) -> Option<f32> {
        (self.percent > 0.0).then(|| max_health * (self.percent - self.per_level * extra_bars.max(0.0)).max(self.per_level))
    }
}

impl Default for Vulnerability {
    fn default() -> Self {
        Vulnerability { name: String::new(), animal: false, health: None, body: 1.0, head: 1.0, torso: 1.0, legs: 1.0, harms: [1.0; 10], attack: None }
    }
}

impl Vulnerability {
    pub fn harm(&self, h: Harm) -> f32 {
        self.harms[HARMS.iter().position(|(x, _)| *x == h).unwrap_or(0)]
    }

    /// From a counters component (`CFCXCountersComponentAI` or `...Animal`).
    pub fn from_component(name: &str, c: &Object) -> Vulnerability {
        let animal = c.is("CFCXCountersComponentAnimal");
        let mut v = Vulnerability { name: name.to_string(), animal, ..Default::default() };
        let f = |o: &Object, n: &str| float(o, n);
        v.body = f(c, "fFullBodyDamageMultiplier").unwrap_or(1.0);
        // (An archetype's override is -1 when unset.)
        v.health = f(c, "fAgentHealth").or_else(|| f(c, "fAgentHealthOverride")).filter(|h| *h > 0.0);
        if let Some(m) = c.child("Multipliers") {
            let part = |p: &str, n: &str| m.child(p).and_then(|o| f(o, n)).unwrap_or(1.0);
            v.head = part("Head", "fHeadArmorPierceStimModifier");
            v.torso = part("Torso", "fTorsoArmorPierceStimModifier");
            v.legs = part("Legs", "fLegsArmorPierceStimModifier");
        }
        if let Some(h) = f(c, "fHeadDamageMultiplier") {
            v.head = h;
        }
        let stims = c.child("Multipliers").and_then(|m| m.child("GlobalMultipliers")).or_else(|| c.child("GlobalMultipliers")).and_then(|g| g.child("Stims"));
        if let Some(s) = stims {
            for (i, (_, n)) in HARMS.iter().enumerate() {
                v.harms[i] = f(s, n).unwrap_or(1.0);
            }
        }
        v
    }
}

fn float(o: &Object, name: &str) -> Option<f32> {
    let b = o.field(name)?;
    match b.len() {
        4 => Some(f32::from_le_bytes(b.try_into().ok()?)),
        // (A zero is stored as a single byte.)
        1 => Some(b[0] as f32),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Damage {
    pub table: HealthTable,
    pub player: PlayerHealth,
    pub archetypes: Vec<Vulnerability>,
}

impl Damage {
    /// An archetype by name, or the longest one a derived name starts with
    /// ("Pirate_Assaulter_Commander" -> "Pirate_Assaulter").
    pub fn get(&self, name: &str) -> Option<&Vulnerability> {
        let n = name.to_ascii_lowercase();
        self.archetypes
            .iter()
            .filter(|v| {
                let a = v.name.to_ascii_lowercase();
                n == a || n.starts_with(&format!("{a}_")) || n.starts_with(&format!("{a}."))
            })
            .max_by_key(|v| v.name.len())
    }
}

/// The player's health by difficulty from the counters service.
pub fn player_health(templates: &Object) -> Option<PlayerHealth> {
    let mut out = None;
    templates.walk(&mut |o, _| {
        if out.is_some() || !o.str("FullName").is_some_and(|n| n.eq_ignore_ascii_case(SERVICE)) {
            return;
        }
        let mut p = PlayerHealth::default();
        if let Some(d) = o.child("HealthDifficultySettings") {
            for (i, level) in ["HealthCasual", "HealthExperimented", "HealthHardcore", "HealthInfamous"].iter().enumerate() {
                if let Some(v) = d.child(level).and_then(|l| float(l, "fHealthTierValueMultiplier")).filter(|v| *v > 0.0) {
                    p.bar_scale[i] = v;
                }
            }
        }
        if let Some(delays) = o.child("StatsAndCounters").and_then(|s| s.child("Health")).and_then(|h| h.child("HealthRegenerationDelays")) {
            for (i, level) in delays.children_named("DifficultyLevel").take(4).enumerate() {
                if let Some(v) = float(level, "RegenDelay").filter(|v| *v > 0.0) {
                    p.regen_delay[i] = v;
                }
            }
        }
        out = Some(p);
    });
    out
}

/// The counters service's health table from the object templates.
pub fn health_table(templates: &Object) -> Option<HealthTable> {
    let mut out = None;
    templates.walk(&mut |o, _| {
        if out.is_none() && o.str("FullName").is_some_and(|n| n.eq_ignore_ascii_case(SERVICE)) {
            if let Some(t) = o.child("CounterHealthTable") {
                let f = |n: &str, d: f32| float(t, n).filter(|v| *v > 0.0).unwrap_or(d);
                let d = HealthTable::default();
                out = Some(HealthTable {
                    global: f("GlobalHealth", d.global),
                    head: f("HeadHealth", d.head),
                    torso: f("TorsoHealth", d.torso),
                    legs: f("LegsHealth", d.legs),
                });
            }
        }
    });
    out
}

/// The animals' attacks by their personality template (the CRC of its full
/// name, lower case, as animals link it).
fn attacks(templates: &Object) -> std::collections::HashMap<u32, Attack> {
    let mut out = std::collections::HashMap::new();
    templates.walk(&mut |o, _| {
        let Some(name) = o.str("FullName").filter(|n| n.starts_with("aipersonality:")) else { return };
        let Some(a) = o.child("AgressiveBehavior").and_then(|b| b.child("AttackDamage")) else { return };
        let f = |n: &str| float(a, n).unwrap_or(0.0);
        let by_percent = a.field("bDamageToPlayerByPercentage").is_some_and(|b| b.first().copied().unwrap_or(0) != 0);
        let attack = Attack {
            min: f("fMinAttackDamage"),
            max: f("fMaxAttackDamage"),
            percent: if by_percent { f("fDamagePercentage") } else { 0.0 },
            per_level: f("fDamageByPercentageReducedFactorDueToPlayerLevel"),
        };
        out.insert(crate::hash::crc32(name.to_ascii_lowercase().as_bytes()), attack);
    });
    out
}

/// The enemy archetypes' (`FC3_NPC:Merc/BaseMerc_FC3.*`) and animals'
/// (`FC3_Animals:*`) counters from the entity library, with the animals'
/// attacks from their personalities among the templates.
pub fn archetypes(library: &Object, templates: &Object) -> Vec<Vulnerability> {
    let attacks = attacks(templates);
    let mut out = Vec::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let Some(full) = e.str("hidName") else { return };
        let name = if let Some(n) = full.strip_prefix("FC3_NPC:Merc/BaseMerc_FC3.") {
            n.to_string()
        } else if let Some(n) = full.strip_prefix("FC3_Animals:") {
            n.rsplit('/').next().unwrap_or(n).to_string()
        } else {
            return;
        };
        let Some(c) = e.child("Components").and_then(|c| c.child("CFCXCountersComponentAI").or_else(|| c.child("CFCXCountersComponentAnimal"))) else { return };
        let mut v = Vulnerability::from_component(&name, c);
        if v.animal {
            let mut personality = None;
            e.walk(&mut |o, _| {
                if personality.is_none() {
                    personality = o.field("tmplPersonalityType").filter(|b| b.len() == 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
                }
            });
            v.attack = personality.and_then(|p| attacks.get(&p)).copied().filter(|a| a.max > 0.0 || a.percent > 0.0);
        }
        out.push(v);
    });
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

pub fn to_text(d: &Damage) -> String {
    let t = d.table;
    let mut s = format!("{MAGIC}\ntable\t{}\t{}\t{}\t{}\n", t.global, t.head, t.torso, t.legs);
    let list = |v: &[f32; 4]| v.map(|x| x.to_string()).join(",");
    s += &format!("player\t{}\t{}\n", list(&d.player.bar_scale), list(&d.player.regen_delay));
    for v in &d.archetypes {
        let harms = v.harms.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(",");
        let attack = v.attack.map_or("-".to_string(), |a| format!("{},{},{},{}", a.min, a.max, a.percent, a.per_level));
        s += &format!("{}\t{}\t{}\t{}\t{}\t{}\t{harms}\t{}\t{attack}\n", v.name, v.health.unwrap_or(-1.0), v.body, v.head, v.torso, v.legs, u8::from(v.animal));
    }
    s
}

pub fn from_text(text: &str) -> Result<Damage> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a damage table");
    }
    let mut out = Damage::default();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        let n = |i: usize| f.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let four = |i: usize, d: f32| {
            let mut v = [d; 4];
            for (k, x) in f.get(i).copied().unwrap_or_default().split(',').take(4).enumerate() {
                v[k] = x.parse().unwrap_or(d);
            }
            v
        };
        if f[0] == "table" && f.len() >= 5 {
            out.table = HealthTable { global: n(1), head: n(2), torso: n(3), legs: n(4) };
        } else if f[0] == "player" && f.len() >= 3 {
            out.player = PlayerHealth { bar_scale: four(1, 1.0), regen_delay: four(2, 5.0) };
        } else if f.len() >= 7 {
            let mut harms = [1.0; 10];
            for (i, h) in f[6].split(',').take(10).enumerate() {
                harms[i] = h.parse().unwrap_or(1.0);
            }
            out.archetypes.push(Vulnerability {
                name: f[0].into(),
                animal: f.get(7) == Some(&"1"),
                attack: f.get(8).and_then(|a| {
                    let v: Vec<f32> = a.split(',').filter_map(|x| x.parse().ok()).collect();
                    (v.len() == 4).then(|| Attack { min: v[0], max: v[1], percent: v[2], per_level: v[3] })
                }),
                health: Some(n(1)).filter(|h| *h > 0.0),
                body: n(2),
                head: n(3),
                torso: n(4),
                legs: n(5),
                harms,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_finds_derived_archetypes() {
        let mut heavy = Vulnerability { name: "Pirate_HeavyGunner".into(), torso: 0.025, legs: 0.04, ..Default::default() };
        heavy.harms[9] = 4.0;
        let attack = Attack { min: 40.0, max: 70.0, percent: 0.5, per_level: 0.06 };
        let tiger = Vulnerability { name: "Tiger".into(), animal: true, health: Some(800.0), head: 2.3, attack: Some(attack), ..Default::default() };
        assert_eq!(attack.to_player(100.0, 0.0), Some(50.0));
        assert!((attack.to_player(200.0, 4.0).unwrap() - 52.0).abs() < 1e-3);
        assert_eq!(Attack { percent: 0.0, ..attack }.to_player(100.0, 0.0), None);
        let player = PlayerHealth { bar_scale: [1.5, 1.0, 0.6, 0.3], regen_delay: [5.0; 4] };
        let d = Damage { table: HealthTable::default(), player, archetypes: vec![heavy.clone(), tiger.clone()] };
        let back = from_text(&to_text(&d)).unwrap();
        assert_eq!(back, d);
        assert_eq!(back.get("pirate_heavygunner_elite").map(|v| v.torso), Some(0.025));
        assert_eq!(back.get("Pirate_HeavyGunner").unwrap().harm(Harm::Arrow), 4.0);
        assert_eq!(back.get("Tiger").and_then(|v| v.health), Some(800.0));
        assert!(back.get("Pirate").is_none());
    }
}
