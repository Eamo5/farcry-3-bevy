//! Scripted vehicle paths are physical driving orders, not cinematic poses.
//! The finish event is delivered only after the live car reaches the last
//! authored shape point; missing drivers, streaming and collisions cannot
//! be replaced by a timer or a fabricated arrival.

use std::collections::{HashMap, HashSet};
use avian3d::prelude::*;
use bevy::prelude::*;
use crate::vehicles::Vehicle;

#[derive(Resource, Default)]
pub(crate) struct StoryVehiclePaths {
    paths: HashMap<u64, FollowPath>,
    stops: HashSet<u64>,
}

struct FollowPath {
    path: u64,
    points: Vec<Vec3>,
    next: usize,
    speed: f32,
    looped: bool,
    avoidance: bool,
    interruptible: bool,
    stop_on_arrival: bool,
    arrived: bool,
    reported: bool,
    /// How long it has pushed on without getting anywhere, and how long it
    /// has left backing off after that (s).
    stuck: f32,
    backing: f32,
    /// Seconds to the next progress report (debug).
    report: f32,
}

impl StoryVehiclePaths {
    pub(crate) fn native(&mut self, name: &str, args: &[String], data: &fc3_story::data::StoryData) {
        let Some(vehicle) = args.first().and_then(|s| s.parse::<u64>().ok()).filter(|&id| id != u64::MAX) else { return };
        match name {
            "CAPI_Vehicle.StopVehicleFollowPath" => {
                self.paths.remove(&vehicle);
                self.stops.insert(vehicle);
            }
            "CAPI_Vehicle.SetVehicleSpeed" => {
                let Some(speed) = args.get(1).and_then(|s| s.parse::<f32>().ok()).filter(|s| s.is_finite() && *s >= 0.0) else { return };
                if let Some(path) = self.paths.get_mut(&vehicle) { path.speed = speed; }
            }
            "CAPI_Vehicle.StartVehicleFollowPath" => {
                let (Some(path), Some(speed)) = (args.get(1).and_then(|s| s.parse::<u64>().ok()),
                    args.get(2).and_then(|s| s.parse::<f32>().ok()).filter(|s| s.is_finite() && *s >= 0.0)) else { return };
                let flag = |i: usize, default: bool| args.get(i).map_or(default, |s| s == "true" || s == "1");
                if flag(7, false) {
                    warn!("vehicle path {vehicle}: VehicleReturn is not yet supported");
                    return;
                }
                let points = data.entities.get(&path).and_then(|e| e.fields.field_h(fc3_formats::patrols::SHAPE_POINTS))
                    .map(fc3_formats::patrols::shape_points).unwrap_or_default();
                if points.len() < 2 || points.iter().flatten().any(|p| !p.is_finite()) {
                    warn!("vehicle path {vehicle}: missing or invalid shape {path}");
                    return;
                }
                let points: Vec<Vec3> = points.into_iter().map(crate::data::to_bevy).collect();
                info!("vehicle path {vehicle}: start shape {path}, {} points, speed {speed}", points.len());
                debug!("vehicle path {vehicle}: points {:?}", points.iter().map(|p| [p.x.round(), p.y.round(), p.z.round()]).collect::<Vec<_>>());
                self.stops.remove(&vehicle);
                self.paths.insert(
                    vehicle,
                    FollowPath {
                        path,
                        points,
                        next: 0,
                        speed,
                        looped: flag(3, false),
                        avoidance: flag(4, false),
                        interruptible: flag(5, false),
                        stop_on_arrival: flag(6, true),
                        arrived: false,
                        reported: false,
                        stuck: 0.0,
                        backing: 0.0,
                        report: 0.0,
                    },
                );
            }
            _ => {}
        }
    }

