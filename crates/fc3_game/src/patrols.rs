//! Road patrols: enemy crews in technicals driving the world's vehicle patrol
//! routes (see `fc3_formats::patrols`). The driver steers the physics vehicle
//! along the route; a gunner stands in the back. When the crew spots or hears
//! the player they pull up and get out to fight, leaving the vehicle behind.
//! Where an outpost nearby has been liberated, rebels drive the route instead
//! and pay the player no mind. Routes that run along a river (most of their
//! points over deep water) are boat routes: a Zodiac takes them.
//!
//! Boat patrols: at the world's boat-patrol markers (`CampEncounter/*/
//! BoatPatrol`, `PatrolBoat`) a Zodiac with a man on its gun circles the open
//! water round about. Alerted, it keeps circling, slower, while the gunner
//! fires.

use avian3d::prelude::*;
use bevy::prelude::*;
use fc3_formats::patrols::PatrolRoute;

use crate::cache_assets::Fc3Model;
use crate::combat::{DamageEvent, Health};
use crate::npc::{Faction, Npc, NpcState, Riding, Role, SpawnEnemy};
use crate::player::{Player, HALF_HEIGHT};
use crate::vehicles::{Vehicle, VehicleAssets};

pub struct PatrolsPlugin;

impl Plugin for PatrolsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Routes>()
            .add_message::<Convoy>()
            .add_systems(
                Update,
                (load_routes.run_if(resource_exists::<crate::data::CacheInfo>), spawn_patrols, spawn_boat_patrols, send_convoys, (spot, drive_patrols, dismount).chain(), end_patrols, patrol_log)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            )
            .add_systems(PostUpdate, carry_crew.before(TransformSystems::Propagate));
    }
}

/// Patrols on the road at once.
const MAX_PATROLS: usize = 2;
/// A route is used when some part of it is this close to the player.
const ROUTE_RANGE: f32 = 320.0;
/// Patrols start this far from the player (out of sight, not out of reach)...
const START_MIN: f32 = 90.0;
const START_MAX: f32 = 260.0;
/// ...and are removed beyond this.
const DROP_RANGE: f32 = 450.0;
/// Cruising speed (m/s) and how close a waypoint counts as reached.
const CRUISE: f32 = 11.0;
const REACHED: f32 = 9.0;

#[derive(Resource, Default)]
struct Routes {
    loaded: bool,
    list: Vec<PatrolRoute>,
    /// The vehicle driving each route, if any.
    active: Vec<Option<Entity>>,
    /// Seconds before a route is used again.
    cooldown: Vec<f32>,
}

/// A vehicle driving a patrol route.
#[derive(Component)]
pub struct PatrolDriver {
    route: usize,
    next: usize,
    /// +1 along the route, -1 back (routes that don't loop turn around).
    step: i32,
    /// 0..1: how sure the crew is that someone is there; 1 stops the vehicle.
    alert: f32,
    stopping: bool,
    /// Reversing out of a jam (seconds left).
    reverse: f32,
    stuck: f32,
    /// Seconds until the next look for the player.
    look: f32,
    /// Reinforcements: where they are headed (they get out within the radius,
    /// or wherever they get stuck), instead of following the route.
    goal: Option<(Vec3, f32)>,
    jams: u32,
    /// A rebel crew: the player is a friend.
    friendly: bool,
}

/// Reinforcements for an outpost: a technical with four men drives in from
/// the nearest road and they get out to fight.
#[derive(Message, Clone, Copy)]
pub struct Convoy {
    pub to: Vec3,
    pub radius: f32,
    pub garrison: usize,
}

/// Whether a straight drive from `a` to `b` stays on dry land of a slope a
/// technical can climb.
fn drivable(heights: &crate::terrain::Heightfield, a: Vec3, b: Vec3) -> bool {
    let steps = (a.distance(b) / 8.0).ceil().max(1.0) as usize;
    let mut last: Option<f32> = None;
    for i in 0..=steps {
        let p = a.lerp(b, i as f32 / steps as f32);
        let Some(h) = heights.height_at(p) else { return false };
        if h < crate::water::surface(p, Some(h)) + 0.5 {
            return false;
        }
        if let Some(prev) = last {
            if ((h - prev) / 8.0).abs() > 0.4 {
                return false;
            }
        }
        last = Some(h);
    }
    true
}

