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
                self.stops.remove(&vehicle);
                self.paths.insert(vehicle, FollowPath { path, points, next: 0, speed, looped: flag(3, false),
                    avoidance: flag(4, false), interruptible: flag(5, false), stop_on_arrival: flag(6, true), arrived: false, reported: false });
            }
            _ => {}
        }
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

fn flat(v: Vec3) -> Vec2 { Vec2::new(v.x, v.z) }

impl FollowPath {
    fn advance(&mut self, at: Vec3) {
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
                along >= 0.0 && along < REACHED * 2.0 && (to - leg * along).length() <= REACHED
                    && (at.y - target.y).abs() < 4.0
            } else { false };
            if !near && !passed { break; }
            if self.next + 1 < self.points.len() { self.next += 1; }
            else if self.looped { self.next = 0; }
            else { self.arrived = true; break; }
        }
    }

    fn controls(&self, pose: &Transform, velocity: Vec3) -> (f32, f32, bool) {
        if self.speed == 0.0 || (self.arrived && self.stop_on_arrival) { return (0.0, 0.0, true); }
        let forward = pose.rotation * Vec3::NEG_Z;
        let forward = Vec3::new(forward.x, 0.0, forward.z).normalize_or(Vec3::NEG_Z);
        let to = self.points[self.next] - pose.translation;
        let want = if self.arrived { forward } else { Vec3::new(to.x, 0.0, to.z).normalize_or(forward) };
        let angle = forward.cross(want).y.atan2(forward.dot(want));
        let speed = velocity.dot(forward);
        let mut target_speed = self.speed * (1.0 - angle.abs() / std::f32::consts::PI * 1.6).max(0.25);
        if self.stop_on_arrival && !self.looped && self.next + 1 == self.points.len() {
            // Begin braking before the final node, not after shooting past it.
            target_speed = target_speed.min((2.0 * 3.0 * (flat(to).length() - REACHED * 0.5).max(0.0)).sqrt());
        }
        (((target_speed - speed) * 0.4).clamp(-1.0, 1.0), (angle * 2.0).clamp(-1.0, 1.0), speed > target_speed + 2.0)
    }
}

/// Reuse the normal suspension/engine simulation. Cinematic ownership has
/// priority; a stopped/dead/driverless car cannot generate route progress.
pub(crate) fn drive_paths(mut paths: ResMut<StoryVehiclePaths>, cinema: Res<crate::cinema::Cinema>, spatial: SpatialQuery,
    mut vehicles: Query<(Entity, &Transform, &LinearVelocity, &RigidBody, &mut Vehicle, Option<&crate::combat::Health>)>) {
    let mut interrupted = Vec::new();
    for (entity, pose, velocity, body, mut vehicle, health) in &mut vehicles {
        let id = vehicle.story_id();
        let Some(path) = paths.paths.get_mut(&id) else { continue };
        if health.is_some_and(|h| h.dead) { vehicle.control(0.0, 0.0, true); interrupted.push(id); continue; }
        if *body != RigidBody::Dynamic || cinema.owns_entity(id) { continue; }
        let Some(driver) = vehicle.driver() else { vehicle.control(0.0, 0.0, true); continue };
        if driver == fc3_story::PLAYER && path.interruptible { interrupted.push(id); continue; }
        let previous = path.next;
        path.advance(pose.translation);
        if previous != path.next { info!("vehicle path {id}: reached node {previous}, next {} at {:?}", path.next, pose.translation); }
        let (mut throttle, steer, mut brake) = path.controls(pose, velocity.0);
        if path.avoidance && throttle > 0.0 {
            // Conservative obstacle avoidance: brake for a blocked driving
            // corridor. Dynamic detours are not yet implemented.
            let origin = pose.transform_point(Vec3::new(0.0, 1.0, vehicle.bounds().0.z - 0.2));
            let forward = pose.rotation * Vec3::NEG_Z;
            let filter = SpatialQueryFilter::from_excluded_entities([entity, vehicle.body()]);
            if spatial.cast_ray(origin, Dir3::new(forward).unwrap(), (velocity.0.length() * 1.5 + 3.0).min(25.0), true, &filter).is_some() {
                throttle = 0.0; brake = true;
            }
        }
        vehicle.control(throttle, steer, brake);
    }
    for id in interrupted { paths.paths.remove(&id); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> FollowPath {
        FollowPath { path: 1, points: vec![Vec3::ZERO, Vec3::new(0.0, 0.0, -10.0), Vec3::new(-10.0, 0.0, -10.0)],
            next: 0, speed: 11.0, looped: false, avoidance: false, interruptible: false, stop_on_arrival: true, arrived: false, reported: false }
    }

    #[test]
    fn route_progress_requires_ordered_physical_arrival_not_time_or_nearest_point() {
        let mut p = route();
        p.advance(p.points[2]);
        assert_eq!(p.next, 0); assert!(!p.arrived);
        p.advance(Vec3::ZERO); assert_eq!(p.next, 1);
        p.advance(Vec3::new(0.0, 8.0, -10.0)); assert_eq!(p.next, 1);
        p.advance(Vec3::new(0.0, 0.0, -11.0)); assert_eq!(p.next, 2);
        p.advance(Vec3::new(-8.0, 0.0, -10.0)); assert!(p.arrived);
        assert_eq!(p.controls(&Transform::IDENTITY, Vec3::NEG_Z * 11.0), (0.0, 0.0, true));
        p.advance(Vec3::ZERO); assert!(p.arrived);
        let mut looped = route(); looped.looped = true;
        for at in looped.points.clone() { looped.advance(at); }
        assert_eq!(looped.next, 0); assert!(!looped.arrived);
    }

    #[test]
    fn controller_steers_slows_for_turns_obeys_zero_speed_and_continues_when_requested() {
        let mut p = route(); p.advance(Vec3::ZERO);
        let (throttle, steer, brake) = p.controls(&Transform::IDENTITY, Vec3::ZERO);
        assert_eq!((throttle, steer, brake), (1.0, 0.0, false));
        p.next = 2;
        let pose = Transform::from_xyz(0.0, 0.0, -10.0);
        let (throttle, steer, brake) = p.controls(&pose, Vec3::NEG_Z * 11.0);
        assert!(throttle < 0.0); assert!(steer > 0.0); assert!(brake);
        p.speed = 0.0; assert_eq!(p.controls(&pose, Vec3::ZERO), (0.0, 0.0, true));
        p.speed = 11.0; p.arrived = true; p.stop_on_arrival = false;
        assert_eq!(p.controls(&pose, Vec3::ZERO), (1.0, 0.0, false));
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
