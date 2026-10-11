//! How the player moves, from the game's data: the player prototype's body
//! (`player:MainCharacter/PawnPlayer`'s `CPawn` `Body`: walking and
//! crouching speeds, swimming speed, jump height and gravity) and the
//! counters service's stamina (the sprint's speeds, upright and crouched,
//! fast while the stamina lasts and slower after; how fast it drains and
//! comes back).
//! The cache keeps them as text ([`BODY_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const BODY_REL: &str = "characters/body.txt";
const MAGIC: &str = "fc3-body 4";

/// Whether a cached table is in this version's form.
pub fn is_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MAGIC)
}
const PLAYER: &str = "player:MainCharacter/PawnPlayer";
const SERVICE: &str = "countersservice:counters.countersservice";

/// Speeds in metres a second; the jump's height in metres and gravity in
/// metres a second squared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Body {
    pub walk: f32,
    pub crouch: f32,
    pub sprint: f32,
    pub crouch_sprint: f32,
    /// The sprint once the stamina is low (upright, crouched).
    pub sprint_tired: f32,
    pub crouch_sprint_tired: f32,
    /// Stamina: its most, what sprinting drains and resting restores a
    /// second, and the least that keeps the fast sprint.
    pub stamina: f32,
    pub stamina_drain: f32,
    pub stamina_regen: f32,
    pub stamina_fast: f32,
    pub swim: f32,
    pub jump_height: f32,
    pub gravity: f32,
    /// What a fall does (`CPawn`'s `Falling`).
    pub fall: Option<Fall>,
    /// Holding the breath through a scope, and air underwater.
    pub breath_hold: Option<BreathHold>,
    pub oxygen: Option<Oxygen>,
    /// How much faster the sprint key swims (`fSwimSprintMaxMultiplier`).
    pub swim_sprint: f32,
}

/// Holding the breath through a scope (`HoldBreathAbility`): the seconds it
/// takes to steady, the longest hold, the seconds catching it after a full
/// one, and the sway held and out of breath (times the usual).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BreathHold {
    pub steady: f32,
    pub hold: f32,
    pub catch: f32,
    pub held_sway: f32,
    pub out_sway: f32,
}

/// Air underwater (the counters' `Oxygen`): the most, what a second under
/// takes and one in the air restores, and the health a second costs once
/// it is gone (`fDrownHealthDrain`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Oxygen {
    pub max: f32,
    pub drain: f32,
    pub refill: f32,
    pub drown: f32,
}

/// A fall's harm by its height (metres): from each of the first three
/// heights the share of the body's health it takes (the first is a mere
/// landing), and from the fourth, death.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Fall {
    pub heights: [f32; 4],
    pub shares: [f32; 3],
}

impl Fall {
    /// The share of health a fall from this height takes (1 is death).
    pub fn share(&self, height: f32) -> f32 {
        let [a, b, c, d] = self.heights;
        if height >= d {
            1.0
        } else if height >= c {
            self.shares[2]
        } else if height >= b {
            self.shares[1]
        } else if height >= a {
            self.shares[0]
        } else {
            0.0
        }
    }
}