/// How many come, and how fast reinforcements drive.
const CONVOY_CREW: usize = 4;
const CONVOY_SPEED: f32 = 15.0;

#[allow(clippy::too_many_arguments)]
fn send_convoys(
    mut convoys: MessageReader<Convoy>,
    routes: Res<Routes>,
    assets: Option<Res<VehicleAssets>>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<&Transform, With<Player>>,
    mut spawns: MessageWriter<SpawnEnemy>,
    mut commands: Commands,
) {
    let Ok(pt) = players.single() else { return };
    for c in convoys.read() {
        let faction = if c.to.z > 600.0 { Faction::Privateer } else { Faction::Pirate };
        // A road point out of sight of the player, a short drive away; else
        // open ground on a drivable line in (away from the player first).
        let road = routes
            .list
            .iter()
            .flat_map(|r| r.points.iter().map(|p| Vec3::from_array(*p)))
            .filter(|p| (70.0..220.0).contains(&p.distance(c.to)) && p.distance(pt.translation) > 60.0 && heights.height_at(*p).is_some())
            .min_by(|a, b| a.distance(c.to).total_cmp(&b.distance(c.to)));
        let away = Vec3::new(c.to.x - pt.translation.x, 0.0, c.to.z - pt.translation.z).normalize_or(Vec3::X);
        let start = road.or_else(|| {
            (0..8).find_map(|k| {
                let dir = Quat::from_rotation_y((k as f32 * 0.5).ceil() * if k % 2 == 1 { 0.785 } else { -0.785 }) * away;
                let from = c.to + dir * (c.radius + 90.0);
                drivable(&heights, from, c.to).then_some(from).filter(|p| p.distance(pt.translation) > 60.0)
            })
        });
        let vehicle = assets.as_ref().and_then(|a| crate::vehicles::def_named("Technical").and_then(|def| models.get(&a.models[def]).map(|m| (def, m))));
        let (Some(a), Some((def, model))) = (start, vehicle) else {
            // On foot from the side away from the player, as a fallback.
            let base = c.to + away * (c.radius + 25.0);
            for k in 0..CONVOY_CREW {
                let side = Vec3::new(-away.z, 0.0, away.x) * (k as f32 - 1.5) * 3.0;
                spawns.write(SpawnEnemy { at: base + side, hunting: Some(pt.translation), garrison: Some(c.garrison), faction, role: None, ride: None });
            }
            continue;
        };
        let ground = on_road(&heights, a);
        let heading = Vec3::new(c.to.x - a.x, 0.0, c.to.z - a.z).normalize_or(Vec3::NEG_Z);
        let at = Vec3::new(a.x, ground + 1.0, a.z);
        let vehicle = crate::vehicles::build_vehicle(&mut commands, def, model, at, Quat::from_rotation_arc(Vec3::NEG_Z, heading), 0x5100_0000_0000_0000 | c.garrison as u64);
        if let Some(a) = assets.as_ref() {
            crate::vehicles::arm_vehicle(&mut commands, vehicle, model, a, &models);
        }
        commands.entity(vehicle).insert(PatrolDriver {
            route: usize::MAX,
            next: 0,
            step: 1,
            alert: 0.0,
            stopping: false,
            reverse: 0.0,
            stuck: 0.0,
            look: 0.5,
            goal: Some((c.to, c.radius + 12.0)),
            jams: 0,
            friendly: false,
        });
        let (min, max) = (model.aabb_min, model.aabb_max);
        let size = max - min;
        let seats = [
            (Vec3::new(min.x + size.x * 0.3, min.y + 0.6, min.z + size.z * 0.45), false),
            (Vec3::new(max.x - size.x * 0.3, min.y + 0.6, min.z + size.z * 0.45), false),
            (Vec3::new((min.x + max.x) * 0.5, min.y + size.y * 0.42, max.z - size.z * 0.22), true),
            (Vec3::new((min.x + max.x) * 0.5, min.y + size.y * 0.42, max.z - size.z * 0.4), true),
        ];
        for (seat, visible) in seats.into_iter().take(CONVOY_CREW) {
            spawns.write(SpawnEnemy { at, hunting: Some(pt.translation), garrison: Some(c.garrison), faction, role: None, ride: Some(Riding { vehicle, seat, visible }) });
        }
        info!("convoy to outpost {} from {at:.0?}", c.garrison);
    }
}

