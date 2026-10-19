//! Camera shakes and pad rumbles (`fc3_formats::shakes`): the player's
//! authored effects, each a few curves a second or so long, started by the
//! scripts (`CAPI_Player.CameraShakeAndGamePadRumble`) and by the world:
//! explosions near the player (fading with distance as the explosives
//! say), being hurt or mauled, crashing or boarding the vehicle driven, and
//! the weapon in hand firing or being drawn. A hit on the player kicks the
//! view away from where it came from (the game's hit reaction: its curves,
//! scaled by the damage, no more often than its cooldown), with its sounds.
//! The view's roll, pitch and yaw add onto the player's look; the pad's
//! motors follow the rumble curves while vibration is on.

use std::time::Duration;

use bevy::input::gamepad::{Gamepad, GamepadRumbleIntensity, GamepadRumbleRequest};
use bevy::prelude::*;
use fc3_formats::shakes::{sample, Shakes};

use crate::combat::{DamageEvent, DamageKind};
use crate::player::Player;
use crate::weapons::{Explosion, Inventory, WeaponClass};

pub struct ShakePlugin;

impl Plugin for ShakePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShakeTable>()
            .init_resource::<ViewShake>()
            .add_message::<Shake>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_shakes)
            .add_systems(Update, (shake_test, world_shakes, run_shakes).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// `FC3_SHAKE_TEST`: a harmless blast 6 m ahead every 3 s, logging the
/// view's peak roll after each.
fn shake_test(
    time: Res<Time>,
    view: Res<ViewShake>,
    mut state: Local<(f32, f32)>,
    players: Query<&GlobalTransform, With<Player>>,
    mut explosions: MessageWriter<Explosion>,
) {
    if std::env::var_os("FC3_SHAKE_TEST").is_none() {
        return;
    }
    let Ok(at) = players.single() else { return };
    let (wait, peak) = &mut *state;
    *peak = peak.max(view.roll.abs()).max(view.pitch.abs());
    *wait -= time.delta_secs();
    if *wait <= 0.0 {
        if *peak > 0.0 {
            info!("shake test: peak view offset {:.3} rad", *peak);
        }
        *wait = 3.0;
        *peak = 0.0;
        explosions.write(Explosion { at: at.translation() + at.forward().as_vec3() * 6.0, radius: 0.1, damage: 0.0, instigator: None, effect: None });
    }
}

/// Starts one of the player's effects ("explosionrumble"), scaled.
#[derive(Message, Clone)]
pub struct Shake {
    pub effect: String,
    pub intensity: f32,
}

impl Shake {
    pub fn new(effect: &str, intensity: f32) -> Self {
        Shake { effect: effect.to_ascii_lowercase(), intensity }
    }
}

/// The view's shake this frame (radians), added to the player's look.
#[derive(Resource, Default, Clone, Copy)]
pub struct ViewShake {
    pub roll: f32,
    pub pitch: f32,
    pub yaw: f32,
}

#[derive(Resource, Default)]
pub struct ShakeTable {
    table: Shakes,
    /// Effects playing: (index, seconds in, intensity).
    active: Vec<(usize, f32, f32)>,
    /// Hit reactions playing: (seconds in, scale, side), and the seconds
    /// until another may start.
    hits: Vec<(f32, f32, f32)>,
    hit_wait: f32,
    /// Seconds until the pad's motors are next set.
    rumble_wait: f32,
    rumbling: bool,
}

/// How often the pad's motors follow the curves.
const RUMBLE_STEP: f32 = 0.08;
/// No more effects than this at once (a barrage stays a barrage).
const MAX_ACTIVE: usize = 12;
/// The most the view rolls or pitches however many effects add up.
const MAX_ANGLE: f32 = 0.35;
/// Velocity lost in a step by the vehicle driven that shakes as a crash
/// (m/s), and what shakes it fully.
const CRASH_JOLT: f32 = 4.0;
const CRASH_FULL: f32 = 14.0;

fn load_shakes(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<ShakeTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::shakes::SHAKES_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::shakes::from_text(&t))
    {
        Ok(shakes) => {
            info!("shakes: {} camera shake and rumble effects", shakes.effects.len());
            table.table = shakes;
        }
        Err(e) => warn!("shakes: no camera shake table ({e:#}); re-run the converter"),
    }
}

