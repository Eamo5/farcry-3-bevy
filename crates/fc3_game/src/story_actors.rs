//! The story's characters in the world: the pawns placed on mission layers
//! (and the world's own named characters), dressed from their graphic kit
//! and spawned near the player while their layer is present. Scripts know
//! them by their editor ids; their positions go back to the story each frame
//! and their deaths become the story's `Die` events.
//!
//! Armed characters hostile to the player fight with the enemy AI (`npc`).
//! While the story drives one (`npc::Scripted`: a contextual action, or the
//! AI starting under script control) it still sees and hears; once it
//! notices something its action is broken off, as the engine does, and the
//! AI takes it from there.

use std::collections::{HashMap, HashSet};

use avian3d::prelude::*;
use bevy::prelude::*;
use fc3_formats::kits;

use crate::cache_assets::Fc3Model;
use crate::characters::{load_kit, spawn_kit_character, AnimKind, AnimRequest, CharacterAssets, LoadedKit};
use crate::combat::Health;
use crate::data::{to_bevy, CacheInfo};
use crate::mission_layers::MissionLayers;
use crate::player::{Player, HALF_HEIGHT};
use crate::story::StoryHost;
use crate::weapons::Rng;

pub struct StoryActorsPlugin;

impl Plugin for StoryActorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StoryKits>()
            .init_resource::<CaMoves>()
            .init_resource::<DressedEntities>()
            .add_systems(Update, note_dressed)
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_story_kits, load_ca_moves))
            .init_resource::<ActorOrders>()
            .add_systems(
                Update,
                (spawn_actors, despawn_actors, actor_tasks, actor_alerts, actor_idle, actor_deaths).chain().run_if(in_state(crate::menu::GameState::Playing)),
            );
    }
}

/// Characters spawn within this distance and go beyond `DESPAWN`.
const SPAWN: f32 = 140.0;
const DESPAWN: f32 = 180.0;

/// Every graphic kit by id (parsed), and those loaded so far.
#[derive(Resource, Default)]
pub struct StoryKits {
    defs: HashMap<u64, kits::Kit>,
    loaded: HashMap<u64, LoadedKit>,
}

/// The story entities dressed as characters here, whose placed models (one
/// outfit piece in its bind pose) the world leaves out.
#[derive(Resource, Default)]
pub struct DressedEntities(pub HashSet<u64>);

fn note_dressed(host: Option<NonSend<StoryHost>>, mut dressed: ResMut<DressedEntities>) {
    let Some(host) = host else { return };
    if dressed.0.is_empty() {
        dressed.0 = host.story.data().entities.values().filter(|e| kit_of(e).is_some()).map(|e| e.id).collect();
    }
}

/// The clips contextual actions' moves play (`fc3_formats::moves`).
#[derive(Resource, Default)]
struct CaMoves(std::collections::BTreeMap<String, Vec<String>>);

fn load_ca_moves(info: Res<CacheInfo>, mut moves: ResMut<CaMoves>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::moves::CA_MOVES_REL)).unwrap_or_default();
    moves.0 = fc3_formats::moves::from_text(&text);
}

/// A story character in the world.
#[derive(Component)]
pub struct StoryActor {
    pub id: u64,
    pub rig: Option<Entity>,
    pub dead: bool,
    /// Holds a weapon (its contextual actions play the armed builds).
    pub armed: bool,
    /// Its own clips, while a contextual action's loop stands in for its
    /// idle.
    base_clips: Option<std::sync::Arc<crate::characters::ClipSet>>,
    /// An action's start clip playing: seconds left of it, and the loop
    /// that follows.
    ca_then: Option<(f32, Handle<crate::cache_assets::Fc3Clip>)>,
    /// The contextual action it is carrying out.
    pub task: Option<Task>,
    /// The way to the task's spot.
    path: Vec<Vec3>,
    /// A scene (`cinema`) is playing it.
    pub in_scene: bool,
}