    /// A vehicle the game itself sends along a path (a reinforcement's way
    /// in): avoiding what moves, stopping at the end.
    pub(crate) fn follow(&mut self, vehicle: u64, path: u64, points: Vec<Vec3>, speed: f32) {
        if points.len() < 2 {
            return;
        }
        info!("vehicle path {vehicle}: follow shape {path}, {} points, speed {speed}", points.len());
        self.stops.remove(&vehicle);
        self.paths.insert(
            vehicle,
            FollowPath {
                path,
                points,
                next: 0,
                speed,
                looped: false,
                avoidance: true,
                interruptible: false,
                stop_on_arrival: true,
                arrived: false,
                reported: false,
                stuck: 0.0,
                backing: 0.0,
                report: 0.0,
            },
        );
    }

    /// The vehicle stops where it is (its path given up).
    pub(crate) fn stop(&mut self, vehicle: u64) {
        if self.paths.remove(&vehicle).is_some() {
            self.stops.insert(vehicle);
        }
    }

    /// The vehicle is on its way along a path.
    pub(crate) fn following(&self, vehicle: u64) -> bool {
        self.paths.get(&vehicle).is_some_and(|p| !p.arrived)
    }

    /// The vehicle got to the end of its path.
    pub(crate) fn arrived(&self, vehicle: u64) -> bool {
        self.paths.get(&vehicle).is_some_and(|p| p.arrived)
    }

    /// A noninterruptible path retains driver controls if the player boards.
    pub(crate) fn owns_controls(&self, vehicle: u64) -> bool {
        self.paths.get(&vehicle).is_some_and(|p| !p.interruptible)
    }
}

/// Flush callbacks outside FixedUpdate so original Lua can stop/restart paths
/// normally. Orders survive missing render entities but not layer removal.
pub(crate) fn story_paths(mut paths: ResMut<StoryVehiclePaths>, host: Option<NonSendMut<crate::story::StoryHost>>,
    mut vehicles: Query<&mut Vehicle>) {
    let Some(mut host) = host else { return };
    let cancelled: Vec<u64> = paths.paths.iter().filter_map(|(&id, p)|
        (!host.story.is_loaded(id) || !host.story.is_loaded(p.path)).then_some(id)).collect();
    for id in cancelled { paths.paths.remove(&id); paths.stops.insert(id); }
    for mut vehicle in &mut vehicles {
        if paths.stops.remove(&vehicle.story_id()) { vehicle.control(0.0, 0.0, true); }
    }
    paths.stops.retain(|id| host.story.is_loaded(*id));
    let arrived: Vec<u64> = paths.paths.iter_mut().filter_map(|(&id, path)| {
        if path.arrived && !path.reported { path.reported = true; Some(id) } else { None }
    }).collect();
    for vehicle in arrived {
        info!("vehicle path {vehicle}: physical arrival; FollowScriptedPathFinished");
        host.story.entity_event(vehicle, "FollowScriptedPathFinished", vec![]);
    }
}

const REACHED: f32 = 2.5;
/// A node already behind the vehicle this near counts as passed: one that
/// slid by a few metres wide (a boat at speed) would otherwise circle it for
/// good, out of the corridor its mission keeps it in.
const PASSED_WITHIN: f32 = 10.0;
/// Pushing on this long at a crawl (s), a driver backs off for `BACK_OFF`
/// (steering the other way) and tries the turn again.
const STUCK_AFTER: f32 = 2.0;
const BACK_OFF: f32 = 1.6;
/// How far on along the route a boat's driver steers for (pure pursuit):
/// this much, and this much more per m/s.
const LOOKAHEAD: f32 = 6.0;
const LOOKAHEAD_PER_MS: f32 = 1.0;
/// A boat's rudder turns it by torque, so steering on the heading alone
/// swings it past and back across a narrow channel onto the bank: it
/// counter-steers this much per rad/s it is already turning.
const YAW_DAMPING: f32 = 1.5;

fn flat(v: Vec3) -> Vec2 { Vec2::new(v.x, v.z) }