impl Body {
    /// The upward speed that reaches the jump's height.
    pub fn jump_speed(&self) -> f32 {
        (2.0 * self.gravity * self.jump_height).max(0.0).sqrt()
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

/// The player's body from the entity library and the object templates.
pub fn body(library: &Object, templates: &Object) -> Option<Body> {
    let mut found: Option<Body> = None;
    library.walk(&mut |o, _| {
        if found.is_some() || !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity").filter(|e| e.str("hidName").as_deref() == Some(PLAYER)) else { return };
        let Some(pawn) = e.child("Components").and_then(|c| c.child("CPawn")) else { return };
        let Some(b) = pawn.child("Body") else { return };
        let f = |n: &str| float(b, n).unwrap_or(0.0);
        let breath_hold = pawn.child("Procedural_sway").and_then(|s| s.child("HoldBreathAbility")).map(|h| {
            let f = |n: &str| float(h, n).unwrap_or(0.0);
            BreathHold {
                steady: f("fGoingIntoHoldBreathTime"),
                hold: f("fHoldBreathMaxTime"),
                catch: f("fCatchingBreathTime"),
                held_sway: f("fHoldingBreathSpecialSwayModifier"),
                out_sway: f("fOutOfBreathSpecialSwayModifier"),
            }
        });
        let fall = pawn.child("Falling").map(|l| {
            let f = |n: &str| float(l, n).unwrap_or(0.0);
            Fall {
                heights: [f("fFallDistanceA"), f("fFallDistanceB"), f("fFallDistanceC"), f("fFallDistanceD")],
                shares: [f("fFallDistancePawnDamagePercentA"), f("fFallDistancePawnDamagePercentB"), f("fFallDistancePawnDamagePercentC")],
            }
        });
        found = Some(Body {
            walk: f("fWalkingMaxSpeed"),
            crouch: f("fWalkingMaxSpeedCrouch"),
            sprint: 0.0,
            crouch_sprint: 0.0,
            sprint_tired: 0.0,
            crouch_sprint_tired: 0.0,
            stamina: 0.0,
            stamina_drain: 0.0,
            stamina_regen: 0.0,
            stamina_fast: 0.0,
            swim: f("fSwimmingMaxSpeed"),
            jump_height: f("fJumpHeight"),
            gravity: -f("fGravity"),
            fall: fall.filter(|l| l.heights[3] > 0.0),
            breath_hold: breath_hold.filter(|h| h.hold > 0.0),
            oxygen: None,
            swim_sprint: 0.0,
        });
    });
    let mut body = found?;
    templates.walk(&mut |o, _| {
        if !o.str("FullName").is_some_and(|n| n.eq_ignore_ascii_case(SERVICE)) {
            return;
        }
        if let Some(s) = o.child("StatsAndCounters").and_then(|s| s.child("Stamina")) {
            let f = |n: &str| float(s, n).unwrap_or(0.0);
            body.sprint = f("fSprintMaxSpeed");
            body.crouch_sprint = f("fSprintCrouchedMaxSpeed");
            body.sprint_tired = f("fSprintMinSpeed");
            body.crouch_sprint_tired = f("fSprintCrouchedMinSpeed");
            body.stamina = f("fMax");
            body.stamina_drain = -f("fSprintDrain");
            body.stamina_regen = f("fRegenRate");
            body.stamina_fast = f("fStaminaMinForFastSprint");
            body.swim_sprint = f("fSwimSprintMaxMultiplier");
            let drown = -f("fDrownHealthDrain");
            if let Some(o) = o.child("StatsAndCounters").and_then(|s| s.child("Oxygen")) {
                let f = |n: &str| float(o, n).unwrap_or(0.0);
                body.oxygen =
                    Some(Oxygen { max: f("fMax"), drain: -f("fOxygenDrain"), refill: f("fOxygenRefill"), drown }).filter(|o| o.max > 0.0 && o.drain > 0.0);
            }
        }
    });
    (body.walk > 0.0 && body.gravity > 0.0).then_some(body)
}

pub fn to_text(b: &Body) -> String {
    format!(
        "{MAGIC}\nwalk {}\ncrouch {}\nsprint {}\ncrouch_sprint {}\nsprint_tired {}\ncrouch_sprint_tired {}\nstamina {}\nstamina_drain {}\nstamina_regen {}\nstamina_fast {}\nswim {}\njump_height {}\ngravity {}\n",
        b.walk,
        b.crouch,
        b.sprint,
        b.crouch_sprint,
        b.sprint_tired,
        b.crouch_sprint_tired,
        b.stamina,
        b.stamina_drain,
        b.stamina_regen,
        b.stamina_fast,
        b.swim,
        b.jump_height,
        b.gravity
    ) + &b.fall.map_or(String::new(), |l| {
        format!("fall {}\nfall_shares {}\n", l.heights.map(|v| v.to_string()).join(","), l.shares.map(|v| v.to_string()).join(","))
    }) + &b.breath_hold.map_or(String::new(), |h| format!("breath_hold {},{},{},{},{}\n", h.steady, h.hold, h.catch, h.held_sway, h.out_sway))
        + &b.oxygen.map_or(String::new(), |o| format!("oxygen {},{},{},{}\n", o.max, o.drain, o.refill, o.drown))
        + &format!("swim_sprint {}\n", b.swim_sprint)
}

pub fn from_text(text: &str) -> Result<Body> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a body table");
    }
    let mut b = Body {
        walk: 0.0,
        crouch: 0.0,
        sprint: 0.0,
        crouch_sprint: 0.0,
        sprint_tired: 0.0,
        crouch_sprint_tired: 0.0,
        stamina: 0.0,
        stamina_drain: 0.0,
        stamina_regen: 0.0,
        stamina_fast: 0.0,
        swim: 0.0,
        jump_height: 0.0,
        gravity: 0.0,
        fall: None,
        breath_hold: None,
        oxygen: None,
        swim_sprint: 0.0,
    };
    for l in lines {
        let Some((k, v)) = l.split_once(' ') else { continue };
        let list: Vec<f32> = v.trim().split(',').filter_map(|x| x.parse().ok()).collect();
        match k {
            "fall" if list.len() == 4 => b.fall.get_or_insert_with(Fall::default).heights = [list[0], list[1], list[2], list[3]],
            "fall_shares" if list.len() == 3 => b.fall.get_or_insert_with(Fall::default).shares = [list[0], list[1], list[2]],
            "breath_hold" if list.len() == 5 => {
                b.breath_hold = Some(BreathHold { steady: list[0], hold: list[1], catch: list[2], held_sway: list[3], out_sway: list[4] })
            }
            "oxygen" if list.len() == 4 => b.oxygen = Some(Oxygen { max: list[0], drain: list[1], refill: list[2], drown: list[3] }),
            _ => {}
        }
        let v: f32 = v.trim().parse().unwrap_or(0.0);
        match k {
            "walk" => b.walk = v,
            "crouch" => b.crouch = v,
            "sprint" => b.sprint = v,
            "crouch_sprint" => b.crouch_sprint = v,
            "sprint_tired" => b.sprint_tired = v,
            "crouch_sprint_tired" => b.crouch_sprint_tired = v,
            "stamina" => b.stamina = v,
            "stamina_drain" => b.stamina_drain = v,
            "stamina_regen" => b.stamina_regen = v,
            "stamina_fast" => b.stamina_fast = v,
            "swim" => b.swim = v,
            "jump_height" => b.jump_height = v,
            "gravity" => b.gravity = v,
            "swim_sprint" => b.swim_sprint = v,
            _ => {}
        }
    }
    Ok(b)
}