/// A contextual action (a "smart terrain point"): go to its spot, then do
/// what it says there (for as long as it lasts, or until interrupted).
#[derive(Debug, Clone)]
pub struct Task {
    pub action: u64,
    pub target: Vec3,
    /// Facing at the spot (radians, Bevy yaw).
    pub yaw: f32,
    pub run: bool,
    /// Seconds once there (0: until interrupted).
    pub duration: f32,
    pub arrived: bool,
    pub time: f32,
    /// The action's start and loop clips (loading on the way there).
    pub clips: (Option<Handle<crate::cache_assets::Fc3Clip>>, Option<Handle<crate::cache_assets::Fc3Clip>>),
    /// A path to walk (a patrol-path action): it starts as the walk does and
    /// ends at the path's end.
    pub patrol: Option<Vec<Vec3>>,
}

/// The contextual action the story gave each character (by id; `None`:
/// stop), applied when they are in the world.
#[derive(Resource, Default)]
pub struct ActorOrders(pub HashMap<u64, Option<u64>>);

fn load_story_kits(info: Res<CacheInfo>, mut kits: ResMut<StoryKits>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::ALL_KITS_REL)).unwrap_or_default();
    kits.defs = kits::from_text(&text).into_iter().filter(|k| k.id != 0).map(|k| (k.id, k)).collect();
}

/// How a story character fights, when it is armed and hostile to the player.
#[derive(Clone, Copy, Debug)]
struct Fighter {
    faction: crate::npc::Faction,
    role: crate::npc::Role,
    /// Its AI starts in the scripts' hands.
    scripted: bool,
}

/// From the pawn's AI: its army's hostility to the player's (grey) army, its
/// primary weapon, and whether scripts hold it from the start.
fn fighter_of(e: &fc3_story::data::Entry) -> Option<Fighter> {
    let agent = e.component("CFCXAIComponent")?.child("AIObject")?.child("CPawnAgent")?;
    if !agent.child("ArmyHostileMask").and_then(|m| m.bool("bGrey")).unwrap_or(false) {
        return None;
    }
    let role = crate::npc::role_from_weapon(&agent.child("ShootingPrefsSingle").and_then(|w| w.str("tmplPrimaryWeapon")).unwrap_or_default())?;
    let name = e.name.to_ascii_lowercase();
    let faction = if name.contains("privateer") || name.contains("mercenar") { crate::npc::Faction::Privateer } else { crate::npc::Faction::Pirate };
    Some(Fighter { faction, role, scripted: agent.bool("bStartOnScriptControl").unwrap_or(false) })
}

/// A character's kit id, if it is one the story can dress (pawns, and the
/// kit-dressed figures scenes animate).
fn kit_of(e: &fc3_story::data::Entry) -> Option<u64> {
    e.component("CGraphicKitComponent").and_then(|c| c.u64("kitidGraphicKit")).filter(|&k| k != 0 && k != u64::MAX)
}