impl FollowPath {
    /// A driver caught on something (a rock at a tight bend) backs off and
    /// tries again rather than pushing into it for good: the controls to use
    /// this step, given the route's and how fast the car is going.
    fn recover(&mut self, dt: f32, speed: f32, (throttle, steer, brake): (f32, f32, bool)) -> (f32, f32, bool) {
        if self.backing > 0.0 {
            self.backing -= dt;
            return (-0.7, -steer, false);
        }
        if throttle > 0.2 && !brake && speed < 0.6 {
            self.stuck += dt;
            if self.stuck > STUCK_AFTER {
                self.stuck = 0.0;
                self.backing = BACK_OFF;
                debug!("vehicle path {}: caught short of node {}; backing off", self.path, self.next);
            }
        } else {
            self.stuck = 0.0;
        }
        (throttle, steer, brake)
    }

    /// `heading`: which way the vehicle faces (zero when not known).
    fn advance(&mut self, at: Vec3, heading: Vec3) {
        if self.arrived { return; }
        // Consume coincident or already crossed points, but never skip a turn
        // by choosing a nearest point further down the route.
        for _ in 0..self.points.len() {
            let target = self.points[self.next];
            let to = flat(at - target);
            let near = to.length() <= REACHED && (at.y - target.y).abs() < 4.0;
            let passed = if self.next > 0 {
                let leg = flat(target - self.points[self.next - 1]).normalize_or_zero();
                let along = to.dot(leg);
                along >= 0.0 && along < REACHED * 2.0 && (to - leg * along).length() <= REACHED && (at.y - target.y).abs() < 4.0
            } else {
                false
            };
            let behind = heading != Vec3::ZERO && to.length() <= PASSED_WITHIN && to.dot(flat(heading)) > 0.0 && (at.y - target.y).abs() < 4.0;
            if !near && !passed && !behind {
                break;
            }
            if self.next + 1 < self.points.len() {
                self.next += 1;
            } else if self.looped {
                self.next = 0;
            } else {
                self.arrived = true;
                break;
            }
        }
    }

    /// The point `ahead` metres on along the route from the vehicle's place
    /// on the leg it is on (pure pursuit): steering for it keeps the vehicle
    /// to the line between nodes, where steering for the node itself let a
    /// boat that slides weave across a narrow channel onto its bank.
    fn aim(&self, at: Vec3, ahead: f32) -> Vec3 {
        let n = self.points.len();
        let target = self.points[self.next];
        if self.arrived || (self.next == 0 && !self.looped) {
            return target;
        }
        let from = self.points[(self.next + n - 1) % n];
        let leg = flat(target - from);
        let len = leg.length();
        if len < 0.01 {
            return target;
        }
        let along = flat(at - from).dot(leg / len).clamp(0.0, len);
        let mut left = ahead;
        if along + left <= len {
            return from.lerp(target, (along + left) / len);
        }
        left -= len - along;
        let mut i = self.next;
        for _ in 0..n {
            if i + 1 >= n && !self.looped {
                break;
            }
            let (a, b) = (self.points[i], self.points[(i + 1) % n]);
            let l = flat(b - a).length();
            if left <= l {
                return a.lerp(b, left / l.max(0.01));
            }
            left -= l;
            i = (i + 1) % n;
        }
        self.points[i]
    }

