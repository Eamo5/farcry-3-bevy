//! How often the AI's fire lands on the player, from the object templates'
//! shooting preferences (`aishooterprefs:shootingpreferences.<kind>`): the
//! AI's *focus* on its target (0 to 1) is the product of multipliers for the
//! distance, how long it has kept shooting without losing the target, how
//! the target moves, how exposed it is and how many are shooting at it; the
//! focus gives, by difficulty, the *damage lap* (seconds between damaging
//! hits), the least time before the first, and the *damage power* (the
//! share of the weapon's most damage a hit does); shots between hits miss by
//! a distance that shrinks as the next hit nears. With them, the NPC
//! weapons' most damage (`WeaponProperties:FC3/*_NPC`). The cache keeps
//! them as text ([`AIM_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const AIM_REL: &str = "characters/aim.txt";
const MAGIC: &str = "fc3-aim 2";
/// The kinds of shooting preferences kept.
pub const KINDS: [&str; 7] = ["assaultrifle", "shotgun", "rifle", "machinegun", "smg", "rpg", "flamer"];
const PREFIX: &str = "aishooterprefs:shootingpreferences.";
const LEVELS: [&str; 4] = ["Casual", "Experimented", "Hardcore", "Infamous"];

/// A curve's knots (x, y); two at one x make a step.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Curve(pub Vec<(f32, f32)>);

impl Curve {
    /// The value at `x`: linear between knots, the later of two at one x
    /// from there on, the ends held beyond.
    pub fn at(&self, x: f32) -> f32 {
        let k = &self.0;
        let (Some(first), Some(last)) = (k.first(), k.last()) else { return 0.0 };
        if x <= first.0 {
            return first.1;
        }
        if x >= last.0 {
            return last.1;
        }
        let i = k.partition_point(|p| p.0 <= x).clamp(1, k.len() - 1);
        let (a, b) = (k[i - 1], k[i]);
        if b.0 <= a.0 {
            return b.1;
        }
        a.1 + (b.1 - a.1) * (x - a.0) / (b.0 - a.0)
    }

    fn text(&self) -> String {
        self.0.iter().map(|(x, y)| format!("{x}:{y}")).collect::<Vec<_>>().join(";")
    }

    fn parse(s: &str) -> Curve {
        Curve(s.split(';').filter_map(|p| p.split_once(':')).filter_map(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))).collect())
    }
}

/// One kind's preferences.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Prefs {
    /// "assaultrifle", ...
    pub kind: String,
    /// It won't fire beyond this (m).
    pub firing_range: f32,
    /// Focus multipliers: by distance (m), by seconds shooting without
    /// losing the target.
    pub distance: Curve,
    pub chaining: Curve,
    /// The chaining starts over once the target has been out of sight this
    /// long (s; negative: never by time) or moved this far out of sight (m).
    pub chain_reset_time: f32,
    pub chain_reset_move: f32,
    /// The target's movement: below `min_speed` none; otherwise moving
    /// across, away, or toward the shooter (toward only within
    /// `towards_within` m).
    pub min_speed: f32,
    pub lateral: f32,
    pub away: f32,
    pub towards: f32,
    pub towards_within: f32,
    /// The target's exposure: high, medium, low.
    pub exposure: [f32; 3],
    /// Two, three, four or more shooting at once.
    pub agents: [f32; 3],
    /// By difficulty (casual, normal, hardcore, infamous): the damage lap
    /// (s) and least time before damage (s) from focus, and the damage power
    /// from focus.
    pub lap: [Curve; 4],
    pub buffer: [Curve; 4],
    pub power: [Curve; 4],
    /// How far a shot between hits misses (m), by seconds to the next hit.
    pub miss: Curve,
}

impl Prefs {
    /// The focus for a target `distance` away, shot at for `chained`
    /// seconds, moving as `movement` and exposed as `exposure` say, with
    /// `agents` shooting at it.
    pub fn focus(&self, distance: f32, chained: f32, movement: f32, exposure: f32, agents: u32) -> f32 {
        let agents = match agents {
            0 | 1 => 1.0,
            2 => self.agents[0],
            3 => self.agents[1],
            _ => self.agents[2],
        };
        (self.distance.at(distance) * self.chaining.at(chained) * movement * exposure * agents).clamp(0.0, 1.0)
    }