#[allow(clippy::too_many_arguments)]
fn spawn_actors(
    host: Option<NonSend<StoryHost>>,
    layers: Res<MissionLayers>,
    chars: Option<Res<CharacterAssets>>,
    mut kits: ResMut<StoryKits>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    players: Query<&Transform, With<Player>>,
    existing: Query<&StoryActor>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    mut last: Local<f32>,
    time: Res<Time>,
    faces: Option<ResMut<crate::faces::FaceLibrary>>,
) {
    let (Some(host), Some(chars)) = (host, chars) else { return };
    let mut faces = faces;
    // Twice a second is plenty.
    if time.elapsed_secs() - *last < 0.5 {
        return;
    }
    *last = time.elapsed_secs();
    let Ok(pt) = players.single() else { return };
    let have: HashSet<u64> = existing.iter().map(|a| a.id).collect();
    let data = host.story.data();
    let mut budget = 3;
    for e in data.entities.values() {
        if budget == 0 {
            break;
        }
        let Some(kit_id) = kit_of(e) else { continue };
        if have.contains(&e.id) || !host.story.is_loaded(e.id) || host.story.is_dead(e.id) {
            continue;
        }
        if e.layer != fc3_formats::world::DEFAULT_MISSION_LAYER && !layers.present.contains(&e.layer) {
            continue;
        }
        let at = to_bevy(e.pos);
        if at.distance(pt.translation) > SPAWN {
            continue;
        }
        // The kit's parts load the first time one of its characters shows.
        if !kits.loaded.contains_key(&kit_id) {
            let Some(def) = kits.defs.get(&kit_id).cloned() else { continue };
            kits.loaded.insert(kit_id, load_kit(&server, def));
        }
        let kit = &kits.loaded[&kit_id];
        if !kit.settled(&server) {
            continue;
        }
        budget -= 1;
        let yaw = (e.angles[2]).to_radians();
        let fighter = fighter_of(e);
        let health = fighter.map_or(100.0, |f| f.role.stats(f.faction).health);
        let root = commands
            .spawn((
                Transform::from_translation(at + Vec3::Y * HALF_HEIGHT).with_rotation(Quat::from_rotation_y(yaw)),
                Visibility::Visible,
                Health::new(health),
                RigidBody::Kinematic,
                Collider::capsule(0.3, 1.1),
                Name::new(e.name.clone()),
            ))
            .id();
        // Fighters carry their faction's gun for the role.
        let weapon = fighter.and_then(|f| chars.looks_for(f.faction, f.role).first().and_then(|&l| models.get(&chars.looks[l].weapon)));
        let rig = spawn_kit_character(&mut commands, root, &chars, kit, &models, weapon, &mut rng, Vec3::new(0.0, -HALF_HEIGHT, 0.0));
        // Its face rig, for scenes and spoken lines.
        if let (Some(rig), Some(faces), Some(c)) = (rig, faces.as_mut(), e.component("CFacialPoseComponent")) {
            if let Some(face) = faces.rig_for(c) {
                let phase = (e.id % 977) as f32 * 0.01;
                commands.entity(rig).insert(crate::faces::Face::new(face, faces.idle(), phase));
            }
        }
        commands.entity(root).insert(StoryActor {
            id: e.id,
            rig,
            dead: false,
            armed: fighter.is_some(),
            base_clips: None,
            ca_then: None,
            task: None,
            path: Vec::new(),
            in_scene: false,
        });
        if let Some(f) = fighter {
            commands.entity(root).insert(crate::npc::story_npc(f.faction, f.role, at, yaw, e.id, rig, &mut rng));
            if f.scripted {
                commands.entity(root).insert(crate::npc::Scripted);
            }
        }
        debug!("story actor {} ({}) at {:.0} m, rig {}, fighter {:?}", e.name, e.id, at.distance(pt.translation), rig.is_some(), fighter);
    }
}

fn despawn_actors(
    host: Option<NonSend<StoryHost>>,
    layers: Res<MissionLayers>,
    players: Query<&Transform, With<Player>>,
    actors: Query<(Entity, &StoryActor, &Transform)>,
    mut commands: Commands,
) {
    let Some(host) = host else { return };
    let Ok(pt) = players.single() else { return };
    let data = host.story.data();
    for (e, a, t) in &actors {
        let layer = data.entities.get(&a.id).map(|x| x.layer).unwrap_or(fc3_formats::world::DEFAULT_MISSION_LAYER);
        let gone = layer != fc3_formats::world::DEFAULT_MISSION_LAYER && !layers.present.contains(&layer);
        if gone || !host.story.is_loaded(a.id) || t.translation.distance(pt.translation) > DESPAWN {
            commands.entity(e).despawn();
        }
    }
}

/// A contextual action entity's task.
fn task_for(data: &fc3_story::data::StoryData, action: u64) -> Option<Task> {
    let e = data.entities.get(&action)?;
    let ca = e.component("CAIContextualActionComponent");
    let run = ca.and_then(|c| c.u32("selMoveToSTPSpeed")).unwrap_or(1) >= 2;
    let params = ca.and_then(|c| c.child("Parameters"));
    let duration = params.and_then(|p| p.u32("Duration")).unwrap_or(0) as f32;
    // A patrol path: the shape entity its parameters name (field 3DD4EBA2),
    // walked point by point.
    let patrol = ca
        .filter(|c| c.str_h(0xD54F_0DD2).is_some_and(|s| s.to_ascii_lowercase().contains("patrolpath")))
        .and_then(|_| params?.field_h(0x3DD4_EBA2).filter(|v| v.len() == 8).map(|v| u64::from_le_bytes(v.try_into().unwrap())))
        .and_then(|path| data.entities.get(&path))
        .and_then(|p| shape_points(&p.fields))
        .filter(|pts| pts.len() >= 2);
    let target = patrol.as_ref().and_then(|p| p.last().copied()).unwrap_or(to_bevy(e.pos));
    Some(Task { action, target, yaw: e.angles[2].to_radians(), run, duration, arrived: false, time: 0.0, clips: (None, None), patrol })
}

