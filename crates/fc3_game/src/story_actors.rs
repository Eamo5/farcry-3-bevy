//! The story's characters in the world: the pawns placed on mission layers
//! (and the world's own named characters), dressed from their graphic kit
//! and spawned near the player while their layer is present. Scripts know
//! them by their editor ids; their positions go back to the story each frame
//! and their deaths become the story's `Die` events.
//!
//! Reinforcements the story calls in (`SpawnReinforcement`) are a point's
//! mercs, made from their library archetypes. With the point's vehicle they
//! ride in on it (`story_rides`); otherwise they come ashore where the
//! point's vehicle path meets land. Then they search the battlefield.
//!
//! Armed characters hostile to the player fight with the enemy AI (`npc`).
//! While the story drives one (`npc::Scripted`: a contextual action, or the
//! AI starting under script control) it still sees and hears; once it
//! notices something its action is broken off, as the engine does, and the
//! AI takes it from there.
//!
//! A character at rest takes up the relax point it was placed by (a
//! `Relax/SimpleAnimation.*` contextual action on its own layer: looting
//! crates, smoking, leaning on a wall), as the AI's rest does, until
//! something breaks it off. The story isn't told: no script asked.

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

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ActorSet;

impl Plugin for StoryActorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StoryKits>()
            .init_resource::<CaMoves>()
            .init_resource::<RelaxOrders>()
            .init_resource::<DressedEntities>()
            .add_systems(Update, note_dressed)
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_story_kits, load_ca_moves))
            .init_resource::<ActorOrders>()
            .init_resource::<Arrivals>()
            .init_resource::<CalledIn>()
            .add_systems(
                Update,
                (
                    land_reinforcements,
                    spawn_actors,
                    despawn_actors,
                    actor_relax,
                    actor_tasks,
                    companions,
                    actor_alerts,
                    ai_events,
                    actor_idle,
                    actor_health,
                    actor_deaths,
                    keep_watched_bodies,
                )
                    .chain()
                    .in_set(ActorSet)
                    .after(crate::story::EntityPosesSet)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            );
    }
}

/// Mercs the story has called in at a reinforcement point, under the ids it
/// gave them, with their archetypes.
#[derive(Clone, Debug, PartialEq)]
pub struct Reinforcement {
    pub point: u64,
    pub battlefield: Option<u64>,
    /// The vehicle bringing them (its id and archetype), if the point has one.
    pub vehicle: Option<(u64, u64)>,
    pub mercs: Vec<(u64, u64)>,
    /// Called in relaxed (`SpawnInRelax`): at their posts, not on their way.
    pub relaxed: bool,
}

/// Where a relaxed point's mercs stand: at the point itself, on whatever it
/// stands on (JAS_190's boat crew on the cenote's dock, the sea under it),
/// when no way in brings them and the point isn't out in the sea.
fn at_post(relaxed: bool, way_in: bool, point: Vec3) -> Option<Vec3> {
    (relaxed && !way_in && point.y >= crate::sky::SEA_LEVEL - 0.25).then_some(point)
}

/// Called-in characters, placed and waiting to be spawned like the story's
/// own, and where they head (the battlefield).
#[derive(Resource, Default)]
pub struct Arrivals(pub Vec<(fc3_story::data::Entry, Option<Vec3>)>);

/// Called-in characters in the world, as they were called in: one left
/// behind out of range waits to come back where it was (the story still
/// counts it).
#[derive(Resource, Default)]
struct CalledIn(HashMap<u64, fc3_story::data::Entry>);

/// Where a reinforcement comes ashore: along from the end of the point's
/// vehicle path (or the point itself) toward the battlefield, the first spot
/// whose ground stands clear of the sea. None while that ground isn't loaded.
fn landing(start: Vec3, toward: Option<Vec3>, ground: impl Fn(Vec3) -> Option<f32>) -> Option<Vec3> {
    let dir = toward.map(|t| (t - start).with_y(0.0)).filter(|d| d.length() > 1.0).map_or(Vec3::ZERO, Vec3::normalize);
    for step in 0..=120 {
        let at = start + dir * step as f32;
        let g = ground(at)?;
        if g > crate::sky::SEA_LEVEL + 0.3 {
            return Some(at.with_y(g));
        }
        if dir == Vec3::ZERO {
            break;
        }
    }
    None
}