/// The minimap's reach (`CFCXPawnHudComponent`'s `MiniMap`): the ground
/// radius it shows on foot, and in a vehicle once it goes between
/// `vehicle_from` and `vehicle_to` km/h (eased between), and on the glider.
/// The cache keeps it as text ([`MINIMAP_REL`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Minimap {
    pub radius: f32,
    pub vehicle_radius: f32,
    pub vehicle_from: f32,
    pub vehicle_to: f32,
    pub glider_radius: f32,
}

pub const MINIMAP_REL: &str = "characters/minimap.txt";
const MINIMAP_MAGIC: &str = "fc3-minimap 1";

impl Minimap {
    /// The radius at a speed (km/h): on foot, or in a vehicle or glider.
    pub fn radius_at(&self, vehicle: bool, glider: bool, kmh: f32) -> f32 {
        if glider {
            return self.glider_radius;
        }
        if !vehicle {
            return self.radius;
        }
        let f = if self.vehicle_to > self.vehicle_from { ((kmh - self.vehicle_from) / (self.vehicle_to - self.vehicle_from)).clamp(0.0, 1.0) } else { 1.0 };
        self.radius + (self.vehicle_radius - self.radius) * f
    }

    pub fn to_text(&self) -> String {
        format!("{MINIMAP_MAGIC}\n{} {} {} {} {}\n", self.radius, self.vehicle_radius, self.vehicle_from, self.vehicle_to, self.glider_radius)
    }

    pub fn from_text(text: &str) -> Result<Minimap> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(MINIMAP_MAGIC) {
            bail!("not a minimap table");
        }
        let v: Vec<f32> = lines.next().unwrap_or_default().split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() < 5 {
            bail!("a short minimap table");
        }
        Ok(Minimap { radius: v[0], vehicle_radius: v[1], vehicle_from: v[2], vehicle_to: v[3], glider_radius: v[4] })
    }
}

/// The player's minimap reach from the entity library.
pub fn minimap(library: &Object) -> Option<Minimap> {
    let mut out = None;
    library.walk(&mut |o, _| {
        if out.is_some() || !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity").filter(|e| e.str("hidName").as_deref() == Some(PLAYER)) else { return };
        let Some(sp) =
            e.child("Components").and_then(|c| c.child("CFCXPawnHudComponent")).and_then(|h| h.child("MiniMap")).and_then(|m| m.child("SinglePlayer"))
        else {
            return;
        };
        let vehicle = sp.child("VehicleScale").and_then(|v| v.child("LandOrWaterVehicle"));
        let glider = sp.child("VehicleScale").and_then(|v| v.child("GliderVehicle"));
        let f = |o: Option<&std::sync::Arc<Object>>, n: &str| o.and_then(|o| float(o, n)).unwrap_or(0.0);
        out = Some(Minimap {
            radius: float(sp, "fDefaultRadiusDistance").unwrap_or(0.0),
            vehicle_radius: f(vehicle, "fVehicleRadiusDistance"),
            vehicle_from: f(vehicle, "fVehicleMinSpeed"),
            vehicle_to: f(vehicle, "fVehicleMaxSpeed"),
            glider_radius: f(glider, "fGliderRadiusDistance"),
        })
        .filter(|m| m.radius > 0.0);
    });
    out
}