/// A shape entity's points (`hidShapePoints`: a count, then x, y, z each),
/// in Bevy space.
fn shape_points(fields: &fc3_formats::fcb::Object) -> Option<Vec<Vec3>> {
    let raw = fields.field("hidShapePoints")?;
    let n = u32::from_le_bytes(raw.get(..4)?.try_into().ok()?) as usize;
    let f = |c: &[u8], i: usize| f32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap());
    Some(raw.get(4..)?.chunks_exact(12).take(n).map(|c| to_bevy([f(c, 0), f(c, 1), f(c, 2)])).collect())
}

/// Characters carry out the contextual actions the story gives them: walk
/// (or run) to the spot along the navmesh, face its way, stay for its
/// duration; the story hears when they get there and when they are done.
#[allow(clippy::too_many_arguments)]
fn actor_tasks(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    mut orders: ResMut<ActorOrders>,
    nav: Res<crate::nav::NavMesh>,
    heights: Res<crate::terrain::Heightfield>,
    mut actors: Query<(Entity, &mut StoryActor, &mut Transform, Option<&mut crate::npc::Npc>, Has<crate::npc::Scripted>)>,
    mut anims: Query<&mut AnimRequest>,
    mut commands: Commands,
    (mut rigs, moves, server, clips): (Query<&mut crate::characters::CharacterRig>, Res<CaMoves>, Res<AssetServer>, Res<Assets<crate::cache_assets::Fc3Clip>>),
) {
    let Some(mut host) = host else { return };
    let dt = time.delta_secs();
    let data = host.story.data();
    for (entity, mut a, mut t, npc, scripted) in &mut actors {
        if a.dead {
            continue;
        }
        let id = a.id;
        if let Some(order) = orders.0.remove(&id) {
            a.task = None;
            a.path.clear();
            restore_clips(&mut *a, &mut rigs);
            if let (Some(mut npc), Some(_)) = (npc, order) {
                if npc.state == crate::npc::NpcState::Combat {
                    // Busy fighting: the action is broken off at once.
                    host.story.action_interrupted(id);
                    continue;
                }
                if !scripted {
                    commands.entity(entity).insert(crate::npc::Scripted);
                }
                npc.state = crate::npc::NpcState::Idle;
                npc.awareness = 0.0;
                npc.last_known = None;
            }
            if let Some(mut task) = order.and_then(|action| task_for(&data, action)) {
                let load = |c: Option<String>| c.map(|c| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(&c))));
                let (start, looped) = action_clips(&data, task.action, &moves, a.armed);
                task.clips = (load(start), load(looped));
                a.path = match &task.patrol {
                    // A patrol starts as the walk does.
                    Some(points) => {
                        host.story.action_started(id);
                        points.clone()
                    }
                    None => nav.path(t.translation, task.target).unwrap_or_else(|| vec![task.target]),
                };
                debug!(
                    "story actor {id}: to {} ({:.0} m, {} waypoints)",
                    data.name_of(task.action).unwrap_or("?"),
                    t.translation.distance(task.target),
                    a.path.len()
                );
                a.task = Some(task);
            }
        }
        let a = &mut *a;
        if a.in_scene {
            continue;
        }
        if a.task.as_ref().is_some_and(|t| !t.arrived) && (time.elapsed_secs() * 2.0) as u32 != ((time.elapsed_secs() - dt) * 2.0) as u32 {
            trace!("story actor {id}: at {:.1?}, {} waypoints left, next {:.1?}", t.translation, a.path.len(), a.path.first());
        }
        let (path, rig) = (&mut a.path, a.rig);
        let Some(task) = a.task.as_mut() else { continue };
        let mut kind = AnimKind::Idle;
        let mut end = false;
        if !task.arrived {
            let speed = if task.run { 4.0 } else { 1.6 };
            let next = path.first().copied().unwrap_or(task.target);
            let to = Vec3::new(next.x - t.translation.x, 0.0, next.z - t.translation.z);
            let step = speed * dt;
            if to.length() <= step.max(0.3) {
                if !path.is_empty() {
                    path.remove(0);
                }
                if path.is_empty() && task.patrol.is_some() {
                    // The path walked: the action is done.
                    debug!("story actor {id}: walked {}", data.name_of(task.action).unwrap_or("?"));
                    end = true;
                } else if path.is_empty() {
                    task.arrived = true;
                    debug!("story actor {id}: arrived at {}", data.name_of(task.action).unwrap_or("?"));
                    t.rotation = Quat::from_rotation_y(task.yaw);
                    host.story.action_started(id);
                    // The action's start plays, then its loop stands in for
                    // the idle while it lasts.
                    if let Some(mut r) = rig.and_then(|r| rigs.get_mut(r).ok()) {
                        let (start, looped) = task.clips.clone();
                        let start = start.and_then(|h| clips.get(&h).map(|c| (h, c.0.duration)));
                        let first = start.as_ref().map(|s| s.0.clone()).or(looped.clone());
                        if let Some(first) = first {
                            let mut set = crate::characters::ClipSet::clone(r.clips());
                            a.base_clips.get_or_insert_with(|| r.clips().clone());
                            set.clips.insert(AnimKind::Idle, first);
                            r.set_clips(std::sync::Arc::new(set));
                        }
                        a.ca_then = start.zip(looped).map(|((_, d), l)| (d, l));
                    }
                }
            } else {
                let dir = to.normalize();
                t.translation += dir * step;
                if let Some(y) = nav.height_at(t.translation).or_else(|| heights.height_at(t.translation)) {
                    t.translation.y = y + HALF_HEIGHT;
                }
                t.rotation = Quat::from_rotation_y((-dir.x).atan2(-dir.z));
                kind = if task.run { AnimKind::Run } else { AnimKind::Walk };
            }
        } else {
            task.time += dt;
            end = task.duration > 0.0 && task.time >= task.duration;
            // The start clip done: on to the loop.
            if let Some((left, looped)) = a.ca_then.as_mut() {
                *left -= dt;
                if *left <= 0.0 {
                    if let Some(mut r) = rig.and_then(|r| rigs.get_mut(r).ok()) {
                        let mut set = crate::characters::ClipSet::clone(r.clips());
                        set.clips.insert(AnimKind::Idle, looped.clone());
                        r.set_clips(std::sync::Arc::new(set));
                    }
                    a.ca_then = None;
                }
            }
        }
        if end {
            a.task = None;
            restore_clips(a, &mut rigs);
            host.story.action_finished(id);
        }
        if let Some(mut r) = rig.and_then(|rig| anims.get_mut(rig).ok()) {
            if r.kind != kind && r.kind != AnimKind::Death {
                *r = AnimRequest { kind, rate: 1.0 };
            }
        }
    }
}