/// Turns the story's reinforcement orders into placed characters (and the
/// vehicles that bring them).
fn land_reinforcements(
    host: Option<NonSendMut<StoryHost>>,
    heights: Res<crate::terrain::Heightfield>,
    mut arrivals: ResMut<Arrivals>,
    (mut rides, mut flights, server): (ResMut<crate::story_rides::Rides>, ResMut<crate::story_helis::Flights>, Res<AssetServer>),
    players: Query<&Transform, With<Player>>,
) {
    let Some(mut host) = host else { return };
    if host.reinforcements.is_empty() {
        return;
    }
    let Ok(player) = players.single().map(|t| t.translation) else { return };
    let orders = std::mem::take(&mut host.reinforcements);
    let data = host.story.data();
    let mut waiting = Vec::new();
    for order in orders {
        let Some(point) = data.entities.get(&order.point) else {
            warn!("story: reinforcement point {} isn't in the story's data", order.point);
            continue;
        };
        let rp = point.component("CFCXAIComponent").and_then(|ai| ai.child("AIObject")).and_then(|o| o.child("CReinforcementPoint"));
        // The vehicle's way in, when it has one: its end is where it lands.
        let path_id = rp.and_then(|rp| rp.u64("entVehiclePathId").or_else(|| rp.u64("entPathId")).filter(|&p| p != u64::MAX));
        let points = path_id.and_then(|p| data.entities.get(&p)).and_then(|p| shape_points(&p.fields)).unwrap_or_default();
        let field = order.battlefield.and_then(|b| data.entities.get(&b)).map(|b| to_bevy(b.pos));
        // Flying in: the whole of its way, the crew aboard from its start.
        let heli = order.vehicle.and_then(|(_, ark)| data.archetypes.get(&ark)).and_then(crate::story_helis::heli_data);
        if let (Some((vehicle, _)), Some(heli), true) = (order.vehicle, heli, points.len() >= 2) {
            let speed = rp.and_then(|rp| rp.f32("VehicleMaxSpeed")).filter(|s| *s > 0.0).unwrap_or(15.0);
            let land = rp.and_then(|rp| rp.bool("bVehicleLandOnArrival")).unwrap_or(false);
            let mut crew = Vec::new();
            for (id, ark) in &order.mercs {
                let Some(archetype) = data.archetypes.get(ark) else {
                    warn!("story: reinforcement archetype {ark:016x} isn't in the cache (reconvert)");
                    continue;
                };
                let mut e = archetype.clone();
                e.id = *id;
                e.pos = crate::data::to_dunia(points[0]);
                arrivals.0.push((e, None));
                crew.push(*id);
            }
            debug!("story: reinforcement helicopter {vehicle} flies in along {path_id:?}, {} aboard", crew.len());
            flights.0.push(crate::story_helis::Flight::new(&server, vehicle, heli, points.clone(), speed, land, crew, field));
            continue;
        }
        // Riding in: from where its way comes into reach of the player (the
        // crew are only in the world that near), the crew beside the vehicle.
        let def = order.vehicle.and_then(|(_, ark)| data.archetypes.get(&ark)).and_then(crate::story_rides::vehicle_def);
        let begin = points.iter().position(|p| p.distance(player) < SPAWN - 15.0).filter(|&i| i + 1 < points.len());
        if let (Some((vehicle, ark)), Some(def), Some(path), Some(begin)) = (order.vehicle, def, path_id, begin) {
            let way = points[begin..].to_vec();
            let speed = rp.and_then(|rp| rp.f32("VehicleMaxSpeed")).filter(|s| *s > 0.0).unwrap_or(12.0);
            let ahead = (way[1] - way[0]).with_y(0.0).normalize_or(Vec3::NEG_Z);
            let side = Vec3::new(-ahead.z, 0.0, ahead.x);
            let mut crew = Vec::new();
            for (i, (id, ark)) in order.mercs.iter().enumerate() {
                let Some(archetype) = data.archetypes.get(ark) else {
                    warn!("story: reinforcement archetype {ark:016x} isn't in the cache (reconvert)");
                    continue;
                };
                let at = way[0] + side * (3.0 + i as f32 * 1.2);
                let ground = heights.height_at(at).unwrap_or(way[0].y).max(crate::sky::SEA_LEVEL);
                let mut e = archetype.clone();
                e.id = *id;
                e.pos = crate::data::to_dunia(at.with_y(ground));
                arrivals.0.push((e, None));
                crew.push(*id);
            }
            debug!("story: reinforcement vehicle {vehicle} ({ark:016x}) rides in along {path} from node {begin}, {} aboard", crew.len());
            rides.0.push(crate::story_rides::Ride::new(vehicle, ark, def, path, way, speed, crew, field));
            continue;
        }
        if order.vehicle.is_some() {
            debug!("story: reinforcement vehicle of {} not ridden (no model or no way in near the player); the mercs come ashore", order.point);
        }
        if let Some(post) = at_post(order.relaxed, !points.is_empty(), to_bevy(point.pos)) {
            for (i, (id, ark)) in order.mercs.iter().enumerate() {
                let Some(archetype) = data.archetypes.get(ark) else {
                    warn!("story: reinforcement archetype {ark:016x} isn't in the cache (reconvert)");
                    continue;
                };
                let mut e = archetype.clone();
                e.id = *id;
                e.pos = crate::data::to_dunia(post + Vec3::X * i as f32 * 1.2);
                e.angles = point.angles;
                debug!("story: reinforcement {} ({id}) at its post {post:.1}", e.name);
                arrivals.0.push((e, None));
            }
            continue;
        }
        let start = points.last().copied().unwrap_or_else(|| to_bevy(point.pos));
        let Some(ashore) = landing(start, field, |p| heights.height_at(p)) else {
            waiting.push(order);
            continue;
        };
        let face = field.map(|f| f - ashore).filter(|d| d.with_y(0.0).length() > 0.5).map_or(0.0, |d| (-d.x).atan2(-d.z));
        let side = Vec3::new(face.cos(), 0.0, -face.sin());
        for (i, (id, ark)) in order.mercs.iter().enumerate() {
            let Some(archetype) = data.archetypes.get(ark) else {
                warn!("story: reinforcement archetype {ark:016x} isn't in the cache (reconvert)");
                continue;
            };
            let at = ashore + side * (i as f32 - (order.mercs.len() as f32 - 1.0) * 0.5) * 1.5;
            let at = at.with_y(heights.height_at(at).unwrap_or(ashore.y));
            let mut e = archetype.clone();
            e.id = *id;
            e.pos = crate::data::to_dunia(at);
            e.angles = [0.0, 0.0, -face.to_degrees()];
            debug!("story: reinforcement {} ({id}) ashore at {at:.1}", e.name);
            arrivals.0.push((e, field));
        }
    }
    host.reinforcements = waiting;
}

/// An event a script logged in the army's diary (`LogEventInAIDiary` on a
/// `CAIEventPoint`): for its lifetime, the army's soldiers within its reach
/// take the state it names about its threat (the player, unless it names
/// one): on to him in combat (3) or alerted (2), to the point when
/// suspicious (1).
#[derive(Clone, Debug, PartialEq)]
struct AiEvent {
    at: Vec3,
    army: u32,
    state: u32,
    reach: f32,
    left: f32,
    threat: Option<u64>,
}

impl AiEvent {
    fn from_point(e: &fc3_story::data::Entry) -> Option<AiEvent> {
        let ai = e.component("CAIComponent")?.child("AIObject")?;
        Some(AiEvent {
            at: to_bevy(e.pos),
            army: ai.u32("selArmy").unwrap_or(1),
            state: ai.u32("selState").unwrap_or(3),
            reach: ai.f32("fDistEffect").unwrap_or(50.0),
            left: ai.f32("fLifetime").unwrap_or(10.0),
            threat: ai.u64("entThreatId").filter(|&t| t != u64::MAX),
        })
    }

    /// Whether a soldier of `faction` belongs to its army: 1 is Red, the
    /// pirates' and privateers' (the player's foes).
    fn takes(&self, faction: crate::npc::Faction) -> bool {
        match self.army {
            1 => faction.hostile_to_player(),
            _ => false,
        }
    }

