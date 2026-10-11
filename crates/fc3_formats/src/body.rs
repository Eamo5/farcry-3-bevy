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
const MAGIC: &str = "fc3-body 2";
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
        let Some(b) = e.child("Components").and_then(|c| c.child("CPawn")).and_then(|p| p.child("Body")) else { return };
        let f = |n: &str| float(b, n).unwrap_or(0.0);
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
    )
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
    };
    for l in lines {
        let Some((k, v)) = l.split_once(' ') else { continue };
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
        };
        assert_eq!(from_text(&to_text(&b)).unwrap(), b);
        let m = Minimap { radius: 40.0, vehicle_radius: 100.0, vehicle_from: 10.0, vehicle_to: 15.0, glider_radius: 100.0 };
        assert_eq!(Minimap::from_text(&m.to_text()).unwrap(), m);
        assert_eq!(m.radius_at(true, false, 12.5), 70.0);
        assert_eq!(m.radius_at(false, false, 50.0), 40.0);
        let v = b.jump_speed();
        assert!((v * v / (2.0 * b.gravity) - 1.25).abs() < 1e-4);
    }
}