/// The clips a contextual action starts with and loops (its moves' builds
/// for an armed or unarmed character), where found.
fn action_clips(data: &fc3_story::data::StoryData, action: u64, moves: &CaMoves, armed: bool) -> (Option<String>, Option<String>) {
    use fc3_formats::moves::{pick_clip, split_move, Phase};
    let Some(params) = data.entities.get(&action).and_then(|e| e.component("CAIContextualActionComponent")).and_then(|c| c.child("Parameters")) else {
        return (None, None);
    };
    let names = fc3_formats::moves::move_names(params);
    let clip = |phase: Phase| names.iter().find(|m| split_move(m).1 == phase).and_then(|m| moves.0.get(m)).and_then(|c| pick_clip(c, armed)).cloned();
    let (start, looped) = (clip(Phase::Start), clip(Phase::Loop));
    // (Some start moves play the loop's own clip: no start then.)
    (start.filter(|s| looped.as_ref() != Some(s)), looped)
}

/// Gives a character back its own clips after a contextual action.
fn restore_clips(a: &mut StoryActor, rigs: &mut Query<&mut crate::characters::CharacterRig>) {
    a.ca_then = None;
    if let (Some(base), Some(mut r)) = (a.base_clips.take(), a.rig.and_then(|r| rigs.get_mut(r).ok())) {
        r.set_clips(base);
    }
}