    /// What a soldier at `pos` makes of it: the state to take and where to
    /// look, or nothing (out of reach, or a state it doesn't raise).
    fn order(&self, pos: Vec3, threat: Vec3) -> Option<(crate::npc::NpcState, Vec3)> {
        if pos.distance(self.at) > self.reach {
            return None;
        }
        match self.state {
            2 | 3 => Some((crate::npc::NpcState::Search, threat)),
            1 => Some((crate::npc::NpcState::Suspicious, self.at)),
            _ => None,
        }
    }
}

/// Puts the army's soldiers on the events scripts log.
fn ai_events(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    players: Query<&Transform, With<Player>>,
    targets: Query<(&StoryActor, &Transform), Without<crate::npc::Npc>>,
    mut npcs: Query<(&Transform, &mut crate::npc::Npc)>,
    mut active: Local<Vec<AiEvent>>,
) {
    let Some(mut host) = host else { return };
    for id in std::mem::take(&mut host.ai_events) {
        match host.story.data().entities.get(&id).and_then(AiEvent::from_point) {
            Some(event) => {
                debug!("story: AI event {id}: army {} state {} within {} m for {} s", event.army, event.state, event.reach, event.left);
                active.push(event);
            }
            None => warn!("story: AI event point {id} isn't one"),
        }
    }
    if active.is_empty() {
        return;
    }
    let Ok(player) = players.single() else { return };
    for event in active.iter() {
        let threat = match event.threat {
            Some(t) => targets.iter().find(|(a, _)| a.id == t).map_or(event.at, |(_, at)| at.translation),
            None => player.translation,
        };
        for (at, mut npc) in &mut npcs {
            if !event.takes(npc.faction) || matches!(npc.state, crate::npc::NpcState::Combat) {
                continue;
            }
            if let Some((state, look)) = event.order(at.translation, threat) {
                if npc.state != state || npc.last_known != Some(look) {
                    npc.state = state;
                    npc.last_known = Some(look);
                }
            }
        }
    }
    let dt = time.delta_secs();
    active.retain_mut(|e| {
        e.left -= dt;
        e.left > 0.0
    });
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
pub(crate) struct CaMoves(pub(crate) std::collections::BTreeMap<String, Vec<String>>);

fn load_ca_moves(info: Res<CacheInfo>, mut moves: ResMut<CaMoves>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::moves::CA_MOVES_REL)).unwrap_or_default();
    moves.0 = fc3_formats::moves::from_text(&text);
}

/// A story character that is an animal (a seagull, a dog), not a person.
#[derive(Component)]
pub struct StoryCreature;

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
    /// A physical vehicle seat owns movement (cinematics may still pose it).
    pub seated: bool,
}

impl StoryActor {
    pub(crate) fn new(id: u64, rig: Option<Entity>, armed: bool) -> Self {
        Self { id, rig, dead: false, armed, base_clips: None, ca_then: None,
            task: None, path: Vec::new(), in_scene: false, seated: false }
    }

    /// Authored actions and cinematics own this actor until released.
    pub(crate) fn script_driven(&self) -> bool {
        self.in_scene || self.seated || self.task.is_some()
    }

    /// A teleport invalidates the route from the old position. Keep the
    /// action itself, but find a fresh route to its spot or next patrol leg.
    pub fn relocate(&mut self, at: Vec3, nav: &crate::nav::NavMesh) {
        let Some(task) = self.task.as_ref().filter(|t| !t.arrived) else { return };
        if task.patrol.is_some() {
            if let Some(&next) = self.path.first() {
                let rest = self.path.iter().skip(1).copied();
                self.path = nav.path(at, next).unwrap_or_else(|| vec![next]).into_iter().chain(rest).collect();
            }
        } else {
            self.path = nav.path(at, task.target).unwrap_or_else(|| vec![task.target]);
        }
    }
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
    /// Taken up by the character itself (a relax point it rests at), not
    /// the story's: the story hears nothing of it, and it lasts until
    /// broken off.
    pub own: bool,
}

/// The contextual action the story gave each character (by id; `None`:
/// stop), applied when they are in the world.
#[derive(Resource, Default)]
pub struct ActorOrders(pub HashMap<u64, Option<u64>>);

/// The relax points characters at rest take up (character, point), and
/// those already looked for.
#[derive(Resource, Default)]
pub struct RelaxOrders {
    orders: HashMap<u64, u64>,
    tried: HashSet<u64>,
    /// The relax points (id, layer, place), read once.
    points: Option<Vec<(u64, u64, Vec3)>>,
}

/// How near (m, across the ground) a character must stand to the relax
/// point it takes up.
const RELAX_REACH: f32 = 2.5;

/// Characters at rest by a relax point take it up (once each: one broken
/// off stays with its AI).
fn actor_relax(
    host: Option<NonSendMut<StoryHost>>,
    mut relax: ResMut<RelaxOrders>,
    orders: Res<ActorOrders>,
    actors: Query<(&StoryActor, &Transform, Option<&crate::npc::Npc>)>,
) {
    let Some(host) = host else { return };
    let data = host.story.data();
    let relax = &mut *relax;
    let points = relax.points.get_or_insert_with(|| {
        data.entities
            .values()
            .filter(|e| e.name.starts_with("Relax/SimpleAnimation.") && e.component("CAIContextualActionComponent").is_some())
            .map(|e| (e.id, e.layer, to_bevy(e.pos)))
            .collect()
    });
    let taken: HashSet<u64> = actors.iter().filter_map(|(a, ..)| a.task.as_ref().map(|t| t.action)).chain(relax.orders.values().copied()).collect();
    for (a, t, npc) in &actors {
        if relax.tried.contains(&a.id) || a.dead || a.seated || a.in_scene || a.task.is_some() || orders.0.contains_key(&a.id) || a.rig.is_none() {
            continue;
        }
        if npc.is_some_and(|n| n.state != crate::npc::NpcState::Idle || n.awareness > 0.0) {
            continue;
        }
        relax.tried.insert(a.id);
        let Some(layer) = data.entities.get(&a.id).map(|e| e.layer) else { continue };
        let feet = t.translation - Vec3::Y * HALF_HEIGHT;
        let near = points
            .iter()
            .filter(|(id, l, at)| *l == layer && !taken.contains(id) && at.with_y(0.0).distance(feet.with_y(0.0)) < RELAX_REACH && (at.y - feet.y).abs() < 2.0)
            .min_by(|x, y| x.2.distance_squared(feet).total_cmp(&y.2.distance_squared(feet)));
        if let Some(&(point, ..)) = near {
            debug!("story actor {}: rests at {}", a.id, data.name_of(point).unwrap_or("?"));
            relax.orders.insert(a.id, point);
        }
    }
}

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
/// primary weapon (none for a beheader's machete), and whether scripts hold
/// it from the start.
/// A placed story character that isn't a fighter (a captive, a guide):
/// sent to a battlefield it is given, as its AI would take it there.
pub(crate) fn walks_to_battlefields(data: &fc3_story::data::StoryData, id: u64) -> bool {
    data.entities.get(&id).is_some_and(|e| e.component("CFCXAIComponent").is_some() && fighter_of(e).is_none())
}

