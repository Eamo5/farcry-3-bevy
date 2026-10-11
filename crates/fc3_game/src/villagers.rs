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
        app.add_message::<PlaceVillager>().add_systems(OnExit(crate::menu::GameState::Loading), load_spots).add_systems(
            Update,
            (place_villagers, spawn_villagers, despawn_villagers, execute, thank_player, villager_ai, spot_clips, villager_log)
                .chain()
                .before(crate::hud::HudSet)
                .run_if(in_state(crate::menu::GameState::Playing)),
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
    /// Going to the player who saved them (where he is), to thank him.
    Thank(Vec3),
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
    spawn_at(commands, chars, models, rng, zone, p, yaw, female);
}

/// Spawns a villager standing at `p` (feet), facing `yaw`.
#[allow(clippy::too_many_arguments)]
fn spawn_at(commands: &mut Commands, chars: &CharacterAssets, models: &Assets<Fc3Model>, rng: &mut Rng, zone: u64, p: Vec3, yaw: f32, female: bool) -> Entity {
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
        home: p,
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
    root
}

/// A civilian an encounter puts somewhere (dead: a body it leaves).
#[derive(Message, Clone, Copy)]
pub struct PlaceVillager {
    pub at: Vec3,
    pub yaw: f32,
    pub female: bool,
    pub dead: bool,
    /// Held by the encounter's pirates (a hostage, prisoner or victim).
    pub hostage: bool,
    /// The cash they pay the player for their rescue, per survivor (0:
    /// none).
    pub reward: u32,
    /// The encounter they belong to.
    pub group: u64,
}

/// A civilian of an encounter (by its id).
#[derive(Component)]
pub struct EncounterCivilian(pub u64);

/// An encounter's executioner (or one of its firing squad): once the
/// player comes within the script's radius of the encounter, he kills one
/// of its captives every interval while he lives.
#[derive(Component)]
pub struct Executioner {
    group: u64,
    at: Vec3,
    near: f32,
    interval: f32,
    /// The points he fires from (by each captive).
    points: Vec<Vec3>,
    timer: Option<f32>,
}

impl Executioner {
    pub fn new(group: u64, at: Vec3, x: &fc3_formats::encounters::Execution, points: Vec<Vec3>) -> Executioner {
        Executioner { group, at, near: x.near, interval: x.interval, points, timer: None }
    }
}

/// An executioner this near (m) his firing point fires.
const AT_POINT: f32 = 1.6;

/// Executioners at work: a shot (the flash, the rifle) into the nearest
/// captive of theirs still alive. (Their own comrades take it for what it
/// is: no alarm.)
#[allow(clippy::too_many_arguments)]
fn execute(
    time: Res<Time>,
    mut shooters: Query<(Entity, &mut Executioner, &mut crate::npc::Npc, &Transform, &Health)>,
    victims: Query<(Entity, &EncounterCivilian, &Transform, &Health), With<Hostage>>,
    players: Query<&Transform, With<Player>>,
    mut damage: MessageWriter<crate::combat::DamageEvent>,
    mut loops: MessageWriter<crate::sound::LoopSfx>,
    mut particles: ResMut<crate::particles::Particles>,
    mut rng: ResMut<Rng>,
) {
    let Ok(pt) = players.single() else { return };
    let dt = time.delta_secs();
    for (e, mut x, mut npc, t, h) in &mut shooters {
        if h.dead {
            continue;
        }
        let Some(timer) = x.timer.as_mut() else {
            // Waiting for the player to come near. (FC3_EXECUTION_TEST: from
            // twice as far, as encounters appear 35 m off or more.)
            let near = if std::env::var_os("FC3_EXECUTION_TEST").is_some() { x.near * 2.0 } else { x.near };
            if pt.translation.distance(x.at) < near {
                x.timer = Some(x.interval);
                if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
                    info!("encounter: the execution at {:.0?} begins", x.at);
                }
            }
            continue;
        };
        *timer -= dt;
        let pos = t.translation;
        let group = x.group;
        let Some((victim, at)) = victims
            .iter()
            .filter(|(_, c, _, vh)| c.0 == group && !vh.dead)
            .map(|(v, _, vt, _)| (v, vt.translation))
            .min_by(|a, b| a.1.distance(pos).total_cmp(&b.1.distance(pos)))
        else {
            continue;
        };
        // To the point by them, facing them (left to himself: in a fight he
        // fires from where he stands).
        let point = x.points.iter().copied().min_by(|a, b| a.distance(at).total_cmp(&b.distance(at)));
        if let Some(p) = point.filter(|_| npc.state == crate::npc::NpcState::Idle) {
            npc.post = p;
            npc.at_spot = true;
            npc.post_yaw = (-(at.x - p.x)).atan2(-(at.z - p.z));
            if Vec2::new(pos.x - p.x, pos.z - p.z).length() > AT_POINT {
                continue;
            }
        }
        if x.timer.is_some_and(|t| t > 0.0) {
            continue;
        }
        x.timer = Some(x.interval);
        // Turned on them, a burst into them.
        let to = at - pos;
        npc.yaw = (-to.x).atan2(-to.z);
        let dir = to.normalize_or(Vec3::NEG_Z);
        let muzzle = pos + Vec3::Y * 0.35 + Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero() * 0.7;
        crate::particles::fx::muzzle_flash(&mut particles, &mut rng, muzzle, dir, 1.1);
        loops.write(crate::sound::LoopSfx { key: e.to_bits(), event: crate::sound::ev::RIFLE_3P, at: Some(muzzle), volume: 1.0, ..default() });
        damage.write(crate::combat::DamageEvent {
            target: victim,
            amount: 10_000.0,
            kind: crate::combat::DamageKind::Bullet,
            source: Some(muzzle),
            instigator: Some(e),
            headshot: false,
            hit: crate::combat::Hit::Whole,
        });
        if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
            info!("encounter: a captive executed at {at:.0?}, from {pos:.1?}");
        }
    }
}