fn load_routes(info: Res<crate::data::CacheInfo>, mut routes: ResMut<Routes>) {
    if routes.loaded {
        return;
    }
    routes.loaded = true;
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::PATROLS_REL)).unwrap_or_default();
    routes.list = fc3_formats::patrols::from_text(&text);
    routes.active = vec![None; routes.list.len()];
    routes.cooldown = vec![0.0; routes.list.len()];
    info!("{} patrol routes", routes.list.len());
}

fn point(r: &PatrolRoute, i: usize) -> Vec3 {
    Vec3::from_array(r.points[i])
}

#[allow(clippy::too_many_arguments)]
fn spawn_patrols(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut routes: ResMut<Routes>,
    assets: Option<Res<VehicleAssets>>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<crate::terrain::Heightfield>,
    players: Query<&Transform, With<Player>>,
    vehicles: Query<(), With<PatrolDriver>>,
    outposts: Res<crate::outposts::Outposts>,
    mut spawns: MessageWriter<SpawnEnemy>,
    mut commands: Commands,
) {
    if std::env::var("FC3_PEACEFUL").is_ok() && !args.patrol_test {
        return;
    }
    let dt = time.delta_secs();
    for c in &mut routes.cooldown {
        *c -= dt;
    }
    // Forget routes whose vehicle is gone.
    let Routes { active, cooldown, .. } = &mut *routes;
    for (a, c) in active.iter_mut().zip(cooldown.iter_mut()) {
        if a.is_some_and(|e| !vehicles.contains(e)) {
            *a = None;
            *c = c.max(90.0);
        }
    }
    if routes.active.iter().flatten().count() >= MAX_PATROLS {
        return;
    }
    let Ok(pt) = players.single() else { return };
    let Some(assets) = assets else { return };
    let (Some(car), Some(boat)) = (crate::vehicles::def_named("Technical"), crate::vehicles::def_named("Boat")) else { return };
    let (Some(car_model), Some(boat_model)) = (models.get(&assets.models[car]), models.get(&assets.models[boat])) else { return };
    // Terrain under the route must be streamed in.
    for (ri, r) in routes.list.iter().enumerate() {
        if routes.active[ri].is_some() || routes.cooldown[ri] > 0.0 {
            continue;
        }
        let near = r.points.iter().map(|p| Vec3::from_array(*p).distance(pt.translation)).fold(f32::MAX, f32::min);
        if near > ROUTE_RANGE {
            continue;
        }
        let forced = args.patrol_test;
        let afloat = water_route(&heights, r);
        let (def, model) = if afloat { (boat, boat_model) } else { (car, car_model) };
        // Somewhere it belongs (dry ground for a car, deep water for a boat)
        // if the route has some in range, else anywhere on it.
        let pick = |fits: bool| {
            (0..r.points.len()).find(|&i| {
                let p = point(r, i);
                let d = p.distance(pt.translation);
                (if forced { (25.0..START_MAX).contains(&d) } else { (START_MIN..START_MAX).contains(&d) })
                    && heights.height_at(p).is_some()
                    && (!fits || if afloat { deep(&heights, p) } else { dry_start(&heights, p) })
            })
        };
        let Some(start) = pick(true).or_else(|| pick(false)) else {
            continue;
        };
        let n = r.points.len();
        let next = (start + 1) % n;
        let (a, b) = (point(r, start), point(r, if r.looped || start + 1 < n { next } else { start - 1 }));
        let heading = Vec3::new(b.x - a.x, 0.0, b.z - a.z).normalize_or(Vec3::NEG_Z);
        let rotation = Quat::from_rotation_arc(Vec3::NEG_Z, heading);
        let at = if afloat { Vec3::new(a.x, crate::water::surface_at(a, &heights) + 0.4, a.z) } else { Vec3::new(a.x, on_road(&heights, a) + 1.0, a.z) };
        let vehicle = crate::vehicles::build_vehicle(&mut commands, def, model, at, rotation, 0x5000_0000_0000_0000 | ri as u64);
        let armed = crate::vehicles::arm_vehicle(&mut commands, vehicle, model, &assets, &models);
        let step = if r.looped || start + 1 < n { 1 } else { -1 };
        let first = if step > 0 { next } else { start - 1 };
        // Pirates hold the north island, privateers the south; rebels the
        // country round liberated outposts.
        let friendly = outposts.rebel_country(a);
        let faction = if friendly {
            Faction::Rebel
        } else if a.z > 600.0 {
            Faction::Privateer
        } else {
            Faction::Pirate
        };
        commands.entity(vehicle).insert(PatrolDriver { route: ri, next: first, step, alert: 0.0, stopping: false, reverse: 0.0, stuck: 0.0, look: 0.5, goal: None, jams: 0, friendly });
        let (min, max) = (model.aabb_min, model.aabb_max);
        let size = max - min;
        let (driver, gunner) = if afloat {
            (Vec3::new((min.x + max.x) * 0.5, min.y + 0.5, max.z - size.z * 0.2), Vec3::new((min.x + max.x) * 0.5, min.y + 0.5, min.z + size.z * 0.3))
        } else {
            (Vec3::new(min.x + size.x * 0.3, min.y + 0.6, min.z + size.z * 0.45), Vec3::new((min.x + max.x) * 0.5, min.y + size.y * 0.42, max.z - size.z * 0.22))
        };
        for (seat, visible, role) in [(driver, false, Role::Assault), (gunner, armed || !afloat, Role::Assault)] {
            spawns.write(SpawnEnemy { at, hunting: None, garrison: None, faction, role: Some(role), ride: Some(Riding { vehicle, seat, visible }) });
        }
        routes.active[ri] = Some(vehicle);
        info!("patrol on route {ri} ({faction:?}, {}) from {at:.0?}", if afloat { "boat" } else { "technical" });
        return;
    }
}

