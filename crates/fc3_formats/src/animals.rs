//! How animals behave, from their AI personalities among the object
//! templates (`aipersonality:animalpersonality.*`) and the sensory system
//! each animal archetype (`FC3_Animals:*`) links (`tmplSettings`): its
//! temper, what makes it aware of someone (smell, an immediate radius, a
//! moving vehicle, sounds by kind) and how soon awareness turns to action,
//! how far it wanders, avoids and flees, and how it paces its attacks. The
//! cache keeps them as text ([`ANIMALS_REL`]).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const ANIMALS_REL: &str = "characters/animals.txt";
const MAGIC: &str = "fc3-animals 2";

/// Whether a cached table is in this version's form.
pub fn is_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MAGIC)
}

/// An animal's temper (`selTemperType`).
pub const TEMPER_AGGRESSIVE: u8 = 1;

/// One animal archetype's behaviour. Radii and distances are metres, times
/// seconds; a negative radius means no limit (or, for `stealth_smell`, the
/// same as `smell`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Personality {
    /// The archetype's name (the last part of `FC3_Animals:…/Leopard`).
    pub name: String,
    /// Its sensory system ("aisensorysystem:default.default.animaldefault").
    pub senses: Option<String>,
    pub temper: u8,
    /// Charges a threat that comes too near, then gives up.
    pub defensive: bool,
    /// Turns on whoever hurts it.
    pub self_defend: bool,
    /// While idle, ignores someone in its smell radius (but not nearer).
    pub ignore_while_idle: bool,
    /// Someone this near becomes known to it (aware).
    pub smell: f32,
    pub stealth_smell: f32,
    /// Someone this near makes it act (flee or attack) at once.
    pub immediate: f32,
    /// A vehicle at full speed is noticed this far (less when slower).
    pub vehicle: f32,
    /// Aware this long with the threat within smell, it acts.
    pub aware_to_active: f32,
    pub aware_to_active_vehicle: f32,
    /// The threat beyond smell this long, it relaxes.
    pub aware_cooldown: f32,
    /// Nothing more than this far above or below is noticed.
    pub max_height: f32,
    pub interesting_sound: f32,
    pub threatening_sound: f32,
    pub explosion_sound: f32,
    /// The chance it wanders when idle, and how far.
    pub wander_chance: f32,
    pub wander: (f32, f32),
    /// How far it moves off from a threat it avoids, and how long before it
    /// relaxes again.
    pub avoid: (f32, f32),
    pub avoid_cooldown: f32,
    /// How far it runs from a threat that scares it.
    pub flee: (f32, f32),
    /// A defensive animal unhurt and hearing no threat this long stops.
    pub defensive_stop: f32,
    /// Seconds between attacks, and how many before it stops to look.
    pub attack_delay: (f32, f32),
    pub attacks_before_pause: (u32, u32),
    /// The farthest it can strike from where it stands, and the height
    /// difference it can strike across.
    pub attack_reach: f32,
    pub attack_height: f32,
    /// Its paces (`CAnimalAgentFC3`'s speeds, metres a second): walking,
    /// running and sprinting; 0 where the archetype gives none.
    pub walk: f32,
    pub run: f32,
    pub sprint: f32,
}

impl Personality {
    /// Aggressive animals that are not merely defensive hunt their threats.
    pub fn predator(&self) -> bool {
        self.temper == TEMPER_AGGRESSIVE && !self.defensive
    }

