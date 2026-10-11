//! Aim assist for a pad, by the game's aiming help for the difficulty
//! (`fc3_formats::assist`) and the weapon's (`fc3_formats::weapons::Aim`):
//! the target nearest the sights, in view and near enough; the view
//! following it as it moves across while the sights are over it (its
//! billboards); the sights snapping onto it as they come up (its cylinder,
//! to the head when aimed close to it, else the body), following it a while
//! and fading off; and shots bending toward it (`AimAssist::correct`). Only
//! while the pad is in use and the option is on.

use avian3d::prelude::*;
use bevy::prelude::*;
use fc3_formats::assist::Assist;

use crate::player::{Player, HALF_HEIGHT};

pub struct AimAssistPlugin;

impl Plugin for AimAssistPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AssistTable>()
            .init_resource::<AimAssist>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_assist)
            .add_systems(Update, aim_assist.before(crate::player::player_look).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The game's aiming help, once read from the cache.
#[derive(Resource, Default)]
pub struct AssistTable(Option<Assist>);

fn load_assist(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<AssistTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::assist::ASSIST_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::assist::from_text(&t))
    {
        Ok(a) => table.0 = Some(a),
        Err(e) => warn!("aim assist: no aiming help table ({e:#}); re-run the converter"),
    }
}

/// A target's body: its head, the middle of its torso and its feet, and
/// whether it is an animal.
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub entity: Entity,
    pub head: Vec3,
    pub torso: Vec3,
    pub feet: Vec3,
    pub animal: bool,
}

impl Body {
    /// The point of its body a ray from `eye` along `dir` passes nearest:
    /// on its upright line at the ray's depth, held between its feet and
    /// head.
    fn nearest(&self, eye: Vec3, dir: Vec3) -> (Vec3, Vec3) {
        let depth = (self.torso - eye).dot(dir).max(0.0);
        let at = eye + dir * depth;
        (at, Vec3::new(self.torso.x, at.y.clamp(self.feet.y, self.head.y), self.torso.z))
    }
}

/// A snap onto a target: where on it (from its torso), and the seconds left
/// following it and fading off once the sights are there.
#[derive(Clone, Copy, Debug)]
struct Lock {
    entity: Entity,
    offset: Vec3,
    arrived: bool,
    follow: f32,
    fade: f32,
}

#[derive(Resource, Default)]
pub struct AimAssist {
    /// The target nearest the sights, and the difficulty's help.
    pub target: Option<Body>,
    aiming: Option<fc3_formats::assist::Aiming>,
    /// How far the view follows (eased in), and the target's bearing last
    /// frame (yaw, pitch from the eye).
    influence: f32,
    bearing: Option<(Entity, Vec2)>,
    lock: Option<Lock>,
    was_aiming: bool,
    test_at: Option<f32>,
    /// How much of a pad's turn the view takes this frame (yaw, pitch): eased
    /// while the sights are over a near target (the right stick's assist).
    pub stick: Vec2,
}

impl AimAssist {
    /// A pad's look turned as the stick's assist eases it (1: as is).
    pub fn ease_stick(&self, look: Vec2) -> Vec2 {
        if self.stick == Vec2::ZERO {
            look
        } else {
            look * self.stick
        }
    }

    /// A shot's direction bent toward the target within the weapon's
    /// correction (from the hip or aimed) and the difficulty's: if it passes
    /// within the target's cylinder (scaled) and the target is near enough,
    /// it turns toward the nearest point of the body by up to the strength
    /// (degrees), and up or down only by the vertical share of it.
    pub fn correct(&self, eye: Vec3, dir: Vec3, aimed: bool, weapon: Option<&fc3_formats::weapons::Aim>) -> Vec3 {
        let (Some(t), Some(a), Some(w)) = (self.target, self.aiming, weapon) else { return dir };
        let [strength, cylinder, reach] = w.correction[usize::from(aimed)];
        let distance = t.torso.distance(eye);
        if strength <= 0.0 || distance > reach {
            return dir;
        }
        let (at, point) = t.nearest(eye, dir);
        let (r, h) = a.cylinder(distance, t.animal);
        let r = r * cylinder * a.correction.1;
        let side = Vec2::new(at.x - point.x, at.z - point.z).length();
        let over = (at.y - t.head.y).max(t.feet.y - at.y).max(0.0);
        if side > r || over > h {
            return dir;
        }
        let want = (point - eye).normalize_or(dir);
        let most = (strength * a.correction.0).to_radians();
        // Across in full, up and down by its share.
        let turn = |from: f32, to: f32, most: f32| from + (to - from).clamp(-most, most);
        let (yaw, pitch) = bearing(dir);
        let (want_yaw, want_pitch) = bearing(want);
        let vertical = if t.animal { a.correction_vertical.0 } else { a.correction_vertical.1 };
        let yaw = turn(yaw, yaw + wrap(want_yaw - yaw), most);
        let pitch = turn(pitch, want_pitch, most * vertical);
        from_bearing(yaw, pitch)
    }
}