/// A civilian an encounter rewards the rescue of: once no armed enemy is
/// near, they call the player over, and pay when he comes to them.
#[derive(Component)]
pub struct Grateful {
    cash: u32,
    group: u64,
    called: bool,
}

/// No living enemy this near (m) and the player this near: a rescued
/// civilian goes to him; this near him, they pay.
const RESCUED: f32 = 50.0;
const THANK_REACH: f32 = 45.0;
const THANK_NEAR: f32 = 2.2;

/// Rescued civilians go to the player, call him over, and pay on the
/// interact key (the encounter library's reward: a thank-you, then the
/// cash).
#[allow(clippy::too_many_arguments)]
fn thank_player(
    mut villagers: Query<(Entity, &mut Villager, &Transform, &Health, &mut Grateful), Without<Hostage>>,
    civilians: Query<(&EncounterCivilian, &Health)>,
    enemies: Query<(&Transform, &Health, &crate::npc::Npc), Without<Villager>>,
    players: Query<&Transform, (With<Player>, Without<Villager>)>,
    mut actions: ResMut<crate::controls::ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<crate::hud::HudState>,
    mut progress: ResMut<crate::progression::Progress>,
    barks: Res<crate::barks::BarkTable>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
) {
    let Ok(pt) = players.single() else { return };
    let me = pt.translation;
    for (e, mut v, t, h, mut g) in &mut villagers {
        let pos = t.translation;
        if h.dead || matches!(v.state, State::Flee | State::Dead) {
            continue;
        }
        let threat = enemies.iter().any(|(c, ch, n)| n.faction.hostile_to_player() && !ch.dead && c.translation.distance(pos) < RESCUED);
        if threat || pos.distance(me) > THANK_REACH {
            if matches!(v.state, State::Thank(_)) {
                v.state = State::Idle;
                v.timer = 1.0;
            }
            continue;
        }
        v.state = State::Thank(me);
        // Their voice: an encounter civilian's (by the entity, for a steady one).
        let voice = format!("outsider_nh_{}_0{}", if v.female { "female" } else { "male" }, e.to_bits() % 3 + 1);
        let say = |stimulus: &str, rng: &mut Rng, sfx: &mut MessageWriter<crate::sound::PlaySfx>| {
            if let Some((sound, _)) = barks.line(&voice, stimulus, rng.f32()) {
                sfx.write(crate::sound::PlaySfx { event: sound, at: Some(pos + Vec3::Y * 0.7), volume: 1.0 });
            }
        };
        if !g.called {
            g.called = true;
            say("inform_callplayer", &mut rng, &mut sfx);
        }
        if pos.distance(me) > THANK_NEAR + 0.6 {
            continue;
        }
        if hud.prompt.is_none() {
            hud.prompt = Some(format!("{}  take the reward", actions.label(crate::controls::Action::Interact, &settings)));
        }
        // FC3_REWARD_TEST (with FC3_CLEAR_TEST): take it when they come.
        let test = std::env::var_os("FC3_REWARD_TEST").is_some();
        if actions.just_pressed(crate::controls::Action::Interact) || test {
            actions.consume(crate::controls::Action::Interact);
            if test {
                info!("reward-test: a rescued civilian pays ${} a survivor (had ${})", g.cash, progress.money);
            }
            // (For each of the encounter's civilians still alive.)
            let survivors = civilians.iter().filter(|(c, ch)| c.0 == g.group && !ch.dead).count().max(1) as u32;
            let paid = progress.add_money(g.cash * survivors);
            hud.notify(format!("Rescued: ${paid}"));
            say("salutation", &mut rng, &mut sfx);
            commands.entity(e).remove::<Grateful>();
            v.state = State::Idle;
            v.timer = rng.range(4.0, 8.0);
        }
    }
}

/// A civilian held by armed men: they kneel, cowering, while any are near
/// and alive, then get up and run.
#[derive(Component)]
pub struct Hostage;

/// Captors this near (m) hold a hostage; their own, even this far.
const HOSTAGE_GUARD: f32 = 25.0;
const HOSTAGE_HOLD: f32 = 120.0;

/// Encounters' civilians belong to no village zone.
const NO_ZONE: u64 = u64::MAX;

