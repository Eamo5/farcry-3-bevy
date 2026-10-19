//! The flamethrower's own effects (its `FireStrategyProperties`, through the
//! weapons' table): its nozzle's, on the gun's muzzle (alight and idle while
//! held, shooting while the trigger is down, stopping as it's let go), and
//! those it sends down the stream while it fires, each launched every so
//! often (more often as the aim swings) at its speed for its distance or
//! until the stream strikes something, where it stops. Our flame stream
//! stands in when none of them is cached.

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::weapons::{Inventory, Muzzle};

pub struct FlamethrowerPlugin;

impl Plugin for FlamethrowerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (player_flame, fly).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A flamethrower's flame: its nozzle's effects (first person, then third)
/// and those it sends down the stream.
#[derive(Clone, Debug, Default)]
pub struct Flame {
    pub idle: Option<&'static str>,
    pub shoot: Option<&'static str>,
    pub stop: Option<&'static str>,
    pub shoot_3p: Option<&'static str>,
    pub stop_3p: Option<&'static str>,
    pub stream: Vec<Stream>,
}

/// An effect sent down the stream: one every so often (s; by how fast the
/// aim turns), going so fast (m/s) so far (m).
#[derive(Clone, Debug)]
pub struct Stream {
    pub effect: Option<&'static str>,
    pub effect_3p: Option<&'static str>,
    pub timing: fc3_formats::weapons::StreamEffect,
}

impl Flame {
    pub fn from_stats(f: &fc3_formats::weapons::Flame) -> Flame {
        let name = |n: &str| (!n.is_empty()).then(|| crate::authored_fx::intern(n));
        Flame {
            idle: name(&f.tip_idle),
            shoot: name(&f.tip_shoot),
            stop: name(&f.tip_stop),
            shoot_3p: name(&f.tip_shoot_3p),
            stop_3p: name(&f.tip_stop_3p),
            stream: f
                .stream
                .iter()
                .filter(|s| s.every > 0.0 && s.speed > 0.0)
                .map(|s| Stream { effect: name(&s.effect), effect_3p: name(&s.effect_3p), timing: s.clone() })
                .collect(),
        }
    }

    /// Whether any of its stream's effects is cached (else our stream
    /// stands in).
    pub fn streams(&self, third_person: bool) -> bool {
        self.stream.iter().filter_map(|s| if third_person { s.effect_3p } else { s.effect }).any(crate::authored_fx::available)
    }
}

/// The most stream effects in flight at once.
const MAX_IN_FLIGHT: usize = 320;

/// A stream effect on its way, and the seconds it has left to go (where it
/// stops: what it emitted lives out).
#[derive(Component)]
struct Launched {
    velocity: Vec3,
    left: f32,
}

