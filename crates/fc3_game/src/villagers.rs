//! Villagers: the island's locals going about the world's villager zones,
//! dressed from the game's generic villager kits (men and women) and moving
//! on its civilian clips. They wander their patch, stop at the world's
//! villager spots nearby to hoe a garden, lean on a wall, sit, pray or go
//! through a body's pockets, run from gunfire and explosions, cower, and can
//! be hurt.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::cache_assets::{cache_path, Fc3Clip, Fc3Model, Fc3Sector};
use crate::characters::{AnimKind, AnimRequest, CharacterAssets, CharacterRig, ClipSet, CLIP_ROOT};
use crate::combat::Health;
use crate::objects::SectorObjects;
use crate::player::{Player, HALF_HEIGHT};
use crate::weapons::Rng;

pub struct VillagersPlugin;

impl Plugin for VillagersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(crate::menu::GameState::Loading), load_spots).add_systems(
            Update,
            (spawn_villagers, despawn_villagers, villager_ai, spot_clips, villager_log).chain().run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

/// Zones fill with villagers within this distance; they go beyond the second.
const SPAWN: f32 = 110.0;
const DESPAWN: f32 = 170.0;
/// Villagers per zone, how far they wander from it, and their pace.
const PER_ZONE: usize = 3;
const WANDER: f32 = 7.0;
const WALK_SPEED: f32 = 1.2;
const RUN_SPEED: f32 = 5.0;
/// A noise this much nearer than its own reach scares them.
const HEAR: f32 = 0.8;
/// Seconds they flee for, then cower for.
const FLEE_TIME: f32 = 5.0;
const COWER_TIME: (f32, f32) = (6.0, 11.0);
const CORPSE_TIME: f32 = 60.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Idle,
    Walk(Vec3),
    /// Doing what a villager spot is for.
    AtSpot,
    Flee,
    Cower,
    Dead,
}

/// A villager goes to a free spot this near their patch (m), now and then
/// (the share of idle turns), and stays a while (s).
const SPOT_REACH: f32 = 75.0;
/// Gardens (hoeing spots within this of each other, m) get farmhands of their
/// own, this many, unless a villager zone is near.
const GARDEN: f32 = 40.0;
const FARMHANDS: usize = 2;
const SPOT_CHANCE: f32 = 0.6;
const SPOT_STAY: (f32, f32) = (30.0, 80.0);

struct Spot {
    kind: &'static str,
    at: Vec3,
    yaw: f32,
}

#[derive(Resource, Default)]
struct VillagerSpots {
    list: Vec<Spot>,
    taken: HashMap<usize, Entity>,
    /// Centres of the gardens (groups of hoeing spots).
    gardens: Vec<Vec3>,
}

/// Groups the hoeing spots into gardens: each spot joins the first garden
/// whose centre is within `GARDEN`, else starts one.
fn gardens(list: &[Spot]) -> Vec<Vec3> {
    let mut groups: Vec<(Vec3, usize)> = Vec::new();
    for s in list.iter().filter(|s| s.kind == "hoe") {
        match groups.iter_mut().find(|(c, _)| c.distance(s.at) < GARDEN) {
            Some((c, n)) => {
                *c = (*c * *n as f32 + s.at) / (*n + 1) as f32;
                *n += 1;
            }
            None => groups.push((s.at, 1)),
        }
    }
    groups.into_iter().map(|g| g.0).collect()
}