/// What the world does to the player that shakes the view or the pad.
#[allow(clippy::too_many_arguments)]
fn world_shakes(
    mut table: ResMut<ShakeTable>,
    mut explosions: MessageReader<Explosion>,
    mut damage: MessageReader<DamageEvent>,
    players: Query<(Entity, &GlobalTransform, Option<&Inventory>), With<Player>>,
    animals: Query<(), With<crate::wildlife::WildAnimal>>,
    driving: Res<crate::vehicles::Driving>,
    velocities: Query<&avian3d::prelude::LinearVelocity>,
    mut last: Local<(Option<Entity>, Vec3, f32, Option<(usize, u32)>)>,
    time: Res<Time>,
    mut shakes: MessageWriter<Shake>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let Ok((player, at, inventory)) = players.single() else { return };
    let right = at.right().as_vec3();
    let at = at.translation();
    table.hit_wait -= time.delta_secs();
    for ex in explosions.read() {
        let scale = table.table.explosion.scale(ex.at.distance(at));
        if scale > 0.0 {
            shakes.write(Shake::new("explosionrumble", scale));
        }
    }
    for d in damage.read().filter(|d| d.target == player && d.amount > 0.0) {
        let mauled = d.kind == DamageKind::Melee && d.instigator.is_some_and(|i| animals.contains(i));
        // Shot or struck: the hit reaction, away from where it came from.
        if matches!(d.kind, DamageKind::Bullet | DamageKind::Melee) && table.hit_wait <= 0.0 {
            if let Some(hit) = table.table.hit.clone() {
                let side = d.source.map_or(1.0, |s| if (s - at).dot(right) >= 0.0 { 1.0 } else { -1.0 });
                table.hits.push((0.0, hit.angle * hit.scale(d.amount), side));
                table.hit_wait = hit.cooldown;
                for event in [hit.hit_sound, hit.vocal].into_iter().filter(|&e| e != 0) {
                    sfx.write(crate::sound::PlaySfx { event, at: None, volume: 0.8 });
                }
            }
        }
        let effect = match d.kind {
            _ if mauled => "animalattack",
            DamageKind::Fire => "burnshakeandrumble",
            // (An explosion's own shake has started already.)
            DamageKind::Explosion | DamageKind::Drown => continue,
            _ => "avatarreceivestim",
        };
        shakes.write(Shake::new(effect, 1.0));
    }
    let (was_driving, was_velocity, crash_wait, was_weapon) = &mut *last;
    // Boarding, and a hard knock to the vehicle driven.
    *crash_wait -= time.delta_secs();
    let velocity = driving.vehicle.and_then(|v| velocities.get(v).ok()).map(|v| v.0).unwrap_or_default();
    if driving.vehicle != *was_driving {
        if driving.vehicle.is_some() {
            shakes.write(Shake::new("getinvehicle", 1.0));
        }
        *was_driving = driving.vehicle;
    } else if driving.vehicle.is_some() {
        let jolt = (velocity - *was_velocity).length();
        if jolt > CRASH_JOLT && *crash_wait <= 0.0 {
            *crash_wait = 0.6;
            shakes.write(Shake::new("vehiclecollision", ((jolt - CRASH_JOLT) / (CRASH_FULL - CRASH_JOLT)).clamp(0.3, 1.0)));
        }
    }
    *was_velocity = velocity;
    // The weapon in hand: drawn, and each shot.
    if let Some(inv) = inventory.filter(|i| !i.defs.is_empty() && !i.slots.is_empty()) {
        let now = (inv.current, inv.shots_fired);
        if let Some((current, shots)) = *was_weapon {
            if current != now.0 {
                shakes.write(Shake::new("weapondraw", 1.0));
            } else if now.1 > shots {
                let effect = match inv.def().class {
                    WeaponClass::Melee | WeaponClass::Bow => None,
                    WeaponClass::Flamethrower => Some("weaponflameshoot"),
                    WeaponClass::Launcher | WeaponClass::GrenadeLauncher => Some("weaponprojectileshoot"),
                    _ => Some("weaponbulletshoot"),
                };
                if let Some(e) = effect {
                    shakes.write(Shake::new(e, 1.0));
                }
            }
        }
        *was_weapon = Some(now);
    }
}