/// Whether a route runs on water: most of its points (where the terrain has
/// loaded) over water deep enough for a boat.
fn water_route(heights: &crate::terrain::Heightfield, r: &PatrolRoute) -> bool {
    let (mut wet, mut known) = (0, 0);
    for p in r.points.iter().map(|p| Vec3::from_array(*p)) {
        if heights.height_at(p).is_some() {
            known += 1;
            wet += deep(heights, p) as usize;
        }
    }
    known > 0 && wet * 2 > known
}

/// Water at least 1.5 m deep at `p`.
fn deep(heights: &crate::terrain::Heightfield, p: Vec3) -> bool {
    heights.height_at(p).is_some_and(|g| crate::water::surface(p, Some(g)) > g + 1.5)
}

/// A route point to set a patrol down at: on the ground (not a bridge, whose
/// collision may not have loaded yet) and out of the water.
fn dry_start(heights: &crate::terrain::Heightfield, p: Vec3) -> bool {
    heights.height_at(p).is_some_and(|g| (p.y - g).abs() < 1.5 && crate::water::surface(p, Some(g)) < g + 0.3)
}

/// Where a vehicle goes down at a route point: the road's own height where
/// that's a little above the ground (a bridge deck), else the ground.
fn on_road(heights: &crate::terrain::Heightfield, a: Vec3) -> f32 {
    let ground = heights.height_at(a).unwrap_or(a.y);
    if (0.0..6.0).contains(&(a.y - ground)) { a.y } else { ground }
}

/// Boat patrols at once, how near a marker must be to put one to sea, the
/// circle's reach over water, the depth it keeps to, and the speed once alerted.
const MAX_BOATS: usize = 2;
const BOAT_RANGE: f32 = 380.0;
const BOAT_REACH: f32 = 160.0;
const BOAT_DEPTH: f32 = 2.0;
const BOAT_ALERT_SPEED: f32 = 5.0;
/// Boat patrol vehicles carry spawn ids from here (marker index below).
const BOAT_SPAWN: u64 = 0x5100_0000_0000_0000;