/// The loop for a spot (under `animations\`), by kind and sex; `None` where
/// the game has no clip for that sex.
fn spot_clip(kind: &str, female: bool) -> Option<&'static str> {
    Some(match (kind, female) {
        ("hoe", false) => r"smart_terrain_points\civilians\themespecific\extworking\simpleanim\gardening\3rdcivm_fulbst_gardenninghoe_loop_+000fw_nowep.mab",
        ("lean", false) => r"smart_terrain_points\civilians\generic_natives\simple_animation\neutral\lean\3rdcivm_fulbln_leanonwall_loop_v00_+000fw_nowep.mab",
        ("lean", true) => r"smart_terrain_points\civilians\generic_natives\simple_animation\neutral\lean\3rdcivf_fulbln_leanonwall_loop_v00_+000fw_nowep.mab",
        ("sit", false) => r"smart_terrain_points\civilians\generic_natives\simple_animation\neutral\ground\3rdcivm_fulbgr_sitonground_loop_+000fw_nowep.mab",
        ("sit", true) => r"smart_terrain_points\civilians\generic_natives\simple_animation\neutral\ground\3rdcivf_fulbgr_sitonground_loop_+000fw_nowep.mab",
        ("pray", false) => {
            r"smart_terrain_points\civilians\generic_natives\simple_animation\praying\ground\3rdcivm_fulbgr_praying_v00_loop_+000fw_noweapon.mab"
        }
        ("pray", true) => r"smart_terrain_points\civilians\generic_natives\simple_animation\praying\ground\3rdcivf_fulbgr_praying_v00_loop_+000fw_noweapon.mab",
        ("loot", false) => {
            r"smart_terrain_points\civilians\generic_natives\simple_animation\lootingcorpse\3rdcivm_fulb_lootingcorpse_v00_loop_+000fw_noweap.mab"
        }
        ("loot", true) => {
            r"smart_terrain_points\civilians\generic_natives\simple_animation\lootingcorpse\3rdcivf_fulb_lootingcorpse_v00_loop_+000fw_noweap.mab"
        }
        _ => return None,
    })
}

/// The hoe a gardening villager works with.
const HOE: &str = r"graphics\__fc3_graphics\singleplayer\props\household\hoegardentool_01.xbg";

fn load_spots(mut commands: Commands, info: Res<crate::CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::VILLAGER_SPOTS_REL)).unwrap_or_default();
    let list: Vec<Spot> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let kind = match it.next()? {
                "hoe" => "hoe",
                "lean" => "lean",
                "sit" => "sit",
                "pray" => "pray",
                "loot" => "loot",
                _ => return None,
            };
            let v: Vec<f32> = it.filter_map(|x| x.parse().ok()).collect();
            let [x, y, z, qx, qy, qz, qw] = v[..] else { return None };
            let (yaw, _, _) = Quat::from_xyzw(qx, qy, qz, qw).to_euler(EulerRot::YXZ);
            Some(Spot { kind, at: Vec3::new(x, y, z), yaw })
        })
        .collect();
    let gardens = gardens(&list);
    info!("{} villager spots, {} gardens", list.len(), gardens.len());
    commands.insert_resource(VillagerSpots { list, taken: HashMap::new(), gardens });
}

#[derive(Component)]
pub struct Villager {
    zone: u64,
    home: Vec3,
    state: State,
    timer: f32,
    yaw: f32,
    /// What they're running from.
    fear: Vec3,
    rig: Option<Entity>,
    female: bool,
    /// The villager spot they're headed for or at.
    spot: Option<usize>,
    /// Their own clips while a spot's loop plays, and the tool in hand.
    base: Option<Arc<ClipSet>>,
    prop: Option<Entity>,
}

/// Ground under a point: the navigation mesh where it's loaded (village
/// floors and terraces), else the terrain.
fn ground(nav: &crate::nav::NavMesh, heights: &crate::terrain::Heightfield, p: Vec3) -> Option<f32> {
    nav.height_at(p).or_else(|| heights.height_at(p))
}