/// A direction's yaw (as the player's: forward is -Z) and pitch.
fn bearing(d: Vec3) -> (f32, f32) {
    ((-d.x).atan2(-d.z), d.y.atan2(Vec2::new(d.x, d.z).length()))
}

fn from_bearing(yaw: f32, pitch: f32) -> Vec3 {
    Vec3::new(-yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos())
}

fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

#[allow(clippy::too_many_arguments)]
fn aim_assist(
    time: Res<Time>,
    actions: Res<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    table: Res<AssistTable>,
    mut assist: ResMut<AimAssist>,
    spatial: SpatialQuery,
    mut players: Query<(Entity, &mut Player, &Transform, &crate::combat::Health, Option<&crate::weapons::Inventory>)>,
    npcs: Query<(Entity, &Transform, &crate::npc::Npc, &crate::combat::Health, Option<&crate::npc::HeadPos>), Without<Player>>,
    animals: Query<(Entity, &Transform, &crate::wildlife::WildAnimal, &crate::combat::Health), Without<Player>>,
) {
    let dt = time.delta_secs();
    let Ok((pe, mut p, pt, ph, inv)) = players.single_mut() else { return };
    let level = settings.difficulty.index().min(3);
    let help = table.0.as_ref().map(|a| (a.aiming[level], a.follow[level]));
    let aiming_now = inv.is_some_and(|i| i.aiming);
    let mut started = aiming_now && !assist.was_aiming;
    assist.was_aiming = aiming_now;
    // FC3_ASSIST_TEST: as with a pad; 10 s in, the view turned a little off
    // the nearest target and the sights brought up, then how far off it
    // stays logged.
    let test = std::env::var_os("FC3_ASSIST_TEST").is_some();
    let now_s = time.elapsed_secs();
    if test && assist.test_at.is_none() && assist.target.is_some() {
        assist.test_at = Some(now_s + 10.0);
    }
    if test && assist.test_at.is_some_and(|t| now_s >= t && now_s - dt < t) {
        p.yaw += 0.06;
        started = true;
        info!("assist-test: view turned 3.4 degrees off, sights up");
    }
    let pad = actions.gamepad_active || test;
    let off = !settings.aim_assist || !pad || ph.dead || p.driving || p.noclip || p.spawning || p.mounted.is_some();
    let Some((aiming, follow)) = help.filter(|_| !off) else {
        *assist = AimAssist { was_aiming: aiming_now, test_at: assist.test_at, ..default() };
        return;
    };
    assist.aiming = Some(aiming);
    let eye = pt.translation + Vec3::Y * (p.eye - HALF_HEIGHT);
    let dir = from_bearing(p.yaw, p.pitch);
    // Targets: enemies and animals alive, near enough and in plain view;
    // the one nearest the sights.
    let reach = follow.distances[3].max(inv.and_then(|i| i.def().aim).map_or(0.0, |a| a.lock_fade.1));
    let bodies = npcs
        .iter()
        .filter(|(_, _, n, h, _)| !h.dead && n.faction.hostile_to_player())
        .map(|(e, t, _, _, head)| Body {
            entity: e,
            head: head.map_or(t.translation + Vec3::Y * 0.75, |h| h.0),
            torso: t.translation,
            feet: t.translation - Vec3::Y * HALF_HEIGHT,
            animal: false,
        })
        .chain(animals.iter().filter(|(_, _, _, h)| !h.dead).map(|(e, t, a, _)| {
            let r = a.body_radius();
            Body { entity: e, head: t.translation + Vec3::Y * r, torso: t.translation, feet: t.translation - Vec3::Y * r, animal: true }
        }));
    let mut best: Option<(f32, Body)> = None;
    for b in bodies {
        let to = b.torso - eye;
        let d = to.length();
        if d > reach || d < 0.5 || to.dot(dir) <= 0.0 {
            continue;
        }
        let angle = dir.angle_between(to);
        if best.is_some_and(|(a, _)| a <= angle) || angle > 0.6 {
            continue;
        }
        let Ok(ray) = Dir3::new(to) else { continue };
        let filter = SpatialQueryFilter::from_excluded_entities([pe, b.entity]);
        if spatial.cast_ray(eye, ray, d, true, &filter).is_some_and(|h| h.distance < d - 0.6) {
            continue;
        }
        best = Some((angle, b));
    }
    let target = best.map(|(_, b)| b);
    assist.target = target;

    // The view follows a target while the sights are over its billboards:
    // by its movement across the view, as much as its distance allows; and a
    // turn of the stick over a near one is eased.
    let mut want = 0.0;
    assist.stick = Vec2::ONE;
    if let Some(t) = target {
        let (at, _) = t.nearest(eye, dir);
        let right = Vec3::new(-dir.z, 0.0, dir.x).normalize_or(Vec3::X);
        let x = (at - t.torso).dot(right).abs();
        let y = at.y - t.head.y;
        let within = |b: [f32; 3]| x <= b[0] && y <= b[1] && y >= -b[2];
        let falloff = |v: f32, inner: f32, outer: f32| if v <= inner { 1.0 } else { ((outer - v) / (outer - inner).max(1e-3)).clamp(0.0, 1.0) };
        let weight = if within(follow.inner) {
            1.0
        } else if within(follow.outer) {
            let wy = if y > follow.inner[1] { falloff(y, follow.inner[1], follow.outer[1]) } else { falloff(-y, follow.inner[2], follow.outer[2]) };
            falloff(x, follow.inner[0], follow.outer[0]) * wy
        } else {
            0.0
        };
        want = weight * follow.reach(t.torso.distance(eye)) * if p.sprinting { follow.sprint } else { 1.0 };
        if let Some((_, yaw, pitch)) = follow.stick.filter(|(near, _, _)| t.torso.distance(eye) <= *near) {
            assist.stick = Vec2::new(1.0 - (1.0 - yaw) * weight, 1.0 - (1.0 - pitch) * weight);
        }
    }
    assist.influence += (want - assist.influence) * if follow.damping > 0.0 { (dt / follow.damping).min(1.0) } else { 1.0 };
    let now = target.map(|t| {
        let (y, pi) = bearing(t.torso - eye);
        (t.entity, Vec2::new(y, pi))
    });
    if let (Some((e, b)), Some((last, before))) = (now, assist.bearing) {
        if e == last {
            let moved = Vec2::new(wrap(b.x - before.x), b.y - before.y);
            p.yaw += moved.x * follow.intensity.0 * assist.influence;
            p.pitch = (p.pitch + moved.y * follow.intensity.1 * assist.influence).clamp(-1.5, 1.5);
        }
    }
    assist.bearing = now;
    if test && assist.test_at.is_some_and(|t| now_s >= t && now_s < t + 1.5) && (now_s * 10.0) as u32 != ((now_s - dt) * 10.0) as u32 {
        if let Some(t) = target {
            let (y, pi) = bearing(t.torso - eye);
            info!(
                "assist-test: {:.2}s off by {:.2} deg yaw, {:.2} pitch at {:.0} m, influence {:.2}, locked {}",
                now_s - assist.test_at.unwrap_or(now_s),
                wrap(y - p.yaw).to_degrees(),
                (pi - p.pitch).to_degrees(),
                t.torso.distance(eye),
                assist.influence,
                assist.lock.is_some()
            );
        }
    }

    // The sights snap onto a target as they come up: to its head when aimed
    // within its reach of it, else the body between below the head and
    // above the torso, at the weapon's speed; following it a while, then
    // fading off (or at once if it leaves its cylinder, or the sights go
    // down).
    let weapon = inv.and_then(|i| i.def().aim).filter(|a| a.lock_on);
    if let (true, Some(t), Some(w)) = (started, target, weapon) {
        let distance = t.torso.distance(eye);
        let (at, _) = t.nearest(eye, dir);
        let (r, h) = aiming.cylinder(distance, t.animal);
        let side = Vec2::new(at.x - t.torso.x, at.z - t.torso.z).length();
        let over = (at.y - t.head.y).max(t.feet.y - at.y).max(0.0);
        if distance <= w.lock_fade.1 && side <= r * w.lock_cylinder && over <= h {
            let point = if at.distance(t.head) <= aiming.head_radius {
                t.head
            } else {
                Vec3::new(t.torso.x, at.y.clamp(t.torso.y + aiming.above_torso, t.head.y - aiming.below_head), t.torso.z)
            };
            assist.lock = Some(Lock { entity: t.entity, offset: point - t.torso, arrived: false, follow: aiming.follow, fade: aiming.fade });
        }
    }
    if let Some(mut lock) = assist.lock {
        let speed = weapon.map_or(0.0, |w| w.lock_speed.to_radians());
        let kept = target.filter(|t| t.entity == lock.entity).filter(|t| {
            let (at, _) = t.nearest(eye, dir);
            let (r, h) = aiming.cylinder(t.torso.distance(eye), t.animal);
            Vec2::new(at.x - t.torso.x, at.z - t.torso.z).length() <= r * aiming.keep.0 * 1.5
                && (at.y - t.head.y).max(t.feet.y - at.y).max(0.0) <= h * aiming.keep.1 * 1.5
        });
        match kept {
            Some(t) if aiming_now && speed > 0.0 => {
                let (want_yaw, want_pitch) = bearing(t.torso + lock.offset - eye);
                let (dy, dp) = (wrap(want_yaw - p.yaw), want_pitch - p.pitch);
                // Once there: following, then fading off.
                let strength = if !lock.arrived {
                    1.0
                } else if lock.follow > 0.0 {
                    lock.follow -= dt;
                    1.0
                } else if lock.fade > 0.0 {
                    lock.fade -= dt;
                    (lock.fade / aiming.fade.max(1e-3)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let most = speed * dt * strength;
                p.yaw += dy.clamp(-most, most);
                p.pitch = (p.pitch + dp.clamp(-most, most)).clamp(-1.5, 1.5);
                lock.arrived |= dy.abs().max(dp.abs()) < 0.01;
                assist.lock = (strength > 0.0).then_some(lock);
            }
            _ => assist.lock = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shots_bend_toward_a_target_within_its_cylinder_only() {
        let aiming = fc3_formats::assist::Aiming {
            radius: (0.75, 0.12),
            height: (0.5, 0.06),
            animal: (1.0, 1.0),
            correction: (1.0, 1.0),
            correction_vertical: (0.01, 0.01),
            ..Default::default()
        };
        let body = Body {
            entity: Entity::PLACEHOLDER,
            head: Vec3::new(1.0, 1.7, -10.0),
            torso: Vec3::new(1.0, 1.0, -10.0),
            feet: Vec3::new(1.0, 0.0, -10.0),
            animal: false,
        };
        let assist = AimAssist { target: Some(body), aiming: Some(aiming), ..default() };
        let weapon = fc3_formats::weapons::Aim { correction: [[3.0, 1.0, 15.0], [2.0, 0.75, 25.0]], ..Default::default() };
        let eye = Vec3::new(0.0, 1.5, 0.0);
        // Aimed a metre left of him at 10 m: turned toward him, by no more
        // than the strength.
        let bent = assist.correct(eye, Vec3::NEG_Z, false, Some(&weapon));
        let turned = bent.angle_between(Vec3::NEG_Z).to_degrees();
        assert!(turned > 1.0 && turned <= 3.0 + 1e-3, "{turned}");
        assert!(bent.x > 0.0);
        // Far off to the side, or beyond the weapon's reach: untouched.
        assert_eq!(assist.correct(eye, Vec3::new(-1.0, 0.0, -1.0).normalize(), false, Some(&weapon)), Vec3::new(-1.0, 0.0, -1.0).normalize());
        let far = AimAssist {
            target: Some(Body { torso: Vec3::new(1.0, 1.0, -40.0), head: Vec3::new(1.0, 1.7, -40.0), feet: Vec3::new(1.0, 0.0, -40.0), ..body }),
            ..assist
        };
        assert_eq!(far.correct(eye, Vec3::NEG_Z, false, Some(&weapon)), Vec3::NEG_Z);
    }
}