fn boat_marker(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("campencounter/") && (n.contains("boatpatrol") || n.contains("patrolboat"))
}

/// A loop over open water round `centre`: on each of 12 bearings, as far out
/// (up to `BOAT_REACH`) as the water stays `BOAT_DEPTH` deep all the way, the
/// loop passing through 70% of that. None where there isn't room.
fn water_loop(heights: &crate::terrain::Heightfield, centre: Vec3) -> Option<PatrolRoute> {
    // The sea, or a river or lake.
    let sea = crate::water::surface_at(centre, heights);
    let deep = |p: Vec3| heights.height_at(p).is_some_and(|h| h < sea - BOAT_DEPTH && crate::water::plane_level(p) == crate::water::plane_level(centre));
    if !deep(centre) {
        return None;
    }
    let mut points = Vec::new();
    for k in 0..12 {
        let a = k as f32 / 12.0 * std::f32::consts::TAU;
        let dir = Vec3::new(a.cos(), 0.0, a.sin());
        let mut reach = 0.0;
        let mut r = 6.0;
        while r <= BOAT_REACH && deep(centre + dir * r) {
            reach = r;
            r += 6.0;
        }
        if reach >= 24.0 {
            let p = centre + dir * reach * 0.7;
            points.push([p.x, sea, p.z]);
        }
    }
    (points.len() >= 4).then_some(PatrolRoute { points, looped: true })
}

/// Puts a Zodiac and its crew on the water at boat-patrol markers near the
/// player (not too near) in enemy country, circling about them.
#[allow(clippy::too_many_arguments)]
fn spawn_boat_patrols(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut routes: ResMut<Routes>,
    mut boats: Local<std::collections::HashMap<u64, (usize, f32)>>,
    assets: Option<Res<VehicleAssets>>,
    models: Res<Assets<Fc3Model>>,
    heights: Res<crate::terrain::Heightfield>,
    sectors: Res<Assets<crate::cache_assets::Fc3Sector>>,
    loaded: Query<&crate::objects::SectorObjects>,
    players: Query<&Transform, With<Player>>,
    vehicles: Query<&Vehicle, With<PatrolDriver>>,
    outposts: Res<crate::outposts::Outposts>,
    mut spawns: MessageWriter<SpawnEnemy>,
    mut commands: Commands,
) {
    if std::env::var("FC3_PEACEFUL").is_ok() && !args.patrol_test {
        return;
    }
    let dt = time.delta_secs();
    for (_, c) in boats.values_mut() {
        *c -= dt;
    }
    let afloat = vehicles.iter().filter(|v| v.kind() == crate::vehicles::Kind::Boat).count();
    if afloat >= MAX_BOATS {
        return;
    }
    let (Ok(pt), Some(assets)) = (players.single(), assets) else { return };
    let Some(def) = crate::vehicles::def_named("Boat") else { return };
    let Some(model) = models.get(&assets.models[def]) else { return };
    for so in &loaded {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        for e in sector.entities.iter().filter(|e| boat_marker(&e.name)) {
            let centre = Vec3::from_array(e.translation);
            let d = Vec2::new(centre.x - pt.translation.x, centre.z - pt.translation.z).length();
            // None in country the rebels hold.
            if d > BOAT_RANGE || (d < 60.0 && !args.patrol_test) || outposts.rebel_country(centre) {
                continue;
            }
            // One boat per marker at a time, and a while before the next.
            let ri = match boats.get(&e.id) {
                Some(&(ri, cooldown)) => {
                    if cooldown > 0.0 || routes.active.get(ri).is_some_and(|a| a.is_some()) {
                        continue;
                    }
                    ri
                }
                None => {
                    let Some(route) = water_loop(&heights, centre) else {
                        boats.insert(e.id, (usize::MAX, 600.0));
                        continue;
                    };
                    routes.list.push(route);
                    routes.active.push(None);
                    routes.cooldown.push(0.0);
                    routes.list.len() - 1
                }
            };
            if ri == usize::MAX {
                continue;
            }
            boats.insert(e.id, (ri, 240.0));
            let r = &routes.list[ri];
            let (a, b, waypoints) = (point(r, 0), point(r, 1), r.points.len());
            let heading = Vec3::new(b.x - a.x, 0.0, b.z - a.z).normalize_or(Vec3::NEG_Z);
            let at = Vec3::new(a.x, a.y + 0.4, a.z);
            let vehicle = crate::vehicles::build_vehicle(&mut commands, def, model, at, Quat::from_rotation_arc(Vec3::NEG_Z, heading), BOAT_SPAWN | ri as u64);
            let armed = crate::vehicles::arm_vehicle(&mut commands, vehicle, model, &assets, &models);
            commands.entity(vehicle).insert(PatrolDriver { route: ri, next: 1, step: 1, alert: 0.0, stopping: false, reverse: 0.0, stuck: 0.0, look: 0.5, goal: None, jams: 0, friendly: false });
            let faction = if a.z > 600.0 { Faction::Privateer } else { Faction::Pirate };
            let (min, max) = (model.aabb_min, model.aabb_max);
            let size = max - min;
            let driver = Vec3::new((min.x + max.x) * 0.5, min.y + 0.5, max.z - size.z * 0.2);
            let gunner = Vec3::new((min.x + max.x) * 0.5, min.y + 0.5, min.z + size.z * 0.3);
            for (seat, visible) in [(driver, false), (gunner, armed)] {
                spawns.write(SpawnEnemy { at, hunting: None, garrison: None, faction, role: Some(Role::Assault), ride: Some(Riding { vehicle, seat, visible }) });
            }
            routes.active[ri] = Some(vehicle);
            info!("boat patrol at {centre:.0?} ({faction:?}, {waypoints} waypoints, armed {armed})");
            return;
        }
    }
}