#[allow(clippy::too_many_arguments)]
fn spawn_villagers(
    mut spawned: Local<HashSet<u64>>,
    chars: Option<Res<CharacterAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    sectors: Res<Assets<Fc3Sector>>,
    loaded: Query<&SectorObjects>,
    players: Query<&Transform, With<Player>>,
    existing: Query<&Villager>,
    outposts: Res<crate::outposts::Outposts>,
    (nav, heights): (Res<crate::nav::NavMesh>, Res<crate::terrain::Heightfield>),
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    spots: Option<Res<VillagerSpots>>,
) {
    let debug = std::env::var("FC3_VILLAGER_TEST").is_ok();
    let Some(chars) = chars else { return };
    if !chars.ready(&server) || chars.villager_kits.iter().all(Option::is_none) {
        if debug {
            debug!("villagers: assets ready {} kits {:?}", chars.ready(&server), chars.villager_kits);
        }
        return;
    }
    let Ok(pt) = players.single() else { return };
    if debug {
        let zones: Vec<String> = loaded
            .iter()
            .filter_map(|so| sectors.get(&so.sector))
            .flat_map(|s| s.entities.iter().filter(|e| e.class.contains("Villager")))
            .map(|e| {
                let at = Vec3::from_array(e.translation);
                let held = outposts.list.iter().filter(|o| !o.liberated).map(|o| (o.centre.distance(at), o.radius)).min_by(|a, b| a.0.total_cmp(&b.0));
                let d = crate::data::to_dunia(at);
                format!("zone at {:.0},{:.0} {:.0} m (nearest held outpost {held:?})", d[0], d[1], at.distance(pt.translation))
            })
            .collect();
        debug!("villagers: zones {zones:?}");
    }
    let live: HashSet<u64> = existing.iter().map(|v| v.zone).collect();
    spawned.retain(|z| live.contains(z));
    for so in &loaded {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        for e in sector.entities.iter().filter(|e| e.class == "CVillagerZone") {
            let at = Vec3::from_array(e.translation);
            if spawned.contains(&e.id) || at.distance(pt.translation) > SPAWN {
                continue;
            }
            // Not in an outpost the enemy still holds.
            if outposts.list.iter().any(|o| !o.liberated && o.centre.distance(at) < o.radius + 10.0) {
                continue;
            }
            spawned.insert(e.id);
            for k in 0..PER_ZONE {
                // Women are about a third of them.
                let female = chars.villager_kits[1].is_some() && (k == 2 || chars.villager_kits[0].is_none());
                spawn_one(&mut commands, &chars, &models, &nav, &heights, &mut rng, e.id, at, female);
            }
        }
    }
    // Gardens with no village by them get farmhands (the hoeing is men's work).
    let Some(spots) = spots else { return };
    if chars.villager_kits[0].is_none() {
        return;
    }
    for (i, &centre) in spots.gardens.iter().enumerate() {
        let id = GARDEN_ZONE | i as u64;
        if spawned.contains(&id) || centre.distance(pt.translation) > SPAWN {
            continue;
        }
        let village = loaded
            .iter()
            .filter_map(|so| sectors.get(&so.sector))
            .flat_map(|s| s.entities.iter().filter(|e| e.class == "CVillagerZone"))
            .any(|e| Vec3::from_array(e.translation).distance(centre) < SPOT_REACH);
        if village || outposts.list.iter().any(|o| !o.liberated && o.centre.distance(centre) < o.radius + 10.0) {
            continue;
        }
        spawned.insert(id);
        for _ in 0..FARMHANDS {
            spawn_one(&mut commands, &chars, &models, &nav, &heights, &mut rng, id, centre, false);
        }
    }
}

/// Garden "zones" have ids of their own.
const GARDEN_ZONE: u64 = 1 << 62;

/// One villager near `at` (their patch).
#[allow(clippy::too_many_arguments)]
fn spawn_one(
    commands: &mut Commands,
    chars: &CharacterAssets,
    models: &Assets<Fc3Model>,
    nav: &crate::nav::NavMesh,
    heights: &crate::terrain::Heightfield,
    rng: &mut Rng,
    zone: u64,
    at: Vec3,
    female: bool,
) {
    let a = rng.f32() * std::f32::consts::TAU;
    let mut p = at + Vec3::new(a.cos(), 0.0, a.sin()) * rng.range(0.5, WANDER * 0.6);
    let Some(y) = ground(nav, heights, p) else { return };
    p.y = y;
    let yaw = rng.range(-3.1, 3.1);
    let root = commands
        .spawn((
            Transform::from_translation(p + Vec3::Y * HALF_HEIGHT).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::Visible,
            Health::new(50.0),
            RigidBody::Kinematic,
            Collider::capsule(0.3, 1.1),
            Name::new("villager"),
        ))
        .id();
    let rig = crate::characters::spawn_villager(commands, root, chars, models, female, rng, Vec3::new(0.0, -HALF_HEIGHT, 0.0));
    commands.entity(root).insert(Villager {
        zone,
        home: at,
        state: State::Idle,
        timer: rng.range(1.0, 6.0),
        yaw,
        fear: Vec3::ZERO,
        rig,
        female,
        spot: None,
        base: None,
        prop: None,
    });
}

