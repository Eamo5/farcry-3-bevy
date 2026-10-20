//! Sticky fire: what a flamethrower's stream or a molotov leaves burning
//! where it lands, on any surface (the fire manager's sticky streams,
//! `fire_settings`, by its weapon's profile). It plays the sticky fires'
//! effect for its kind (a flamethrower's, a molotov's) as long as its
//! profile's fire grows, burns and dies down, and while it burns it sets
//! fire about it as far as its burn reaches (`fire::Ignite`: the grass,
//! whoever stands in it, what burns). The stream landing again near one
//! (within its flames' spacing) keeps it burning instead of starting
//! another, and as many burn at once as the effect's pool allows (the
//! oldest giving way).

use bevy::prelude::*;
use fc3_formats::fire_settings::{self as fs, StickyProfile};

use crate::fire::Ignite;

pub struct StickyFirePlugin;

impl Plugin for StickyFirePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<StickyFire>()
            .init_resource::<Profiles>()
            .init_resource::<Burning>()
            .add_systems(Startup, load)
            .add_systems(Update, burn.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// Leaves a weapon's sticky fire (by its profile's name: `fs::MOLOTOV`...)
/// on a surface (its normal) where it landed.
#[derive(Message, Clone, Copy)]
pub struct StickyFire {
    pub at: Vec3,
    pub normal: Vec3,
    pub profile: &'static str,
    pub instigator: Option<Entity>,
}

/// The sticky streams, and their effects by kind (with how many may play).
#[derive(Resource, Default)]
struct Profiles {
    list: Vec<StickyProfile>,
    effects: Vec<(&'static str, usize)>,
}

/// One burning: where, its profile, its effect, how long it has burnt and
/// has, and when it next sets fire about it.
struct Fire {
    at: Vec3,
    profile: usize,
    effect: Option<(Entity, &'static str)>,
    age: f32,
    life: f32,
    next: f32,
    instigator: Option<Entity>,
}

#[derive(Resource, Default)]
struct Burning(Vec<Fire>);

/// How often (s) a sticky fire sets fire about it.
const STIM_EVERY: f32 = 0.5;

fn load(mut profiles: ResMut<Profiles>) {
    let root = fc3_formats::cache::default_cache_root();
    let Ok(text) = std::fs::read_to_string(root.join(fs::FIRE_STICKY_REL)) else { return };
    profiles.list = fs::sticky_from_text(&text);
    let effects = std::fs::read_to_string(root.join(fs::FIRE_SETTINGS_REL)).map(|t| fs::from_text(&t)).unwrap_or_default();
    profiles.effects = effects
        .into_iter()
        .filter(|f| f.kind == fs::STICKY && crate::authored_fx::available(&f.effect))
        .map(|f| (crate::authored_fx::intern(&f.effect), f.pool as usize))
        .collect();
    info!("sticky fire: {} profiles, {} effects", profiles.list.len(), profiles.effects.len());
}

fn burn(
    time: Res<Time>,
    profiles: Res<Profiles>,
    mut burning: ResMut<Burning>,
    mut events: MessageReader<StickyFire>,
    mut ignite: MessageWriter<Ignite>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for ev in events.read() {
        let Some(i) = profiles.list.iter().position(|p| p.name == ev.profile) else { continue };
        let p = &profiles.list[i];
        // Landing again near one keeps it burning.
        if let Some(f) = burning.0.iter_mut().find(|f| f.profile == i && f.at.distance(ev.at) < p.resolution.max(0.3)) {
            f.age = f.age.min(p.starting);
            continue;
        }
        // Its effect, room made for it in its pool (the oldest going).
        let effect = profiles.effects.get(p.kind as usize).copied();
        if let Some((name, pool)) = effect {
            let using: Vec<usize> = (0..burning.0.len()).filter(|&k| burning.0[k].effect.is_some_and(|(_, n)| n == name)).collect();
            if using.len() >= pool.max(1) {
                if let Some(&oldest) = using.iter().max_by(|a, b| burning.0[**a].age.total_cmp(&burning.0[**b].age)) {
                    let gone = burning.0.remove(oldest);
                    if let Some((e, _)) = gone.effect {
                        commands.entity(e).try_despawn();
                    }
                }
            }
        }
        let effect = effect.map(|(name, _)| {
            let turn = crate::authored_fx::surface_turn(ev.normal);
            (commands.spawn((Transform::from_translation(ev.at).with_rotation(turn), crate::authored_fx::AuthoredFx(name))).id(), name)
        });
        let life = (p.starting + p.alive + p.extinction).max(0.5);
        debug!("sticky fire: {} at {:.1?} ({:?}, {life:.1} s)", p.name, ev.at, effect.map(|e| e.1));
        burning.0.push(Fire { at: ev.at, profile: i, effect, age: 0.0, life, next: 0.0, instigator: ev.instigator });
    }
    burning.0.retain_mut(|f| {
        let p = &profiles.list[f.profile];
        f.age += dt;
        f.next -= dt;
        if f.next <= 0.0 && f.age < p.starting + p.alive {
            f.next = STIM_EVERY;
            ignite.write(Ignite { at: f.at, radius: p.burn.max(0.5), instigator: f.instigator, damage: (p.damage > 0.0).then_some(p.damage) });
        }
        let on = f.age < f.life;
        if !on {
            if let Some((e, _)) = f.effect {
                commands.entity(e).try_despawn();
            }
        }
        on
    });
}