/// Puts riders at their seats, facing forward (gunners turn to face trouble).
pub fn carry_crew(vehicles: Query<(&Transform, &PatrolDriver), Without<Riding>>, mut crew: Query<(&Riding, &mut Transform, &mut Npc), Without<PatrolDriver>>) {
    for (ride, mut t, mut npc) in &mut crew {
        let Ok((vt, d)) = vehicles.get(ride.vehicle) else { continue };
        t.translation = vt.transform_point(ride.seat) + Vec3::Y * HALF_HEIGHT;
        let (yaw, _, _) = vt.rotation.to_euler(EulerRot::YXZ);
        let look = if d.alert > 0.3 { npc.yaw } else { yaw };
        npc.yaw = look;
        t.rotation = Quat::from_rotation_y(look);
    }
}

/// The crew watches (and listens) for the player while driving.
#[allow(clippy::too_many_arguments)]
fn spot(
    time: Res<Time>,
    spatial: SpatialQuery,
    mut noises: MessageReader<crate::npc::Noise>,
    mut damage: MessageReader<DamageEvent>,
    players: Query<(Entity, &Transform, &Player, &Health)>,
    mut vehicles: Query<(Entity, &Transform, &Vehicle, &mut PatrolDriver)>,
    mut crew: Query<(&Riding, &mut Npc)>,
) {
    let dt = time.delta_secs();
    let Ok((pe, pt, player, ph)) = players.single() else { return };
    let noises: Vec<crate::npc::Noise> = noises.read().copied().collect();
    let hurt: Vec<Entity> = damage.read().map(|d| d.target).collect();
    for (ve, vt, v, mut d) in &mut vehicles {
        if d.friendly {
            continue;
        }
        // Shot at, or a crewman hit: everyone knows.
        if hurt.contains(&ve) || crew.iter().any(|(r, _)| r.vehicle == ve) && hurt.iter().any(|h| crew.get(*h).is_ok_and(|(r, _)| r.vehicle == ve)) {
            d.alert = 1.0;
        }
        if noises.iter().any(|n| n.at.distance(vt.translation) < n.radius.min(80.0)) {
            d.alert = d.alert.max(0.8) + 0.2;
        }
        d.look -= dt;
        if d.look <= 0.0 && !ph.dead && !player.noclip {
            d.look = 0.25;
            let eye = vt.transform_point(Vec3::new(0.0, v.bounds().1.y + 0.4, 0.0));
            let to = pt.translation + Vec3::Y * 0.4 - eye;
            let dist = to.length();
            let range = if player.crouching { 30.0 } else { 55.0 };
            let seen = dist < range
                && Dir3::new(to).ok().is_some_and(|dir| {
                    let filter = SpatialQueryFilter::from_excluded_entities([ve, v.body()]);
                    spatial.cast_ray(eye, dir, dist, true, &filter).is_none_or(|h| h.entity == pe || h.distance > dist - 0.6)
                });
            if seen {
                // A couple of seconds to be sure at range, at once up close.
                d.alert += 0.25 * (1.6 - dist / range) * if player.driving { 2.0 } else { 1.0 };
            } else {
                d.alert = (d.alert - 0.02).max(0.0);
            }
        }
        if d.alert >= 1.0 && !d.stopping {
            d.stopping = true;
            for (r, mut npc) in &mut crew {
                if r.vehicle == ve {
                    npc.awareness = 1.0;
                    npc.last_known = Some(pt.translation);
                }
            }
        }
    }
}