    /// The movement multiplier for a target moving at `velocity`, with
    /// `to_shooter` the direction from it to the shooter and `distance`
    /// between them.
    pub fn movement(&self, velocity: [f32; 3], to_shooter: [f32; 3], distance: f32) -> f32 {
        let speed = (velocity[0] * velocity[0] + velocity[2] * velocity[2]).sqrt();
        if speed < self.min_speed.max(0.01) {
            return 1.0;
        }
        let len = (to_shooter[0] * to_shooter[0] + to_shooter[2] * to_shooter[2]).sqrt().max(1e-3);
        let along = (velocity[0] * to_shooter[0] + velocity[2] * to_shooter[2]) / (len * speed);
        if along > 0.7 {
            if distance <= self.towards_within {
                self.towards
            } else {
                1.0
            }
        } else if along < -0.7 {
            self.away
        } else {
            self.lateral
        }
    }
}

/// Everything kept.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Aim {
    pub prefs: Vec<Prefs>,
    /// The player's damage group: an AI's progress on him falls back (by
    /// `decay_rate` percent a second) once it has stopped for `decay_start`
    /// seconds.
    pub decay_start: f32,
    pub decay_rate: f32,
    /// NPC weapons' most damage, by name after `FC3/` ("AK47.AK47_NPC").
    pub weapons: Vec<(String, f32)>,
}

impl Aim {
    pub fn prefs(&self, kind: &str) -> Option<&Prefs> {
        self.prefs.iter().find(|p| p.kind == kind)
    }

    /// An NPC weapon's most damage, by the end of its name ("AK47_NPC").
    pub fn weapon(&self, name: &str) -> Option<f32> {
        self.weapons
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name) || n.to_ascii_lowercase().ends_with(&format!(".{}", name.to_ascii_lowercase())))
            .map(|w| w.1)
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