/// A fighter the story drives that notices something (a noise, the player,
/// a body) breaks off its contextual action, and its AI takes over from
/// where it stands.
fn actor_alerts(
    host: Option<NonSendMut<StoryHost>>,
    mut actors: Query<(Entity, &mut StoryActor, &Transform, &mut crate::npc::Npc), With<crate::npc::Scripted>>,
    mut commands: Commands,
    mut noises: MessageReader<crate::npc::Noise>,
    mut rigs: Query<&mut crate::characters::CharacterRig>,
) {
    for n in noises.read() {
        trace!("noise at {:.0?}, heard {:.0} m off", n.at, n.radius);
    }
    let Some(mut host) = host else { return };
    for (entity, mut a, t, mut npc) in &mut actors {
        if a.dead || a.in_scene || npc.awareness <= 0.35 || npc.last_known.is_none() {
            continue;
        }
        debug!(
            "story actor {}: alerted (awareness {:.2}, sees the player {}, by {:.0?} {:.0} m off), AI takes over",
            a.id,
            npc.awareness,
            npc.sees_player,
            npc.last_known,
            npc.last_known.map_or(0.0, |k| k.distance(t.translation))
        );
        commands.entity(entity).remove::<crate::npc::Scripted>();
        npc.post = t.translation - Vec3::Y * HALF_HEIGHT;
        npc.post_yaw = t.rotation.to_euler(EulerRot::YXZ).0;
        npc.yaw = npc.post_yaw;
        a.path.clear();
        restore_clips(&mut *a, &mut rigs);
        if a.task.take().is_some() {
            host.story.action_interrupted(a.id);
        }
    }
}

/// Characters stand at their places on the human idle until scripts or AI
/// move them.
fn actor_idle(
    actors: Query<&StoryActor, Or<(Without<crate::npc::Npc>, With<crate::npc::Scripted>)>>,
    mut anims: Query<&mut AnimRequest>,
    rigs: Query<&crate::characters::CharacterRig>,
    mut commands: Commands,
    time: Res<Time>,
    mut logged: Local<bool>,
) {
    let log = !*logged && time.elapsed_secs() > 30.0 && std::env::var("FC3_STORY_ANIM_DEBUG").is_ok();
    for a in &actors {
        let Some(rig) = a.rig else { continue };
        // Moving characters animate as they go (`actor_tasks`); scenes pose
        // theirs.
        if a.in_scene || a.task.as_ref().is_some_and(|t| !t.arrived) {
            continue;
        }
        if log {
            *logged = true;
            info!("story actor {}: rig {:?}", a.id, rigs.get(rig).map(|r| r.summary()));
        }
        let kind = if a.dead { AnimKind::Death } else { AnimKind::Idle };
        match anims.get_mut(rig) {
            Ok(mut r) => {
                if r.kind != kind {
                    *r = AnimRequest { kind, rate: 1.0 };
                }
            }
            Err(_) => {
                commands.entity(rig).insert(AnimRequest { kind, rate: 1.0 });
            }
        }
    }
}

/// A character killed: the story hears of it (group `Die` events, as the
/// engine raises them for every group the victim is in).
fn actor_deaths(host: Option<NonSendMut<StoryHost>>, mut actors: Query<(&mut StoryActor, &Health)>) {
    let Some(mut host) = host else { return };
    for (mut a, h) in &mut actors {
        if h.dead && !a.dead {
            a.dead = true;
            let id = a.id;
            host.story.killed(id);
        }
    }
}