/// Follows the route: steer toward the next point, slow for bends, back out
/// of jams; brake to a stop once alerted (boats circle on, slower).
fn drive_patrols(time: Res<Time>, routes: Res<Routes>, mut vehicles: Query<(&Transform, &LinearVelocity, &mut Vehicle, &mut PatrolDriver)>) {
    let dt = time.delta_secs();
    for (t, vel, mut v, mut d) in &mut vehicles {
        let speed = vel.0.length();
        let boat = v.kind() == crate::vehicles::Kind::Boat;
        if d.stopping && !boat {
            v.control(0.0, 0.0, true);
            continue;
        }
        let mut target = Vec3::ZERO;
        let mut cruise_speed = if d.stopping { BOAT_ALERT_SPEED } else { CRUISE };
        if let Some((goal, radius)) = d.goal {
            // Reinforcements: straight for the outpost; out at its edge, or
            // where the truck can't get any further.
            if Vec2::new(goal.x - t.translation.x, goal.z - t.translation.z).length() < radius || d.jams >= 3 {
                d.stopping = true;
                continue;
            }
            target = goal;
            cruise_speed = CONVOY_SPEED;
        }
        if let Some(r) = routes.list.get(d.route).filter(|_| d.goal.is_none()) {
        let n = r.points.len();
        target = point(r, d.next);
        let to = Vec2::new(target.x - t.translation.x, target.z - t.translation.z);
        let ahead = t.rotation * Vec3::NEG_Z;
        // Reached, or close by and already behind (it would only circle it).
        let passed = to.length() < REACHED * 2.5 && to.normalize_or_zero().dot(Vec2::new(ahead.x, ahead.z).normalize_or_zero()) < -0.2;
        if to.length() < REACHED || passed {
            let next = d.next as i32 + d.step;
            if r.looped {
                d.next = next.rem_euclid(n as i32) as usize;
            } else if next < 0 || next >= n as i32 {
                d.step = -d.step;
                d.next = (d.next as i32 + d.step).clamp(0, n as i32 - 1) as usize;
            } else {
                d.next = next as usize;
            }
            target = point(r, d.next);
        }
        } else if d.goal.is_none() {
            continue;
        }
        let fwd = t.rotation * Vec3::NEG_Z;
        let flat_fwd = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z);
        let want = Vec3::new(target.x - t.translation.x, 0.0, target.z - t.translation.z).normalize_or(flat_fwd);
        // Positive: the target is to the left.
        let angle = flat_fwd.cross(want).y.atan2(flat_fwd.dot(want));
        // Stuck against something: reverse with the wheel the other way.
        if d.reverse > 0.0 {
            d.reverse -= dt;
            v.control(-0.7, -angle.signum(), false);
            continue;
        }
        if speed < 1.0 {
            d.stuck += dt;
            if d.stuck > 3.0 {
                d.stuck = 0.0;
                d.reverse = 2.0;
                d.jams += 1;
            }
        } else {
            d.stuck = 0.0;
        }
        let cruise = cruise_speed * (1.0 - angle.abs() / std::f32::consts::PI * 1.6).max(0.3);
        let throttle = ((cruise - speed) * 0.35).clamp(-1.0, 1.0);
        v.control(throttle, (angle * 2.0).clamp(-1.0, 1.0), speed > cruise + 4.0);
    }
}