fn fighter_of(e: &fc3_story::data::Entry) -> Option<Fighter> {
    let agent = e.component("CFCXAIComponent")?.child("AIObject")?.child("CPawnAgent")?;
    if !agent.child("ArmyHostileMask").and_then(|m| m.bool("bGrey")).unwrap_or(false) {
        return None;
    }
    let mut personality = String::new();
    agent.walk(&mut |o, _| {
        if personality.is_empty() {
            personality = o.str("tmplPersonalityType").map(|p| p.to_ascii_lowercase()).unwrap_or_default();
        }
    });
    let beheader = personality.contains("personality.beheader");
    let primary = agent.child("ShootingPrefsSingle").and_then(|w| w.str("tmplPrimaryWeapon")).unwrap_or_default();
    // (A sniper's own weapon preference is "Unused": his personality says
    // what he is. HOY_010's last pirate, a sniper, was nobody's foe.)
    let by_personality = || (!personality.is_empty() && !beheader).then(|| crate::npc::role_from_weapon(&personality)).flatten();
    let role = crate::npc::role_from_weapon(&primary).or_else(by_personality).or(beheader.then_some(crate::npc::Role::Charger))?;
    let name = e.name.to_ascii_lowercase();
    let faction = if name.contains("privateer") || name.contains("mercenar") { crate::npc::Faction::Privateer } else { crate::npc::Faction::Pirate };
    Some(Fighter { faction, role, scripted: agent.bool("bStartOnScriptControl").unwrap_or(false) })
}

/// The skeleton a pawn's animation component names (`fileSkeleton`'s
/// path): the Ink Monster's own, where people share the master's.
pub(crate) fn pawn_skeleton(e: &fc3_story::data::Entry) -> Option<String> {
    let mut path = None;
    e.components.walk(&mut |o, _| {
        if path.is_none() {
            path = o.str_h(0xF9F2_D5F4).filter(|p| p.to_ascii_lowercase().ends_with(".skeleton"));
        }
    });
    path
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
    animals: Option<Res<crate::wildlife::AnimalAssets>>,
    mut kits: ResMut<StoryKits>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    players: Query<&Transform, With<Player>>,
    existing: Query<&StoryActor>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    (mut last, time, mut arrivals, mut called): (Local<f32>, Res<Time>, ResMut<Arrivals>, ResMut<CalledIn>),
    faces: Option<ResMut<crate::faces::FaceLibrary>>,
    placements: Res<crate::story::EntityPlacements>,
    damage: Res<crate::combat::DamageTable>,
    (seats, vehicles): (Res<crate::vehicles::StorySeatRequests>, Query<(&Transform, &crate::vehicles::Vehicle), Without<Player>>),
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
    let mut arrived = HashSet::new();
    let called_in = arrivals.0.iter().map(|(e, field)| (e, Some(*field)));
    for (e, field) in data.entities.values().map(|e| (e, None)).chain(called_in) {
        if budget == 0 {
            break;
        }
        let Some(kit_id) = kit_of(e) else { continue };
        if field.is_some() && (have.contains(&e.id) || host.story.is_dead(e.id)) {
            arrived.insert(e.id);
            continue;
        }
        if have.contains(&e.id) || !host.story.is_loaded(e.id) || host.story.is_dead(e.id) {
            continue;
        }
        let animal = if e.component("CAIDogNetworkComponent").is_some() {
            crate::wildlife::story_species("attackdog_fc3")
        } else {
            // (A story shark has an animal's counters but no network
            // component: it is an animal all the same.)
            e.component("CAnimalNetworkComponent").or_else(|| e.component("CFCXCountersComponentAnimal")).and_then(|_| crate::wildlife::story_species(&e.name))
        };
        if let Some(species) = animal {
            if animals.as_ref().is_none_or(|a| !a.ready_for_story(species, &server)) {
                continue;
            }
        }
        if e.layer != fc3_formats::world::DEFAULT_MISSION_LAYER && !layers.present.contains(&e.layer) {
            continue;
        }
        let pose = placements.pose(e.id).unwrap_or_else(|| Transform {
            translation: to_bevy(e.pos),
            rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)),
            scale: Vec3::ONE,
        });
        // One a script has waiting to board a vehicle in the world comes
        // with it, however far (a convoy's crew, out of sight): the seat
        // takes him.
        let boarding = seats.boarding(e.id).and_then(|v| vehicles.iter().find(|(_, x)| x.story_id() == v)).map(|(t, _)| t.translation + Vec3::Y);
        let at = boarding.unwrap_or(pose.translation);
        if boarding.is_none() && at.distance(pt.translation) > SPAWN {
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
        let yaw = pose.rotation.to_euler(EulerRot::YXZ).0;
        let fighter = fighter_of(e);
        // Its own counters (health, armour, what harms it), as placed.
        let counters = damage.0.as_ref().and(e.component("CFCXCountersComponentAI")).map(|c| {
            let name = e.name.rsplit(['/', ':']).next().unwrap_or(&e.name);
            damage.counters(fc3_formats::counters::Vulnerability::from_component(name, c))
        });
        let health = counters.as_ref().map(|c| c.1).unwrap_or_else(|| fighter.map_or(100.0, |f| f.role.stats(f.faction).health));
        let root = commands
            .spawn((
                Transform { translation: at + Vec3::Y * HALF_HEIGHT, ..pose },
                Visibility::Visible,
                Health::new(health),
                RigidBody::Kinematic,
                Collider::capsule(0.3, 1.1),
                Name::new(e.name.clone()),
            ))
            .id();
        if let Some((c, _)) = counters {
            commands.entity(root).insert(c);
        }
        // Fighters carry their faction's gun for the role.
        let weapon = fighter.and_then(|f| chars.looks_for(f.faction, f.role).first().and_then(|&l| models.get(&chars.looks[l].weapon)));
        let rig = spawn_kit_character(&mut commands, root, &chars, kit, &models, weapon, fighter.map(|f| f.role), &mut rng, Vec3::new(0.0, -HALF_HEIGHT, 0.0));
        // Its face rig, for scenes and spoken lines.
        if let (Some(rig), Some(faces), Some(c)) = (rig, faces.as_mut(), e.component("CFacialPoseComponent")) {
            if let Some(face) = faces.rig_for(c) {
                let phase = (e.id % 977) as f32 * 0.01;
                commands.entity(rig).insert(crate::faces::Face::new(face, faces.idle(), phase));
            }
        }
        commands.entity(root).insert(StoryActor::new(e.id, rig, fighter.is_some()));
        // What killing him scores in a trial (his kind's bonus).
        if let Some(kind) = e.component("CFCXChallengeBonusComponent").and_then(|c| c.u32("selIncrementType")).filter(|k| *k != 0) {
            commands.entity(root).insert(crate::trials::ChallengeBonus(kind));
        }
        if e.component("CAnimalNetworkComponent").is_some()
            || e.component("CFCXCountersComponentAnimal").is_some()
            || e.component("CAIDogNetworkComponent").is_some()
            || e.name.contains("Animal")
        {
            commands.entity(root).insert(StoryCreature);
        }
        // Its own loot (a mission's operator and his codes).
        if let Some(own) = e.component("CFCXPawnLootComponent").and_then(|c| crate::pickups::OwnLoot::from_component(c)) {
            commands.entity(root).insert(own);
        }
        if let (Some(species), Some(animals)) = (animal, animals.as_ref()) {
            animals.bind_story(&mut commands, root, species, rig, at, yaw, &mut rng);
            debug!("story animal {} ({}) bound to wildlife", e.name, e.id);
        }
        if let Some(f) = fighter {
            let mut npc = crate::npc::story_npc(f.faction, f.role, at, yaw, e.id, rig, &mut rng);
            // Called in: they come looking.
            if let Some(Some(field)) = field {
                npc.0.state = crate::npc::NpcState::Search;
                npc.0.last_known = Some(field);
                arrived.insert(e.id);
            }
            commands.entity(root).insert(npc);
            if f.scripted {
                commands.entity(root).insert(crate::npc::Scripted);
            }
        }
        if field.is_some() {
            arrived.insert(e.id);
            called.0.insert(e.id, e.clone());
        }
        debug!("story actor {} ({}) at {:.0} m, rig {}, fighter {:?}", e.name, e.id, at.distance(pt.translation), rig.is_some(), fighter);
    }
    if !arrived.is_empty() {
        arrivals.0.retain(|(e, _)| !arrived.contains(&e.id));
    }
}