fn curve(o: Option<&Object>, name: &str) -> Curve {
    let Some(list) = o.and_then(|o| o.child(name)).and_then(|c| c.child("Knots")) else { return Curve::default() };
    Curve(
        list.children_named("Knot")
            .filter_map(|k| {
                let v = k.field("Value")?;
                let f = |i: usize| v.get(i * 4..i * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
                Some((f(0)?, f(1)?))
            })
            .collect(),
    )
}

fn prefs(kind: &str, t: &Object) -> Prefs {
    let child = |o: &Object, path: &[&str]| -> Option<std::sync::Arc<Object>> {
        let mut cur = o.child(path[0])?.clone();
        for p in &path[1..] {
            cur = cur.child(p)?.clone();
        }
        Some(cur)
    };
    let focus = t.child("Focus");
    let movement = focus.and_then(|f| f.child("TargetMovement"));
    let exposure = focus.and_then(|f| f.child("TargetExposure"));
    let agents = focus.and_then(|f| f.child("AgentCountMuliplier"));
    let f = |o: Option<&std::sync::Arc<Object>>, n: &str, d: f32| o.and_then(|o| float(o, n)).unwrap_or(d);
    let by_level = |section: &str, name: &str| -> [Curve; 4] { LEVELS.map(|l| curve(child(t, &[section, "Difficulty", l]).as_deref(), name)) };
    Prefs {
        kind: kind.to_string(),
        firing_range: f(t.child("Ranges"), "fFiringRange", 80.0),
        distance: curve(focus.and_then(|f| f.child("Distance")).map(|a| a.as_ref()), "curveDistanceMultiplier"),
        chaining: curve(focus.and_then(|f| f.child("Chaining")).map(|a| a.as_ref()), "curveChainingTimeMultiplier"),
        chain_reset_time: f(focus.and_then(|f| f.child("Chaining")), "ChainingResetBrokenLOSTime", -1.0),
        chain_reset_move: f(focus.and_then(|f| f.child("Chaining")), "ChainingResetBrokenLOSTargetMoveDistance", 2.0),
        min_speed: f(movement, "TargetMinVelocity", 3.0),
        lateral: f(movement, "TargetMoveLaterallyMultiplier", 1.0),
        away: f(movement, "TargetMoveAwayMultiplier", 1.0),
        towards: f(movement, "TargetMoveTowardsMultiplier", 1.0),
        towards_within: f(movement, "TargetMoveTowardsMaxDistance", 0.0),
        exposure: [
            f(exposure, "TargetHighExposureMultiplier", 1.0),
            f(exposure, "TargetMediumExposureMultiplier", 1.0),
            f(exposure, "TargetLowExposureMultiplier", 1.0),
        ],
        agents: [f(agents, "AgentCount_2", 1.0), f(agents, "AgentCount_3", 1.0), f(agents, "AgentCount_4_and_more", 1.0)],
        lap: by_level("DamageLap", "curveDamageLapFromFocus"),
        buffer: by_level("DamageLap", "curveDamageLapBufferTimeFromFocus"),
        power: by_level("DamagePower", "curveDamagePowerFromFocus"),
        miss: curve(t.child("Aiming").map(|a| a.as_ref()), "curveMissDistanceFromTimeBeforeNextHit"),
    }
}

/// The kinds' preferences from the object templates, and the NPC weapons'
/// most damage from the entity library.
pub fn aim(templates: &Object, library: &Object) -> Aim {
    let mut out = Aim::default();
    templates.walk(&mut |o, _| {
        let Some(name) = o.str("FullName") else { return };
        let Some(kind) = name.strip_prefix(PREFIX).filter(|k| KINDS.contains(k)) else { return };
        if !out.prefs.iter().any(|p| p.kind == kind) {
            out.prefs.push(prefs(kind, o));
        }
    });
    out.decay_start = 1.0;
    out.decay_rate = 20.0;
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        // The player's damage group.
        if e.str("hidName").as_deref() == Some("player:MainCharacter/PawnPlayer") {
            e.walk(&mut |c, _| {
                if c.is("DamageGroups") {
                    if let Some(p) = c.child("Default").and_then(|d| d.child("ProgressSettings")) {
                        out.decay_start = float(p, "DecayStartTime").unwrap_or(out.decay_start);
                        out.decay_rate = float(p, "DecayRate").unwrap_or(out.decay_rate);
                    }
                }
            });
        }
        let Some(name) = e.str("hidName").and_then(|n| n.strip_prefix("WeaponProperties:FC3/").map(str::to_string)) else { return };
        if !name.to_ascii_lowercase().contains("_npc") || name.to_ascii_lowercase().contains("coop") {
            return;
        }
        let mut damage = None;
        e.walk(&mut |c, _| {
            if damage.is_none() && c.is("BaseDamage") {
                damage = float(c, "fLevel").filter(|d| *d > 0.0);
            }
        });
        if let Some(d) = damage {
            out.weapons.push((name, d));
        }
    });
    out.prefs.sort_by(|a, b| a.kind.cmp(&b.kind));
    out.weapons.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn to_text(a: &Aim) -> String {
    let mut s = format!("{MAGIC}\ndecay\t{}\t{}\n", a.decay_start, a.decay_rate);
    for p in &a.prefs {
        let nums = [
            p.firing_range,
            p.min_speed,
            p.lateral,
            p.away,
            p.towards,
            p.towards_within,
            p.exposure[0],
            p.exposure[1],
            p.exposure[2],
            p.agents[0],
            p.agents[1],
            p.agents[2],
            p.chain_reset_time,
            p.chain_reset_move,
        ]
        .map(|v| v.to_string())
        .join(",");
        let mut cols = vec![format!("prefs\t{}\t{nums}", p.kind), p.distance.text(), p.chaining.text(), p.miss.text()];
        for set in [&p.lap, &p.buffer, &p.power] {
            cols.extend(set.iter().map(|c| c.text()));
        }
        s += &cols.join("\t");
        s.push('\n');
    }
    for (n, d) in &a.weapons {
        s += &format!("weapon\t{n}\t{d}\n");
    }
    s
}

