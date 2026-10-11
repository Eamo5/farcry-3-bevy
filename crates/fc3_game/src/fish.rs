//! Reef fish: schools of the game's coral-reef fish over the shallows near
//! the player. Each school is one skinned model (the fish laid out round a
//! dummy root) played by the game's group clips: cruising, idling, and
//! scattering when the player swims close. They're scenery, not game.

use std::sync::Arc;

use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Model, ModelSettings};
use crate::characters::{spawn_rig_with, AnimKind, AnimRequest, ClipSet};
use crate::player::Player;
use crate::sky::SEA_LEVEL;
use crate::terrain::Heightfield;
use crate::weapons::Rng;

pub struct FishPlugin;

impl Plugin for FishPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_fish).add_systems(Update, (spawn_schools, swim));
    }
}

const MODELS: &str = r"graphics\__fc3_graphics\_common\characters\animals\fish_coral_reef\";
const CLIPS: &str = r"graphics\characters\_common\animations\animals\fish_coral_reef\";
/// The school models (each a whole school).
const SCHOOLS: [&str; 4] = ["coral_reef_fish_yellow.xbg", "coral_reef_fish_red.xbg", "emperor_angel.xbg", "french_angel.xbg"];

/// Schools at once, how near the player they're placed and kept, and the
/// water they keep to (depth of the bed below the sea).
const MAX_SCHOOLS: usize = 6;
const PLACE: (f32, f32) = (25.0, 70.0);
const KEEP: f32 = 110.0;
const DEPTH: (f32, f32) = (1.8, 18.0);
/// Cruising and fleeing speeds (m/s), and how near the player sends them off.
const CRUISE: f32 = 0.9;
const FLEE: f32 = 4.0;
const SCARE: f32 = 6.0;

#[derive(Resource)]
struct FishAssets {
    models: Vec<Handle<Fc3Model>>,
    set: Arc<ClipSet>,
}

#[derive(Component)]
struct School {
    goal: Vec3,
    yaw: f32,
    fleeing: f32,
    rig: Option<Entity>,
}

fn load_fish(mut commands: Commands, server: Res<AssetServer>) {
    let models = SCHOOLS
        .iter()
        .map(|m| {
            server.load_builder().with_settings(|s: &mut ModelSettings| s.skinned = true).load(skinned_cache_path(&cache::model_rel(&format!("{MODELS}{m}"))))
        })
        .collect();
    let clip = |name: &str| server.load(cache_path(&cache::anim_rel(&format!("{CLIPS}{name}"))));
    let mut clips = std::collections::HashMap::new();
    clips.insert(AnimKind::Idle, clip("crfish_group_idle_+000fw_nowep.mab"));
    clips.insert(AnimKind::Walk, clip("crfish_group_swim_+000fw_nowep.mab"));
    clips.insert(AnimKind::Run, clip("crfish_group_flee_+000fw_nowep.mab"));
    let set = Arc::new(ClipSet { skeleton: server.load(cache_path(&cache::skeleton_rel(&format!("{MODELS}dummy_ref.skeleton")))), clips });
    commands.insert_resource(FishAssets { models, set });
}

/// Water this deep at `p` (the bed `DEPTH` below the sea), and where the fish
/// swim there: a third of the way down.
fn swim_height(heights: &Heightfield, p: Vec3) -> Option<f32> {
    let bed = heights.height_at(p)?;
    let depth = SEA_LEVEL - bed;
    (DEPTH.0..DEPTH.1).contains(&depth).then(|| SEA_LEVEL - (depth * 0.35).clamp(1.0, 4.0))
}

fn yaw_to(v: Vec3) -> f32 {
    (-v.x).atan2(-v.z)
}