/// The player's HUD (`CFCXPawnHudComponent`, `CFCXTaggingComponentPawn`):
/// how far the camera tags, how near collectibles show on the minimap, and
/// how often unspent skill points are brought up (seconds). The cache keeps
/// it as text ([`HUD_REL`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Hud {
    pub tag_range: f32,
    pub collectibles: f32,
    pub skill_reminder: f32,
}

pub const HUD_REL: &str = "characters/hud.txt";
const HUD_MAGIC: &str = "fc3-hud 1";

impl Hud {
    pub fn to_text(&self) -> String {
        format!("{HUD_MAGIC}\ntag_range {}\ncollectibles {}\nskill_reminder {}\n", self.tag_range, self.collectibles, self.skill_reminder)
    }

    pub fn from_text(text: &str) -> Result<Hud> {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(HUD_MAGIC) {
            bail!("not a HUD table");
        }
        let mut h = Hud::default();
        for l in lines {
            let Some((k, v)) = l.split_once(' ') else { continue };
            let v: f32 = v.trim().parse().unwrap_or(0.0);
            match k {
                "tag_range" => h.tag_range = v,
                "collectibles" => h.collectibles = v,
                "skill_reminder" => h.skill_reminder = v,
                _ => {}
            }
        }
        Ok(h)
    }
}

/// The player's HUD from the entity library.
pub fn hud(library: &Object) -> Option<Hud> {
    let mut out = None;
    library.walk(&mut |o, _| {
        if out.is_some() || !o.is("EntityPrototype") {
            return;
        }
        let Some(c) = o.child("Entity").filter(|e| e.str("hidName").as_deref() == Some(PLAYER)).and_then(|e| e.child("Components")) else { return };
        let hud = c.child("CFCXPawnHudComponent");
        let f = |o: Option<&std::sync::Arc<Object>>, n: &str| o.and_then(|o| float(o, n)).unwrap_or(0.0);
        let pickable = hud.and_then(|h| h.child("MiniMap")).and_then(|m| m.child("SinglePlayer")).and_then(|s| s.child("Pickable"));
        out = Some(Hud {
            tag_range: f(c.child("CFCXTaggingComponentPawn"), "fTaggingRange"),
            collectibles: f(pickable, "fCollectibleDisplayRadius"),
            skill_reminder: f(hud.and_then(|h| h.child("PopupReminder")), "fSkillPointReminderTimer"),
        });
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_jumps_to_its_height() {
        let b = Body {
            walk: 4.0,
            crouch: 1.75,
            sprint: 7.5,
            crouch_sprint: 3.25,
            sprint_tired: 6.25,
            crouch_sprint_tired: 3.25,
            stamina: 100.0,
            stamina_drain: 20.0,
            stamina_regen: 25.0,
            stamina_fast: 20.0,
            swim: 3.0,
            jump_height: 1.25,
            gravity: 20.0,
            fall: Some(Fall { heights: [3.0, 4.6, 8.0, 11.9], shares: [0.0, 0.3, 0.6] }),
            breath_hold: Some(BreathHold { steady: 0.5, hold: 3.0, catch: 3.0, held_sway: 0.0, out_sway: 2.5 }),
            oxygen: Some(Oxygen { max: 100.0, drain: 3.25, refill: 100.0, drown: 20.0 }),
            swim_sprint: 1.5,
        };
        assert_eq!(from_text(&to_text(&b)).unwrap(), b);
        let fall = b.fall.unwrap();
        assert_eq!((fall.share(2.0), fall.share(4.0), fall.share(5.0), fall.share(9.0), fall.share(12.0)), (0.0, 0.0, 0.3, 0.6, 1.0));
        let m = Minimap { radius: 40.0, vehicle_radius: 100.0, vehicle_from: 10.0, vehicle_to: 15.0, glider_radius: 100.0 };
        assert_eq!(Minimap::from_text(&m.to_text()).unwrap(), m);
        let h = Hud { tag_range: 300.0, collectibles: 30.0, skill_reminder: 600.0 };
        assert_eq!(Hud::from_text(&h.to_text()).unwrap(), h);
        assert_eq!(m.radius_at(true, false, 12.5), 70.0);
        assert_eq!(m.radius_at(false, false, 50.0), 40.0);
        let v = b.jump_speed();
        assert!((v * v / (2.0 * b.gravity) - 1.25).abs() < 1e-4);
    }
}