pub fn from_text(text: &str) -> Result<Aim> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not an aim table");
    }
    let mut out = Aim::default();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        match f.first().copied() {
            Some("weapon") if f.len() >= 3 => out.weapons.push((f[1].into(), f[2].parse().unwrap_or(0.0))),
            Some("decay") if f.len() >= 3 => {
                out.decay_start = f[1].parse().unwrap_or(1.0);
                out.decay_rate = f[2].parse().unwrap_or(20.0);
            }
            Some("prefs") if f.len() >= 18 => {
                let n: Vec<f32> = f[2].split(',').map(|v| v.parse().unwrap_or(1.0)).collect();
                if n.len() < 14 {
                    continue;
                }
                let four = |at: usize| [0, 1, 2, 3].map(|i| Curve::parse(f[at + i]));
                out.prefs.push(Prefs {
                    kind: f[1].into(),
                    firing_range: n[0],
                    min_speed: n[1],
                    lateral: n[2],
                    away: n[3],
                    towards: n[4],
                    towards_within: n[5],
                    exposure: [n[6], n[7], n[8]],
                    agents: [n[9], n[10], n[11]],
                    chain_reset_time: n[12],
                    chain_reset_move: n[13],
                    distance: Curve::parse(f[3]),
                    chaining: Curve::parse(f[4]),
                    miss: Curve::parse(f[5]),
                    lap: four(6),
                    buffer: four(10),
                    power: four(14),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_round_trip_and_focus() {
        let power = Curve(vec![(0.0, 0.4), (0.5, 0.4), (0.5, 0.6), (0.9, 0.6), (0.9, 0.8), (1.0, 0.8)]);
        assert_eq!(power.at(0.2), 0.4);
        assert_eq!(power.at(0.5), 0.6);
        assert_eq!(power.at(0.95), 0.8);
        let lap = Curve(vec![(0.0, 5.0), (0.1, 3.0), (0.3, 1.5), (0.7, 0.7), (0.9, 0.4), (1.0, 0.4)]);
        assert!((lap.at(0.5) - 1.1).abs() < 1e-5);
        let p = Prefs {
            kind: "assaultrifle".into(),
            firing_range: 80.0,
            distance: Curve(vec![(0.0, 1.5), (7.5, 1.0), (10.0, 0.83), (20.0, 0.5), (60.0, 0.1)]),
            chaining: Curve(vec![(0.0, 1.0), (10.0, 1.3)]),
            chain_reset_time: -1.0,
            chain_reset_move: 2.0,
            min_speed: 3.0,
            lateral: 0.85,
            away: 0.55,
            towards: 2.0,
            towards_within: 10.0,
            exposure: [0.85, 0.7, 0.2],
            agents: [1.0; 3],
            lap: [lap.clone(), lap.clone(), lap.clone(), lap],
            buffer: Default::default(),
            power: [power.clone(), power.clone(), power.clone(), power],
            miss: Curve(vec![(0.0, 0.25), (0.5, 1.0)]),
        };
        let a = Aim { prefs: vec![p.clone()], decay_start: 1.0, decay_rate: 20.0, weapons: vec![("AK47.AK47_NPC".into(), 40.0)] };
        assert_eq!(from_text(&to_text(&a)).unwrap(), a);
        assert_eq!(a.weapon("AK47_NPC"), Some(40.0));
        // 20 m, standing, fully exposed: 0.5 * 0.85.
        assert!((p.focus(20.0, 0.0, 1.0, p.exposure[0], 1) - 0.425).abs() < 1e-5);
        assert_eq!(p.movement([0.0, 0.0, 5.0], [0.0, 0.0, 1.0], 8.0), 2.0);
        assert_eq!(p.movement([5.0, 0.0, 0.0], [0.0, 0.0, 1.0], 8.0), 0.85);
        assert_eq!(p.movement([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], 8.0), 1.0);
    }
}