fn despawn_actors(
    host: Option<NonSend<StoryHost>>,
    seats: Res<crate::vehicles::StorySeatRequests>,
    layers: Res<MissionLayers>,
    players: Query<&Transform, With<Player>>,
    actors: Query<(Entity, &StoryActor, &Transform, Option<&crate::npc::Npc>)>,
    mut called: ResMut<CalledIn>,
    mut arrivals: ResMut<Arrivals>,
    mut commands: Commands,
) {
    let Some(host) = host else { return };
    let Ok(pt) = players.single() else { return };
    let data = host.story.data();
    for (e, a, t, npc) in &actors {
        let layer = data.entities.get(&a.id).map(|x| x.layer).unwrap_or(fc3_formats::world::DEFAULT_MISSION_LAYER);
        let gone = layer != fc3_formats::world::DEFAULT_MISSION_LAYER && !layers.present.contains(&layer);
        let loaded = host.story.is_loaded(a.id);
        // (A body a script still watches stays where it fell: a VIP's, to
        // be photographed.)
        let watched_body = a.dead && host.story.watches_entity(a.id);
        // (Nor one riding, or about to: he goes with his vehicle.)
        let aboard = a.seated || seats.boarding(a.id).is_some();
        if gone || !loaded || (t.translation.distance(pt.translation) > DESPAWN && !watched_body && !aboard) {
            commands.entity(e).despawn();
            // Called in and left behind alive: back when the player is.
            if let Some(mut entry) = called.0.remove(&a.id).filter(|_| loaded && !a.dead && !host.story.is_dead(a.id)) {
                entry.pos = crate::data::to_dunia(t.translation - Vec3::Y * HALF_HEIGHT);
                debug!("story: called-in {} ({}) left behind at {:.0}", entry.name, a.id, t.translation);
                arrivals.0.push((entry, npc.and_then(|n| n.last_known)));
            }
        }
    }
}