/// Once stopped, the crew climbs out and fights.
fn dismount(
    mut commands: Commands,
    heights: Res<crate::terrain::Heightfield>,
    vehicles: Query<(Entity, &Transform, &LinearVelocity, &Vehicle, &PatrolDriver)>,
    mut crew: Query<(Entity, &Riding, &mut Transform, &mut Npc, &mut Visibility, Has<crate::turrets::Gunner>), Without<PatrolDriver>>,
) {
    for (ve, vt, vel, v, d) in &vehicles {
        // Boat crews fight from the boat.
        if !d.stopping || vel.0.length() > 3.0 || v.kind() == crate::vehicles::Kind::Boat {
            continue;
        }
        let (min, max) = v.bounds();
        for (e, ride, mut t, mut npc, mut vis, gunner) in &mut crew {
            if ride.vehicle != ve {
                continue;
            }
            // The man on the gun stays on it.
            if gunner {
                npc.state = NpcState::Combat;
                continue;
            }
            // Out on the side of the seat, onto the ground.
            let side = if ride.seat.x < (min.x + max.x) * 0.5 { min.x - 1.0 } else { max.x + 1.0 };
            let mut at = vt.transform_point(Vec3::new(side, 0.0, ride.seat.z));
            at.y = heights.height_at(at).unwrap_or(at.y) + HALF_HEIGHT;
            t.translation = at;
            npc.post = at - Vec3::Y * HALF_HEIGHT;
            npc.state = NpcState::Combat;
            npc.state_time = 0.0;
            *vis = Visibility::Inherited;
            commands.entity(e).remove::<(Riding, ColliderDisabled)>();
        }
        commands.entity(ve).remove::<PatrolDriver>();
    }
}

/// --patrol-test: each patrol's progress every two seconds.
fn patrol_log(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut next: Local<f32>,
    players: Query<&Transform, With<Player>>,
    vehicles: Query<(&Transform, &LinearVelocity, Option<&PatrolDriver>), With<Vehicle>>,
    crew: Query<(&Npc, &Transform), Without<Vehicle>>,
) {
    if !args.patrol_test || time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    let Ok(pt) = players.single() else { return };
    for (t, v, d) in &vehicles {
        if let Some(d) = d {
            let up = (t.rotation * Vec3::Y).y;
            if d.goal.is_some() {
                info!("convoy at {:.0?} {:.1} m/s jams {} stopping {} dist {:.0}", t.translation, v.0.length(), d.jams, d.stopping, t.translation.distance(pt.translation));
                continue;
            }
            info!(
                "patrol: route {} wp {} at {:.0?} {:.1} m/s up {up:.2} alert {:.2} stopping {} dist {:.0}",
                d.route, d.next, t.translation, v.0.length(), d.alert, d.stopping, t.translation.distance(pt.translation)
            );
        }
    }
    let fighting = crew.iter().filter(|(n, _)| n.state == NpcState::Combat).count();
    if fighting > 0 {
        info!("patrol: {fighting} crew fighting");
    }
}

/// Patrols far from the player are taken off the road with their crews.
fn end_patrols(
    mut commands: Commands,
    players: Query<&Transform, With<Player>>,
    vehicles: Query<(Entity, &Transform), With<PatrolDriver>>,
    crew: Query<(Entity, &Riding)>,
) {
    let Ok(pt) = players.single() else { return };
    for (ve, vt) in &vehicles {
        if vt.translation.distance(pt.translation) > DROP_RANGE {
            for (e, r) in &crew {
                if r.vehicle == ve {
                    commands.entity(e).despawn();
                }
            }
            commands.entity(ve).despawn();
        }
    }
}