    /// `boat`: steer a boat for a point on along the line, damped by how
    /// fast it is already turning (`yaw_rate`, rad/s, left positive); other
    /// vehicles steer for the node.
    fn controls(&self, pose: &Transform, velocity: Vec3, yaw_rate: f32, boat: bool) -> (f32, f32, bool) {
        if self.speed == 0.0 || (self.arrived && self.stop_on_arrival) { return (0.0, 0.0, true); }
        let forward = pose.rotation * Vec3::NEG_Z;
        let forward = Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
        let to = self.points[self.next] - pose.translation;
        let speed = velocity.dot(forward);
        let aim = if boat { self.aim(pose.translation, LOOKAHEAD + LOOKAHEAD_PER_MS * speed.max(0.0)) - pose.translation } else { to };
        let want = if self.arrived { forward } else { Vec3::new(aim.x, 0.0, aim.z).normalize_or(forward) };
        let angle = forward.cross(want).y.atan2(forward.dot(want));
        let mut target_speed = self.speed * (1.0 - angle.abs() / std::f32::consts::PI * 1.6).max(0.25);
        if self.stop_on_arrival && !self.looped && self.next + 1 == self.points.len() {
            // Begin braking before the final node, not after shooting past it.
            target_speed = target_speed.min((2.0 * 3.0 * (flat(to).length() - REACHED * 0.5).max(0.0)).sqrt());
        }
        let damping = if boat { YAW_DAMPING * yaw_rate } else { 0.0 };
        (((target_speed - speed) * 0.4).clamp(-1.0, 1.0), (angle * 2.0 - damping).clamp(-1.0, 1.0), speed > target_speed + 2.0)
    }
}

