//! Birds overhead: vultures and the odd black eagle circling on thermals over
//! the land, gulls wheeling along the shore. They use the game's bird models
//! and flight cycles, and a shot brings one down.

use std::sync::Arc;

use avian3d::prelude::*;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Model, ModelSettings};
use crate::characters::{spawn_rig_with, AnimKind, AnimRequest, ClipSet};
use crate::combat::Health;
use crate::player::Player;
use crate::sky::SEA_LEVEL;
use crate::weapons::Rng;

pub struct BirdsPlugin;

impl Plugin for BirdsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(crate::menu::GameState::Loading), load_birds)
            .add_systems(Update, (spawn_birds, fly_birds, bird_test).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Vulture,
    Eagle,
    Gull,
}

struct BirdDef {
    kind: Kind,
    model: &'static str,
    skeleton: &'static str,
    fly: &'static str,
    die: &'static str,
    /// Air speed (m/s), circle radius range, height above the ground (or
    /// the sea) range, and how many fly together.
    speed: f32,
    radius: (f32, f32),
    height: (f32, f32),
    flock: usize,
    /// Skin material stem for models that only carry a placeholder.
    skin: Option<&'static str>,
    /// Its call and its cry when shot (sound events from its archetype's
    /// `sndIdleSound` and `sndDeathSound`; 0 is none).
    call: u32,
    cry: u32,
}

const MODELS: &str = r"graphics\__fc3_graphics\_common\characters\animals\";
const CLIPS: &str = r"graphics\characters\_common\animations\animals\";

const BIRDS: [BirdDef; 3] = [
    BirdDef {
        kind: Kind::Vulture,
        model: r"vulture\vulture.xbg",
        skeleton: r"vulture\hips_ref.skeleton",
        fly: r"vulture\vulture_fulb_flyingcycle_inair_+000fw_nowep.mab",
        die: r"vulture\vulture_fulb_die01_insky_+000fw_nowep.mab",
        speed: 8.0,
        radius: (25.0, 45.0),
        height: (35.0, 70.0),
        flock: 3,
        skin: Some("yanzhou-m-0811201153918833"),
        call: 0x0019_586C,
        cry: 0x0019_586E,
    },
    BirdDef {
        kind: Kind::Eagle,
        model: r"black_eagle\black_eagle.xbg",
        skeleton: r"black_eagle\pelvis_ref.skeleton",
        fly: r"black_eagle\beagle_fulb_flyingcycle_+000fw_nowep.mab",
        die: r"black_eagle\beagle_fulb_flyingcycle_+000fw_nowep.mab",
        speed: 10.0,
        radius: (40.0, 70.0),
        height: (60.0, 110.0),
        flock: 1,
        skin: None,
        call: 0x001A_404A,
        cry: 0,
    },
    BirdDef {
        kind: Kind::Gull,
        model: r"bird_seagull\bird_seagull.xbg",
        skeleton: r"bird_seagull\pelvis_ref.skeleton",
        fly: r"bird_seagull\seagull_fulb_flycycle_+000fw_nowep.mab",
        die: r"bird_seagull\seagull_fulb_die_+000fw_nowep.mab",
        speed: 7.0,
        radius: (12.0, 25.0),
        height: (8.0, 22.0),
        flock: 4,
        skin: None,
        call: 0x000A_482D,
        cry: 0,
    },
];

/// Flocks kept around the player, and how far out they circle / go.
const FLOCKS: usize = 3;
const NEAR: (f32, f32) = (40.0, 160.0);
const GONE: f32 = 320.0;

#[derive(Resource)]
struct BirdAssets {
    models: Vec<Handle<Fc3Model>>,
    sets: Vec<Arc<ClipSet>>,
    skins: Vec<Option<Handle<StandardMaterial>>>,
}