fn despawn_villagers(players: Query<&Transform, With<Player>>, villagers: Query<(Entity, &Villager, &Transform)>, mut commands: Commands) {
    let Ok(pt) = players.single() else { return };
    for (e, v, t) in &villagers {
        let gone = v.state == State::Dead && v.timer > CORPSE_TIME;
        if gone || t.translation.distance(pt.translation) > DESPAWN && v.home.distance(pt.translation) > DESPAWN {
            commands.entity(e).despawn();
        }
    }
}

/// Wandering, fleeing and cowering.
#[allow(clippy::too_many_arguments)]
fn villager_ai(
    time: Res<Time>,
    mut noises: MessageReader<crate::npc::Noise>,
    mut damage: MessageReader<crate::combat::DamageEvent>,
    (nav, heights): (Res<crate::nav::NavMesh>, Res<crate::terrain::Heightfield>),
    mut rng: ResMut<Rng>,
    mut villagers: Query<(Entity, &mut Villager, &mut Transform, &Health)>,
    mut anims: Query<&mut AnimRequest>,
    mut commands: Commands,
    spots: Option<ResMut<VillagerSpots>>,
) {
    let dt = time.delta_secs();
    let Some(mut spots) = spots else { return };
    spots.taken.retain(|_, e| villagers.contains(*e));
    let noises: Vec<crate::npc::Noise> = noises.read().copied().collect();
    let hurt: Vec<(Entity, Option<Vec3>)> = damage.read().map(|d| (d.target, d.source)).collect();
    for (e, mut v, mut t, h) in &mut villagers {
        v.timer += if v.state == State::Dead { dt } else { 0.0 };
        if h.dead {
            if v.state != State::Dead {
                v.state = State::Dead;
                v.timer = 0.0;
                commands.entity(e).insert(ColliderDisabled);
                if let Some(i) = v.spot.take() {
                    spots.taken.remove(&i);
                }
            }
            if let Some(mut a) = v.rig.and_then(|r| anims.get_mut(r).ok()) {
                *a = AnimRequest { kind: AnimKind::Death, rate: 1.0 };
            }
            continue;
        }
        let pos = t.translation;
        // Gunfire, an explosion or being hit: run from it.
        let scare = hurt
            .iter()
            .find(|(target, _)| *target == e)
            .map(|(_, from)| from.unwrap_or(pos))
            .or_else(|| noises.iter().find(|n| n.at.distance(pos) < (n.radius * HEAR).min(60.0)).map(|n| n.at));
        if let Some(from) = scare {
            if v.state != State::Flee {
                v.state = State::Flee;
                v.timer = FLEE_TIME;
                if let Some(i) = v.spot.take() {
                    spots.taken.remove(&i);
                }
            }
            v.fear = from;
        }
        let mut step = Vec3::ZERO;
        let (kind, rate) = match v.state {
            State::Idle => {
                v.timer -= dt;
                if v.timer <= 0.0 {
                    // Off to a free spot nearby now and then, else a stroll.
                    let home = v.home;
                    let female = v.female;
                    let free = (rng.f32() < SPOT_CHANCE)
                        .then(|| {
                            spots
                                .list
                                .iter()
                                .enumerate()
                                .filter(|(i, s)| !spots.taken.contains_key(i) && spot_clip(s.kind, female).is_some() && s.at.distance(home) < SPOT_REACH)
                                .min_by(|a, b| a.1.at.distance(pos).total_cmp(&b.1.at.distance(pos)))
                                .map(|(i, s)| (i, s.at))
                        })
                        .flatten();
                    if let Some((i, at)) = free {
                        spots.taken.insert(i, e);
                        v.spot = Some(i);
                        v.state = State::Walk(at);
                    } else {
                        let a = rng.f32() * std::f32::consts::TAU;
                        let to = v.home + Vec3::new(a.cos(), 0.0, a.sin()) * rng.range(1.0, WANDER);
                        v.state = State::Walk(to);
                    }
                }
                (AnimKind::Idle, 1.0)
            }
            State::AtSpot => {
                v.timer -= dt;
                if v.timer <= 0.0 {
                    if let Some(i) = v.spot.take() {
                        spots.taken.remove(&i);
                    }
                    v.state = State::Idle;
                    v.timer = rng.range(3.0, 9.0);
                }
                (AnimKind::Idle, 1.0)
            }
            State::Walk(to) => {
                let flat = Vec3::new(to.x - pos.x, 0.0, to.z - pos.z);
                if flat.length() < 0.4 {
                    match v.spot.and_then(|i| spots.list.get(i)) {
                        // There: take up the spot's place and facing.
                        Some(s) => {
                            v.state = State::AtSpot;
                            v.timer = rng.range(SPOT_STAY.0, SPOT_STAY.1);
                            v.yaw = s.yaw;
                            t.translation = s.at + Vec3::Y * HALF_HEIGHT;
                        }
                        None => {
                            v.state = State::Idle;
                            v.timer = rng.range(3.0, 9.0);
                        }
                    }
                } else {
                    step = flat.normalize() * WALK_SPEED * dt;
                }
                (AnimKind::Walk, 1.0)
            }
            State::Flee => {
                v.timer -= dt;
                let away = Vec3::new(pos.x - v.fear.x, 0.0, pos.z - v.fear.z).normalize_or(Vec3::X);
                step = away * RUN_SPEED * dt;
                if v.timer <= 0.0 {
                    v.state = State::Cower;
                    v.timer = rng.range(COWER_TIME.0, COWER_TIME.1);
                }
                (AnimKind::Run, 1.0)
            }
            State::Cower => {
                v.timer -= dt;
                if v.timer <= 0.0 {
                    // Back to their patch, slowly.
                    v.state = State::Walk(v.home);
                }
                (AnimKind::Cover, 1.0)
            }
            State::Dead => (AnimKind::Death, 1.0),
        };
        if step != Vec3::ZERO {
            let next = pos + step;
            // Stay on walkable ground (the mesh, where it's loaded); blocked:
            // turn aside, or give up on the walk.
            let on_mesh = !nav.ready()
                || nav.height_at(next - Vec3::Y * HALF_HEIGHT).is_some()
                || heights.height_at(next).is_some_and(|g| (g - (pos.y - HALF_HEIGHT)).abs() < 0.6);
            if on_mesh {
                if let Some(g) = ground(&nav, &heights, next - Vec3::Y * HALF_HEIGHT) {
                    t.translation = Vec3::new(next.x, g + HALF_HEIGHT, next.z);
                }
                v.yaw = (-step.x).atan2(-step.z);
            } else if let State::Walk(_) = v.state {
                v.state = State::Idle;
                v.timer = rng.range(1.0, 3.0);
                if let Some(i) = v.spot.take() {
                    spots.taken.remove(&i);
                }
            } else {
                // Fleeing into a wall: veer.
                v.fear = pos - Quat::from_rotation_y(0.8) * (v.fear - pos);
            }
        }
        t.rotation = Quat::from_rotation_y(v.yaw);
        if let Some(mut a) = v.rig.and_then(|r| anims.get_mut(r).ok()) {
            *a = AnimRequest { kind, rate };
        }
    }
}

