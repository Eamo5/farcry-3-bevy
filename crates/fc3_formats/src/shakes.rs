//! The player's camera shakes and pad rumbles from the entity library: the
//! player prototype's `CCameraShakeAndPadRumbleComponent` names each effect
//! ("explosionrumble", ...) and points at four `Curves:` prototypes (the
//! view's roll and pitch, the pad's high- and low-frequency motors) as
//! value-over-seconds knots. With them goes how an explosive's shake fades
//! with distance (`CExplosive`'s `CameraShakeAndRumble`), and the player's
//! reaction to being hit (the hit progression service's first state: the
//! view's kick by curves, scaled by the damage, its cooldown and sounds).
//! The cache keeps them as text ([`SHAKES_REL`]).

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const SHAKES_REL: &str = "fx/shakes.txt";
const MAGIC: &str = "fc3-shakes 2";
const HIT_SERVICE: &str = "hitprogressionservice:hitprogression.hitprogressionservice";
const PLAYER: &str = "player:MainCharacter/PawnPlayer";

/// A curve's knots, (seconds, value), in time order.
pub type Knots = Vec<(f32, f32)>;

/// The value at a time: flat-tangent keys, eased between (the authored
/// handles are level), and nothing outside the curve.
pub fn sample(knots: &[(f32, f32)], t: f32) -> f32 {
    let (Some(first), Some(last)) = (knots.first(), knots.last()) else { return 0.0 };
    if t <= first.0 || t >= last.0 {
        return 0.0;
    }
    let i = knots.partition_point(|k| k.0 <= t).clamp(1, knots.len() - 1);
    let (a, b) = (knots[i - 1], knots[i]);
    let f = if b.0 > a.0 { ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) } else { 1.0 };
    a.1 + (b.1 - a.1) * f * f * (3.0 - 2.0 * f)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Effect {
    /// Lower case ("explosionrumble").
    pub name: String,
    /// The view's roll and pitch (radians).
    pub roll: Knots,
    pub pitch: Knots,
    /// The pad's high- and low-frequency motors (0..1).
    pub rumble_high: Knots,
    pub rumble_low: Knots,
}

impl Effect {
    /// Seconds until every curve has ended.
    pub fn length(&self) -> f32 {
        [&self.roll, &self.pitch, &self.rumble_high, &self.rumble_low].iter().filter_map(|k| k.last()).map(|k| k.0).fold(0.0, f32::max)
    }
}

/// An explosion's shake: whole within `near` metres (times `near_scale`),
/// eased to `far_scale` at `far`, none beyond.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Falloff {
    pub near: f32,
    pub near_scale: f32,
    pub far: f32,
    pub far_scale: f32,
}

impl Falloff {
    pub fn scale(&self, distance: f32) -> f32 {
        if distance <= self.near {
            self.near_scale
        } else if distance >= self.far || self.far <= self.near {
            0.0
        } else {
            let f = (distance - self.near) / (self.far - self.near);
            self.near_scale + (self.far_scale - self.near_scale) * f
        }
    }
}

/// The player's reaction to a hit: the view's roll, pitch and yaw over time
/// (curve units times `angle` radians), times `by_damage` at the damage
/// taken; no sooner than `cooldown` after the last; with its hit and vocal
/// sounds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HitReaction {
    pub roll: Knots,
    pub pitch: Knots,
    pub yaw: Knots,
    pub angle: f32,
    pub cooldown: f32,
    pub by_damage: Knots,
    pub hit_sound: u32,
    pub vocal: u32,
}

impl HitReaction {
    /// The scale for a hit of `damage` (held at the curve's ends).
    pub fn scale(&self, damage: f32) -> f32 {
        let k = &self.by_damage;
        let (Some(first), Some(last)) = (k.first(), k.last()) else { return 1.0 };
        if damage <= first.0 {
            return first.1;
        }
        if damage >= last.0 {
            return last.1;
        }
        let i = k.partition_point(|p| p.0 <= damage).clamp(1, k.len() - 1);
        let (a, b) = (k[i - 1], k[i]);
        a.1 + (b.1 - a.1) * (damage - a.0) / (b.0 - a.0).max(1e-6)
    }