fn load_birds(mut commands: Commands, server: Res<AssetServer>) {
    let models = BIRDS
        .iter()
        .map(|b| {
            server
                .load_builder()
                .with_settings(|m: &mut ModelSettings| m.skinned = true)
                .load(skinned_cache_path(&cache::model_rel(&format!("{MODELS}{}", b.model))))
        })
        .collect();
    let sets = BIRDS
        .iter()
        .map(|b| {
            let clip = |n: &str| server.load(cache_path(&cache::anim_rel(&format!("{CLIPS}{n}"))));
            let clips = std::collections::HashMap::from([(AnimKind::Idle, clip(b.fly)), (AnimKind::Walk, clip(b.fly)), (AnimKind::Death, clip(b.die))]);
            Arc::new(ClipSet { skeleton: server.load(cache_path(&cache::skeleton_rel(&format!("{MODELS}{}", b.skeleton)))), clips })
        })
        .collect();
    let skins = BIRDS.iter().map(|b| b.skin.map(|stem| server.load(cache_path(&cache::material_rel(&cache::animal_skin_path(stem)))))).collect();
    commands.insert_resource(BirdAssets { models, sets, skins });
}

/// A bird circling a thermal: its centre, radius, height, where it is on the
/// circle, and its direction.
#[derive(Component)]
pub struct Bird {
    def: usize,
    flock: u32,
    centre: Vec3,
    radius: f32,
    height: f32,
    angle: f32,
    dir: f32,
    rig: Option<Entity>,
    /// Shot down: falling speed, then seconds on the ground.
    falling: Option<f32>,
    grounded: f32,
    /// Seconds to its next call.
    call_in: f32,
}

#[allow(clippy::too_many_arguments)]
fn spawn_birds(
    assets: Option<Res<BirdAssets>>,
    models: Res<Assets<Fc3Model>>,
    server: Res<AssetServer>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<&Transform, With<Player>>,
    birds: Query<(Entity, &Bird, &Transform)>,
    mut next_flock: Local<u32>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
) {
    let Some(assets) = assets else { return };
    let Ok(pt) = players.single() else { return };
    // Gone too far, or long on the ground.
    for (e, b, t) in &birds {
        let flat = Vec2::new(t.translation.x - pt.translation.x, t.translation.z - pt.translation.z).length();
        if flat > GONE || b.grounded > 30.0 {
            commands.entity(e).despawn();
        }
    }
    let mut flocks: Vec<u32> = birds.iter().map(|(_, b, _)| b.flock).collect();
    flocks.sort_unstable();
    flocks.dedup();
    if flocks.len() >= FLOCKS {
        return;
    }
    // A new flock somewhere out of the way: gulls over the shore, the others
    // over land.
    let a = rng.f32() * std::f32::consts::TAU;
    let centre = pt.translation + Vec3::new(a.cos(), 0.0, a.sin()) * rng.range(NEAR.0, NEAR.1);
    let Some(ground) = heights.height_at(centre) else { return };
    let shore = (ground - SEA_LEVEL).abs() < 6.0 || ground < SEA_LEVEL;
    let candidates: Vec<usize> = (0..BIRDS.len())
        .filter(|&i| match BIRDS[i].kind {
            Kind::Gull => shore,
            _ => ground > SEA_LEVEL + 10.0,
        })
        .collect();
    let Some(&def) = candidates.get((rng.f32() * candidates.len() as f32) as usize % candidates.len().max(1)) else { return };
    let d = &BIRDS[def];
    if !crate::cache_assets::usable(&server, assets.models[def].id()) || !assets.sets[def].ready(&server) {
        return;
    }
    let Some(model) = models.get(&assets.models[def]) else { return };
    *next_flock += 1;
    let base = ground.max(SEA_LEVEL);
    let height = base + rng.range(d.height.0, d.height.1);
    let dir = if rng.f32() < 0.5 { 1.0 } else { -1.0 };
    for _ in 0..d.flock {
        let radius = rng.range(d.radius.0, d.radius.1);
        let angle = rng.f32() * std::f32::consts::TAU;
        let bird = Bird {
            def,
            flock: *next_flock,
            centre,
            radius,
            height: height + rng.range(-4.0, 4.0),
            angle,
            dir,
            rig: None,
            falling: None,
            grounded: 0.0,
            call_in: rng.range(4.0, 20.0),
        };
        let root = commands
            .spawn((
                Transform::from_translation(centre + Vec3::new(angle.cos() * radius, height, angle.sin() * radius)),
                Visibility::Visible,
                Health::new(15.0),
                RigidBody::Kinematic,
                Collider::sphere(0.6),
                Name::new("bird"),
            ))
            .id();
        let rig = spawn_rig_with(&mut commands, root, &[model], None, assets.sets[def].clone(), Vec3::ZERO, assets.skins[def].clone());
        if let Some(r) = rig {
            commands.entity(r).insert(NotShadowCaster);
        }
        commands.entity(root).insert(Bird { rig, ..bird });
    }
}