/// Plays the effects: the view's offset this frame and the pad's motors.
fn run_shakes(
    time: Res<Time>,
    settings: Res<crate::settings::Settings>,
    mut requests: MessageReader<Shake>,
    mut table: ResMut<ShakeTable>,
    mut view: ResMut<ViewShake>,
    post: Option<Res<crate::post_effects::PostShake>>,
    gamepads: Query<Entity, With<Gamepad>>,
    mut rumble: MessageWriter<GamepadRumbleRequest>,
) {
    let dt = time.delta_secs();
    let ShakeTable { table, active, rumble_wait, rumbling, hits, .. } = &mut *table;
    for r in requests.read() {
        match table.effects.iter().position(|e| e.name == r.effect) {
            Some(i) if r.intensity > 0.0 && active.len() < MAX_ACTIVE => active.push((i, 0.0, r.intensity)),
            Some(_) => {}
            None => debug!("shakes: no effect {:?}", r.effect),
        }
    }
    let (mut roll, mut pitch, mut high, mut low) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    active.retain_mut(|(i, t, intensity)| {
        *t += dt;
        let e = &table.effects[*i];
        roll += sample(&e.roll, *t) * *intensity;
        pitch += sample(&e.pitch, *t) * *intensity;
        high += sample(&e.rumble_high, *t).max(0.0) * *intensity;
        low += sample(&e.rumble_low, *t).max(0.0) * *intensity;
        *t < e.length()
    });
    let mut yaw = 0.0f32;
    if let Some(hit) = &table.hit {
        hits.retain_mut(|(t, scale, side)| {
            *t += dt;
            roll += sample(&hit.roll, *t) * *scale * *side;
            pitch += sample(&hit.pitch, *t) * *scale;
            yaw += sample(&hit.yaw, *t) * *scale * *side;
            *t < hit.length()
        });
    }
    // (Screen effects' camera-shake clips add theirs.)
    let p = post.map_or(Vec3::ZERO, |p| p.0);
    let (roll, pitch, yaw) = (roll + p.x, pitch + p.y, yaw + p.z);
    *view = ViewShake { roll: roll.clamp(-MAX_ANGLE, MAX_ANGLE), pitch: pitch.clamp(-MAX_ANGLE, MAX_ANGLE), yaw: yaw.clamp(-MAX_ANGLE, MAX_ANGLE) };
    // The pad: each step, its motors set to where the curves are.
    *rumble_wait -= dt;
    if *rumble_wait > 0.0 {
        return;
    }
    *rumble_wait = RUMBLE_STEP;
    let on = settings.vibration && (high > 0.02 || low > 0.02);
    if !on && !*rumbling {
        return;
    }
    for gamepad in &gamepads {
        rumble.write(GamepadRumbleRequest::Stop { gamepad });
        if on {
            let intensity = GamepadRumbleIntensity { strong_motor: low.min(1.0), weak_motor: high.min(1.0) };
            rumble.write(GamepadRumbleRequest::Add { gamepad, intensity, duration: Duration::from_secs_f32(RUMBLE_STEP * 1.5) });
        }
    }
    *rumbling = on;
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::shakes::{Effect, Falloff};

    #[test]
    fn effects_shake_the_view_then_settle() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<Shake>()
            .add_message::<GamepadRumbleRequest>()
            .init_resource::<ViewShake>()
            .init_resource::<crate::settings::Settings>()
            .insert_resource(ShakeTable {
                table: Shakes {
                    effects: vec![Effect { name: "explosionrumble".into(), roll: vec![(0.0, 0.0), (0.05, 0.2), (0.1, 0.0)], ..default() }],
                    explosion: Falloff { near: 10.0, near_scale: 1.0, far: 15.0, far_scale: 0.0 },
                    hit: None,
                },
                ..default()
            })
            .add_systems(Update, run_shakes);
        app.world_mut().write_message(Shake::new("ExplosionRumble", 0.5));
        app.world_mut().write_message(Shake::new("nosuchshake", 1.0));
        let mut peak = 0.0f32;
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(10));
            app.update();
            peak = peak.max(app.world().resource::<ViewShake>().roll);
        }
        assert!(peak > 0.02 && peak <= 0.1 + 1e-4, "half intensity peaks under the curve's 0.2: {peak}");
        assert_eq!(app.world().resource::<ViewShake>().roll, 0.0, "settled once the curve ends");
        assert!(app.world().resource::<ShakeTable>().active.is_empty());
    }
}