    /// Seconds until every curve has ended.
    pub fn length(&self) -> f32 {
        [&self.roll, &self.pitch, &self.yaw].iter().filter_map(|k| k.last()).map(|k| k.0).fold(0.0, f32::max)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shakes {
    pub effects: Vec<Effect>,
    pub explosion: Falloff,
    pub hit: Option<HitReaction>,
}

fn uint(o: &Object, name: &str) -> Option<u64> {
    let b = o.field(name)?;
    (!b.is_empty() && b.len() <= 8).then(|| b.iter().rev().fold(0u64, |v, &x| (v << 8) | x as u64))
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

fn knots(curve: &Object) -> Knots {
    let mut out: Knots = Vec::new();
    let Some(list) = curve.child("curveCurve").and_then(|c| c.child("Knots")) else { return out };
    for k in list.children_named("Knot") {
        let Some(v) = k.field("Value") else { continue };
        let f = |i: usize| v.get(i * 4..i * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
        let (Some(t), Some(y)) = (f(0), f(1)) else { continue };
        // (A key's in and out handles come as two knots at one time.)
        if out.last().is_none_or(|l| (l.0 - t).abs() > 1e-5) {
            out.push((t.max(0.0), y));
        }
    }
    // A curve that never leaves zero is no curve ("NoCurve").
    if out.iter().all(|k| k.1.abs() < 1e-6) {
        out.clear();
    }
    out
}

/// The player's hit reaction from the hit progression service, its curves
/// from the library's.
fn hit_reaction(templates: &Object, curves: &HashMap<u64, Knots>) -> Option<HitReaction> {
    let mut out = None;
    templates.walk(&mut |o, _| {
        if out.is_some() || !o.str("FullName").is_some_and(|n| n.eq_ignore_ascii_case(HIT_SERVICE)) {
            return;
        }
        let Some(state) = o.child("ProgressionStates").and_then(|p| p.child("StateList")).and_then(|l| l.children_named("State").next()) else { return };
        let mut hit = HitReaction::default();
        for effect in state.child("Effects").into_iter().flat_map(|e| e.children_named("Effect")) {
            let Some(c) = effect.child("Configuration") else { continue };
            if c.field("arkRollCurve").is_some() {
                let curve = |n: &str| uint(c, n).and_then(|id| curves.get(&id)).cloned().unwrap_or_default();
                hit.roll = curve("arkRollCurve");
                hit.pitch = curve("arkPitchCurve");
                hit.yaw = curve("arkYawCurve");
                hit.by_damage = curve("arkDamageMultiplierCurve");
                hit.angle = float(c, "fHitAngleMultiplier").unwrap_or(0.0);
                hit.cooldown = float(c, "fCoolDownTimer").unwrap_or(0.0);
            }
            let sound = |n: &str| c.str(n).and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).filter(|&v| v != u32::MAX).unwrap_or(0);
            if c.field("sndSuccessfulHitSoundFXid").is_some() {
                hit.hit_sound = sound("sndSuccessfulHitSoundFXid");
                hit.vocal = sound("sndSuccessfulHitVocalSoundId");
            }
        }
        out = Some(hit).filter(|h| h.angle > 0.0);
    });
    out
}

/// The player's effects and the explosives' falloff from the entity library,
/// with the hit reaction from the object templates.
pub fn shakes(library: &Object, templates: &Object) -> Shakes {
    let mut curves: HashMap<u64, Knots> = HashMap::new();
    let mut player: Option<&Object> = None;
    let mut explosion: Option<Falloff> = None;
    library.walk(&mut |o, _| {
        if o.is("EntityPrototype") {
            let Some(e) = o.child("Entity") else { return };
            let name = e.str("hidName").unwrap_or_default();
            if name.starts_with("Curves:") {
                if let Some(id) = uint(o, "disLibItemId") {
                    curves.insert(id, knots(e));
                }
            } else if name == PLAYER {
                player = e.child("Components").and_then(|c| c.child("CCameraShakeAndPadRumbleComponent")).map(|c| c.as_ref());
            }
        } else if explosion.is_none() && o.is("CExplosive") {
            if let Some(s) = o.child("CameraShakeAndRumble") {
                let f = |n: &str| float(s, n).unwrap_or(0.0);
                explosion = Some(Falloff {
                    near: f("fNearDistance"),
                    near_scale: f("fNearRumbleMultiplier"),
                    far: f("fFarDistance"),
                    far_scale: f("fFarRumbleMultiplier"),
                });
            }
        }
    });
    let mut effects = Vec::new();
    for e in player.and_then(|p| p.child("Effects")).map(|l| l.children_named("Effect").collect::<Vec<_>>()).unwrap_or_default() {
        let Some(name) = e.str("sEffectName").filter(|n| !n.is_empty()) else { continue };
        let curve = |n: &str| uint(e, n).and_then(|id| curves.get(&id)).cloned().unwrap_or_default();
        effects.push(Effect {
            name: name.to_ascii_lowercase(),
            roll: curve("arkCamera_roll"),
            pitch: curve("arkCamera_pitch"),
            rumble_high: curve("arkRumble_HF"),
            rumble_low: curve("arkRumble_LF"),
        });
    }
    let hit = hit_reaction(templates, &curves);
    Shakes { effects, explosion: explosion.unwrap_or_default(), hit }
}

fn knots_text(k: &Knots) -> String {
    k.iter().map(|(t, v)| format!("{t}:{v}")).collect::<Vec<_>>().join(";")
}

fn knots_from(s: &str) -> Knots {
    s.split(';').filter_map(|p| p.split_once(':')).filter_map(|(t, v)| Some((t.parse().ok()?, v.parse().ok()?))).collect()
}

pub fn to_text(s: &Shakes) -> String {
    let f = s.explosion;
    let mut out = format!("{MAGIC}\nexplosion\t{}\t{}\t{}\t{}\n", f.near, f.near_scale, f.far, f.far_scale);
    for e in &s.effects {
        out += &format!("{}\t{}\t{}\t{}\t{}\n", e.name, knots_text(&e.roll), knots_text(&e.pitch), knots_text(&e.rumble_high), knots_text(&e.rumble_low));
    }
    if let Some(h) = &s.hit {
        out += &format!(
            "hit\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            knots_text(&h.roll),
            knots_text(&h.pitch),
            knots_text(&h.yaw),
            h.angle,
            h.cooldown,
            knots_text(&h.by_damage),
            h.hit_sound,
            h.vocal
        );
    }
    out
}

pub fn from_text(text: &str) -> Result<Shakes> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a camera shake table");
    }
    let mut out = Shakes::default();
    for l in lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f[0] == "hit" && f.len() >= 9 {
            out.hit = Some(HitReaction {
                roll: knots_from(f[1]),
                pitch: knots_from(f[2]),
                yaw: knots_from(f[3]),
                angle: f[4].parse().unwrap_or(0.0),
                cooldown: f[5].parse().unwrap_or(0.0),
                by_damage: knots_from(f[6]),
                hit_sound: f[7].parse().unwrap_or(0),
                vocal: f[8].parse().unwrap_or(0),
            });
        } else if f[0] == "explosion" && f.len() >= 5 {
            let n = |i: usize| f[i].parse().unwrap_or(0.0);
            out.explosion = Falloff { near: n(1), near_scale: n(2), far: n(3), far_scale: n(4) };
        } else if f.len() >= 5 {
            out.effects.push(Effect {
                name: f[0].into(),
                roll: knots_from(f[1]),
                pitch: knots_from(f[2]),
                rumble_high: knots_from(f[3]),
                rumble_low: knots_from(f[4]),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_samples_and_fades() {
        let s = Shakes {
            effects: vec![Effect {
                name: "boom".into(),
                roll: vec![(0.0, 0.0), (0.1, 0.2), (0.3, 0.0)],
                rumble_low: vec![(0.0, 1.0), (1.0, 0.0)],
                ..default_effect()
            }],
            explosion: Falloff { near: 10.0, near_scale: 1.0, far: 15.0, far_scale: 0.0 },
            hit: Some(HitReaction {
                roll: vec![(0.0, 0.0), (0.08, 7.0), (0.4, 0.0)],
                angle: 0.009,
                cooldown: 0.77,
                by_damage: vec![(0.0, 0.15), (40.0, 1.1), (180.0, 1.5)],
                hit_sound: 0x4BF628,
                ..Default::default()
            }),
        };
        assert_eq!(from_text(&to_text(&s)).unwrap(), s);
        let h = s.hit.as_ref().unwrap();
        assert!((h.scale(20.0) - 0.625).abs() < 1e-4);
        assert_eq!(h.scale(500.0), 1.5);
        let e = &s.effects[0];
        assert_eq!(sample(&e.roll, 0.1), 0.2);
        assert!((sample(&e.roll, 0.05) - 0.1).abs() < 1e-6, "eased halfway is the midpoint");
        assert_eq!(sample(&e.roll, 0.5), 0.0);
        assert_eq!(e.length(), 1.0);
        assert_eq!(s.explosion.scale(5.0), 1.0);
        assert_eq!(s.explosion.scale(12.5), 0.5);
        assert_eq!(s.explosion.scale(20.0), 0.0);
    }

    fn default_effect() -> Effect {
        Effect::default()
    }
}