/// Circling; shot ones fall.
#[allow(clippy::too_many_arguments)]
fn fly_birds(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    mut rng: ResMut<Rng>,
    players: Query<&Transform, (With<Player>, Without<Bird>)>,
    mut birds: Query<(&mut Bird, &mut Transform, &Health)>,
    mut anims: Query<&mut AnimRequest>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let dt = time.delta_secs();
    let listener = players.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for (mut b, mut t, h) in &mut birds {
        let d = &BIRDS[b.def];
        // A call now and then from one in earshot; a cry as one's shot.
        b.call_in -= dt;
        if b.call_in <= 0.0 && !h.dead {
            b.call_in = rng.range(8.0, 25.0);
            if d.call != 0 && t.translation.distance(listener) < 140.0 {
                sfx.write(crate::sound::PlaySfx { event: d.call, at: Some(t.translation), volume: 1.0 });
            }
        }
        if h.dead && b.falling.is_none() && d.cry != 0 {
            sfx.write(crate::sound::PlaySfx { event: d.cry, at: Some(t.translation), volume: 1.0 });
        }
        if h.dead {
            // Down it comes, tumbling, until it hits the ground or the sea.
            let v = b.falling.unwrap_or(0.0) + 9.81 * dt;
            b.falling = Some(v);
            let ground = heights.height_at(t.translation);
            let floor = ground.unwrap_or(SEA_LEVEL).max(crate::water::surface(t.translation, ground) - 0.3) + 0.15;
            if t.translation.y > floor {
                t.translation.y = (t.translation.y - v * dt).max(floor);
                t.rotate_local_z(dt * 3.0);
            } else {
                b.grounded += dt;
            }
            if let Some(mut a) = b.rig.and_then(|r| anims.get_mut(r).ok()) {
                *a = AnimRequest { kind: AnimKind::Death, rate: 1.0 };
            }
            continue;
        }
        b.angle += b.dir * d.speed / b.radius * dt;
        let pos = b.centre + Vec3::new(b.angle.cos() * b.radius, 0.0, b.angle.sin() * b.radius);
        // A slow rise and fall on the thermal.
        let y = b.height + (b.angle * 0.7).sin() * 3.0;
        // Along the circle, banked into the turn.
        let tangent = Vec3::new(-b.angle.sin(), 0.0, b.angle.cos()) * b.dir;
        let yaw = (-tangent.x).atan2(-tangent.z);
        let bank = -b.dir * (d.speed * d.speed / (b.radius * 9.81)).atan().min(0.6);
        t.translation = Vec3::new(pos.x, y, pos.z);
        t.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_z(bank);
        if let Some(mut a) = b.rig.and_then(|r| anims.get_mut(r).ok()) {
            *a = AnimRequest { kind: AnimKind::Walk, rate: 1.0 };
        }
    }
}

/// `FC3_BIRD_TEST`: looks at the nearest bird and logs the birds about.
fn bird_test(time: Res<Time>, mut last: Local<f32>, birds: Query<(&Bird, &Transform), Without<Player>>, mut players: Query<(&mut Transform, &mut Player)>) {
    let Ok(mode) = std::env::var("FC3_BIRD_TEST") else { return };
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    // `close`: ride along 8 m behind the first bird (with --noclip).
    if mode == "close" {
        if let Some((b, t)) = birds.iter().next() {
            let tangent = Vec3::new(-b.angle.sin(), 0.0, b.angle.cos()) * b.dir;
            pt.translation = t.translation - tangent * 8.0 + Vec3::Y * 1.5;
        }
    }
    let eye = pt.translation + Vec3::Y * 0.7;
    if let Some((_, t)) = birds.iter().min_by(|a, b| a.1.translation.distance(eye).total_cmp(&b.1.translation.distance(eye))) {
        let to = t.translation - eye;
        p.yaw = (-to.x).atan2(-to.z);
        p.pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
    }
    if time.elapsed_secs() - *last >= 2.0 {
        *last = time.elapsed_secs();
        let list: Vec<String> = birds.iter().map(|(b, t)| format!("{:?} {:.0} m", BIRDS[b.def].kind, t.translation.distance(eye))).collect();
        info!("bird-test: {list:?}");
    }
}