/// Reuse the normal suspension/engine simulation. Cinematic ownership has
/// priority; a stopped/dead/driverless car cannot generate route progress.
pub(crate) fn drive_paths(
    time: Res<Time>,
    mut paths: ResMut<StoryVehiclePaths>,
    cinema: Res<crate::cinema::Cinema>,
    spatial: SpatialQuery,
    mut vehicles: Query<(Entity, &Transform, &LinearVelocity, &AngularVelocity, &RigidBody, &mut Vehicle, Option<&crate::combat::Health>)>,
    (colliders, bodies, cars): (Query<&ColliderOf>, Query<&RigidBody, Without<Vehicle>>, Query<(), With<Vehicle>>),
) {
    let mut interrupted = Vec::new();
    for (entity, pose, velocity, spin, body, mut vehicle, health) in &mut vehicles {
        let id = vehicle.story_id();
        let Some(path) = paths.paths.get_mut(&id) else { continue };
        if health.is_some_and(|h| h.dead) { vehicle.control(0.0, 0.0, true); interrupted.push(id); continue; }
        if *body != RigidBody::Dynamic || cinema.owns_entity(id) { continue; }
        let Some(driver) = vehicle.driver() else { vehicle.control(0.0, 0.0, true); continue };
        if driver == fc3_story::PLAYER && path.interruptible { interrupted.push(id); continue; }
        let previous = path.next;
        path.advance(pose.translation, pose.rotation * Vec3::NEG_Z);
        if previous != path.next { info!("vehicle path {id}: reached node {previous}, next {} at {:?}", path.next, pose.translation); }
        let boat = vehicle.kind() == crate::vehicles::Kind::Boat;
        let (mut throttle, mut steer, mut brake) = path.controls(pose, velocity.0, spin.0.y, boat);
        path.report -= time.delta_secs();
        if path.report <= 0.0 && !path.arrived {
            path.report = 2.0;
            debug!(
                "vehicle path {id}: at {:.1?}, {:.1} m/s, node {} {:.1} m off, controls {throttle:.2} {steer:.2} {brake}",
                pose.translation,
                velocity.0.length(),
                path.next,
                flat(path.points[path.next] - pose.translation).length()
            );
        }
        // Round a fixed obstacle between it and the next node (a corner of
        // rock the straight line cuts across, as the road bends round it):
        // the nearest heading either side that's clear for a while.
        if !path.arrived {
            let filter = SpatialQueryFilter::from_excluded_entities([entity, vehicle.body()]);
            let origin = pose.transform_point(Vec3::new(0.0, 1.2, vehicle.bounds().0.z - 0.2));
            let to = path.points[path.next] - pose.translation;
            let reach = flat(to).length().clamp(3.0, 8.0);
            // (A slope it can drive up isn't in the way.)
            let clear = |d: Vec3| Dir3::new(d).map_or(true, |dir| spatial.cast_ray(origin, dir, reach, true, &filter).is_none_or(|h| h.normal.y > 0.6));
            let want = Vec3::new(to.x, 0.0, to.z).normalize_or_zero();
            if want != Vec3::ZERO && !clear(want) {
                let around =
                    (1..=4).flat_map(|k| [1.0, -1.0].map(|side| Quat::from_rotation_y(side * k as f32 * 20f32.to_radians()) * want)).find(|d| clear(*d));
                if let Some(d) = around {
                    let forward = pose.rotation * Vec3::NEG_Z;
                    let forward = Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
                    steer = (forward.cross(d).y.atan2(forward.dot(d)) * 2.0).clamp(-1.0, 1.0);
                }
            }
        }
        if path.avoidance && throttle > 0.0 {
            // Conservative obstacle avoidance: brake for something moving in
            // the driving corridor (a car, a body, a character). The route
            // itself keeps clear of the world's fixed shapes (a rock at a
            // bend it turns before), so those don't stop it. Dynamic
            // detours are not yet implemented.
            let origin = pose.transform_point(Vec3::new(0.0, 1.0, vehicle.bounds().0.z - 0.2));
            let forward = pose.rotation * Vec3::NEG_Z;
            let filter = SpatialQueryFilter::from_excluded_entities([entity, vehicle.body()]);
            let hit = spatial.cast_ray(origin, Dir3::new(forward).unwrap(), (velocity.0.length() * 1.5 + 3.0).min(25.0), true, &filter);
            let moving = hit.is_some_and(|h| {
                let body = colliders.get(h.entity).map_or(h.entity, |c| c.body);
                // (A vehicle's own parts count: it can move.)
                cars.get(body).is_ok() || bodies.get(body).is_ok_and(|b| *b != RigidBody::Static)
            });
            if moving {
                throttle = 0.0; brake = true;
                if let Some(h) = hit {
                    debug!("vehicle path {id}: braking for {:?} {:.1} m ahead", h.entity, h.distance);
                }
            }
        }
        (throttle, steer, brake) = path.recover(time.delta_secs(), velocity.0.length(), (throttle, steer, brake));
        vehicle.control(throttle, steer, brake);
    }
    for id in interrupted { paths.paths.remove(&id); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> FollowPath {
        FollowPath {
            path: 1,
            points: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0), Vec3::new(-10.0, 0.0, -10.0)],
            next: 0,
            speed: 11.0,
            looped: false,
            avoidance: false,
            interruptible: false,
            stop_on_arrival: true,
            arrived: false,
            reported: false,
            stuck: 0.0,
            backing: 0.0,
            report: 0.0,
        }
    }

    /// Pushing on at a crawl for a while, the driver backs off, steering the
    /// other way, then drives on; moving freely, he never does.
    #[test]
    fn a_driver_caught_at_a_bend_backs_off_and_tries_again() {
        let mut p = route();
        let ahead = (1.0, 0.8, false);
        for _ in 0..19 {
            assert_eq!(p.recover(0.1, 0.1, ahead), ahead);
        }
        // Past two seconds caught: back off.
        p.recover(0.2, 0.1, ahead);
        let (throttle, steer, brake) = p.recover(0.1, 0.1, ahead);
        assert!(throttle < 0.0 && steer < 0.0 && !brake);
        for _ in 0..20 {
            p.recover(0.1, 0.5, ahead);
        }
        assert_eq!(p.recover(0.1, 3.0, ahead), ahead);
        // Moving: no backing off however long.
        let mut q = route();
        for _ in 0..100 {
            assert_eq!(q.recover(0.1, 5.0, ahead), ahead);
        }
    }

    #[test]
    fn a_node_slid_past_wide_counts_once_it_is_behind() {
        let mut p = route();
        p.next = 1;
        // Going on south, 4 m beside the node and 3 m short of it: not yet.
        p.advance(Vec3::new(4.0, 0.0, -7.0), Vec3::NEG_Z);
        assert_eq!(p.next, 1);
        // 3 m beyond it: passed, rather than circled back to.
        p.advance(Vec3::new(4.0, 0.0, -13.0), Vec3::NEG_Z);
        assert_eq!(p.next, 2);
        // Further off than that, it must still be reached.
        let mut far = route();
        far.next = 1;
        far.advance(Vec3::new(12.0, 0.0, -13.0), Vec3::NEG_Z);
        assert_eq!(far.next, 1);
    }

    #[test]
    fn route_progress_requires_ordered_physical_arrival_not_time_or_nearest_point() {
        let mut p = route();
        p.advance(p.points[2], Vec3::ZERO);
        assert_eq!(p.next, 0);
        assert!(!p.arrived);
        p.advance(Vec3::ZERO, Vec3::ZERO);
        assert_eq!(p.next, 1);
        p.advance(Vec3::new(0.0, 8.0, -10.0), Vec3::ZERO);
        assert_eq!(p.next, 1);
        p.advance(Vec3::new(0.0, 0.0, -11.0), Vec3::ZERO);
        assert_eq!(p.next, 2);
        p.advance(Vec3::new(-8.0, 0.0, -10.0), Vec3::ZERO);
        assert!(p.arrived);
        assert_eq!(p.controls(&Transform::IDENTITY, Vec3::NEG_Z * 11.0, 0.0, false), (0.0, 0.0, true));
        p.advance(Vec3::ZERO, Vec3::ZERO);
        assert!(p.arrived);
        let mut looped = route();
        looped.looped = true;
        for at in looped.points.clone() {
            looped.advance(at, Vec3::ZERO);
        }
        assert_eq!(looped.next, 0);
        assert!(!looped.arrived);
    }

    /// Off to one side of a leg, the driver steers for a point on along the
    /// line, back onto it, not straight at the node; near a node's turn, for
    /// a point round it on the next leg.
    #[test]
    fn a_driver_steers_back_onto_the_line_between_nodes() {
        let mut p = route();
        p.next = 1;
        // 3 m to the right of the leg (0,0) -> (0,-10), 2 m along it.
        let aim = p.aim(Vec3::new(3.0, 0.0, -2.0), 4.0);
        assert!(aim.abs_diff_eq(Vec3::new(0.0, 0.0, -6.0), 1e-4), "{aim:?}");
        // 8 m along, 4 m ahead: 2 m round the turn onto the next leg.
        let aim = p.aim(Vec3::new(0.0, 0.0, -8.0), 4.0);
        assert!(aim.abs_diff_eq(Vec3::new(-2.0, 0.0, -10.0), 1e-4), "{aim:?}");
        // Past the route's end: its last node.
        let aim = p.aim(Vec3::new(0.0, 0.0, -8.0), 40.0);
        assert!(aim.abs_diff_eq(Vec3::new(-10.0, 0.0, -10.0), 1e-4), "{aim:?}");
    }

    /// A boat already swinging toward its line eases off the rudder before
    /// it gets there, and counter-steers if swinging fast.
    #[test]
    fn a_boat_turning_already_eases_off_the_rudder() {
        let mut p = route();
        p.next = 1;
        // 2 m right of the leg, facing along it: steer left.
        let pose = Transform::from_xyz(2.0, 0.0, -2.0);
        let (_, still, _) = p.controls(&pose, Vec3::NEG_Z * 5.0, 0.0, true);
        assert!(still > 0.0);
        let (_, easing, _) = p.controls(&pose, Vec3::NEG_Z * 5.0, 0.1, true);
        assert!(easing < still && easing > 0.0);
        let (_, back, _) = p.controls(&pose, Vec3::NEG_Z * 5.0, 1.0, true);
        assert!(back < 0.0);
    }

    #[test]
    fn controller_steers_slows_for_turns_obeys_zero_speed_and_continues_when_requested() {
        let mut p = route();
        p.advance(Vec3::ZERO, Vec3::ZERO);
        let (throttle, steer, brake) = p.controls(&Transform::IDENTITY, Vec3::ZERO, 0.0, false);
        assert_eq!((throttle, steer, brake), (1.0, 0.0, false));
        p.next = 2;
        let pose = Transform::from_xyz(0.0, 0.0, -10.0);
        let (throttle, steer, brake) = p.controls(&pose, Vec3::NEG_Z * 11.0, 0.0, false);
        assert!(throttle < 0.0);
        assert!(steer > 0.0);
        assert!(brake);
        p.speed = 0.0;
        assert_eq!(p.controls(&pose, Vec3::ZERO, 0.0, false), (0.0, 0.0, true));
        p.speed = 11.0;
        p.arrived = true;
        p.stop_on_arrival = false;
        assert_eq!(p.controls(&pose, Vec3::ZERO, 0.0, false), (1.0, 0.0, false));
    }

    #[test]
    fn native_orders_reject_invalid_shapes_speeds_and_do_not_finish_on_stop() {
        let mut data = fc3_story::data::StoryData::default();
        let blob: Vec<u8> = 3u32.to_le_bytes().into_iter().chain([0.0f32, 0.0, 0.0, 0.0, 10.0, 0.0, -10.0, 10.0, 0.0]
            .into_iter().flat_map(f32::to_le_bytes)).collect();
        data.entities.insert(200, fc3_story::data::Entry { id: 200, name: "route".into(), class: "CBasicShapeEntity".into(),
            pos: [0.0; 3], angles: [0.0; 3], layer: 0, origin: fc3_story::data::Origin::Global,
            components: std::sync::Arc::new(fc3_formats::fcb::Object::default()), fields: std::sync::Arc::new(fc3_formats::fcb::Object {
                fields: vec![(fc3_formats::patrols::SHAPE_POINTS, blob)], ..default() }) });
        let mut paths = StoryVehiclePaths::default();
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        paths.native("CAPI_Vehicle.StartVehicleFollowPath", &args(&["100", "200", "11"]), &data);
        assert!(paths.owns_controls(100)); assert_eq!(paths.paths[&100].points[1], Vec3::new(0.0, 0.0, -10.0));
        paths.native("CAPI_Vehicle.SetVehicleSpeed", &args(&["100", "NaN"]), &data);
        assert_eq!(paths.paths[&100].speed, 11.0);
        paths.native("CAPI_Vehicle.SetVehicleSpeed", &args(&["100", "0"]), &data);
        assert_eq!(paths.paths[&100].speed, 0.0); assert!(!paths.paths[&100].arrived);
        paths.native("CAPI_Vehicle.StartVehicleFollowPath", &args(&["100", "999", "11"]), &data);
        assert_eq!(paths.paths[&100].path, 200);
        paths.native("CAPI_Vehicle.StopVehicleFollowPath", &args(&["100"]), &data);
        assert!(!paths.owns_controls(100)); assert!(paths.stops.contains(&100));
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_briefing_rebel_path_uses_all_thirteen_authored_world_points() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let data = fc3_story::data::StoryData::load(&root).unwrap();
        let mut paths = StoryVehiclePaths::default();
        paths.native("CAPI_Vehicle.StartVehicleFollowPath", &[
            "2078018927633442802".into(), "2078018932205234166".into(), "11".into(), "false".into(),
            "false".into(), "false".into(), "true".into(), "false".into()], &data);
        let path = &paths.paths[&2078018927633442802];
        assert_eq!(path.points.len(), 13);
        assert!(path.points[0].abs_diff_eq(Vec3::new(-637.621, 135.86, -2269.48), 0.01));
        assert!(path.points[12].x > -600.0 && path.points[12].z < -2380.0);
        assert!(!path.arrived && !path.reported);
    }
}