/// Puts encounters' civilians where they stand once the villagers' looks are
/// in (given up after half a minute).
fn place_villagers(
    time: Res<Time>,
    mut requests: MessageReader<PlaceVillager>,
    mut pending: Local<Vec<(PlaceVillager, f32)>>,
    chars: Option<Res<CharacterAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    pending.extend(requests.read().map(|r| (*r, now)));
    let Some(chars) = chars.filter(|c| c.ready(&server)) else {
        pending.retain(|(_, since)| now - since < 30.0);
        return;
    };
    for (r, _) in pending.drain(..) {
        let e = spawn_at(&mut commands, &chars, &models, &mut rng, NO_ZONE, r.at, r.yaw, r.female);
        if r.dead {
            commands.entity(e).insert(crate::combat::DieOnSpawn);
        }
        if r.hostage {
            commands.entity(e).insert(Hostage);
        }
        commands.entity(e).insert(EncounterCivilian(r.group));
        if r.reward > 0 && !r.dead {
            commands.entity(e).insert(Grateful { cash: r.reward, group: r.group, called: false });
        }
    }
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
    mut villagers: Query<(Entity, &mut Villager, &mut Transform, &Health, Has<Hostage>, Has<Grateful>, Option<&EncounterCivilian>)>,
    mut anims: Query<&mut AnimRequest>,
    mut commands: Commands,
    spots: Option<ResMut<VillagerSpots>>,
    captors: Query<(&Transform, &Health, &crate::npc::Npc), Without<Villager>>,
) {
    let dt = time.delta_secs();
    let Some(mut spots) = spots else { return };
    spots.taken.retain(|_, e| villagers.contains(*e));
    let noises: Vec<crate::npc::Noise> = noises.read().copied().collect();
    let hurt: Vec<(Entity, Option<Vec3>)> = damage.read().map(|d| (d.target, d.source)).collect();
    for (e, mut v, mut t, h, held, grateful, group) in &mut villagers {
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
        // A hostage kneels while their captors are near; once they're dead or
        // gone, up and away from where they were.
        if held {
            // (Their own captors hold them while any live, wherever they have
            // gone to look; others only while near.)
            let own = |n: &crate::npc::Npc| group.is_some_and(|g| g.0 == n.marker_id);
            let near = |(c, ch, n): &(&Transform, &Health, &crate::npc::Npc)| {
                n.faction.hostile_to_player()
                    && !ch.dead
                    && (own(n) && c.translation.distance(pos) < HOSTAGE_HOLD || c.translation.distance(pos) < HOSTAGE_GUARD)
            };
            if captors.iter().any(|c| near(&c)) {
                v.state = State::Cower;
                v.timer = 1.0;
                t.rotation = Quat::from_rotation_y(v.yaw);
                if let Some(mut a) = v.rig.and_then(|r| anims.get_mut(r).ok()) {
                    *a = AnimRequest { kind: AnimKind::Cover, rate: 1.0 };
                }
                continue;
            }
            commands.entity(e).remove::<Hostage>();
            let last = captors
                .iter()
                .filter(|(c, _, n)| n.faction.hostile_to_player() && c.translation.distance(pos) < HOSTAGE_GUARD * 2.0)
                .map(|(c, _, _)| c.translation)
                .next();
            // (One the encounter rewards stays, to thank the player.)
            if grateful {
                v.state = State::Idle;
                v.timer = 2.0;
            } else {
                v.state = State::Flee;
                v.timer = FLEE_TIME;
                v.fear = last.unwrap_or(pos - Vec3::new(-v.yaw.sin(), 0.0, -v.yaw.cos()));
            }
            if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
                info!("encounter: a hostage at {pos:.0?} is free");
            }
        }
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
            // To the player, then facing him.
            State::Thank(to) => {
                let flat = Vec3::new(to.x - pos.x, 0.0, to.z - pos.z);
                if flat.length() > THANK_NEAR {
                    let far = flat.length() > 12.0;
                    step = flat.normalize() * if far { RUN_SPEED } else { WALK_SPEED } * dt;
                    (if far { AnimKind::Run } else { AnimKind::Walk }, 1.0)
                } else {
                    v.yaw = (-flat.x).atan2(-flat.z);
                    (AnimKind::Idle, 1.0)
                }
            }
        };
        if step != Vec3::ZERO {
            // Stay on walkable ground (the mesh, where it's loaded); blocked:
            // turn aside, or give up on the walk.
            let walkable = |next: Vec3| {
                !nav.ready()
                    || nav.height_at(next - Vec3::Y * HALF_HEIGHT).is_some()
                    || heights.height_at(next).is_some_and(|g| (g - (pos.y - HALF_HEIGHT)).abs() < 0.6)
            };
            // (Going to the player, round whatever is in the way.)
            if matches!(v.state, State::Thank(_)) && !walkable(pos + step) {
                if let Some(aside) = [0.6f32, -0.6, 1.2, -1.2, 1.8, -1.8].into_iter().map(|a| Quat::from_rotation_y(a) * step).find(|s| walkable(pos + *s)) {
                    step = aside;
                }
            }
            let next = pos + step;
            let on_mesh = walkable(next);
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