    /// The smell radius for a stealthy or upright threat.
    pub fn smell_for(&self, stealthy: bool) -> f32 {
        if stealthy && self.stealth_smell >= 0.0 {
            self.stealth_smell
        } else {
            self.smell
        }
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

fn int(o: &Object, name: &str) -> Option<u32> {
    let b = o.field(name)?;
    match b.len() {
        4 => Some(u32::from_le_bytes(b.try_into().ok()?)),
        1 => Some(b[0] as u32),
        _ => None,
    }
}

fn flag(o: &Object, name: &str) -> bool {
    o.field(name).is_some_and(|b| b.first().copied().unwrap_or(0) != 0)
}

fn link(o: &Object, name: &str) -> Option<u32> {
    o.field(name).filter(|b| b.len() == 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).filter(|h| *h != u32::MAX)
}

/// A personality template's behaviour.
fn personality(t: &Object) -> Personality {
    let group = |n: &str| t.child(n);
    let f = |g: Option<&std::sync::Arc<Object>>, n: &str| g.and_then(|g| float(g, n)).unwrap_or(0.0);
    let base = group("BaseBehavior");
    let aware = group("AwarenessAndDetection");
    let events = group("EventProcessing");
    let idle = group("IdleBehavior");
    let avoid = group("AvoidBehavior");
    let flee = group("FleeBehavior");
    let aggressive = group("AgressiveBehavior");
    let damage = aggressive.and_then(|a| a.child("AttackDamage"));
    let ranges = aggressive.and_then(|a| a.child("AttackRanges"));
    let i = |g: Option<&std::sync::Arc<Object>>, n: &str| g.and_then(|g| int(g, n)).unwrap_or(0);
    Personality {
        temper: i(base, "selTemperType") as u8,
        defensive: base.is_some_and(|b| flag(b, "bIsDefensive")),
        self_defend: base.is_some_and(|b| flag(b, "bCanSelfDefend")),
        ignore_while_idle: aware.is_some_and(|a| flag(a, "bIgnorePawnsDetectionWhileIdle")),
        smell: f(aware, "fThreatPosSmellDetectionRadius"),
        stealth_smell: f(aware, "fThreatStealthPosSmellDetectionRadius"),
        immediate: f(aware, "fThreatPosImmediateDetectionRadius"),
        vehicle: f(aware, "fMovingVehicleMaxDetectionRadius"),
        aware_to_active: f(aware, "fAwareToActiveTransitionTime"),
        aware_to_active_vehicle: f(aware, "fAwareToActiveTransitionTimeForVehicle"),
        aware_cooldown: f(aware, "fAwareCooldownTime"),
        max_height: f(aware, "fMaxHeightDetectionDistance"),
        interesting_sound: f(events, "fMaxInterestingSoundDistance"),
        threatening_sound: f(events, "fMaxThreateningSoundDistance"),
        explosion_sound: f(events, "fMaxExplosionSoundDistance"),
        wander_chance: f(idle, "fWanderingProbability"),
        wander: (f(idle, "fWanderingMinRadius"), f(idle, "fWanderingMaxRadius")),
        avoid: (f(avoid, "fRunAnnoyedMinRadius"), f(avoid, "fRunAnnoyedMaxRadius")),
        avoid_cooldown: f(avoid, "fFleeCooldownTime"),
        flee: (f(flee, "fRunScaredMinRadius"), f(flee, "fRunScaredMaxRadius")),
        defensive_stop: f(aggressive, "fDefensiveStopAggressiveTime"),
        attack_delay: (f(ranges, "fMinDelayBetweenAttacks"), f(ranges, "fMaxDelayBetweenAttacks")),
        attacks_before_pause: (i(ranges, "iMinAttackTimesBeforeDelay"), i(ranges, "iMaxAttackTimesBeforeDelay")),
        attack_reach: f(ranges, "fOnSpotAttackDistance"),
        attack_height: f(damage, "fMinVerticalAttackDistance"),
        ..Default::default()
    }
}

/// Every animal archetype in the entity library with its personality (and
/// sensory system) from the templates.
pub fn animals(library: &Object, templates: &Object) -> Vec<Personality> {
    let mut personalities = HashMap::new();
    let mut senses = HashMap::new();
    templates.walk(&mut |o, _| {
        let Some(name) = o.str("FullName") else { return };
        let hash = crate::hash::crc32(name.to_ascii_lowercase().as_bytes());
        if name.starts_with("aipersonality:animalpersonality") {
            personalities.insert(hash, personality(o));
        } else if name.starts_with("aisensorysystem:") {
            senses.insert(hash, name);
        }
    });
    let mut out = Vec::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let Some(full) = e.str("hidName") else { return };
        let Some(name) = full.strip_prefix("FC3_Animals:").map(|n| n.rsplit('/').next().unwrap_or(n).to_string()) else { return };
        let mut links = None;
        e.walk(&mut |o, _| {
            if links.is_none() {
                links = link(o, "tmplPersonalityType").map(|p| (p, link(o, "tmplSettings")));
            }
        });
        let Some((p, s)) = links else { return };
        let Some(base) = personalities.get(&p) else { return };
        let mut paces = [0.0f32; 3];
        e.walk(&mut |o, _| {
            for (i, n) in ["fSpeedsWalk", "fSpeedsRun", "fSpeedsSprint"].iter().enumerate() {
                if paces[i] == 0.0 {
                    paces[i] = float(o, n).unwrap_or(0.0);
                }
            }
        });
        out.push(Personality { name, senses: s.and_then(|s| senses.get(&s).cloned()), walk: paces[0], run: paces[1], sprint: paces[2], ..base.clone() });
    });
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

impl Personality {
    fn numbers(&self) -> [f32; 24] {
        [
            self.temper as f32,
            u8::from(self.defensive) as f32,
            u8::from(self.self_defend) as f32,
            u8::from(self.ignore_while_idle) as f32,
            self.smell,
            self.stealth_smell,
            self.immediate,
            self.vehicle,
            self.aware_to_active,
            self.aware_to_active_vehicle,
            self.aware_cooldown,
            self.max_height,
            self.interesting_sound,
            self.threatening_sound,
            self.explosion_sound,
            self.wander_chance,
            self.wander.0,
            self.wander.1,
            self.avoid.0,
            self.avoid.1,
            self.avoid_cooldown,
            self.flee.0,
            self.flee.1,
            self.defensive_stop,
        ]
    }
}

pub fn to_text(all: &[Personality]) -> String {
    let mut s = format!("{MAGIC}\n");
    for p in all {
        let mut n: Vec<String> = p.numbers().iter().map(|v| v.to_string()).collect();
        n.extend([p.attack_delay.0, p.attack_delay.1].map(|v| v.to_string()));
        n.extend([p.attacks_before_pause.0, p.attacks_before_pause.1].map(|v| v.to_string()));
        n.extend([p.attack_reach, p.attack_height, p.walk, p.run, p.sprint].map(|v| v.to_string()));
        s += &format!("{}\t{}\t{}\n", p.name, p.senses.as_deref().unwrap_or("-"), n.join(","));
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<Personality>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not an animals table");
    }
    let mut out = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        let v: Vec<f32> = f[2].split(',').map(|x| x.parse().unwrap_or(0.0)).collect();
        if v.len() < 33 {
            continue;
        }
        out.push(Personality {
            name: f[0].into(),
            senses: (f[1] != "-").then(|| f[1].to_string()),
            temper: v[0] as u8,
            defensive: v[1] != 0.0,
            self_defend: v[2] != 0.0,
            ignore_while_idle: v[3] != 0.0,
            smell: v[4],
            stealth_smell: v[5],
            immediate: v[6],
            vehicle: v[7],
            aware_to_active: v[8],
            aware_to_active_vehicle: v[9],
            aware_cooldown: v[10],
            max_height: v[11],
            interesting_sound: v[12],
            threatening_sound: v[13],
            explosion_sound: v[14],
            wander_chance: v[15],
            wander: (v[16], v[17]),
            avoid: (v[18], v[19]),
            avoid_cooldown: v[20],
            flee: (v[21], v[22]),
            defensive_stop: v[23],
            attack_delay: (v[24], v[25]),
            attacks_before_pause: (v[26] as u32, v[27] as u32),
            attack_reach: v[28],
            attack_height: v[29],
            walk: v[30],
            run: v[31],
            sprint: v[32],
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_reads_tempers() {
        let p = Personality {
            name: "Leopard".into(),
            senses: Some("aisensorysystem:default.default.animaldefault".into()),
            temper: TEMPER_AGGRESSIVE,
            smell: 25.0,
            stealth_smell: 5.0,
            immediate: 5.0,
            flee: (30.0, 40.0),
            attack_delay: (0.9, 1.2),
            attacks_before_pause: (2, 3),
            attack_reach: 2.5,
            ..Default::default()
        };
        let boar = Personality { name: "Boar".into(), temper: 2, defensive: true, stealth_smell: -1.0, smell: 25.0, ..Default::default() };
        let back = from_text(&to_text(&[p.clone(), boar.clone()])).unwrap();
        assert_eq!(back, vec![p.clone(), boar.clone()]);
        assert!(p.predator() && !boar.predator());
        assert_eq!(p.smell_for(true), 5.0);
        assert_eq!(boar.smell_for(true), 25.0);
    }
}