/// The player's flame: the nozzle's effect playing, whether he was firing,
/// each stream effect's clock, and where he aimed last frame.
#[derive(Default)]
struct PlayerFlame {
    tip: Option<(Entity, &'static str)>,
    firing: bool,
    clocks: Vec<f32>,
    aim: Option<Vec3>,
}

#[allow(clippy::too_many_arguments)]
fn player_flame(
    time: Res<Time>,
    players: Query<(Entity, &Inventory, &crate::player::Player)>,
    muzzles: Query<(Entity, &GlobalTransform), With<Muzzle>>,
    cameras: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    in_flight: Query<(), With<Launched>>,
    spatial: SpatialQuery,
    mut state: Local<PlayerFlame>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let held = players.single().ok().filter(|(_, inv, _)| inv.switching <= 0.0 && inv.slots.get(inv.current).is_some()).and_then(|(e, inv, p)| {
        let d = inv.def();
        // (Firing while its shots keep coming.)
        d.flame.clone().map(|f| (e, f, inv.since_shot < d.interval * 2.5, p.velocity))
    });
    let muzzle = muzzles.single().ok();
    let (want, firing) = match (&held, muzzle) {
        (Some((_, f, firing, _)), Some(_)) => (if *firing { f.shoot } else { f.idle }, *firing),
        _ => (None, false),
    };
    // The nozzle: alight, shooting, or nothing in hand.
    if state.tip.map(|t| t.1) != want {
        if let Some((e, _)) = state.tip.take() {
            commands.entity(e).try_despawn();
        }
        if let (Some(name), Some((m, _))) = (want, muzzle) {
            state.tip = crate::authored_fx::attach_muzzle(&mut commands, name, m).map(|e| (e, name));
        }
    }
    if state.firing && !firing {
        if let (Some((_, f, _, _)), Some((m, _))) = (&held, muzzle) {
            if let Some(stop) = f.stop {
                crate::authored_fx::play_muzzle(&mut commands, stop, m);
            }
        }
    }
    state.firing = firing;
    // The stream, while it fires: each effect launched along the aim from
    // the nozzle, as far as it reaches.
    let (Some((player, f, true, velocity)), Some((_, at)), Ok(cam)) = (&held, muzzle, cameras.single()) else {
        state.clocks.clear();
        state.aim = None;
        return;
    };
    let from = at.translation();
    let forward = cam.forward();
    // (How fast the aim swings, deg/s.)
    let turn = state.aim.map_or(0.0, |a| a.angle_between(forward.as_vec3()).to_degrees() / dt.max(1e-3));
    state.aim = Some(forward.as_vec3());
    let filter = SpatialQueryFilter::from_excluded_entities([*player]);
    let reach = spatial.cast_ray(from, forward, 60.0, true, &filter).map(|h| h.distance).unwrap_or(f32::MAX);
    let frame = crate::authored_fx::muzzle_frame(forward.as_vec3());
    let mut flying = in_flight.iter().count();
    state.clocks.resize(f.stream.len(), 0.0);
    for (s, clock) in f.stream.iter().zip(state.clocks.iter_mut()) {
        *clock += dt;
        let Some(name) = s.effect.filter(|n| crate::authored_fx::available(n)) else { continue };
        let t = &s.timing;
        let every = (t.every * t.turning_ratio(turn)).max(1e-3);
        let left = t.distance.min(reach) / t.speed;
        while *clock >= every {
            *clock -= every;
            if flying >= MAX_IN_FLIGHT {
                continue;
            }
            // (Each from where it would be had it left on its tick.)
            let start = from + forward * (t.speed * *clock).min(t.distance.min(reach));
            commands.spawn((
                Transform::from_translation(start).with_rotation(frame),
                crate::authored_fx::AuthoredFx(name),
                Launched { velocity: forward * t.speed + *velocity, left: (left - *clock).max(0.0) },
            ));
            flying += 1;
        }
    }
}

/// A pulse of a flamer's flame (the characters' `LPO-50`), seen from
/// outside: each third-person stream effect launched as often as it goes
/// in `span` seconds, staggered along the stream as if each had left on
/// its tick. False when none is cached (our stream stands in).
pub fn launch_3p(commands: &mut Commands, flame: &Flame, from: Vec3, dir: Vec3, velocity: Vec3, reach: f32, span: f32) -> bool {
    let frame = crate::authored_fx::muzzle_frame(dir);
    let mut any = false;
    for s in &flame.stream {
        let Some(name) = s.effect_3p.filter(|n| crate::authored_fx::available(n)) else { continue };
        let t = &s.timing;
        let every = (t.every * t.turning_ratio(0.0)).max(1e-3);
        let end = t.distance.min(reach);
        for i in 0..((span / every).round() as usize).clamp(1, 8) {
            let gone = (t.speed * i as f32 * every).min(end);
            commands.spawn((
                Transform::from_translation(from + dir * gone).with_rotation(frame),
                crate::authored_fx::AuthoredFx(name),
                Launched { velocity: dir * t.speed + velocity, left: (end - gone) / t.speed },
            ));
        }
        any = true;
    }
    any
}

/// Stream effects go on their way until they've gone their distance or
/// struck something, and stop there.
fn fly(time: Res<Time>, mut launched: Query<(Entity, &mut Transform, &mut Launched)>, mut commands: Commands) {
    let dt = time.delta_secs();
    for (e, mut t, mut l) in &mut launched {
        t.translation += l.velocity * l.left.min(dt);
        l.left -= dt;
        if l.left <= 0.0 {
            // (It may have played out already.)
            commands.entity(e).try_despawn();
        }
    }
}