/// A contextual action entity's task.
fn task_for(data: &fc3_story::data::StoryData, action: u64) -> Option<Task> {
    let e = data.entities.get(&action)?;
    let ca = e.component("CAIContextualActionComponent");
    // (A battlefield, not an action: run there.)
    let run = ca.map_or(true, |c| c.u32("selMoveToSTPSpeed").unwrap_or(1) >= 2);
    let params = ca.and_then(|c| c.child("Parameters"));
    // (Seconds, as a float; read as an integer, 2.4 s was a lifetime and
    // the action never ended. A whole number stored as one still reads.)
    let duration = params
        .and_then(|p| {
            p.f32("Duration")
                .filter(|d| d.is_finite() && (0.01..3600.0).contains(d))
                .or_else(|| p.u32("Duration").filter(|d| *d < 3600).map(|d| d as f32))
        })
        .unwrap_or(0.0);
    // A patrol path: the shape entity its parameters name (field 3DD4EBA2),
    // walked point by point.
    let patrol = ca
        .filter(|c| c.str_h(0xD54F_0DD2).is_some_and(|s| s.to_ascii_lowercase().contains("patrolpath")))
        .and_then(|_| params?.field_h(0x3DD4_EBA2).filter(|v| v.len() == 8).map(|v| u64::from_le_bytes(v.try_into().unwrap())))
        .and_then(|path| data.entities.get(&path))
        .and_then(|p| shape_points(&p.fields))
        .filter(|pts| pts.len() >= 2);
    let target = patrol.as_ref().and_then(|p| p.last().copied()).unwrap_or(to_bevy(e.pos));
    Some(Task { action, target, yaw: e.angles[2].to_radians(), run, duration, arrived: false, time: 0.0, clips: (None, None), patrol, own: false })
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
    mut debug_actor: Local<Option<Option<u64>>>,
    (spatial, bodies): (SpatialQuery, Query<(), Or<(With<crate::npc::Npc>, With<StoryActor>, With<Player>)>>),
    mut relax: ResMut<RelaxOrders>,
) {
    let Some(mut host) = host else { return };
    let dt = time.delta_secs();
    let data = host.story.data();
    let debug_id = *debug_actor.get_or_insert_with(|| std::env::var("FC3_STORY_ACTOR_DEBUG").ok().and_then(|s| s.parse().ok()));
    for (entity, mut a, mut t, npc, scripted) in &mut actors {
        if debug_id == Some(a.id) && time.elapsed_secs() as u32 != (time.elapsed_secs() - dt) as u32 {
            info!("story actor diagnostic {}: at {:.2?}, scene {}, seated {}, dead {}, task {:?}, pending {:?}, next {:?}",
                a.id, t.translation, a.in_scene, a.seated, a.dead,
                a.task.as_ref().map(|t| (t.action, t.arrived)), orders.0.get(&a.id), a.path.first());
        }
        if a.dead || a.seated {
            continue;
        }
        let id = a.id;
        // The story's order, else a relax point taken up.
        let order = orders.0.remove(&id).map(|o| (o, false)).or_else(|| relax.orders.remove(&id).map(|p| (Some(p), true)));
        if let Some((order, own)) = order {
            if a.task.take().is_some_and(|t| t.own) && !own {
                debug!("story actor {id}: leaves its rest for the story's action");
            }
            a.path.clear();
            restore_clips(&mut *a, &mut rigs);
            if let (Some(mut npc), Some(_)) = (npc, order) {
                if npc.state == crate::npc::NpcState::Combat {
                    // Busy fighting: the action is broken off at once.
                    if !own {
                        host.story.action_interrupted(id);
                    }
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
                // (A rest lasts until broken off.)
                if own {
                    task.own = true;
                    task.duration = 0.0;
                }
                let load = |c: Option<String>| c.map(|c| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(&c))));
                let (start, looped) = action_clips(&data, task.action, &moves, a.armed);
                task.clips = (load(start), load(looped));
                a.path = match &task.patrol {
                    // A patrol starts as the walk does.
                    Some(points) => {
                        if !task.own {
                            host.story.action_started(id);
                        }
                        points.clone()
                    }
                    // (A path stopping short of the spot, off the
                    // navigation mesh, is finished straight to it.)
                    None => {
                        let mut path = nav.path(t.translation, task.target).unwrap_or_else(|| vec![task.target]);
                        if path.last().is_none_or(|l| l.with_y(0.0).distance(task.target.with_y(0.0)) > 0.5) {
                            path.push(task.target);
                        }
                        path
                    }
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
                    if !task.own {
                        host.story.action_started(id);
                    }
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
                // On a floor the navigation mesh and the terrain don't know
                // (a boardwalk over the water): what is under the feet,
                // a step up at most (not someone's head).
                let feet = t.translation.y - HALF_HEIGHT;
                let reach = 1.2;
                let floor = spatial
                    .cast_ray_predicate(t.translation.with_y(feet + reach), Dir3::NEG_Y, reach + 2.0, true, &SpatialQueryFilter::default(), &|e| {
                        e != entity && !bodies.contains(e)
                    })
                    .map(|h| feet + reach - h.distance);
                let ground = nav.height_at(t.translation).or_else(|| heights.height_at(t.translation));
                let y = match (floor, ground) {
                    (Some(f), Some(g)) => Some(f.max(g)),
                    (f, g) => f.or(g),
                };
                if let Some(y) = y {
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
            let own = a.task.take().is_some_and(|t| t.own);
            restore_clips(a, &mut rigs);
            if !own {
                host.story.action_finished(id);
            }
        }
        if let Some(mut r) = rig.and_then(|rig| anims.get_mut(rig).ok()) {
            if r.kind != kind && r.kind != AnimKind::Death {
                *r = AnimRequest { kind, rate: 1.0 };
            }
        }
    }
}

/// How far a following companion lets the player get before going after
/// him, and how close it comes.
const FOLLOW_FAR: f32 = 5.0;
const FOLLOW_NEAR: f32 = 2.5;
/// How fast companions go (m/s): leading, and catching the player up.
const LEAD_SPEED: f32 = 2.2;
const FOLLOW_SPEED: f32 = 3.5;

/// Companions (`CAPI_Companion`): one following goes after the player when
/// he gets away; one leading walks its path from its first point, waiting
/// (facing him) while he is further off than its lead distance, and tells
/// the story when it reaches the end; one waiting stands. Not while it is on a contextual action or in
/// a scene.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn companions(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    nav: Res<crate::nav::NavMesh>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<&Transform, (With<crate::player::Player>, Without<StoryActor>)>,
    mut actors: Query<(Entity, &StoryActor, &mut Transform, Option<&mut crate::npc::Npc>, Has<crate::npc::Scripted>), Without<crate::player::Player>>,
    mut anims: Query<&mut AnimRequest>,
    mut commands: Commands,
    mut legs: Local<HashMap<u64, (u64, usize)>>,
) {
    let Some(mut host) = host else { return };
    let orders = host.story.companions();
    if orders.is_empty() {
        legs.clear();
        return;
    }
    let Ok(player) = players.single() else { return };
    let data = host.story.data();
    let dt = time.delta_secs();
    let mut led = Vec::new();
    for (entity, a, mut t, npc, scripted) in &mut actors {
        let Some(&(_, order)) = orders.iter().find(|o| o.0 == a.id) else { continue };
        if a.dead || a.seated || a.in_scene || a.task.is_some() {
            continue;
        }
        // The story drives it, not its own AI.
        if let Some(mut npc) = npc {
            if !scripted {
                commands.entity(entity).insert(crate::npc::Scripted);
            }
            npc.state = crate::npc::NpcState::Idle;
        }
        let to_player = (player.translation - t.translation).with_y(0.0);
        let goal = match order {
            fc3_story::Companion::Follow => {
                (to_player.length() > FOLLOW_FAR).then(|| (player.translation - to_player.normalize_or_zero() * FOLLOW_NEAR, FOLLOW_SPEED))
            }
            fc3_story::Companion::Lead { path, distance } => {
                let points = data.entities.get(&path).and_then(|p| shape_points(&p.fields)).unwrap_or_default();
                let flat = |p: Vec3| (p - t.translation).with_y(0.0).length();
                let leg = legs.entry(a.id).or_insert((path, usize::MAX));
                if leg.0 != path || leg.1 == usize::MAX {
                    // A new path: to its first point, then along it (a path
                    // to a house begins at its door).
                    debug!("story companion {}: lead along {path} ({} points, waiting past {distance} m)", a.id, points.len());
                    *leg = (path, 0);
                }
                while leg.1 < points.len() && flat(points[leg.1]) < 0.6 {
                    leg.1 += 1;
                }
                if leg.1 >= points.len() {
                    led.push(a.id);
                    None
                } else if to_player.length() > distance {
                    None
                } else {
                    Some((points[leg.1], LEAD_SPEED))
                }
            }
            fc3_story::Companion::Wait => None,
        };
        let kind = match goal {
            Some((at, speed)) => {
                let to = (at - t.translation).with_y(0.0);
                let dir = to.normalize_or_zero();
                t.translation += dir * (speed * dt).min(to.length());
                if let Some(y) = nav.height_at(t.translation).or_else(|| heights.height_at(t.translation)) {
                    t.translation.y = y + HALF_HEIGHT;
                }
                if dir != Vec3::ZERO {
                    t.rotation = Quat::from_rotation_y((-dir.x).atan2(-dir.z));
                }
                if speed > 3.0 {
                    AnimKind::Run
                } else {
                    AnimKind::Walk
                }
            }
            None => {
                // Waiting for the player: turned to him.
                if matches!(order, fc3_story::Companion::Lead { .. }) && to_player.length() > 0.5 {
                    t.rotation = Quat::from_rotation_y((-to_player.x).atan2(-to_player.z));
                }
                AnimKind::Idle
            }
        };
        if let Some(mut r) = a.rig.and_then(|rig| anims.get_mut(rig).ok()) {
            if r.kind != kind && r.kind != AnimKind::Death {
                *r = AnimRequest { kind, rate: 1.0 };
            }
        }
    }
    for id in led {
        debug!("story companion {id}: led to its path's end");
        legs.remove(&id);
        host.story.companion_led(id);
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
        if a.dead || a.in_scene || a.seated || npc.awareness <= 0.35 || npc.last_known.is_none() {
            continue;
        }
        debug!(
            "story actor {}: alerted (awareness {:.2}, sees the player {}, by {:.0?} {:.0} m off), AI takes over; task {:?}, story action {:?}",
            a.id,
            npc.awareness,
            npc.sees_player,
            npc.last_known,
            npc.last_known.map_or(0.0, |k| k.distance(t.translation)),
            a.task.as_ref().map(|t| t.action),
            host.story.current_action(a.id)
        );
        commands.entity(entity).remove::<crate::npc::Scripted>();
        npc.post = t.translation - Vec3::Y * HALF_HEIGHT;
        npc.post_yaw = t.rotation.to_euler(EulerRot::YXZ).0;
        npc.yaw = npc.post_yaw;
        a.path.clear();
        restore_clips(&mut *a, &mut rigs);
        if a.task.take().is_some_and(|t| !t.own) {
            host.story.action_interrupted(a.id);
        }
    }
}

/// Characters stand at their places on the human idle until scripts or AI
/// move them.
fn actor_idle(
    actors: Query<&StoryActor, (Without<crate::wildlife::WildAnimal>, Or<(Without<crate::npc::Npc>, With<crate::npc::Scripted>)>)>,
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
        if a.in_scene || a.seated || a.task.as_ref().is_some_and(|t| !t.arrived) {
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

/// Health changes of the story's characters and the player: the story hears
/// them on the health counter (`CCounterEvent`), before any death.
fn actor_health(
    host: Option<NonSendMut<StoryHost>>,
    actors: Query<(Entity, &StoryActor, &Health), Changed<Health>>,
    players: Query<(Entity, &Health), (With<Player>, Changed<Health>)>,
    harm: Res<crate::combat::LastHarm>,
    mut seen: Local<HashMap<u64, f32>>,
) {
    let Some(mut host) = host else { return };
    let changes = actors.iter().map(|(e, a, h)| (e, a.id, h.current)).chain(players.iter().map(|(e, h)| (e, fc3_story::PLAYER, h.current)));
    for (e, id, now) in changes {
        let before = seen.insert(id, now);
        if let Some(before) = before.filter(|b| *b != now) {
            // What hurt it (an arrow names itself; a round, the gun in hand).
            let (source, emitter) = match harm.0.get(&e).filter(|_| now < before) {
                Some((_, crate::combat::Hit::Arrow(_))) => ("BulletImpact", Some("arrow")),
                Some((kind, _)) => (kind.source_type(), None),
                None => ("Generic", None),
            };
            host.story.health_changed(id, before, now.max(0.0), fc3_story::PLAYER, source, emitter);
        }
    }
}

/// A story character's body a script still watches (a VIP's, to be
/// photographed), or one carrying an item a script waits to see looted
/// until it is searched (JAS_200's boss and the role sheet): it doesn't
/// sink away with the other bodies.
#[derive(Component)]
pub struct KeptBody;

fn keep_watched_bodies(
    host: Option<NonSend<StoryHost>>,
    actors: Query<(Entity, &StoryActor, Has<KeptBody>, Option<&crate::pickups::OwnLoot>, Has<crate::pickups::Searched>)>,
    crafting: Option<Res<crate::crafting::CraftCatalog>>,
    mut commands: Commands,
) {
    let Some(host) = host else { return };
    // (Asked of the scripts only when there is a body to ask about.)
    let mut wanted: Option<Vec<u32>> = None;
    for (e, a, kept, own, searched) in &actors {
        let carries = |own: &crate::pickups::OwnLoot| {
            let wanted = wanted.get_or_insert_with(|| host.story.watched_loot_filters());
            crafting.as_ref().is_some_and(|c| own.gives(&c.groups, |id| c.item(id).map(|i| i.name_id), wanted))
        };
        let keep = a.dead && (host.story.watches_entity(a.id) || (!searched && own.is_some_and(carries)));
        if keep && !kept {
            commands.entity(e).insert(KeptBody);
        } else if !keep && kept {
            commands.entity(e).remove::<KeptBody>();
        }
    }
}

/// A character killed: the story hears of it (group `Die` events, as the
/// engine raises them for every group the victim is in). (Debug: what hurts
/// the characters who aren't fighters, captives and the like.)
fn actor_deaths(
    host: Option<NonSendMut<StoryHost>>,
    mut actors: Query<(Entity, &mut StoryActor, &Health, Has<StoryCreature>, Has<crate::pickups::OwnLoot>)>,
    mut hurts: MessageReader<crate::combat::DamageEvent>,
    fighters: Query<(), With<crate::npc::Npc>>,
    players: Query<(), With<Player>>,
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    for d in hurts.read() {
        if fighters.contains(d.target) {
            continue;
        }
        let Ok((_, a, h, _, _)) = actors.get(d.target) else { continue };
        let by = match d.instigator {
            Some(i) if players.contains(i) => "the player".to_string(),
            Some(i) => actors.get(i).map_or(format!("{i:?}"), |b| b.1.id.to_string()),
            None => "nothing".into(),
        };
        debug!("story actor {} hurt: {:.0} {:?} by {by} from {:.1?} ({:.0} health left)", a.id, d.amount, d.kind, d.source, h.current);
    }
    for (e, mut a, h, creature, own) in &mut actors {
        if h.dead && !a.dead {
            a.dead = true;
            let id = a.id;
            // An animal carrying its own loot is skinned for it (a hunt's
            // rare one: its skin).
            if creature && own {
                commands.entity(e).insert(crate::pickups::LootableBody { role: crate::npc::Role::Assault, rig: a.rig, skin: true });
            }
            if std::env::var("FC3_STORY_WAIT_DEBUG").is_ok() {
                info!("story actor death: {id}");
            }
            host.story.killed(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body is kept for an item scripts wait to see looted only when one
    /// of its own loot groups can give it: the group names the catalog's
    /// item, the scripts its name id.
    #[test]
    fn a_body_carrying_a_wanted_item_is_known_by_its_loot_groups() {
        use fc3_formats::progression::LootGroup;
        let groups =
            vec![LootGroup { id: 1021, items: vec![(997, 100.0)], ..Default::default() }, LootGroup { id: 8, items: vec![(5, 50.0)], ..Default::default() }];
        let name_of = |id: u32| match id {
            997 => Some(104540),
            5 => Some(20),
            _ => None,
        };
        let own = crate::pickups::OwnLoot(vec![(1021, 100.0)]);
        assert!(own.gives(&groups, name_of, &[104540]));
        assert!(!own.gives(&groups, name_of, &[997]));
        assert!(!own.gives(&groups, name_of, &[]));
        assert!(!own.gives(&groups, name_of, &[20]));
        assert!(!crate::pickups::OwnLoot(vec![(1021, 0.0)]).gives(&groups, name_of, &[104540]));
    }

    /// An event point puts its army's soldiers within reach on to the
    /// player (combat), or sends them to the point (suspicious); others it
    /// leaves be.
    #[test]
    fn an_ai_event_rouses_its_army_within_reach() {
        use crate::npc::{Faction, NpcState};
        let combat = AiEvent { at: Vec3::ZERO, army: 1, state: 3, reach: 150.0, left: 25.0, threat: None };
        let player = Vec3::new(10.0, 0.0, 0.0);
        assert!(combat.takes(Faction::Pirate) && combat.takes(Faction::Privateer) && !combat.takes(Faction::Rebel));
        assert_eq!(combat.order(Vec3::new(100.0, 0.0, 0.0), player), Some((NpcState::Search, player)));
        assert_eq!(combat.order(Vec3::new(200.0, 0.0, 0.0), player), None);
        let suspicious = AiEvent { state: 1, ..combat.clone() };
        assert_eq!(suspicious.order(Vec3::new(5.0, 0.0, 0.0), player), Some((NpcState::Suspicious, Vec3::ZERO)));
        assert_eq!(AiEvent { state: 0, ..combat }.order(Vec3::ZERO, player), None);
    }

    /// Ashore where the way toward the battlefield first leaves the sea;
    /// nowhere while the ground there isn't loaded, or when there's no way
    /// out of the water.
    #[test]
    fn reinforcements_come_ashore_toward_the_battlefield() {
        let sea = crate::sky::SEA_LEVEL;
        let beach = |p: Vec3| Some(if p.x < 10.0 { sea - 2.0 } else { sea + 1.0 });
        let at = landing(Vec3::new(0.0, sea, 0.0), Some(Vec3::new(50.0, sea, 0.0)), beach).expect("ashore");
        assert!((at - Vec3::new(10.0, sea + 1.0, 0.0)).length() < 1e-3, "{at}");
        // Already on land: there.
        assert_eq!(landing(Vec3::new(20.0, 0.0, 5.0), None, beach), Some(Vec3::new(20.0, sea + 1.0, 5.0)));
        // At sea with nowhere to head, or the ground not loaded.
        assert_eq!(landing(Vec3::ZERO, None, beach), None);
        assert_eq!(landing(Vec3::ZERO, Some(Vec3::new(50.0, 0.0, 0.0)), |_| None), None);
    }

    /// Relaxed mercs stand at their point, a dock's over the sea too; not
    /// those a way in brings, nor any called in on alert, nor at sea.
    #[test]
    fn relaxed_reinforcements_stand_at_their_posts() {
        let dock = Vec3::new(1.0, crate::sky::SEA_LEVEL + 0.085, 2.0);
        assert_eq!(at_post(true, false, dock), Some(dock));
        assert_eq!(at_post(true, true, dock), None);
        assert_eq!(at_post(false, false, dock), None);
        assert_eq!(at_post(true, false, dock - Vec3::Y * 3.0), None);
    }
}