/// At a spot, a villager plays its loop (with a hoe in hand for the garden);
/// leaving it, their own clips come back.
fn spot_clips(
    server: Res<AssetServer>,
    spots: Option<Res<VillagerSpots>>,
    clips: Res<Assets<Fc3Clip>>,
    models: Res<Assets<Fc3Model>>,
    mut hoe: Local<Option<Handle<Fc3Model>>>,
    mut villagers: Query<&mut Villager>,
    mut rigs: Query<&mut CharacterRig>,
    mut commands: Commands,
) {
    let Some(spots) = spots else { return };
    let hoe = hoe.get_or_insert_with(|| server.load(cache_path(&fc3_formats::cache::model_rel(HOE)))).clone();
    for mut v in &mut villagers {
        let Some(mut rig) = v.rig.and_then(|r| rigs.get_mut(r).ok()) else { continue };
        let at = (v.state == State::AtSpot).then_some(v.spot).flatten().and_then(|i| spots.list.get(i));
        match (at, v.base.is_some()) {
            (Some(s), false) => {
                let Some(path) = spot_clip(s.kind, v.female) else { continue };
                let clip: Handle<Fc3Clip> = server.load(cache_path(&fc3_formats::cache::anim_rel(&format!("{CLIP_ROOT}{path}"))));
                let base = rig.clips().clone();
                let mut set = ClipSet { skeleton: base.skeleton.clone(), clips: base.clips.clone() };
                set.clips.insert(AnimKind::Idle, clip);
                rig.set_clips(Arc::new(set));
                v.base = Some(base);
            }
            (None, true) => {
                if let Some(base) = v.base.take() {
                    rig.set_clips(base);
                }
                if let Some(p) = v.prop.take() {
                    commands.entity(p).despawn();
                }
            }
            _ => {}
        }
        // The hoe, once the clip and model have loaded: in the right hand,
        // where the clip's tool track puts it.
        if at.is_some_and(|s| s.kind == "hoe") && v.prop.is_none() {
            let Some(clip) = rig.clips().clips.get(&AnimKind::Idle).and_then(|h| clips.get(h)) else { continue };
            let (Some(tool), Some(model), Some((_, hand))) = (clip.0.sub_clips.first(), models.get(&hoe), rig.joint(fc3_formats::hash::name_hash("RightHand")))
            else {
                continue;
            };
            let track = tool.tracks.iter().min_by_key(|t| t.bone);
            let rotation = track.and_then(|t| t.sample_rotation(0.0)).map(Quat::from_array).unwrap_or(Quat::from_array(tool.reference_rotation));
            let translation = track.and_then(|t| t.sample_translation(0.0)).map(Vec3::from_array).unwrap_or(Vec3::from_array(tool.reference_translation));
            let root = commands.spawn((Transform { translation, rotation, ..default() }, Visibility::Inherited, ChildOf(hand))).id();
            if let Some(lod) = model.lods.first() {
                for part in &lod.parts {
                    commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), crate::original_shaders::PropPart, ChildOf(root)));
                }
            }
            v.prop = Some(root);
        }
    }
}

/// `FC3_VILLAGER_TEST`: logs the villagers around the player each two
/// seconds (`=panic`: a gunshot at the player 20 s in).
pub fn villager_log(
    time: Res<Time>,
    mut next: Local<f32>,
    mut shot: Local<bool>,
    villagers: Query<(&Villager, &Transform)>,
    players: Query<&Transform, With<Player>>,
    mut noises: MessageWriter<crate::npc::Noise>,
) {
    let Ok(mode) = std::env::var("FC3_VILLAGER_TEST") else { return };
    let Ok(pt) = players.single() else { return };
    if mode == "panic" && !*shot && time.elapsed_secs() > 20.0 && !villagers.is_empty() {
        *shot = true;
        noises.write(crate::npc::Noise { at: pt.translation, radius: 90.0, kind: crate::npc::NoiseKind::Threatening });
        info!("villager-test: a shot");
    }
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    let list: Vec<String> = villagers.iter().map(|(v, t)| format!("{:?} {:.0} m", v.state, t.translation.distance(pt.translation))).collect();
    info!("villager-test: {} villagers {list:?}", list.len());
}