/// Places schools over the reefs round the player, and lets far ones go.
#[allow(clippy::too_many_arguments)]
fn spawn_schools(
    time: Res<Time>,
    mut next: Local<f32>,
    assets: Option<Res<FishAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<Heightfield>,
    players: Query<&Transform, With<Player>>,
    schools: Query<(Entity, &Transform), With<School>>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    regions: Res<crate::wildlife::WildlifeRegions>,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 1.5;
    let (Some(assets), Ok(pt)) = (assets, players.single()) else { return };
    for (e, t) in &schools {
        if t.translation.distance(pt.translation) > KEEP {
            commands.entity(e).despawn();
        }
    }
    // Only near the sea: the player in or by it.
    if heights.height_at(pt.translation).is_none_or(|h| h > SEA_LEVEL + 15.0) || schools.iter().count() >= MAX_SCHOOLS || !assets.set.ready(&server) {
        return;
    }
    let a = rng.f32() * std::f32::consts::TAU;
    let d = rng.range(PLACE.0, PLACE.1);
    let mut at = pt.translation + Vec3::new(a.cos(), 0.0, a.sin()) * d;
    // FC3_FISH_TEST: one school 9 m ahead of the player.
    let test = std::env::var("FC3_FISH_TEST").is_ok();
    if test {
        if schools.iter().count() > 0 {
            return;
        }
        let f = pt.forward().as_vec3();
        at = pt.translation + Vec3::new(f.x, 0.0, f.z).normalize_or(Vec3::NEG_Z) * 9.0;
    }
    // Over the reefs the game's wildlife regions stock with them.
    if !test && regions.reef_at(at) == Some(false) {
        return;
    }
    let Some(y) = swim_height(&heights, at).or(test.then_some(pt.translation.y + 0.5)) else { return };
    at.y = y;
    let i = (rng.f32() * SCHOOLS.len() as f32) as usize % SCHOOLS.len();
    let Some(model) = models.get(&assets.models[i]) else { return };
    let yaw = rng.f32() * std::f32::consts::TAU;
    let root = commands
        .spawn((
            School { goal: at, yaw, fleeing: 0.0, rig: None },
            Transform::from_translation(at).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::Visible,
            Name::new("reef fish"),
        ))
        .id();
    if let Some(rig) = spawn_rig_with(&mut commands, root, &[model], None, assets.set.clone(), Vec3::ZERO, None) {
        commands.entity(root).entry::<School>().and_modify(move |mut s| s.rig = Some(rig));
    }
    debug!("reef fish: {} at {at:.1?} (player {:.1?}), model {:.1?}..{:.1?}", SCHOOLS[i], pt.translation, model.aabb_min, model.aabb_max);
}

/// Cruise from point to point over the reef; scatter from the player.
fn swim(
    time: Res<Time>,
    heights: Res<Heightfield>,
    players: Query<&Transform, (With<Player>, Without<School>)>,
    mut schools: Query<(&mut Transform, &mut School)>,
    mut anims: Query<&mut AnimRequest>,
    mut rng: ResMut<Rng>,
) {
    let dt = time.delta_secs();
    let Ok(pt) = players.single() else { return };
    for (mut t, mut s) in &mut schools {
        let pos = t.translation;
        let away = pos - pt.translation;
        if away.length() < SCARE && s.fleeing <= 0.0 {
            s.fleeing = 3.0;
            let dir = Vec3::new(away.x, 0.0, away.z).normalize_or(Vec3::X);
            s.goal = pos + dir * 25.0;
        }
        s.fleeing -= dt;
        let to = Vec3::new(s.goal.x - pos.x, 0.0, s.goal.z - pos.z);
        if to.length() < 1.5 {
            // A new spot over water deep enough, not far.
            for _ in 0..6 {
                let a = rng.f32() * std::f32::consts::TAU;
                let g = pos + Vec3::new(a.cos(), 0.0, a.sin()) * rng.range(8.0, 20.0);
                if swim_height(&heights, g).is_some() {
                    s.goal = g;
                    break;
                }
            }
        }
        let fleeing = s.fleeing > 0.0;
        let speed = if fleeing { FLEE } else { CRUISE };
        let dir = to.normalize_or_zero();
        if dir != Vec3::ZERO {
            let want = yaw_to(dir);
            let diff = (want - s.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            s.yaw += diff.clamp(-dt * 1.5, dt * 1.5);
        }
        let fwd = Vec3::new(-s.yaw.sin(), 0.0, -s.yaw.cos());
        let mut next = pos + fwd * speed * dt;
        // Over the shallows only (turn back from the shore or the deep).
        match swim_height(&heights, next) {
            Some(y) => next.y += (y - next.y) * (dt * 0.8).min(1.0),
            None => {
                next = pos;
                s.goal = pos - fwd * 10.0;
            }
        }
        t.translation = next;
        t.rotation = Quat::from_rotation_y(s.yaw);
        if let Some(mut req) = s.rig.and_then(|r| anims.get_mut(r).ok()) {
            *req = AnimRequest { kind: if fleeing { AnimKind::Run } else { AnimKind::Walk }, rate: 1.0 };
        }
    }
}
