//! Drivable vehicles at the world's vehicle spawns: cars and trucks on raycast
//! suspension, boats that float and plane, and hang gliders.
//!
//! The player drives from a first-person seat (look around freely), steering
//! with the movement keys or the left stick; the interact key gets in and out.
//! Cars run over enemies and animals at speed. Engines, tyres, brakes and
//! bumps use the game's own vehicle sound sets.

use std::collections::HashSet;

use avian3d::prelude::*;
use bevy::prelude::*;
use fc3_formats::cache;
use fc3_formats::handling::Glider;

use crate::cache_assets::{cache_path, Fc3Model, Fc3Sector};
use crate::combat::{DamageEvent, DamageKind, Health};
use crate::controls::{Action, ActionState};
use crate::objects::SectorObjects;
use crate::player::{Player, HALF_HEIGHT};

pub struct VehiclesPlugin;

impl Plugin for VehiclesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Driving>()
            .init_resource::<Repairing>()
            .init_resource::<StorySeatRequests>()
            .init_resource::<AutopilotDrive>()
            .init_resource::<crate::vehicle_paths::StoryVehiclePaths>()
            .add_systems(
                PreUpdate,
                (story_board_test, story_exit_test, autopilot_exit, autopilot_board)
                    .chain()
                    .after(crate::controls::update_actions)
                    .after(crate::story::story_interactions)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            )
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_vehicles, load_handling))
            .init_resource::<SpeedFov>()
            .add_systems(Update, speed_fov)
            .add_systems(
                Update,
                (
                    spawn_vehicles,
                    despawn_vehicles,
                    (despawn_mission_vehicles, story_seats).chain().after(crate::story::StorySet).before(crate::story_actors::ActorSet),
                    (repair_test, enter_exit, repair_vehicles).chain().before(crate::hud::HudSet).after(crate::turrets::man_turrets),
                    run_over,
                    vehicle_sounds.after(crate::cinema::CinemaSet),
                    vehicle_hud.before(crate::hud::HudSet),
                    drive_test.before(enter_exit),
                    crate::vehicle_paths::story_paths.after(crate::story::StorySet).before(crate::cinema::CinemaSet),
                    autopilot_drive.after(crate::story::StorySet),
                )
                    .run_if(in_state(crate::menu::GameState::Playing)),
            )
            .add_systems(FixedUpdate, (crate::vehicle_paths::drive_paths, drive).chain())
            .add_message::<StrayHit>()
            .add_message::<PlaceVehicle>()
            .add_systems(Update, place_vehicles.run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, scene_unseats_player.after(crate::cinema::CinemaSet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(
                Update,
                (turn_wheels, wreck_rocket, stray_hits, spare_indestructible, vehicle_damage, wreck_test)
                    .chain()
                    .after(crate::cinema::CinemaSet)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            )
            .add_systems(Update, arm_tripod_vehicles.run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PostUpdate, (sync_vehicle_motion, seat_player, seat_story_actors).chain().before(TransformSystems::Propagate));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Car,
    Boat,
    Glider,
}

struct Def {
    /// World entity name prefix, e.g. `Vehicle/Land/Jeep4x4`.
    prefix: &'static str,
    name: &'static str,
    model: &'static str,
    kind: Kind,
    mass: f32,
    /// Damage it takes before it burns out.
    armour: f32,
    /// Driving force (N) and top speed (m/s).
    engine: f32,
    top_speed: f32,
}

impl Def {
    /// Its model's path: under the common vehicles' folder, unless given
    /// whole (the barge's, one of the single-player ones).
    fn model_path(&self) -> String {
        if self.model.starts_with(r"graphics\") {
            self.model.to_string()
        } else {
            format!("{VEHICLES}{}", self.model)
        }
    }
}

const VEHICLES: &str = r"graphics\__fc3_graphics\_common\vehicles\";

/// The game's handling for its land vehicles (`fc3_formats::handling`),
/// once read from the cache.
static HANDLING: std::sync::OnceLock<Vec<fc3_formats::handling::Handling>> = std::sync::OnceLock::new();

fn load_handling(info: Option<Res<crate::data::CacheInfo>>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::handling::HANDLING_REL))
        .map_err(anyhow::Error::from)
        .and_then(|t| fc3_formats::handling::from_text(&t))
    {
        Ok(list) => {
            info!("vehicles: the game's handling for {} kinds", list.len());
            let _ = HANDLING.set(list);
        }
        Err(e) => warn!("vehicles: no handling table ({e:#}); re-run the converter"),
    }
}

const KMH: f32 = 1.0 / 3.6;
const HORSEPOWER: f32 = 745.7;
const AIR_DENSITY: f32 = 1.225;
/// The steering angle the boats' rudder force was tuned to (the engine's
/// default high-speed angle, degrees).
const RUDDER_REFERENCE: f32 = 30.0;

/// How a definition drives: the game's figures where its archetype has them,
/// else ours.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tuning {
    mass: f32,
    /// The speeds the engine cuts off above (m/s), ahead and astern.
    top_speed: f32,
    reverse_speed: f32,
    /// Steering lock (radians) at rest and at top speed (`ours`: easing off
    /// from the first with speed).
    lock_low: f32,
    lock_high: f32,
    ours: bool,
    grip: f32,
    /// Air drag (N per (m/s)²).
    drag: f32,
    /// A car's pull: each gear's ratio and the speed (m/s) it holds to, the
    /// pull per unit of ratio, and astern; without a gearbox, `engine`.
    gears: [(f32, f32); 6],
    gear_count: usize,
    pull: f32,
    reverse_pull: f32,
    engine: f32,
    /// A boat's motor and hull, if from the game's data.
    boat: Option<BoatTune>,
    /// A hang glider's flight, likewise.
    glider: Option<Glider>,
    /// A car's wheels: front left, front right, rear left, rear right; and
    /// the share of its grip a locked wheel keeps.
    wheels: [WheelTune; 4],
    lock_friction: f32,
    /// The chassis: lift (N per (m/s)², negative pressing it down), extra
    /// gravity (m/s²), gravity's factor in the air (eased in over seconds,
    /// above a speed), angular damping, and the stabiliser in the air.
    lift: f32,
    extra_gravity: f32,
    airborne_gravity: f32,
    airborne_fade: f32,
    airborne_min_speed: f32,
    angular_damping: f32,
    stabilizer: f32,
}

/// How high (m) over the bottom of its tyres a car's wheel is mounted.
const WHEEL_MOUNT: f32 = 0.6;

/// A car wheel's suspension (per kilogram of the car, as Havok counts it)
/// and grip.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WheelTune {
    /// The spring's push per metre pressed in; the dampers' per m/s going in
    /// and coming back out.
    strength: f32,
    compression: f32,
    relaxation: f32,
    /// From the mount to the ground with the spring unloaded.
    reach: f32,
    grip: f32,
    /// Held by the handbrake; driven by the engine.
    handbrake: bool,
    driving: bool,
}

impl WheelTune {
    /// Ours: soft, standing a little high, every wheel braked and driven.
    const OURS: WheelTune =
        WheelTune { strength: 9.81 / (4.0 * 0.22), compression: 1.5, relaxation: 1.5, reach: 0.97, grip: 1.3, handbrake: true, driving: true };

    /// The game's: at rest the spring carries a quarter of the car, so it
    /// stands as modelled (its wheels on the ground).
    fn game(w: &fc3_formats::handling::Wheel) -> WheelTune {
        WheelTune {
            strength: w.strength,
            compression: w.compression,
            relaxation: w.relaxation,
            reach: WHEEL_MOUNT + 9.81 / (4.0 * w.strength),
            grip: w.friction,
            handbrake: w.handbrake,
            driving: w.driving,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BoatTune {
    /// Thrust ahead and astern (m/s² at full revs), seconds to full revs.
    thrust: f32,
    reverse: f32,
    rev_time: f32,
    /// The water's damping ahead, astern and to the side (per second), with
    /// the whole hull in the water.
    forward_drag: f32,
    reverse_drag: f32,
    side_drag: f32,
    /// Rudder angle (degrees) at rest and at its top speed (m/s).
    rudder_low: f32,
    rudder_high: f32,
    rudder_top: f32,
    /// The motor's push against the way it goes (m/s² at full revs), and
    /// the brake (m/s²).
    braking: f32,
    brake: f32,
    /// The buoyancy spheres it floats on (Bevy, from its origin: its centre
    /// of mass and their offsets; radius), and how many there are.
    spheres: [(Vec3, f32); MAX_SPHERES],
    sphere_count: usize,
    /// Its centre of mass (Bevy, from its origin).
    com: Vec3,
    /// The spheres' drop at its speed (m per m/s).
    sphere_drop: f32,
    /// Gravity's factor in the water and out of it.
    gravity: f32,
    gravity_air: f32,
    /// Per sphere in the water: friction on rising and falling, going down
    /// into it, and turning about its yaw.
    updown: f32,
    down: f32,
    yaw_friction: f32,
    /// The stabiliser's turn rate past its angle (radians).
    stabilizer: f32,
    stabilizer_angle: f32,
}

/// Buoyancy spheres a boat may have.
const MAX_SPHERES: usize = 8;
/// The water's density (kg/m³), as the engine floats boats.
const WATER_DENSITY: f32 = 1000.0;

/// The wingsuit's flight, parachute and look limits (the game's data for
/// its descriptor folder).
pub(crate) fn wingsuit() -> Option<&'static fc3_formats::handling::Handling> {
    HANDLING.get()?.iter().find(|h| h.folder == r"air\wingsuit")
}

/// The game's handling for a definition, by its model's folder.
pub(crate) fn handling_of(def: usize) -> Option<&'static fc3_formats::handling::Handling> {
    let folder = DEFS[def].model.rsplit_once('\\').map(|(f, _)| f.to_ascii_lowercase())?;
    HANDLING.get()?.iter().find(|h| h.folder == folder)
}

/// How much wider than the player's own the view is while driving (1: as
/// is): the vehicle's driving camera widens with speed, from its angle at
/// the low speed to its angle at the high one (km/h).
#[derive(Resource)]
pub struct SpeedFov(pub f32);

impl Default for SpeedFov {
    fn default() -> Self {
        SpeedFov(1.0)
    }
}

fn speed_fov(time: Res<Time>, driving: Res<Driving>, vehicles: Query<(&Vehicle, &LinearVelocity)>, mut fov: ResMut<SpeedFov>) {
    let want = driving
        .vehicle
        .and_then(|e| vehicles.get(e).ok())
        .and_then(|(v, lv)| {
            let [low, high, at_low, at_high] = handling_of(v.def)?.fov;
            (high > low && at_low > 0.0).then(|| {
                let t = ((lv.0.length() / KMH - low) / (high - low)).clamp(0.0, 1.0);
                (at_low + (at_high - at_low) * t) / at_low
            })
        })
        .unwrap_or(1.0);
    fov.0 += (want - fov.0) * (time.delta_secs() * 2.0).min(1.0);
}

fn tuning(def: usize) -> Tuning {
    let d = &DEFS[def];
    let mut t = Tuning {
        mass: d.mass,
        top_speed: d.top_speed,
        reverse_speed: d.top_speed,
        lock_low: 0.55,
        lock_high: 0.55,
        ours: true,
        grip: 1.3,
        drag: 0.9,
        gears: [(0.0, 0.0); 6],
        gear_count: 0,
        pull: 0.0,
        reverse_pull: 0.0,
        engine: d.engine,
        boat: None,
        glider: None,
        wheels: [WheelTune::OURS; 4],
        lock_friction: 1.0,
        lift: 0.0,
        extra_gravity: 0.0,
        airborne_gravity: 1.0,
        airborne_fade: 0.0,
        airborne_min_speed: 0.0,
        angular_damping: 0.6,
        stabilizer: 4.0,
    };
    let Some(h) = handling_of(def) else { return t };
    match (d.kind, &h.car, &h.boat) {
        // The engine's torque from its horsepower at the optimal revs, through
        // each gear and the final drive the gearbox's top speed sets.
        (Kind::Car, Some(c), _) => {
            let rad = |rpm: f32| rpm * std::f32::consts::TAU / 60.0;
            let torque = c.power * HORSEPOWER / rad(c.optimal_rpm) * c.torque_factor.max(0.1);
            let top_ratio = c.gears.iter().map(|g| g.0).fold(f32::MAX, f32::min);
            t.mass = h.mass;
            t.pull = torque * rad(c.max_rpm) / (c.gearbox_top * top_ratio);
            t.reverse_pull = t.pull * c.reverse_ratio;
            for (i, (ratio, kmh)) in c.gears.iter().take(6).enumerate() {
                t.gears[i] = (*ratio, kmh * KMH);
            }
            t.gear_count = c.gears.len().min(6);
            t.top_speed = c.cutoff;
            t.reverse_speed = c.reverse_cutoff.min(c.cutoff);
            t.lock_low = c.steer_low.to_radians();
            t.lock_high = c.steer_high.to_radians();
            t.ours = false;
            t.grip = c.friction;
            t.drag = 0.5 * AIR_DENSITY * c.drag * c.area;
            // Our four wheels from the data's (front first): a two-wheeler's
            // front and back, a six-wheeler's front and last pair.
            let n = c.wheels.len();
            let pick = match n {
                0 | 1 => None,
                2 => Some([0, 0, 1, 1]),
                3 => Some([0, 0, 1, 2]),
                _ => Some([0, 1, n - 2, n - 1]),
            };
            if let Some(pick) = pick.filter(|_| c.wheels.iter().all(|w| w.strength > 0.0)) {
                t.wheels = pick.map(|i| WheelTune::game(&c.wheels[i]));
                // (A car with none driven or none on the handbrake: all.)
                if !t.wheels.iter().any(|w| w.driving) {
                    t.wheels.iter_mut().for_each(|w| w.driving = true);
                }
                if !t.wheels.iter().any(|w| w.handbrake) {
                    t.wheels.iter_mut().for_each(|w| w.handbrake = true);
                }
            }
            t.lock_friction = c.lock_friction;
            t.lift = 0.5 * AIR_DENSITY * c.lift * c.area;
            t.extra_gravity = c.extra_gravity;
            t.airborne_gravity = c.airborne_gravity;
            t.airborne_fade = c.airborne_fade;
            t.airborne_min_speed = c.airborne_min_speed;
            t.angular_damping = c.angular_damping;
            t.stabilizer = c.stabilizer;
        }
        (Kind::Boat, _, Some(b)) => {
            // (The engine applies each friction at each buoyancy sphere in
            // the water: per unit of mass, against the speed.)
            let wet = b.spheres.len().max(1) as f32;
            let to_bevy = |v: [f32; 3]| Vec3::new(v[0], v[2], -v[1]);
            let com = to_bevy(b.com);
            let mut spheres = [(Vec3::ZERO, 0.0); MAX_SPHERES];
            for (slot, (at, r)) in spheres.iter_mut().zip(&b.spheres) {
                *slot = (com + to_bevy(*at), *r);
            }
            let forward_drag = b.forward_friction * wet;
            t.mass = h.mass;
            t.top_speed = b.forward / forward_drag;
            let reverse_drag = if b.reverse_friction > 0.0 { b.reverse_friction * wet } else { forward_drag };
            t.reverse_speed = b.reverse / reverse_drag;
            t.boat = Some(BoatTune {
                thrust: b.forward,
                reverse: b.reverse,
                rev_time: if b.rev_up > 0.0 { b.max_rpm / b.rev_up } else { 0.0 },
                forward_drag,
                reverse_drag,
                side_drag: b.side_friction * wet,
                rudder_low: b.steer_low,
                rudder_high: b.steer_high,
                rudder_top: b.steer_top.max(1.0),
                braking: b.engine_braking,
                brake: b.brake,
                spheres,
                sphere_count: b.spheres.len().min(MAX_SPHERES),
                com,
                sphere_drop: if b.z_offset_speed > 0.0 { b.z_offset / b.z_offset_speed } else { 0.0 },
                gravity: b.gravity,
                gravity_air: b.gravity_air,
                updown: b.updown_friction,
                down: b.down_friction,
                yaw_friction: b.up_rotate_friction,
                stabilizer: b.stabilizer,
                stabilizer_angle: b.stabilizer_angle.to_radians(),
            });
            t.angular_damping = b.angular_damping;
        }
        (Kind::Glider, ..) if h.glider.is_some() => {
            let g = h.glider.unwrap();
            t.mass = h.mass;
            t.top_speed = g.max_speed;
            t.reverse_speed = 0.0;
            t.glider = Some(g);
        }
        _ => {}
    }
    t
}

/// A hang glider in flight: its bank and pitch (radians, bank left and nose
/// up positive) and how fast they change, its heading and turn rate, its
/// airspeed, how far into landing mode it is (0 to 1), how long it has
/// flown, how long something has held it back, and whether it has come down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Glide {
    held: f32,
    down: bool,
    pub(crate) roll: f32,
    roll_rate: f32,
    pub(crate) pitch: f32,
    pitch_rate: f32,
    pub(crate) yaw: f32,
    pub(crate) turn: f32,
    pub(crate) speed: f32,
    landing: f32,
    pub(crate) flown: f32,
}

/// Getting off a hang glider higher than this above the ground drops the
/// player from it rather than setting them down beside it.
const GLIDER_DROP: f32 = 3.0;

/// A hang glider's collision: the width of its frame and pilot (m), and the
/// thickness of its wing.
const GLIDER_KEEL: f32 = 1.0;
const GLIDER_WING: f32 = 0.3;

/// A hang glider this close over the ground, held back by it, has landed.
const GLIDER_LANDED: f32 = 1.2;

/// A hang glider held back to a crawl this long (s) has crashed, wherever
/// it touched; one falling this fast (m/s) off an edge flies again.
const GLIDER_HELD: f32 = 0.5;
const GLIDER_RELAUNCH: f32 = 3.0;

/// A hang glider this close over the water is down in it.
const GLIDER_WATER: f32 = 0.3;

/// How fast (per second) a hang glider's body is turned to its attitude.
const GLIDER_SETTLE: f32 = 12.0;

/// The stick's pitch below which it is left alone (the glider eases back to
/// its cruising speed).
const GLIDE_DEADZONE: f32 = 0.05;

impl Glide {
    /// Setting off on a heading, nose pitched (up positive), at a speed.
    pub(crate) fn launch(yaw: f32, pitch: f32, speed: f32) -> Glide {
        Glide { yaw, pitch, speed, ..default() }
    }

    /// One step of flight, as the game's paraglider figures describe it. The
    /// stick (`x` bank left, `y` nose down) banks and pitches it, each easing
    /// to the rate the angle still to go asks for; the bank turns it, in full
    /// once it has its turn speed. Nose down gains speed and nose up bleeds
    /// it, until with the speed gone the nose drops back; the brake slows it
    /// to its floor; left alone it eases back to cruising. Near the ground
    /// its landing figures take over. Returns its sink (m/s, negative down)
    /// for its height above the ground and share of health.
    pub(crate) fn step(&mut self, g: &Glider, stick: Vec2, brake: bool, height: f32, health: f32, dt: f32) -> f32 {
        let lerp = |a: f32, b: f32, k: f32| a + (b - a) * k;
        let ease = |rate: f32| (rate * dt).clamp(0.0, 1.0);
        let into = height < g.landing_height;
        let time = if into { g.to_landing } else { g.from_landing };
        let step = if time > 0.0 { dt / time } else { 1.0 };
        self.landing = (self.landing + if into { step } else { -step }).clamp(0.0, 1.0);
        let k = self.landing;
        let max_roll = lerp(g.max_roll, g.landing_roll, k);
        let max_pitch = lerp(g.max_pitch, g.landing_pitch, k);
        let base = lerp(g.base_speed, g.landing_speed, k);
        let top = lerp(g.max_speed, g.landing_max_speed, k);
        // The take-off run takes no controls.
        self.flown += dt;
        let (stick, brake) = if self.flown < g.takeoff { (Vec2::ZERO, false) } else { (stick.clamp(Vec2::NEG_ONE, Vec2::ONE), brake) };
        let want = ((stick.x * max_roll - self.roll) * g.roll_rate).clamp(-g.roll_speed, g.roll_speed);
        self.roll_rate += (want - self.roll_rate) * ease(g.roll_acc);
        self.roll += self.roll_rate * dt;
        let mut nose = -stick.y * max_pitch;
        if nose > 0.0 && self.speed <= g.stall_speed {
            nose = 0.0;
        }
        let want = ((nose - self.pitch) * g.pitch_rate).clamp(-g.pitch_speed, g.pitch_speed);
        self.pitch_rate += (want - self.pitch_rate) * ease(g.pitch_acc);
        self.pitch += self.pitch_rate * dt;
        self.speed += self.pitch * g.pitch_to_speed * dt;
        if brake {
            self.speed += (g.brake_floor - self.speed).min(0.0) * ease(g.braking);
        } else if stick.y.abs() < GLIDE_DEADZONE {
            self.speed += (base - self.speed) * ease(g.brake_restore);
        }
        self.speed = self.speed.clamp(0.0, top.max(0.0));
        let full = if g.turn_full_speed > 0.0 { (self.speed / g.turn_full_speed).min(1.0) } else { 1.0 };
        self.turn += (self.roll * g.roll_to_turn.abs() * full - self.turn) * ease(g.turn_acc);
        self.yaw += self.turn * dt;
        if health < g.broken_health {
            g.broken_sink
        } else if height < g.low_height {
            g.low_sink
        } else {
            lerp(g.sink, g.landing_sink, k)
        }
    }

    /// Its attitude: heading, then pitch, then bank (the model's front is -Z).
    fn attitude(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch) * Quat::from_rotation_z(self.roll)
    }

    /// Its velocity: along its nose at its airspeed, plus the sink.
    pub(crate) fn velocity(&self, sink: f32) -> Vec3 {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch) * Vec3::NEG_Z * self.speed + Vec3::Y * sink
    }
}

impl Tuning {
    /// The front wheels' lock at a speed: from the game's lock at rest to its
    /// lock at top speed (ours: easing off with speed).
    fn lock(&self, speed: f32) -> f32 {
        let s = speed.abs();
        if self.ours {
            self.lock_low / (1.0 + s * 0.06)
        } else {
            let k = (s / self.top_speed.max(1.0)).min(1.0);
            self.lock_low + (self.lock_high - self.lock_low) * k
        }
    }

    /// The engine's pull (N, the whole car's) at a speed for the throttle:
    /// through the gear the gearbox holds at that speed, nothing above the
    /// cut-off (ours: a steady pull to the top speed).
    fn pull(&self, speed: f32, throttle: f32) -> f32 {
        if throttle == 0.0 {
            return 0.0;
        }
        let ahead = throttle > 0.0;
        if (ahead && speed > self.top_speed) || (!ahead && -speed > self.reverse_speed) {
            return 0.0;
        }
        if self.gear_count == 0 {
            return throttle * self.engine;
        }
        let force = if ahead {
            let gears = &self.gears[..self.gear_count];
            let ratio = gears.iter().find(|(_, top)| speed < *top).unwrap_or(&gears[gears.len() - 1]).0;
            self.pull * ratio
        } else {
            self.reverse_pull
        };
        throttle * force
    }
}

const DEFS: &[Def] = &[
    Def {
        prefix: "vehicle/land/jeep4x4",
        name: "4x4",
        model: r"land\4x4\4x4.xbg",
        kind: Kind::Car,
        mass: 1600.0,
        armour: 400.0,
        engine: 11000.0,
        top_speed: 33.0,
    },
    Def {
        prefix: "vehicle/land/technical",
        name: "Technical",
        model: r"land\4x4technical\4x4technical_node.xbg",
        kind: Kind::Car,
        mass: 1800.0,
        armour: 450.0,
        engine: 11000.0,
        top_speed: 30.0,
    },
    Def {
        prefix: "vehicle/land/atv",
        name: "Quad bike",
        model: r"land\atv\quadbike_01.xbg",
        kind: Kind::Car,
        mass: 350.0,
        armour: 180.0,
        engine: 3200.0,
        top_speed: 28.0,
    },
    Def {
        prefix: "vehicle/land/japansedan",
        name: "Sedan",
        model: r"land\civiliancar_01\civiliancar_01.xbg",
        kind: Kind::Car,
        mass: 1200.0,
        armour: 320.0,
        engine: 8000.0,
        top_speed: 36.0,
    },
    Def {
        prefix: "vehicle/land/germanhatch",
        name: "Hatchback",
        model: r"land\civiliancar_02\civiliancar_02.xbg",
        kind: Kind::Car,
        mass: 1100.0,
        armour: 300.0,
        engine: 7500.0,
        top_speed: 34.0,
    },
    Def {
        prefix: "vehicle/land/dunebuggy",
        name: "Buggy",
        model: r"land\dunebuggy\dune_buggy.xbg",
        kind: Kind::Car,
        mass: 700.0,
        armour: 250.0,
        engine: 6500.0,
        top_speed: 38.0,
    },
    Def {
        prefix: "vehicle/land/cargotruck",
        name: "Truck",
        model: r"land\bigtruck\bigtruck_01.xbg",
        kind: Kind::Car,
        mass: 5000.0,
        armour: 800.0,
        engine: 22000.0,
        top_speed: 24.0,
    },
    Def {
        prefix: "vehicle/sea/zodiac",
        name: "Boat",
        model: r"sea\speedboat_01\zodiac_01.xbg",
        kind: Kind::Boat,
        mass: 600.0,
        armour: 300.0,
        engine: 7000.0,
        top_speed: 22.0,
    },
    Def {
        prefix: "vehicle/sea/jetski",
        name: "Jet ski",
        model: r"sea\jetski\jetski.xbg",
        kind: Kind::Boat,
        mass: 300.0,
        armour: 150.0,
        engine: 4500.0,
        top_speed: 24.0,
    },
    Def {
        prefix: "vehicle/sea/patrolboat",
        name: "Patrol boat",
        model: r"sea\rigidhullboat\rigidhullboat_01.xbg",
        kind: Kind::Boat,
        mass: 2500.0,
        armour: 600.0,
        engine: 22000.0,
        top_speed: 20.0,
    },
    Def {
        prefix: "vehicle/air/glider",
        name: "Hang glider",
        model: r"air\glider\glider.xbg",
        kind: Kind::Glider,
        mass: 120.0,
        armour: 60.0,
        engine: 0.0,
        top_speed: 18.0,
    },
    // HUR_040's barge, made by its mission (no world entity is named so):
    // its health its own data's (`armour` 0: see `build_placed_vehicle`),
    // its gun on a tripod (`arm_with_tripod`).
    Def {
        prefix: "vehicle/sea/barge",
        name: "Barge",
        model: r"graphics\__fc3_graphics\singleplayer\vehicles\sea\barge_02.xbg",
        kind: Kind::Boat,
        mass: 30000.0,
        armour: 0.0,
        engine: 60000.0,
        top_speed: 6.0,
    },
];

/// A definition whose gun stands on a tripod (the barge).
fn has_tripod(def: usize) -> bool {
    DEFS[def].model.ends_with(r"\barge_02.xbg")
}

/// Health for a vehicle whose own data gives it none (`armour` 0).
const FALLBACK_ARMOUR: f32 = 5000.0;

fn def_for(name: &str) -> Option<usize> {
    let n = name.to_ascii_lowercase();
    DEFS.iter().position(|d| n.starts_with(d.prefix))
}

/// The vehicle definition drawn with this model (a graphic's `.xbg` path).
pub(crate) fn definition_for_model(path: &str) -> Option<usize> {
    DEFS.iter().position(|d| d.model_path().eq_ignore_ascii_case(path))
}

/// Physics layers (bits): everything is in the first; a vehicle's body is
/// also in `VEHICLE_LAYER`, and what swims (a shark, a fish) only in
/// `SWIMMER_LAYER`, which vehicles pass through: a boat runs on over a
/// shark (as in the game) rather than stopping dead against it. Shots and
/// the player still meet swimmers.
pub(crate) const VEHICLE_LAYER: u32 = 1 << 1;
pub(crate) const SWIMMER_LAYER: u32 = 1 << 2;

pub(crate) fn vehicle_layers() -> CollisionLayers {
    CollisionLayers::new(LayerMask(1 | VEHICLE_LAYER), LayerMask(!SWIMMER_LAYER))
}

pub(crate) fn swimmer_layers() -> CollisionLayers {
    CollisionLayers::new(LayerMask(SWIMMER_LAYER), LayerMask::ALL)
}

pub(crate) fn is_boat(def: usize) -> bool {
    DEFS.get(def).is_some_and(|d| d.kind == Kind::Boat)
}

/// The vehicle definition with this display name (e.g. "Technical").
pub fn def_named(name: &str) -> Option<usize> {
    DEFS.iter().position(|d| d.name == name)
}

#[derive(Resource)]
pub struct VehicleAssets {
    pub models: Vec<Handle<Fc3Model>>,
    /// The mounted gun's base on an armed technical, the ring on a boat, and
    /// the barge's tripod.
    pub turret_base: Handle<Fc3Model>,
    pub boat_ring: Handle<Fc3Model>,
    pub tripod: Handle<Fc3Model>,
}

/// Model-space eyes from the actual seated first-person pose, read once at
/// loading. Missing clips retain the existing sit-point fallback.
#[derive(Resource, Default)]
struct SeatCameras(std::collections::HashMap<&'static str, Vec3>);

fn seated_clip(def: usize, kind: i32, index: i32) -> Option<&'static str> {
    let d = DEFS.get(def)?;
    match (d.name, kind, index) {
        ("Technical", 1, 0) => Some(r"vehicles\4x4_technical\1stge_fulb_driving_nodir_nowep_4x4tech.mab"),
        ("Technical", 3, 0) => Some(r"vehicles\4x4_technical\1stge_fulb_passenger_nodir_nowep_4x4tech.mab"),
        ("4x4", 1, 0) => Some(r"vehicles\4x4\1stge_fulb_driving_nodir_nowep_4x4.mab"),
        ("4x4", 3, 0) => Some(r"vehicles\4x4\1stge_fulb_passenger_nodir_nowep_4x4.mab"),
        ("4x4", 3, 1 | 2) => Some(r"vehicles\4x4\1stge_fulb_backseat_nodir_nowep_4x4.mab"),
        ("Sedan" | "Hatchback", 1, 0) => Some(r"vehicles\car\1stge_fulb_driving_nodir_nowep_civiliancar.mab"),
        ("Sedan" | "Hatchback", 3, 0) => Some(r"vehicles\car\1stge_fulb_passenger_nodir_nowep_civiliancar.mab"),
        ("Buggy", 1, 0) => Some(r"vehicles\buggy\1stge_fulb_driving_nodir_nowep_dune.mab"),
        ("Buggy", 3, 0) => Some(r"vehicles\buggy\1stge_fulb_passenger_nodir_nowep_dune.mab"),
        ("Truck", 1, 0) => Some(r"vehicles\truck\big_truck\1stge_fulb_driving_nodir_nowep_truck.mab"),
        ("Truck", 3, 0) => Some(r"vehicles\truck\big_truck\1stge_fulb_passenger_nodir_nowep_truck.mab"),
        ("Quad bike", 1, 0) => Some(r"vehicles\atv\1stge_fulb_driving_nodir_nowep_atv.mab"),
        ("Jet ski", 1, 0) => Some(r"vehicles\boat\jetski\1stge_fulb_drivingfps_nodir_nowep_jetski.mab"),
        ("Boat", 1, 0) => Some(r"vehicles\boat\zodiac\1stge_fulb_driving_zodiac.mab"),
        ("Patrol boat", 1, 0) => Some(r"vehicles\boat\rigidhullboat\1stge_fulb_driving_rigidhull_boat.mab"),
        ("Hang glider", 1, 0) => Some(r"vehicles\other\glider\1stge_fulb_idle_+000fw_nowep_vo_glider.mab"),
        _ => None,
    }
}

fn clip_eye(skel: &fc3_formats::anim::Skeleton, clip: &fc3_formats::anim::Clip) -> Option<Vec3> {
    let camera = crate::cinema::bone_world(skel, clip, 0.0, "Camera")?;
    let eye = (crate::cinema::clip_root(clip, 0.0).to_matrix() * camera).transform_point3(Vec3::ZERO);
    eye.is_finite().then_some(eye)
}

fn load_vehicles(mut commands: Commands, server: Res<AssetServer>, info: Res<crate::data::CacheInfo>) {
    let models = DEFS.iter().map(|d| server.load(cache_path(&cache::model_rel(&d.model_path())))).collect();
    let turret_base = server.load(cache_path(&cache::model_rel(&format!("{VEHICLES}{TURRET_BASE}"))));
    let boat_ring = server.load(cache_path(&cache::model_rel(&format!("{VEHICLES}{BOAT_RING}"))));
    let tripod = server.load(cache_path(&cache::model_rel(BARGE_TRIPOD)));
    commands.insert_resource(VehicleAssets { models, turret_base, boat_ring, tripod });
    let mut cameras = SeatCameras::default();
    let skeleton = std::fs::read(info.root.join(cache::skeleton_rel(r"graphics\actors_\_skeleton_master\hips_ref.skeleton")))
        .ok().and_then(|b| fc3_formats::anim::parse_skeleton(&b).ok());
    if let Some(skeleton) = skeleton {
        for def in 0..DEFS.len() {
            for (kind, index) in [(1, 0), (3, 0)] {
                let Some(path) = seated_clip(def, kind, index) else { continue };
                if cameras.0.contains_key(path) { continue; }
                let clip = std::fs::read(info.root.join(cache::anim_rel(&format!(r"graphics\characters\_common\animations\{path}"))))
                    .ok().and_then(|b| fc3_formats::anim::parse_clip(&b).ok());
                if let Some(eye) = clip.and_then(|c| clip_eye(&skeleton, &c)) { cameras.0.insert(path, eye); }
            }
        }
    }
    commands.insert_resource(cameras);
}

const TURRET_BASE: &str = r"land\4x4technical\4x4techturretbase.xbg";
const BOAT_RING: &str = r"sea\speedboat_01\zodiac_mountedring_01.xbg";
/// The gun's pivot above the base's mount node (the technical), and above
/// the gunner's feet (any mount).
const GUN_ON_BASE: Vec3 = Vec3::new(0.0, 0.85, -0.02);
const GUN_ABOVE_GUNNER: f32 = 1.37;
/// The barge's gun (`weapons:FC3/Turret_Vehicle_M249.Barge`): a tripod
/// (its gunner at `GunnerPh`, the gun at `GunMount`) and an M249.
const BARGE_TRIPOD: &str = r"graphics\__fc3_graphics\_common\weapons\turret\tripod_barge_01.xbg";

/// A vehicle with a mounted gun: the gun's pivot and its gunner's feet (in
/// the vehicle's model space), and the gun (a weapon by name).
#[derive(Component)]
pub struct ArmedVehicle {
    pub pivot: Vec3,
    pub gunner: Vec3,
    pub gun: &'static str,
}

/// Puts a gun on a vehicle with a mount for one (the technical): its base on
/// the mount node, the gun (see `turrets`) above it and the gunner behind.
pub fn arm_vehicle(commands: &mut Commands, root: Entity, model: &Fc3Model, assets: &VehicleAssets, models: &Assets<Fc3Model>) -> bool {
    let node = |n: &str| crate::weapons::node_pose(model, n).map(|t| t.translation);
    let Some(gunner) = node("GUNNER_POSITION") else { return false };
    // The technical's post, the Zodiac's ring, or the 4x4's roll-bar ring
    // (part of its own mesh).
    let (pivot, base) = if let Some(mount) = node("TurretBaseAttachement") {
        (mount + GUN_ON_BASE, Some((&assets.turret_base, mount)))
    } else if let Some(ring) = node("MountedRing") {
        (Vec3::new(ring.x, gunner.y + GUN_ABOVE_GUNNER, ring.z), Some((&assets.boat_ring, ring)))
    } else if let Some(ring) = node("4x4_Ring_Bone") {
        (Vec3::new(ring.x, gunner.y + GUN_ABOVE_GUNNER, ring.z), None)
    } else {
        return false;
    };
    if let Some((handle, at)) = base {
        let Some(base) = models.get(handle) else { return false };
        for part in base.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
            commands.spawn((
                Mesh3d(part.mesh.clone()),
                MeshMaterial3d(part.material.clone()),
                crate::original_shaders::PropPart,
                Transform::from_translation(at),
                ChildOf(root),
            ));
        }
    }
    commands.entity(root).insert(ArmedVehicle { pivot, gunner, gun: "DShK" });
    true
}

/// Puts the barge's gun on it: its tripod set where its gunner stands at
/// the vehicle's gunner position, the gun on the tripod's mount.
fn arm_with_tripod(commands: &mut Commands, root: Entity, model: &Fc3Model, tripod: &Fc3Model) -> bool {
    let node = |m: &Fc3Model, n: &str| crate::weapons::node_pose(m, n).map(|t| t.translation);
    let (Some(gunner), Some(mount), Some(stand)) = (node(model, "GUNNER_POSITION"), node(tripod, "GunMount"), node(tripod, "GunnerPh")) else {
        return false;
    };
    let base = gunner - stand;
    for part in tripod.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
        commands.spawn((
            Mesh3d(part.mesh.clone()),
            MeshMaterial3d(part.material.clone()),
            crate::original_shaders::PropPart,
            Transform::from_translation(base),
            ChildOf(root),
        ));
    }
    commands.entity(root).insert(ArmedVehicle { pivot: base + mount, gunner, gun: "M249" });
    true
}

/// Vehicles whose gun stands on a tripod (the barge) get it once its model
/// is in.
fn arm_tripod_vehicles(
    assets: Option<Res<VehicleAssets>>,
    models: Res<Assets<Fc3Model>>,
    vehicles: Query<(Entity, &Vehicle), Without<ArmedVehicle>>,
    mut tried: Local<HashSet<Entity>>,
    mut commands: Commands,
) {
    let Some(assets) = assets else { return };
    let Some(tripod) = models.get(&assets.tripod) else { return };
    for (e, v) in &vehicles {
        if !has_tripod(v.def) || tried.contains(&e) {
            continue;
        }
        let Some(model) = models.get(&assets.models[v.def]) else { continue };
        tried.insert(e);
        if !arm_with_tripod(&mut commands, e, model, tripod) {
            warn!("vehicle: no place for {}'s gun", DEFS[v.def].name);
        }
    }
}

/// A vehicle a script has made indestructible (HUR_040's barge until its
/// defence) takes no lasting harm.
fn spare_indestructible(host: Option<NonSend<crate::story::StoryHost>>, mut vehicles: Query<(&Vehicle, &mut Health, &Wear)>) {
    let Some(host) = host else { return };
    for (v, mut h, w) in &mut vehicles {
        if !w.wrecked && (h.current < h.max || h.dead) && host.story.vehicle_rules(v.spawn_id).indestructible {
            h.current = h.max;
            h.dead = false;
        }
    }
}

#[derive(Component)]
pub struct Vehicle {
    def: usize,
    /// World entity id of the spawn it came from.
    spawn_id: u64,
    throttle: f32,
    steer: f32,
    /// The brakes on every wheel (an AI driver's, a wreck's), and the
    /// player's handbrake on the wheels the game puts it on.
    brake: bool,
    handbrake: bool,
    /// Local-space bounds of the model.
    min: Vec3,
    max: Vec3,
    occupied: bool,
    /// Nobody has driven it (or the player got out): the parking brake is on.
    parked: bool,
    /// The body collider (a child), excluded from the wheel rays.
    body: Entity,
    seats: Vec<SeatSlot>,
    vehicle_type: i32,
    authored_seats: bool,
    /// Wheel/engine presentation for a recorded scene (physics velocity is zero).
    recorded_speed: Option<f32>,
    /// A boat motor's revs (a share of its top).
    revs: f32,
    /// A hang glider's flight while someone flies it.
    glide: Glide,
}

#[derive(Clone)]
struct SeatSlot {
    kind: i32,
    /// Among the seats of its kind, and among all the vehicle's seats.
    index: i32,
    slot: i32,
    /// Authored sit-point transform in model space (feet, not camera).
    pose: Transform,
    pawn: Option<u64>,
}

/// Dynamic mission vehicles outlive their source sector, not their mission layer.
#[derive(Component)]
pub(crate) struct MissionVehicle(pub u64);

impl Vehicle {
    pub fn kind(&self) -> Kind {
        DEFS[self.def].kind
    }

    /// Someone is in it.
    pub fn occupied(&self) -> bool {
        self.occupied
    }

    /// Its top speed (m/s).
    pub fn top_speed(&self) -> f32 {
        tuning(self.def).top_speed
    }

    pub fn name(&self) -> &'static str {
        DEFS[self.def].name
    }

    /// Model-space bounds.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        (self.min, self.max)
    }

    pub fn body(&self) -> Entity {
        self.body
    }

    pub fn story_id(&self) -> u64 { self.spawn_id }

    pub(crate) fn def(&self) -> usize {
        self.def
    }

    /// Someone is driving it (the player, an AI driver or a scene's
    /// recording): its engine and lights are on.
    pub(crate) fn running(&self) -> bool {
        self.driver().is_some() || !self.parked || self.recorded_speed.is_some()
    }

    /// Its brakes are on, or it's being slowed against the way it goes.
    pub(crate) fn braking(&self) -> bool {
        self.brake || self.handbrake || self.throttle < -0.1
    }

    pub(crate) fn driver(&self) -> Option<u64> {
        self.seats.iter().find(|s| s.kind == 1).and_then(|s| s.pawn)
    }

    /// This seat is the wheel.
    pub(crate) fn is_driver_seat(&self, seat: usize) -> bool {
        self.seats.get(seat).is_some_and(|s| s.kind == 1)
    }

    /// This pawn has a seat in it.
    pub(crate) fn carries(&self, pawn: u64) -> bool {
        self.seats.iter().any(|s| s.pawn == Some(pawn))
    }

    /// A pawn killed in his seat leaves it free; a driver's leaves the car
    /// to roll to a stop (no one drives it on along its way).
    pub(crate) fn release(&mut self, pawn: u64) {
        for s in &mut self.seats {
            if s.pawn == Some(pawn) {
                s.pawn = None;
                if s.kind == 1 {
                    self.parked = true;
                    self.throttle = 0.0;
                    self.steer = 0.0;
                }
            }
        }
        self.occupied = self.seats.iter().any(|s| s.pawn.is_some());
    }

    /// The player moves over to a seat of this kind and index (from the
    /// 4x4's bed to its front seat, `car_chase`), if it's free: the seat.
    pub(crate) fn move_player(&mut self, kind: i32, index: i32) -> Option<usize> {
        let to = self.seats.iter().position(|s| s.kind == kind && s.index == index && s.pawn.is_none_or(|p| p == fc3_story::PLAYER))?;
        self.release(fc3_story::PLAYER);
        self.seats[to].pawn = Some(fc3_story::PLAYER);
        self.occupied = true;
        Some(to)
    }

    /// The seat a pawn has, its kind (1 the wheel, 2 a gun, 3 a
    /// passenger's), and where it is (its bone, in the vehicle's frame).
    pub(crate) fn seat_of(&self, pawn: u64) -> Option<(usize, i32, Transform)> {
        self.seats.iter().position(|s| s.pawn == Some(pawn)).map(|i| (i, self.seats[i].kind, self.seats[i].pose))
    }

    /// Never play a driver pose for a passenger, gunner or unsupported seat.
    pub(crate) fn first_person_clip(&self, seat: usize) -> Option<&'static str> {
        let s = self.seats.get(seat)?;
        seated_clip(self.def, s.kind, s.index)
    }

    pub fn occupant(&self, seat: usize) -> Option<fc3_story::vehicles::Occupant> {
        let s = self.seats.get(seat)?;
        s.pawn?;
        Some(fc3_story::vehicles::Occupant { vehicle: self.spawn_id, kind: s.kind, index: s.index, vehicle_type: self.vehicle_type })
    }

    pub fn npc_occupants(&self) -> impl Iterator<Item = (u64, fc3_story::vehicles::Occupant)> + '_ {
        self.seats.iter().enumerate().filter_map(|(i, s)| s.pawn.filter(|p| *p != fc3_story::PLAYER).map(|p| (p, self.occupant(i).unwrap())))
    }

    /// Whoever mans its mounted gun, as the story sees them: in its gunner
    /// seat.
    pub fn gunner_occupant(&self) -> Option<fc3_story::vehicles::Occupant> {
        let s = self.seats.iter().find(|s| s.kind == 2)?;
        Some(fc3_story::vehicles::Occupant { vehicle: self.spawn_id, kind: s.kind, index: s.index, vehicle_type: self.vehicle_type })
    }

    fn available_seat(&self, rules: &fc3_story::vehicles::Rules) -> Option<usize> {
        if rules.unusable { return None; }
        self.seats.iter().position(|s| s.kind != 2 && s.pawn.is_none() && !rules.locked.contains(&(s.kind, s.index)))
    }

    /// Driving inputs for an AI driver: throttle -1..1, steer -1..1 (left
    /// positive), and the brake.
    pub fn control(&mut self, throttle: f32, steer: f32, brake: bool) {
        self.parked = false;
        self.throttle = throttle.clamp(-1.0, 1.0);
        self.steer = steer.clamp(-1.0, 1.0);
        self.brake = brake;
    }

    pub(crate) fn recorded_controls(&mut self, gas: f32, steering: f32, speed: f32) {
        self.throttle = gas.clamp(-1.0, 1.0);
        self.steer = steering.clamp(-1.0, 1.0);
        self.recorded_speed = Some(speed);
    }

    fn wheels(&self) -> [Vec3; 4] {
        let inset = Vec3::new(0.3, 0.0, 0.8);
        let y = self.min.y + WHEEL_MOUNT;
        [
            Vec3::new(self.min.x + inset.x, y, self.min.z + inset.z),
            Vec3::new(self.max.x - inset.x, y, self.min.z + inset.z),
            Vec3::new(self.min.x + inset.x, y, self.max.z - inset.z),
            Vec3::new(self.max.x - inset.x, y, self.max.z - inset.z),
        ]
    }

    /// Driver's eye position, model space.
    fn seat(&self) -> Vec3 {
        let c = (self.min + self.max) * 0.5;
        match self.kind() {
            Kind::Glider => Vec3::new(c.x, self.min.y + 0.4, c.z),
            Kind::Boat => Vec3::new(c.x, self.min.y + 1.4, c.z + (self.max.z - self.min.z) * 0.2),
            // Left-hand drive, front seats a little under halfway back (the front is -Z).
            Kind::Car => Vec3::new(
                self.min.x + (self.max.x - self.min.x) * 0.3,
                self.min.y + (self.max.y - self.min.y) * 0.52,
                self.min.z + (self.max.z - self.min.z) * 0.47,
            ),
        }
    }
}

/// The vehicle the player is in, and its yaw last frame (to carry the view).
#[derive(Resource, Default)]
pub struct Driving {
    pub vehicle: Option<Entity>,
    pub seat: usize,
    last_yaw: f32,
    /// First-person seat instead of the chase camera (V toggles).
    pub first_person: bool,
    /// A gameplay carries him about the vehicle (JAS_130's climb from the
    /// bed to the front seat, `car_chase`): his eye and where he looks, in
    /// the vehicle's frame, and how far the view has gone over to them
    /// (0 to 1).
    pub carried: Option<(Vec3, Vec3, f32)>,
}

#[derive(Resource, Default)]
pub(crate) struct StorySeatRequests(Vec<SeatRequest>);

#[derive(Clone)]
struct SeatRequest {
    vehicle: u64,
    pawn: u64,
    index: i32,
    kinds: Vec<i32>,
    alternate: bool,
    exit: bool,
    forced: bool,
    /// Into the seat at once, not by its move in (`SkipEntryAnimation`).
    skip_entry: bool,
}

impl StorySeatRequests {
    pub(crate) fn native(&mut self, name: &str, args: &[String]) {
        let (Some(vehicle), Some(pawn)) = (args.first().and_then(|a| a.parse().ok()), args.get(1).and_then(|a| a.parse().ok())) else { return };
        let exit = name == "CAPI_Vehicle.VehicleUseExit";
        let flag = |i: usize| args.get(i).is_some_and(|a| a == "true" || a == "1");
        let kinds = [(3, 1), (4, 2), (5, 3)].into_iter().filter_map(|(i, k)| flag(i).then_some(k)).collect();
        self.0.retain(|r| r.pawn != pawn);
        self.0.push(SeatRequest {
            vehicle,
            pawn,
            index: args.get(2).and_then(|a| a.parse().ok()).unwrap_or(-1),
            kinds,
            alternate: flag(6),
            exit,
            forced: false,
            skip_entry: flag(7),
        });
    }

    /// The vehicle a character waits to board (a script's `TakeSeat`).
    pub(crate) fn boarding(&self, pawn: u64) -> Option<u64> {
        self.0.iter().find(|r| r.pawn == pawn && !r.exit).map(|r| r.vehicle)
    }

    /// Whether someone still waits to board this vehicle.
    pub(crate) fn awaited(&self, vehicle: u64) -> bool {
        self.0.iter().any(|r| r.vehicle == vehicle && !r.exit)
    }

    /// A called-in crew member takes a seat: the wheel, or a passenger's
    /// (any free one if those are taken).
    pub(crate) fn board(&mut self, vehicle: u64, pawn: u64, driver: bool) {
        self.0.retain(|r| r.pawn != pawn);
        let kinds = if driver { vec![1] } else { vec![3] };
        self.0.push(SeatRequest { vehicle, pawn, index: -1, kinds, alternate: !driver, exit: false, forced: false, skip_entry: false });
    }

    /// ... and gets out.
    pub(crate) fn leave(&mut self, vehicle: u64, pawn: u64) {
        self.0.retain(|r| r.pawn != pawn);
        self.0.push(SeatRequest { vehicle, pawn, index: -1, kinds: Vec::new(), alternate: false, exit: true, forced: false, skip_entry: true });
    }

    pub(crate) fn force_player(&mut self, args: &[String]) {
        let (Some(pawn), Some(kind), Some(index), Some(vehicle)) = (
            args.first().and_then(|a| a.parse::<u64>().ok()), args.get(1).and_then(|a| a.parse::<i32>().ok()),
            args.get(2).and_then(|a| a.parse::<i32>().ok()), args.get(3).and_then(|a| a.parse::<u64>().ok())) else { return };
        if pawn != fc3_story::PLAYER || !(1..=3).contains(&kind) || index < 0 { return; }
        self.0.retain(|r| r.pawn != pawn);
        self.0.push(SeatRequest { vehicle, pawn, index, kinds: vec![kind], alternate: false, exit: false, forced: true, skip_entry: true });
    }
}

/// The seat a script asks for: by its place among all the vehicle's seats
/// (`SeatIndex`, as the game counts it: JAS_190's Zodiac crew ask for 0, the
/// wheel, and 5, the gun, naming no kind), of the kind asked if any; else,
/// with a kind, by its place among that kind's (the seats our own layouts
/// number so); else any free one, when the script allows another.
fn requested_seat(v: &Vehicle, request: &SeatRequest) -> Option<usize> {
    let free = |s: &SeatSlot| s.pawn.is_none_or(|p| p == request.pawn);
    let kind = |s: &SeatSlot| request.kinds.is_empty() || request.kinds.contains(&s.kind);
    // (Forcing the player in, `ForceInVehicle`, counts among the kind's
    // seats only: JAS_130's 4x4 bed is the second passenger seat, after
    // the front one.)
    let by_slot = (request.index >= 0 && !request.forced).then(|| v.seats.iter().position(|s| s.slot == request.index && kind(s) && free(s))).flatten();
    let by_kind = || {
        (request.index < 0 || !request.kinds.is_empty())
            .then(|| v.seats.iter().position(|s| free(s) && kind(s) && (request.index < 0 || request.index == s.index)))
            .flatten()
    };
    by_slot.or_else(by_kind).or_else(|| request.alternate.then(|| v.seats.iter().position(|s| s.pawn.is_none())).flatten())
}

/// Where the player moves over to for a script's character to have the seat
/// he sits in: his seat, and a free one not at the wheel.
fn move_over(v: &Vehicle, request: &SeatRequest) -> Option<(usize, usize)> {
    let mine = v.seats.iter().position(|s| {
        s.pawn == Some(fc3_story::PLAYER) && (request.index < 0 || s.slot == request.index) && (request.kinds.is_empty() || request.kinds.contains(&s.kind))
    })?;
    let other = v.seats.iter().position(|s| s.pawn.is_none() && s.kind != 1)?;
    Some((mine, other))
}

/// Script requests wait for both physical entities. NPC driver requests may
/// use a player-locked driver seat, but never overwrite another occupant.
/// How long a seat request waits for its vehicle or character to be in
/// the world (s).
const SEAT_LOAD_WAIT: f32 = 30.0;

#[allow(clippy::too_many_arguments)]
fn story_seats(
    host: Option<NonSend<crate::story::StoryHost>>,
    mut requests: ResMut<StorySeatRequests>,
    mut driving: ResMut<Driving>,
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle), (Without<Player>, Without<crate::story_actors::StoryActor>)>,
    mut actors: Query<(Entity, &mut Transform, &mut crate::story_actors::StoryActor), (Without<Player>, Without<Vehicle>)>,
    mut players: Query<(Entity, &mut Transform, &mut Player), (Without<Vehicle>, Without<crate::story_actors::StoryActor>)>,
    mut commands: Commands,
    (time, mut unloaded_since): (Res<Time>, Local<std::collections::HashMap<(u64, u64), f32>>),
) {
    let Some(host) = host else { return };
    let now = time.elapsed_secs();
    for (_, _, mut v) in &mut vehicles {
        let mut lost_driver = false;
        for s in &mut v.seats {
            if s.pawn.is_some_and(|pawn| pawn != fc3_story::PLAYER && !actors.iter().any(|(_, _, a)| a.id == pawn && !a.dead)) {
                lost_driver |= s.kind == 1;
                s.pawn = None;
            }
        }
        if lost_driver { v.parked = true; v.throttle = 0.0; v.steer = 0.0; }
        v.occupied = v.seats.iter().any(|s| s.pawn.is_some());
    }
    let mut boarded = Vec::new();
    requests.0.retain(|request| {
        // (Kept a while for either to come: after a reload the mission's
        // characters are back a moment after the scripts seat them, as
        // JAS_200's visitor in his jeep.)
        if !host.story.is_loaded(request.vehicle) || !host.story.is_loaded(request.pawn) {
            let since = *unloaded_since.entry((request.pawn, request.vehicle)).or_insert(now);
            return now - since < SEAT_LOAD_WAIT;
        }
        unloaded_since.remove(&(request.pawn, request.vehicle));
        let Some((ve, vt, mut v)) = vehicles.iter_mut().find(|(_, _, v)| v.spawn_id == request.vehicle) else { return true };
        let pawn = if request.pawn == fc3_story::PLAYER { players.single().ok().map(|p| p.0) }
            else { actors.iter().find(|(_, _, a)| a.id == request.pawn && !a.dead).map(|a| a.0) };
        let Some(pawn) = pawn else { return true };
        if request.exit {
            let Some(seat) = v.seats.iter().position(|s| s.pawn == Some(request.pawn)) else { return false };
            let at = vt.transform_point(v.seats[seat].pose.translation + Vec3::X * 2.0);
            v.seats[seat].pawn = None;
            v.occupied = v.seats.iter().any(|s| s.pawn.is_some());
            if v.seats[seat].kind == 1 { v.parked = true; v.throttle = 0.0; v.steer = 0.0; }
            if request.pawn == fc3_story::PLAYER {
                if let Ok((_, mut pt, mut p)) = players.get_mut(pawn) { pt.translation = at; p.driving = false; p.velocity = Vec3::ZERO; }
                driving.vehicle = None;
            } else if let Ok((_, mut at_pose, mut a)) = actors.get_mut(pawn) { a.seated = false; at_pose.translation = at; }
            commands.entity(pawn).remove::<ColliderDisabled>();
        } else {
            // The seat a script gives a character is the player's: he moves
            // over to a free one, not at the wheel (SAM_015's Sam comes back
            // to drive, the player to man the turret).
            if request.pawn != fc3_story::PLAYER && requested_seat(&v, request).is_none() {
                if let Some((mine, other)) = move_over(&v, request) {
                    info!("vehicle: the player moves over from seat {mine} to {other} of {} for pawn {}", v.spawn_id, request.pawn);
                    if v.seats[mine].kind == 1 {
                        v.parked = true;
                        v.throttle = 0.0;
                        v.steer = 0.0;
                    }
                    v.seats[mine].pawn = None;
                    v.seats[other].pawn = Some(fc3_story::PLAYER);
                    if driving.vehicle == Some(ve) {
                        driving.seat = other;
                    }
                }
            }
            let Some(seat) = requested_seat(&v, request) else { return false };
            let rules = host.story.vehicle_rules(v.spawn_id);
            if request.pawn == fc3_story::PLAYER && !request.forced && (rules.unusable || rules.locked.contains(&(v.seats[seat].kind, v.seats[seat].index))) { return false; }
            // Repeated TakeSeat is idempotent and a change releases the old seat.
            let lost_driver = v.seats[seat].kind != 1 && v.seats.iter().any(|s| s.kind == 1 && s.pawn == Some(request.pawn));
            for s in &mut v.seats { if s.pawn == Some(request.pawn) { s.pawn = None; } }
            if lost_driver { v.parked = true; v.throttle = 0.0; v.steer = 0.0; }
            v.seats[seat].pawn = Some(request.pawn);
            v.occupied = true;
            boarded.push((request.pawn, request.vehicle));
            if request.pawn == fc3_story::PLAYER {
                if let Ok((_, _, mut p)) = players.get_mut(pawn) { p.driving = true; p.yaw = vt.rotation.to_euler(EulerRot::YXZ).0; }
                driving.vehicle = Some(ve); driving.seat = seat; driving.last_yaw = vt.rotation.to_euler(EulerRot::YXZ).0; driving.first_person = true;
            } else if let Ok((_, at_pose, mut a)) = actors.get_mut(pawn) {
                // (Standing in the world: in by the seat's move, from there.)
                if !a.seated && !a.in_scene && !request.skip_entry {
                    commands.entity(pawn).insert(crate::riders::WillGetIn { from: *at_pose, since: now });
                }
                a.seated = true;
            }
            commands.entity(pawn).insert(ColliderDisabled);
            info!("vehicle: pawn {} boarded {} seat type {} index {}", request.pawn, request.vehicle, v.seats[seat].kind, v.seats[seat].index);
        }
        false
    });
    for (_, _, mut v) in &mut vehicles {
        let id = v.spawn_id;
        let mut lost_driver = false;
        for s in &mut v.seats {
            if s.pawn.is_some_and(|pawn| boarded.iter().any(|&(p, destination)| p == pawn && destination != id)) {
                lost_driver |= s.kind == 1;
                s.pawn = None;
            }
        }
        if lost_driver { v.parked = true; v.throttle = 0.0; v.steer = 0.0; }
        v.occupied = v.seats.iter().any(|s| s.pawn.is_some());
    }
}

/// Resolve ownership after this frame's scenes start/stop and pose their props.
/// Kinematic bodies still integrate velocity, so a held prop must have none.
fn sync_vehicle_motion(
    host: Option<NonSend<crate::story::StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    mut vehicles: Query<(Entity, &mut Vehicle, &RigidBody, &mut LinearVelocity, &mut AngularVelocity)>,
    mut commands: Commands,
) {
    for (entity, mut v, body, mut linear, mut angular) in &mut vehicles {
        let animated = cinema.owns_entity(v.spawn_id) || host.as_ref().is_some_and(|h| h.story.vehicle_rules(v.spawn_id).animated);
        let desired = if animated { RigidBody::Kinematic } else { RigidBody::Dynamic };
        if *body != desired { commands.entity(entity).insert(desired); }
        if animated { linear.0 = Vec3::ZERO; angular.0 = Vec3::ZERO; }
        else if v.recorded_speed.take().is_some() { v.throttle = 0.0; v.steer = 0.0; }
    }
}

/// Explicit smoke-test input: move beside a selected mission vehicle and use
/// the normal interaction. No sit events, mission completion or seats injected.
fn story_board_test(
    host: Option<NonSend<crate::story::StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    vehicles: Query<(&Transform, &Vehicle), Without<Player>>,
    mut players: Query<(&mut Transform, &mut Player), Without<Vehicle>>,
    mut actions: ResMut<ActionState>,
    mut finished: Local<bool>,
) {
    let Some(id) = std::env::var("FC3_STORY_BOARD").ok().and_then(|s| s.parse::<u64>().ok()) else { return };
    let Some(host) = host else { return };
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    if p.driving { *finished = true; }
    if *finished || p.spawning || cinema.cutscene() || !host.story.awaits_vehicle_entry(fc3_story::PLAYER) { return; }
    let Some((vt, v)) = vehicles.iter().find(|(_, v)| v.spawn_id == id) else { return };
    if v.available_seat(&host.story.vehicle_rules(id)).is_none() { return; }
    pt.translation = vt.transform_point(Vec3::new(v.max.x + 0.8, 0.8, 0.0));
    p.yaw = vt.rotation.to_euler(EulerRot::YXZ).0 + std::f32::consts::FRAC_PI_2;
    p.velocity = Vec3::ZERO;
    actions.press(Action::Interact);
}

/// The story autopilot gets into the vehicle a script waits for the player
/// to take (CIT_020's cargo truck, whose health it listens to), as a player
/// would: beside it, the normal interaction.
fn autopilot_board(
    host: Option<NonSend<crate::story::StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    time: Res<Time>,
    vehicles: Query<(Entity, &Transform, &Vehicle), Without<Player>>,
    guns: Query<&crate::turrets::Turret>,
    mut players: Query<(&mut Transform, &mut Player), Without<Vehicle>>,
    mut actions: ResMut<ActionState>,
    mut last: Local<f32>,
) {
    if std::env::var_os("FC3_STORY_AUTOPILOT").is_none() || std::env::var_os("FC3_STORY_BOARD").is_some() {
        return;
    }
    let Some(host) = host else { return };
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    if p.driving
        || p.mounted.is_some()
        || p.spawning
        || cinema.cutscene()
        || cinema.plays_player()
        || host.quiet() < 3.0
        || !host.story.awaits_vehicle_entry(fc3_story::PLAYER)
    {
        return;
    }
    // (A press a second at most: the interaction sees it the next frame.)
    let now = time.elapsed_secs();
    if now - *last < 1.0 {
        return;
    }
    let here = pt.translation;
    let named = host.story.awaited_vehicles(fc3_story::PLAYER);
    // (One whose gun stands on a tripod is boarded at its gun, free:
    // HUR_040's barge, its gun manned.)
    let gun = |e: Entity| guns.iter().find(|g| g.on_vehicle() == Some(e) && g.user.is_none());
    let Some((ve, vt, v)) = vehicles
        .iter()
        .filter(|(e, _, v)| {
            (if named.is_empty() { host.story.watches_entity(v.spawn_id) } else { named.contains(&v.spawn_id) })
                && if has_tripod(v.def) { gun(*e).is_some() } else { v.available_seat(&host.story.vehicle_rules(v.spawn_id)).is_some() }
        })
        .min_by(|a, b| a.1.translation.distance(here).total_cmp(&b.1.translation.distance(here)))
    else {
        return;
    };
    *last = now;
    // At the gun, a press gets on it; else to the gun first (the press a
    // second later, once there: a press away from it would take a seat).
    if let Some(g) = gun(ve).filter(|_| has_tripod(v.def)) {
        let (spot, feet) = (g.gunner(), pt.translation - Vec3::Y * HALF_HEIGHT);
        let name = host.story.data().name_of(v.spawn_id).unwrap_or("?").to_string();
        if Vec2::new(feet.x - spot.x, feet.z - spot.z).length() < 1.0 && (feet.y - spot.y).abs() < 1.2 {
            info!("story autopilot: onto the gun of {name}");
            actions.press(Action::Interact);
        } else {
            info!(
                "story autopilot: to the gun of {name} at {spot:.1?} (the player at {feet:.1?}, the gun's pivot {:.1?}, the vehicle {:.1?})",
                g.pivot, vt.translation
            );
            pt.translation = spot + Vec3::Y * (HALF_HEIGHT + 0.05);
            p.yaw = g.yaw;
            p.velocity = Vec3::ZERO;
        }
        return;
    }
    info!("story autopilot: into vehicle {}", host.story.data().name_of(v.spawn_id).unwrap_or("?"));
    pt.translation = vt.transform_point(Vec3::new(v.max.x + 0.8, 0.8, 0.0));
    p.yaw = vt.rotation.to_euler(EulerRot::YXZ).0 + std::f32::consts::FRAC_PI_2;
    p.velocity = Vec3::ZERO;
    actions.press(Action::Interact);
}

/// Where the story autopilot takes the vehicle the player drives (into a box
/// a script waits on), and its heading (else kept).
#[derive(Resource, Default)]
pub struct AutopilotDrive(pub Option<(Vec3, Option<f32>)>);

fn autopilot_drive(
    mut drive: ResMut<AutopilotDrive>,
    driving: Res<Driving>,
    time: Res<Time>,
    spatial: SpatialQuery,
    mut vehicles: Query<(Entity, &mut Transform, &mut LinearVelocity, &mut AngularVelocity, &Vehicle)>,
    mut held: Local<Option<(Vec3, Quat, f32)>>,
) {
    let Some(Ok((root, mut t, mut linear, mut angular, v))) = driving.vehicle.map(|e| vehicles.get_mut(e)) else {
        if let Some((at, _)) = drive.0.take() {
            warn!("story autopilot: no vehicle to drive to {at:.1?}");
        }
        *held = None;
        return;
    };
    if let Some((at, heading)) = drive.0.take() {
        debug!("story autopilot: the vehicle from {:.1?} to {at:.1?}", t.translation);
        let yaw = heading.unwrap_or_else(|| t.rotation.to_euler(EulerRot::YXZ).0);
        *held = Some((at + Vec3::Y * 0.6, Quat::from_rotation_y(yaw), 0.0));
    }
    // Held there until the ground under it is in: set down where the
    // terrain's collider hasn't streamed in yet, it falls through the world.
    let Some((at, rotation, since)) = held.as_mut() else { return };
    t.translation = *at;
    t.rotation = *rotation;
    linear.0 = Vec3::ZERO;
    angular.0 = Vec3::ZERO;
    *since += time.delta_secs();
    let filter = SpatialQueryFilter::from_excluded_entities([root, v.body]);
    if spatial.cast_ray(*at + Vec3::Y * 1.5, Dir3::NEG_Y, 6.0, true, &filter).is_some() || *since > GROUND_WAIT {
        *held = None;
    }
}

/// The longest the autopilot holds a vehicle it set down for the ground
/// under it to come in.
const GROUND_WAIT: f32 = 15.0;

/// Explicit test-only dismount: wait for the authored leave listener and
/// enabled exit, then press normal Interact. Never inject a stand-up callback.
fn story_exit_test(
    host: Option<NonSend<crate::story::StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    driving: Res<Driving>,
    vehicles: Query<&Vehicle>,
    mut actions: ResMut<ActionState>,
    mut requested: Local<bool>,
) {
    let Some(id) = std::env::var("FC3_STORY_EXIT").ok().and_then(|s| s.parse::<u64>().ok()) else { return };
    let Some(host) = host else { return };
    if *requested || cinema.owns_entity(id) || !host.story.awaits_vehicle_exit(fc3_story::PLAYER) || host.story.vehicle_rules(id).exit_disabled { return; }
    let Some(v) = driving.vehicle.and_then(|e| vehicles.get(e).ok()).filter(|v| v.spawn_id == id) else { return };
    if v.seats.get(driving.seat).is_none_or(|s| s.pawn != Some(fc3_story::PLAYER)) { return; }
    *requested = true;
    actions.press(Action::Interact);
}

/// A scene that plays the player's own body, and not the vehicle he's in,
/// takes him out of it: when it ends he's where it left him, on foot
/// (JAS_130's closing scene, the truck's exit disabled since he got in).
fn scene_unseats_player(
    cinema: Res<crate::cinema::Cinema>,
    mut driving: ResMut<Driving>,
    mut vehicles: Query<&mut Vehicle>,
    mut players: Query<(Entity, &mut Player)>,
    mut commands: Commands,
) {
    let Some(ve) = driving.vehicle else { return };
    let Ok(mut v) = vehicles.get_mut(ve) else { return };
    if !cinema.plays_player_without(v.spawn_id) {
        return;
    }
    let was_driver = v.seats.iter().any(|s| s.kind == 1 && s.pawn == Some(fc3_story::PLAYER));
    for s in &mut v.seats {
        if s.pawn == Some(fc3_story::PLAYER) {
            s.pawn = None;
        }
    }
    v.occupied = v.seats.iter().any(|s| s.pawn.is_some());
    if was_driver {
        v.parked = true;
        v.throttle = 0.0;
        v.steer = 0.0;
    }
    driving.vehicle = None;
    if let Ok((e, mut p)) = players.single_mut() {
        p.driving = false;
        p.armed_passenger = false;
        commands.entity(e).remove::<ColliderDisabled>();
    }
    info!("vehicle: a scene takes the player out of {}", v.spawn_id);
}

/// Tests: the story autopilot gets out of a vehicle the story is done with
/// (a mission over in it, the next elsewhere) once the story has been quiet
/// a while: no script listens to it, it isn't on its way along a path, and
/// getting out is allowed. Off a vehicle's gun likewise, listened to or not,
/// once there is no fight near (HUR_040's barge, Hurk to be talked to).
fn autopilot_exit(
    host: Option<NonSend<crate::story::StoryHost>>,
    cinema: Res<crate::cinema::Cinema>,
    driving: Res<Driving>,
    (players, guns): (Query<&Player>, Query<&crate::turrets::Turret>),
    paths: Res<crate::vehicle_paths::StoryVehiclePaths>,
    vehicles: Query<(&Vehicle, &Transform)>,
    foes: Query<(&Transform, &crate::story_actors::StoryActor, &crate::npc::Npc, &Health)>,
    mut actions: ResMut<ActionState>,
    mut last: Local<f32>,
    time: Res<Time>,
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() || cinema.view.is_some() {
        return;
    }
    let Some(host) = host else { return };
    let gun_vehicle = players.single().ok().and_then(|p| p.mounted).and_then(|m| guns.get(m.turret).ok()).and_then(|g| g.on_vehicle());
    let on_gun = driving.vehicle.is_none() && gun_vehicle.is_some();
    let Some((v, vt)) = driving.vehicle.or(gun_vehicle).and_then(|e| vehicles.get(e).ok()) else { return };
    let id = v.spawn_id;
    // Foes a block counts down near (CIT_020 B30's privateers where the
    // convoy stopped): out to fight them, the vehicle still listened to.
    let counted = host.story.counted_members();
    // (On a gun, those it could reach.)
    let reach = if on_gun { FIGHT_FROM_GUN } else { FIGHT_FROM_VEHICLE };
    let fight = foes.iter().any(|(t, a, npc, h)| !h.dead && counted.contains(&a.id) && npc.hostile() && t.translation.distance(vt.translation) < reach);
    if host.quiet() < if fight { 3.0 } else { 20.0 } || time.elapsed_secs() - *last < 5.0 {
        return;
    }
    *last = time.elapsed_secs();
    let (watched, following, locked, entry) =
        (host.story.watches_entity(id), paths.following(id), host.story.vehicle_rules(id).exit_disabled, host.story.awaits_vehicle_entry(fc3_story::PLAYER));
    // Something near a script waits for the player to use (a bomb to
    // plant), which isn't done from a seat: out on foot, the vehicle still
    // listened to (SAM_015's four key areas, driven into with Sam).
    let graph = std::env::var("FC3_STORY_AUTOPILOT_GRAPH").ok();
    let to_use = !fight
        && host
            .story
            .watched_triggers_in_graph(graph.as_deref())
            .iter()
            .any(|w| w.3 && w.1 == "Use" && crate::data::to_bevy(w.2).distance(vt.translation) < ON_FOOT_REACH);
    // (Off a gun with foes about once a while has shown it isn't reaching
    // them.)
    let gunning = on_gun && fight && host.quiet() < GUN_PATIENCE;
    if following || locked || gunning || (!on_gun && !fight && !to_use && (watched || entry)) {
        debug!("story autopilot: staying in vehicle {id} (listened to {watched}, on a path {following}, exit disabled {locked}, entry awaited {entry})");
        return;
    }
    if on_gun {
        info!("story autopilot: off the gun of vehicle {id} (foes about {fight})");
    } else if fight {
        info!("story autopilot: out of vehicle {id} to fight");
    } else if to_use {
        info!("story autopilot: out of vehicle {id}, something to use near");
    } else {
        info!("story autopilot: out of vehicle {id}, which the story is done with");
    }
    actions.press(Action::Interact);
}

/// How near foes a block counts down must be for the autopilot to get out
/// of the vehicle it drives to fight them.
const FIGHT_FROM_VEHICLE: f32 = 150.0;
/// How near something to use must be for it to get out to use it.
const ON_FOOT_REACH: f32 = 150.0;
/// How near foes must be for the autopilot to stay on a vehicle's gun, and
/// how long the story is quiet (s) before it leaves it with foes about.
const FIGHT_FROM_GUN: f32 = 400.0;
const GUN_PATIENCE: f32 = 60.0;

pub(crate) fn seat_story_actors(
    mut vehicles: Query<(&Transform, &mut Vehicle), Without<crate::story_actors::StoryActor>>,
    mut actors: Query<(Entity, &mut Transform, &mut crate::story_actors::StoryActor), Without<Vehicle>>,
    mut commands: Commands,
) {
    for (e, mut at, mut actor) in &mut actors {
        // Killed in his seat (shot at the wheel): the seat is his no
        // longer, and the body stays where it fell.
        if actor.dead && actor.seated {
            for (_, mut v) in &mut vehicles {
                if v.carries(actor.id) {
                    info!("vehicle: pawn {} killed in his seat in {}", actor.id, v.spawn_id);
                    v.release(actor.id);
                }
            }
            actor.seated = false;
            commands.entity(e).remove::<ColliderDisabled>();
            continue;
        }
        let occupied = vehicles.iter().find_map(|(vt, v)| v.seats.iter().find(|s| s.pawn == Some(actor.id)).map(|s| (*vt, s.pose)));
        if let Some((vt, seat)) = occupied {
            if !actor.in_scene { *at = vt * seat; }
        } else if actor.seated {
            actor.seated = false;
            commands.entity(e).remove::<ColliderDisabled>();
        }
    }
}

#[derive(Resource, Default)]
struct Spawned(HashSet<u64>, u32);

#[allow(clippy::too_many_arguments)]
fn spawn_vehicles(
    mut spawned: Local<Spawned>,
    assets: Option<Res<VehicleAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    sectors: Res<Assets<Fc3Sector>>,
    loaded: Query<&SectorObjects>,
    players: Query<&Transform, With<Player>>,
    existing: Query<&Vehicle>,
    spatial: SpatialQuery,
    mut commands: Commands,
) {
    let Some(assets) = assets else { return };
    let Ok(pt) = players.single() else { return };
    let live: HashSet<u64> = existing.iter().map(|v| v.spawn_id).collect();
    spawned.0.retain(|id| live.contains(id));
    let mut budget = 2;
    for so in &loaded {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        for e in &sector.entities {
            let Some(def) = def_for(&e.name) else { continue };
            if spawned.0.contains(&e.id) || budget == 0 {
                continue;
            }
            let pos = Vec3::from_array(e.translation);
            if pos.distance(pt.translation) > 220.0 {
                continue;
            }
            let handle = &assets.models[def];
            if !crate::cache_assets::usable(&server, handle.id()) {
                continue;
            }
            let Some(model) = models.get(handle) else { continue };
            if model.lods.is_empty() {
                continue;
            }
            // Land vehicles and gliders wait for the ground under them (the
            // terrain or the ramp they stand on) to have its collider, or
            // they fall through the world, and once out of range are made
            // again, to fall again.
            if DEFS[def].kind != Kind::Boat && spatial.cast_ray(pos + Vec3::Y * 1.5, Dir3::NEG_Y, 6.0, true, &SpatialQueryFilter::default()).is_none() {
                if std::env::var("FC3_VEHICLE_DEBUG").is_ok() && spawned.1 < 20 {
                    spawned.1 += 1;
                    let far = spatial.cast_ray(pos + Vec3::Y * 50.0, Dir3::NEG_Y, 200.0, true, &SpatialQueryFilter::default()).map(|h| h.distance);
                    info!("vehicle: no ground under {} at {pos:.1?} (a long ray hits after {far:?})", DEFS[def].name);
                }
                continue;
            }
            budget -= 1;
            spawned.0.insert(e.id);
            if std::env::var("FC3_VEHICLE_DEBUG").is_ok() {
                info!("vehicle: spawning {} ({}) at {pos:.1?}", DEFS[def].name, e.name);
            }
            let root = build_vehicle(&mut commands, def, model, pos + Vec3::Y * 0.4, Quat::from_array(e.rotation), e.id);
            // `..._Turret` vehicles carry a mounted gun.
            let name = e.name.to_ascii_lowercase();
            if name.contains("_turret") && !name.contains("noturret") {
                arm_vehicle(&mut commands, root, model, &assets, &models);
            }
        }
    }
}

/// A vehicle to park (an encounter's): by its descriptor's path, at a pose,
/// under an id of its own.
#[derive(Message, Clone)]
pub struct PlaceVehicle {
    pub descriptor: String,
    pub at: Vec3,
    pub rotation: Quat,
    pub id: u64,
}

/// The definition whose model lives in the folder a descriptor names.
fn def_for_descriptor(path: &str) -> Option<usize> {
    let p = path.to_ascii_lowercase();
    DEFS.iter().position(|d| d.model.rsplit_once('\\').is_some_and(|(folder, _)| p.contains(&format!("\\vehicles\\{folder}\\"))))
}

/// Parks the vehicles encounters ask for once their model is in and the
/// ground under them has its collider (given up after half a minute).
fn place_vehicles(
    time: Res<Time>,
    mut requests: MessageReader<PlaceVehicle>,
    mut pending: Local<Vec<(PlaceVehicle, f32)>>,
    assets: Option<Res<VehicleAssets>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    existing: Query<&Vehicle>,
    spatial: SpatialQuery,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    pending.extend(requests.read().map(|r| (r.clone(), now)));
    let Some(assets) = assets else { return };
    let live: HashSet<u64> = existing.iter().map(|v| v.spawn_id).collect();
    pending.retain(|(r, since)| {
        let Some(def) = def_for_descriptor(&r.descriptor) else { return false };
        if live.contains(&r.id) || now - since > 30.0 {
            return false;
        }
        let handle = &assets.models[def];
        let Some(model) = models.get(handle).filter(|m| !m.lods.is_empty() && crate::cache_assets::usable(&server, handle.id())) else { return true };
        if DEFS[def].kind != Kind::Boat && spatial.cast_ray(r.at + Vec3::Y * 1.5, Dir3::NEG_Y, 6.0, true, &SpatialQueryFilter::default()).is_none() {
            return true;
        }
        if std::env::var("FC3_VEHICLE_DEBUG").is_ok() {
            info!("vehicle: parking {} for an encounter at {:.1?}", DEFS[def].name, r.at);
        }
        build_vehicle(&mut commands, def, model, r.at + Vec3::Y * 0.4, r.rotation, r.id);
        false
    });
}

/// Whether a vehicle of a definition waits for the ground under it before
/// it is made (all but boats).
pub fn needs_ground(def: usize) -> bool {
    DEFS[def].kind != Kind::Boat
}

/// Spawns a drivable vehicle of a definition at a pose; returns its root.
pub fn build_vehicle(commands: &mut Commands, def: usize, model: &Fc3Model, at: Vec3, rotation: Quat, spawn_id: u64) -> Entity {
    build_vehicle_inner(commands, def, model, at, rotation, spawn_id, None)
}

fn build_vehicle_inner(commands: &mut Commands, def: usize, model: &Fc3Model, at: Vec3, rotation: Quat, spawn_id: u64,
    placement: Option<(&Fc3Sector, &cache::Placement)>) -> Entity {
    let d = &DEFS[def];
    let (min, max) = (model.aabb_min, model.aabb_max);
    let size = max - min;
    // Body above the wheels; boats and gliders use their whole hull.
    let (body_min_y, body_max_y) = match d.kind {
        Kind::Car => (min.y + 0.55, min.y + size.y * 0.8),
        _ => (min.y, min.y + size.y.min(1.6)),
    };
    let half = Vec3::new(size.x * 0.48, (body_max_y - body_min_y) * 0.5, size.z * 0.48);
    let centre = Vec3::new((min.x + max.x) * 0.5, (body_min_y + body_max_y) * 0.5, (min.z + max.z) * 0.5);
    // Density from the target mass, so inertia matches the body too.
    let volume = (half.x * half.y * half.z * 8.0).max(0.1);
    let root = commands
        .spawn((
            RigidBody::Dynamic,
            LinearDamping(0.05),
            AngularDamping(if d.kind == Kind::Glider { 0.6 } else { tuning(def).angular_damping }),
            Transform::from_translation(at).with_rotation(rotation),
            Visibility::Visible,
            Name::new(d.name),
        ))
        .id();
    // A hang glider is a wide wing high over a narrow frame and pilot: a box
    // the wing's span at the pilot's feet would wedge it in any slope.
    let (shape, volume) = match d.kind {
        Kind::Glider => {
            let keel = Vec3::new(GLIDER_KEEL, half.y * 2.0, size.z * 0.5);
            let wing = Vec3::new(size.x * 0.96, GLIDER_WING, size.z * 0.8);
            let wing_at = Vec3::new(0.0, max.y - GLIDER_WING * 0.5 - centre.y, 0.0);
            let shape = Collider::compound(vec![
                (Vec3::ZERO, Quat::IDENTITY, Collider::cuboid(keel.x, keel.y, keel.z)),
                (wing_at, Quat::IDENTITY, Collider::cuboid(wing.x, wing.y, wing.z)),
            ]);
            (shape, (keel.x * keel.y * keel.z + wing.x * wing.y * wing.z).max(0.1))
        }
        _ => (Collider::cuboid(half.x * 2.0, half.y * 2.0, half.z * 2.0), volume),
    };
    let body = commands
        .spawn((
            shape,
            ColliderDensity(tuning(def).mass / volume),
            vehicle_layers(),
            CollidingEntities::default(),
            Transform::from_translation(centre),
            ChildOf(root),
        ))
        .id();
    commands.entity(root).insert((
        Vehicle {
            def,
            spawn_id,
            throttle: 0.0,
            steer: 0.0,
            brake: false,
            handbrake: false,
            min,
            max,
            occupied: false,
            parked: true,
            body,
            seats: vec![SeatSlot { kind: 1, index: 0, slot: 0, pose: Transform::IDENTITY, pawn: None }],
            vehicle_type: match d.kind {
                Kind::Car => 0,
                Kind::Boat => 1,
                Kind::Glider => 2,
            },
            authored_seats: false,
            recorded_speed: None,
            revs: 0.0,
            glide: Glide::default(),
        },
        Health::new(d.armour),
        Wear::default(),
        crate::combat::DamageScale { bullet: 0.45, explosion: 1.8 },
    ));
    // Cars carry their weight low (engine, axles, fuel), about at the axles,
    // not in the middle of the body box: it keeps them on their wheels in turns.
    if d.kind == Kind::Car {
        commands.entity(root).insert((CenterOfMass(Vec3::new(centre.x, min.y + 0.4, centre.z)), NoAutoCenterOfMass));
    }
    // A boat that floats as the engine has it: about its own centre of mass.
    if let Some(b) = tuning(def).boat.filter(|b| d.kind == Kind::Boat && b.sphere_count > 0) {
        commands.entity(root).insert((CenterOfMass(b.com), NoAutoCenterOfMass));
    }
    // Wheels and the steering wheel turn about their own node.
    let mut pivots: Vec<(String, Entity)> = Vec::new();
    for part in model.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
        let parent = match &part.pivot {
            Some(p) => match pivots.iter().find(|(n, _)| *n == p.name) {
                Some((_, e)) => *e,
                None => {
                    let at = Vec3::from_array(p.at);
                    let name = p.name.to_ascii_uppercase();
                    let kind = if name == "STEERINGWHEEL" {
                        Turning::Steering(Quat::from_array(p.rotation))
                    } else {
                        Turning::Wheel { front: name.contains("FRONT"), radius: (at.y - min.y).max(0.2) }
                    };
                    let e = commands.spawn((Transform::from_translation(at), Visibility::Inherited, TurningPart { kind, spin: 0.0 }, ChildOf(root))).id();
                    pivots.push((p.name.clone(), e));
                    e
                }
            },
            None => root,
        };
        let offset = part.pivot.as_ref().map(|p| -Vec3::from_array(p.at)).unwrap_or(Vec3::ZERO);
        let mut e = commands.spawn((
            Mesh3d(part.mesh.clone()),
            MeshMaterial3d(placement.map(|(s, p)| s.part_material(p, part)).unwrap_or_else(|| part.material.clone())),
            crate::original_shaders::PropPart,
            Transform::from_translation(offset),
            ChildOf(parent),
        ));
        if part.whole_only {
            e.insert(WholeOnly);
        }
    }
    // Its wreck's parts (burnt wheels, smashed windscreens), for when it
    // blows up: all of them stay on it (its loose ones too).
    let wreck: Vec<(Handle<Mesh>, Handle<StandardMaterial>)> =
        model.lods.first().map(|l| l.broken.iter().map(|b| (b.part.mesh.clone(), b.part.material.clone())).collect()).unwrap_or_default();
    if !wreck.is_empty() {
        commands.entity(root).insert(WreckParts(wreck));
    }
    root
}

/// A part of a vehicle there only while it's whole.
#[derive(Component)]
pub(crate) struct WholeOnly;

/// A vehicle's wreck parts: its model's broken state (mesh, material).
#[derive(Component)]
struct WreckParts(Vec<(Handle<Mesh>, Handle<StandardMaterial>)>);

/// A mission placement keeps its actual model/material kit. Names such as
/// JAS_060_080_100_Car are not enough to identify the vehicle archetype.
pub(crate) fn placed_definition(path: &str) -> Option<usize> {
    DEFS.iter().position(|d| cache::model_rel(&d.model_path()) == path)
}

pub(crate) fn build_placed_vehicle(
    commands: &mut Commands,
    sector: &Fc3Sector,
    placement: &cache::Placement,
    model: &Fc3Model,
    id: u64,
    layout: &fc3_story::vehicles::Layout,
    health: Option<f32>,
) -> Option<Entity> {
    let def = placed_definition(sector.model_paths.get(placement.model as usize)?)?;
    let (scale, rotation, at) = (Vec3::splat(placement.scale), Quat::from_array(placement.rotation), Vec3::from_array(placement.translation));
    let root = build_vehicle_inner(commands, def, model, at, rotation, id, Some((sector, placement)));
    commands.entity(root).insert(Transform::from_translation(at).with_rotation(rotation).with_scale(scale));
    // (One with no figure of ours has its own data's.)
    if DEFS[def].armour == 0.0 {
        commands.entity(root).insert(Health::new(health.unwrap_or(FALLBACK_ARMOUR)));
    }
    author_seats(commands, root, model, layout);
    Some(root)
}

/// A vehicle the story makes from an archetype (a reinforcement point's),
/// with the archetype's seats.
pub(crate) fn build_story_vehicle(
    commands: &mut Commands,
    def: usize,
    model: &Fc3Model,
    at: Vec3,
    rotation: Quat,
    id: u64,
    layout: &fc3_story::vehicles::Layout,
) -> Entity {
    let root = build_vehicle_inner(commands, def, model, at, rotation, id, None);
    author_seats(commands, root, model, layout);
    root
}

/// Set seats once the vehicle exists; no guessed bounds for authored seats.
/// (A layout with none keeps the vehicle's own driver seat.)
fn author_seats(commands: &mut Commands, root: Entity, model: &Fc3Model, layout: &fc3_story::vehicles::Layout) {
    let seats: Vec<SeatSlot> = layout
        .seats
        .iter()
        .filter_map(|s| {
            let pose = seat_bone_pose(&model.bones, s.bone)?;
            Some(SeatSlot { kind: s.kind, index: s.index, slot: s.slot, pose, pawn: None })
        })
        .collect();
    if seats.is_empty() && layout.seats.is_empty() {
        return;
    }
    let vehicle_type = layout.vehicle_type;
    commands.queue(move |world: &mut World| {
        if let Some(mut v) = world.get_mut::<Vehicle>(root) { v.seats = seats; v.vehicle_type = vehicle_type; v.authored_seats = true; }
    });
}

fn seat_bone_pose(bones: &[cache::CachedBone], hash: u32) -> Option<Transform> {
    let mut index = bones.iter().position(|b| b.name_hash == hash)?;
    let mut pose = Transform::IDENTITY;
    for _ in 0..bones.len() {
        let bone = bones.get(index)?;
        let local = Transform { translation: Vec3::from_array(bone.translation), rotation: Quat::from_array(bone.rotation), scale: Vec3::from_array(bone.scale) };
        pose = local * pose;
        if bone.parent < 0 { return Some(pose); }
        index = bone.parent as usize;
    }
    None // malformed/cyclic parent chain
}

#[cfg(test)]
mod story_vehicle_tests {
    use super::*;

    fn car() -> Tuning {
        // Round figures of the kind the game's have, worked through as
        // `tuning` does.
        let rad = |rpm: f32| rpm * std::f32::consts::TAU / 60.0;
        let pull = 300.0 * HORSEPOWER / rad(3000.0) * 0.5 * rad(6500.0) / 60.0;
        let mut gears = [(0.0, 0.0); 6];
        gears[..3].copy_from_slice(&[(2.5, 40.0 * KMH), (1.5, 60.0 * KMH), (1.0, 80.0 * KMH)]);
        Tuning {
            mass: 1500.0,
            top_speed: 22.0,
            reverse_speed: 15.0,
            lock_low: 0.7,
            lock_high: 0.5,
            ours: false,
            grip: 1.2,
            drag: 0.6,
            gears,
            gear_count: 3,
            pull,
            reverse_pull: pull * 2.5,
            engine: 0.0,
            boat: None,
            glider: None,
            wheels: [WheelTune::OURS; 4],
            lock_friction: 0.8,
            lift: 0.0,
            extra_gravity: 0.0,
            airborne_gravity: 1.0,
            airborne_fade: 0.0,
            airborne_min_speed: 0.0,
            angular_damping: 0.6,
            stabilizer: 4.0,
        }
    }

    #[test]
    fn steering_lock_runs_from_rest_to_top_speed() {
        let game = car();
        assert_eq!(game.lock(0.0), 0.7);
        assert!((game.lock(-11.0) - 0.6).abs() < 1e-6);
        assert!((game.lock(40.0) - 0.5).abs() < 1e-6);
        // Without the game's figures: ours, easing off with speed.
        let ours = Tuning { ours: true, lock_low: 0.55, ..game };
        assert!((ours.lock(10.0) - 0.55 / 1.6).abs() < 1e-6);
    }

    #[test]
    fn the_gearbox_pulls_hardest_in_first_and_the_engine_cuts_off() {
        let c = car();
        // About 10 kN in first, through second and third, none past 22 m/s.
        assert!((c.pull(1.0, 1.0) - 10_100.0).abs() < 100.0, "{}", c.pull(1.0, 1.0));
        assert!(c.pull(12.0, 1.0) < c.pull(5.0, 1.0));
        assert!(c.pull(19.0, 1.0) < c.pull(12.0, 1.0));
        assert_eq!(c.pull(22.5, 1.0), 0.0);
        assert!(c.pull(-5.0, -1.0) < 0.0);
        assert_eq!(c.pull(-15.5, -1.0), 0.0);
        assert_eq!(c.pull(5.0, 0.0), 0.0);
    }

    /// A glider with round figures of the kind the game's have.
    fn wing() -> Glider {
        Glider {
            max_roll: 0.6,
            roll_rate: 2.0,
            roll_speed: 3.0,
            roll_acc: 3.0,
            max_pitch: 0.9,
            pitch_rate: 2.0,
            pitch_speed: 10.0,
            pitch_acc: 5.0,
            pitch_to_speed: -8.0,
            roll_to_turn: -2.0,
            turn_acc: 2.0,
            turn_full_speed: 10.0,
            stall_speed: 0.0,
            base_speed: 10.0,
            sink: -1.0,
            broken_health: 0.2,
            broken_sink: -8.0,
            low_height: 10.0,
            low_sink: -0.5,
            braking: 1.0,
            brake_restore: 2.0,
            brake_floor: 3.0,
            max_speed: 30.0,
            takeoff: 2.0,
            landing_height: 10.0,
            landing_speed: 10.0,
            landing_roll: 0.5,
            landing_pitch: 0.5,
            landing_max_speed: 10.0,
            landing_sink: -0.5,
            to_landing: 3.0,
            from_landing: 0.5,
        }
    }

    fn fly(g: &Glider, s: &mut Glide, stick: Vec2, brake: bool, height: f32, seconds: f32) -> f32 {
        let dt = 1.0 / 64.0;
        let mut sink = 0.0;
        for _ in 0..(seconds / dt) as usize {
            sink = s.step(g, stick, brake, height, 1.0, dt);
        }
        sink
    }

    #[test]
    fn a_glider_left_alone_cruises_level_and_sinks() {
        let g = wing();
        let mut s = Glide { speed: 25.0, ..default() };
        let sink = fly(&g, &mut s, Vec2::ZERO, false, 100.0, 6.0);
        assert!((s.speed - g.base_speed).abs() < 0.1, "{s:?}");
        assert_eq!(sink, g.sink);
        assert!(s.pitch.abs() < 1e-3 && s.roll.abs() < 1e-3 && s.turn.abs() < 1e-3, "{s:?}");
        let v = s.velocity(sink);
        assert!((v.z + g.base_speed).abs() < 0.1 && (v.y - g.sink).abs() < 0.01, "{v:?}");
        // Broken, it drops; low, it floats.
        assert_eq!(s.step(&g, Vec2::ZERO, false, 100.0, 0.1, 0.01), g.broken_sink);
        assert_eq!(s.step(&g, Vec2::ZERO, false, 5.0, 1.0, 0.01), g.low_sink);
    }

    #[test]
    fn a_glider_dives_for_speed_and_stalls_out_of_a_climb() {
        let g = wing();
        let mut s = Glide { speed: g.base_speed, flown: g.takeoff, ..default() };
        // Nose down: its pitch, then faster, to its top.
        fly(&g, &mut s, Vec2::new(0.0, 1.0), false, 100.0, 1.0);
        assert!(s.pitch < -0.8 && s.speed > g.base_speed + 3.0, "{s:?}");
        fly(&g, &mut s, Vec2::new(0.0, 1.0), false, 100.0, 5.0);
        assert_eq!(s.speed, g.max_speed);
        assert!(s.velocity(g.sink).y < -15.0);
        // Pulled up, it climbs and bleeds speed until the nose drops back.
        fly(&g, &mut s, Vec2::new(0.0, -1.0), false, 100.0, 1.0);
        assert!(s.pitch > 0.5 && s.velocity(g.sink).y > 5.0, "{s:?}");
        fly(&g, &mut s, Vec2::new(0.0, -1.0), false, 100.0, 6.0);
        assert!(s.pitch < 0.1, "{s:?}");
        // The brake holds it to its floor.
        let mut s = Glide { speed: g.base_speed, flown: g.takeoff, ..default() };
        fly(&g, &mut s, Vec2::ZERO, true, 100.0, 8.0);
        assert!((s.speed - g.brake_floor).abs() < 0.1, "{s:?}");
        fly(&g, &mut s, Vec2::ZERO, false, 100.0, 4.0);
        assert!((s.speed - g.base_speed).abs() < 0.1, "{s:?}");
    }

    #[test]
    fn a_glider_banks_into_its_turns_and_lands_gently() {
        let g = wing();
        let mut s = Glide { speed: g.base_speed, ..default() };
        // The take-off run ignores the stick.
        fly(&g, &mut s, Vec2::new(1.0, 0.0), false, 100.0, 1.0);
        assert_eq!((s.roll, s.yaw), (0.0, 0.0));
        // Then banking left turns it left, up to the bank's turn rate.
        fly(&g, &mut s, Vec2::new(1.0, 0.0), false, 100.0, 5.0);
        assert!((s.roll - g.max_roll).abs() < 0.01, "{s:?}");
        assert!((s.turn - g.max_roll * 2.0).abs() < 0.05 && s.yaw > 1.0, "{s:?}");
        let left = s.attitude() * Vec3::X;
        assert!(left.y > 0.5, "the right wing rises banking left: {left:?}");
        // Near the ground its landing figures take over.
        let mut s = Glide { speed: g.max_speed, flown: g.takeoff, ..default() };
        fly(&g, &mut s, Vec2::new(0.0, 1.0), false, 5.0, 4.0);
        assert_eq!(s.landing, 1.0);
        assert!(s.speed <= g.landing_max_speed && s.pitch > -g.landing_pitch - 0.05, "{s:?}");
    }

    fn vehicle() -> Vehicle {
        Vehicle {
            def: 1,
            spawn_id: 100,
            throttle: 0.0,
            steer: 0.0,
            brake: false,
            handbrake: false,
            min: Vec3::splat(-1.0),
            max: Vec3::ONE,
            occupied: false,
            parked: true,
            body: Entity::PLACEHOLDER,
            vehicle_type: 0,
            authored_seats: true,
            recorded_speed: None,
            revs: 0.0,
            glide: Glide::default(),
            seats: vec![
                SeatSlot { kind: 1, index: 0, slot: 0, pose: Transform::from_xyz(-0.6, 0.8, 0.0), pawn: None },
                SeatSlot { kind: 3, index: 0, slot: 1, pose: Transform::from_xyz(0.6, 0.8, 0.0), pawn: None },
            ],
        }
    }

    fn player() -> Player {
        Player {
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            grounded: true,
            crouching: false,
            swimming: false,
            noclip: false,
            spawning: false,
            keep_height: false,
            last_landing_speed: 0.0,
            sprinting: false,
            spawned_at: 0.0,
            driving: false,
            armed_passenger: false,
            eye: crate::player::EYE_HEIGHT,
            air: crate::player::AirMode::None,
            mantle: None,
            mounted: None,
            sliding: None,
            dragging: false,
        }
    }

    /// A character a script seats where the player sits has the player move
    /// over to a free seat (not the wheel); not when none is free, nor for a
    /// seat someone else has.
    #[test]
    fn the_player_moves_over_for_a_character_given_his_seat() {
        let mut v = vehicle();
        v.seats[0].pawn = Some(fc3_story::PLAYER);
        let request = SeatRequest { vehicle: 100, pawn: 200, kinds: Vec::new(), index: 0, alternate: false, exit: false, forced: false, skip_entry: false };
        assert_eq!(requested_seat(&v, &request), None);
        assert_eq!(move_over(&v, &request), Some((0, 1)));
        v.seats[1].pawn = Some(999);
        assert_eq!(move_over(&v, &request), None);
        v.seats[1].pawn = None;
        v.seats[0].pawn = Some(999);
        assert_eq!(move_over(&v, &request), None);
    }

    #[test]
    fn player_seat_selection_obeys_locks_and_occupancy_and_script_driver_cannot_replace_a_pawn() {
        let mut v = vehicle();
        let mut rules = fc3_story::vehicles::Rules::default();
        assert_eq!(v.available_seat(&rules), Some(0));
        rules.locked.insert((1, 0));
        assert_eq!(v.available_seat(&rules), Some(1));
        let request = SeatRequest { vehicle: 100, pawn: 200, kinds: vec![1], index: 0, alternate: false, exit: false, forced: false, skip_entry: false };
        assert_eq!(requested_seat(&v, &request), Some(0)); // script can put Dennis in player-locked seat
        v.seats[0].pawn = Some(999);
        assert_eq!(requested_seat(&v, &request), None);
        let alternate = SeatRequest { alternate: true, ..request.clone() };
        assert_eq!(requested_seat(&v, &alternate), Some(1));
        // No kind named: the index counts all the seats (the passenger's, 1).
        let second = SeatRequest { kinds: Vec::new(), index: 1, ..request.clone() };
        assert_eq!(requested_seat(&v, &second), Some(1));
        let missing = SeatRequest { kinds: Vec::new(), index: 5, ..request };
        assert_eq!(requested_seat(&v, &missing), None);
        v.seats[1].pawn = Some(fc3_story::PLAYER);
        assert_eq!(v.available_seat(&rules), None);
        v.seats[1].pawn = None;
        rules.unusable = true;
        assert_eq!(v.available_seat(&rules), None);
    }

    /// Forcing the player in counts the seats of the kind asked for alone
    /// (JAS_130's 4x4: passenger seat 1 is the bed, behind the front seat).
    #[test]
    fn forcing_the_player_in_counts_the_kinds_seats() {
        let seat = |kind, index, slot| SeatSlot { kind, index, slot, pose: Transform::IDENTITY, pawn: None };
        let v = Vehicle { seats: vec![seat(1, 0, 0), seat(3, 0, 1), seat(3, 1, 2), seat(3, 2, 3)], ..vehicle() };
        let mut requests = StorySeatRequests::default();
        requests.force_player(&["1".into(), "3".into(), "1".into(), "100".into()]);
        assert_eq!(requested_seat(&v, &requests.0[0]), Some(2));
        // (A script's own seat request counts all the seats.)
        let take = SeatRequest { forced: false, ..requests.0[0].clone() };
        assert_eq!(requested_seat(&v, &take), Some(1));
    }

    #[test]
    fn sit_point_composes_its_bind_hierarchy_and_rejects_missing_or_cyclic_bones() {
        let bone = |name_hash, parent, translation| cache::CachedBone { name_hash, parent, translation, rotation: Quat::IDENTITY.to_array(),
            scale: [1.0; 3], ..default() };
        let mut bones = vec![bone(1, -1, [10.0, 0.0, 0.0]), bone(2, 0, [0.0, 2.0, 0.0])];
        assert_eq!(seat_bone_pose(&bones, 2).unwrap().translation, Vec3::new(10.0, 2.0, 0.0));
        bones[0].rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2).to_array();
        bones[0].scale = [2.0; 3];
        bones[1].translation = [1.0, 0.0, 0.0];
        let pose = seat_bone_pose(&bones, 2).unwrap();
        assert!(pose.translation.abs_diff_eq(Vec3::new(10.0, 0.0, -2.0), 1e-5));
        assert!(pose.rotation.abs_diff_eq(Quat::from_array(bones[0].rotation), 1e-5));
        assert_eq!(pose.scale, Vec3::splat(2.0));
        assert!(seat_bone_pose(&bones, 3).is_none());
        bones[0].parent = 1;
        assert!(seat_bone_pose(&bones, 2).is_none());
    }

    #[test]
    fn forced_player_seat_uses_the_original_argument_order_and_rejects_invalid_requests() {
        let mut requests = StorySeatRequests::default();
        requests.force_player(&["1".into(), "3".into(), "0".into(), "100".into()]);
        assert_eq!(requests.0.len(), 1);
        let request = &requests.0[0];
        assert!(request.forced && !request.exit);
        assert_eq!((request.pawn, request.vehicle, request.index), (1, 100, 0));
        assert_eq!(request.kinds, vec![3]);
        for (pawn, kind, index) in [("200", "3", "0"), ("1", "9", "0"), ("1", "3", "-1")] {
            requests.force_player(&[pawn.into(), kind.into(), index.into(), "100".into()]);
            assert_eq!(requests.0.len(), 1);
        }
    }

    #[test]
    fn interact_boards_the_unlocked_passenger_and_respects_disabled_exit() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<Driving>().init_resource::<Repairing>()
            .init_resource::<ActionState>().init_resource::<crate::settings::Settings>()
            .init_resource::<crate::hud::HudState>().init_resource::<crate::terrain::Heightfield>().add_systems(Update, enter_exit);
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        story.domino().lua().load(r#"CAPI_Vehicle.SetIsSeatLocked("100", 1, 0, true)"#).exec().unwrap();
        app.insert_non_send(crate::story::StoryHost::new(story, None));
        let player = app.world_mut().spawn((player(), Transform::IDENTITY, Health::new(100.0))).id();
        let ve = app.world_mut().spawn((vehicle(), Transform::IDENTITY, Health::new(100.0), Wear::default())).id();
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, Some(ve));
        assert_eq!(app.world().resource::<Driving>().seat, 1);
        assert!(app.world().get::<Player>(player).unwrap().driving);
        assert!(app.world().get::<ColliderDisabled>(player).is_some());
        assert_eq!(app.world().get::<Vehicle>(ve).unwrap().seats[0].pawn, None);
        app.world().non_send::<crate::story::StoryHost>().story.domino().lua()
            .load(r#"SendRegisteredEventToEntity("100", "CVehicle_SetExitDisabled", {})"#).exec().unwrap();
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, Some(ve));
        app.world().non_send::<crate::story::StoryHost>().story.domino().lua()
            .load(r#"SendRegisteredEventToEntity("100", "CVehicle_SetExitEnabled", {})"#).exec().unwrap();
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, None);
        assert!(!app.world().get::<Player>(player).unwrap().driving);
        assert!(app.world().get::<ColliderDisabled>(player).is_none());
        assert_eq!(app.world().get::<Vehicle>(ve).unwrap().seats[1].pawn, None);
        assert!(app.world().get::<Transform>(player).unwrap().translation.x > 1.0, "passenger exits on the right");
        app.world_mut().resource_mut::<crate::hud::HudState>().prompt = None;
        app.world_mut().get_mut::<Transform>(player).unwrap().translation = Vec3::ZERO;
        app.world_mut().resource_mut::<ActionState>().press(Action::Interact);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, Some(ve));
        app.world_mut().despawn(ve);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, None);
        assert!(!app.world().get::<Player>(player).unwrap().driving);
        assert!(app.world().get::<ColliderDisabled>(player).is_none());
    }

    #[test]
    fn passenger_input_cannot_drive_and_vehicle_view_yields_to_cinematic_camera() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<Driving>().init_resource::<crate::cinema::Cinema>()
            .init_resource::<ActionState>().init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<avian3d::collider_tree::ColliderTrees>().add_systems(Update, seat_player);
        let player = app.world_mut().spawn((player(), Transform::IDENTITY)).id();
        let ve = app.world_mut().spawn((Vehicle { throttle: 0.4, steer: 0.2, ..vehicle() }, Transform::IDENTITY)).id();
        {
            let mut driving = app.world_mut().resource_mut::<Driving>();
            driving.vehicle = Some(ve); driving.seat = 1; driving.first_person = true;
        }
        {
            let mut actions = app.world_mut().resource_mut::<ActionState>();
            actions.movement = Vec2::new(1.0, 1.0); actions.press(Action::Jump);
        }
        app.update();
        let v = app.world().get::<Vehicle>(ve).unwrap();
        assert_eq!((v.throttle, v.steer, v.handbrake), (0.4, 0.2, false));
        let eye = app.world().get::<Transform>(player).unwrap().translation + Vec3::Y * (crate::player::EYE_HEIGHT - HALF_HEIGHT);
        assert!(eye.abs_diff_eq(Vec3::new(0.6, 1.52, 0.0), 1e-5));
        app.world_mut().resource_mut::<Driving>().seat = 0;
        app.update();
        let v = app.world().get::<Vehicle>(ve).unwrap();
        assert_eq!((v.throttle, v.steer, v.handbrake), (1.0, -1.0, true));
        let before = *app.world().get::<Transform>(player).unwrap();
        app.world_mut().resource_mut::<crate::cinema::Cinema>().view = Some((Transform::from_xyz(100.0, 200.0, 300.0), 70.0));
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::KeyV);
        app.world_mut().resource_mut::<ActionState>().movement = Vec2::ZERO;
        app.update();
        assert_eq!(*app.world().get::<Transform>(player).unwrap(), before);
        assert!(app.world().resource::<Driving>().first_person);
        assert_eq!(app.world().get::<Vehicle>(ve).unwrap().throttle, 1.0);
    }

    #[test]
    fn seated_clips_are_specific_to_the_model_and_seat_not_just_vehicle_kind() {
        let mut v = vehicle();
        v.def = def_named("Technical").unwrap();
        assert!(v.first_person_clip(0).unwrap().contains("driving_nodir_nowep_4x4tech"));
        assert!(v.first_person_clip(1).unwrap().contains("passenger_nodir_nowep_4x4tech"));
        v.seats[1].kind = 2;
        assert!(v.first_person_clip(1).is_none());
        v.seats[1].kind = 3;
        v.seats[1].index = 4;
        assert!(v.first_person_clip(1).is_none(), "do not substitute the cab pose for the bed");
        assert!(v.first_person_clip(10).is_none());
        assert!(seated_clip(def_named("Jet ski").unwrap(), 1, 0).unwrap().contains("jetski"));
        assert!(seated_clip(def_named("Quad bike").unwrap(), 1, 0).unwrap().contains("atv"));
        assert!(seated_clip(def_named("4x4").unwrap(), 3, 2).unwrap().contains("backseat"));
    }

    #[test]
    fn seated_camera_uses_the_clip_eye_and_keeps_vehicle_pitch_roll_and_free_look() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<Driving>().init_resource::<crate::cinema::Cinema>()
            .init_resource::<ActionState>().init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<avian3d::collider_tree::ColliderTrees>().add_systems(Update, seat_player);
        let player = app.world_mut().spawn((player(), Transform::IDENTITY)).id();
        let v = vehicle();
        let clip = v.first_person_clip(1).unwrap();
        let eye = Vec3::new(0.5, 1.45, -0.25);
        app.insert_resource(SeatCameras(std::collections::HashMap::from([(clip, eye)])));
        let vt = Transform::from_xyz(10.0, 20.0, 30.0).with_rotation(Quat::from_euler(EulerRot::YXZ, 0.0, 0.15, -0.1));
        let ve = app.world_mut().spawn((v, vt)).id();
        {
            let mut driving = app.world_mut().resource_mut::<Driving>();
            driving.vehicle = Some(ve); driving.seat = 1; driving.first_person = true;
        }
        app.world_mut().get_mut::<Player>(player).unwrap().yaw = 0.4;
        app.update();
        let pose = app.world().get::<Transform>(player).unwrap();
        let world_eye = pose.transform_point(Vec3::Y * (crate::player::EYE_HEIGHT - HALF_HEIGHT));
        assert!(world_eye.abs_diff_eq(vt.transform_point(eye), 1e-5));
        assert!(pose.rotation.abs_diff_eq(vt.rotation * Quat::from_rotation_y(0.4), 1e-5));
        let camera = vt.transform_point(eye);
        app.world_mut().get_mut::<Player>(player).unwrap().yaw += 0.3;
        app.update();
        let pose = app.world().get::<Transform>(player).unwrap();
        assert!(pose.transform_point(Vec3::Y * (crate::player::EYE_HEIGHT - HALF_HEIGHT)).abs_diff_eq(camera, 1e-5));
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_seated_clips_have_authored_camera_bones_and_the_technical_passenger_matches_its_sitpoint() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let skel = fc3_formats::anim::parse_skeleton(&std::fs::read(root.join(cache::skeleton_rel(
            r"graphics\actors_\_skeleton_master\hips_ref.skeleton"))).unwrap()).unwrap();
        for def in 0..DEFS.len() {
            for (kind, index) in [(1, 0), (3, 0)] {
                let Some(path) = seated_clip(def, kind, index) else { continue };
                let clip = fc3_formats::anim::parse_clip(&std::fs::read(root.join(cache::anim_rel(
                    &format!(r"graphics\characters\_common\animations\{path}")))).unwrap()).unwrap();
                let eye = clip_eye(&skel, &clip).expect(path);
                assert!(eye.is_finite());
                assert!(eye.length() < 10.0, "{path}: {eye:?}");
                if DEFS[def].name == "Technical" && kind == 3 {
                    let start = Vec3::from_array(clip.root_motion.as_ref().unwrap().start_translation);
                    assert!(start.abs_diff_eq(Vec3::new(0.46, 0.858, -0.233), 0.001));
                    assert!(eye.y > start.y + 0.4 && eye.y < start.y + 1.0, "eye {eye:?}, sit {start:?}");
                }
            }
        }
    }

    /// JAS_130's climb (`car_chase`) is made in the 4x4's own frame, as its
    /// seats' clips are: it starts at the bed seat's eye and ends at the
    /// front seat's.
    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_4x4_climb_runs_from_the_bed_seat_to_the_front_seat() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let skel =
            fc3_formats::anim::parse_skeleton(&std::fs::read(root.join(cache::skeleton_rel(r"graphics\actors_\_skeleton_master\hips_ref.skeleton"))).unwrap())
                .unwrap();
        let load = |p: &str| fc3_formats::anim::parse_clip(&std::fs::read(root.join(cache::anim_rel(p))).unwrap()).unwrap();
        let seat = |kind, index| {
            let path = seated_clip(def_named("4x4").unwrap(), kind, index).unwrap();
            clip_eye(&skel, &load(&format!(r"graphics\characters\_common\animations\{path}"))).unwrap()
        };
        let climb = load(crate::car_chase::CLIP);
        let eye =
            |t| (crate::cinema::clip_root(&climb, t).to_matrix() * crate::cinema::bone_world(&skel, &climb, t, "Camera").unwrap()).transform_point3(Vec3::ZERO);
        assert!(eye(0.0).distance(seat(3, 1)) < 0.1, "{} from the bed's {}", eye(0.0), seat(3, 1));
        assert!(eye(climb.duration).distance(seat(3, 0)) < 0.1, "{} from the front's {}", eye(climb.duration), seat(3, 0));
    }

    #[test]
    fn animated_vehicle_stops_physics_velocity_and_returns_to_dynamic_when_released() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<crate::cinema::Cinema>().add_systems(Update, sync_vehicle_motion);
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        story.domino().lua().load(r#"SendRegisteredEventToEntity("100", "CVehicle_SetAsAnimated", {})"#).exec().unwrap();
        app.insert_non_send(crate::story::StoryHost::new(story, None));
        let ve = app.world_mut().spawn((vehicle(), RigidBody::Dynamic, LinearVelocity(Vec3::ONE), AngularVelocity(Vec3::ONE))).id();
        app.update();
        assert_eq!(*app.world().get::<RigidBody>(ve).unwrap(), RigidBody::Kinematic);
        assert_eq!(app.world().get::<LinearVelocity>(ve).unwrap().0, Vec3::ZERO);
        assert_eq!(app.world().get::<AngularVelocity>(ve).unwrap().0, Vec3::ZERO);
        app.world_mut().get_mut::<Vehicle>(ve).unwrap().recorded_controls(0.7, -0.3, 11.0);
        app.update();
        assert_eq!(app.world().get::<Vehicle>(ve).unwrap().recorded_speed, Some(11.0));
        app.world().non_send::<crate::story::StoryHost>().story.domino().lua()
            .load(r#"SendRegisteredEventToEntity("100", "CVehicle_SetAsNotAnimated", {})"#).exec().unwrap();
        app.update();
        assert_eq!(*app.world().get::<RigidBody>(ve).unwrap(), RigidBody::Dynamic);
        let v = app.world().get::<Vehicle>(ve).unwrap();
        assert_eq!((v.recorded_speed, v.throttle, v.steer), (None, 0.0, 0.0));
        app.world_mut().get_mut::<LinearVelocity>(ve).unwrap().0 = Vec3::ONE;
        app.update();
        assert_eq!(app.world().get::<LinearVelocity>(ve).unwrap().0, Vec3::ONE);
    }

    #[test]
    fn scripted_path_waits_for_a_physical_driver_and_motion_then_reports_arrival_once() {
        use crate::vehicle_paths::{StoryVehiclePaths, drive_paths, story_paths};
        use fc3_story::data::{Entry, Origin, StoryData};
        let mut data = StoryData::default();
        for id in [100, 200] {
            let fields = if id == 200 { fc3_formats::fcb::Object { fields: vec![(fc3_formats::patrols::SHAPE_POINTS,
                2u32.to_le_bytes().into_iter().chain([0.0f32, 0.0, 0.0, 0.0, 10.0, 0.0].into_iter().flat_map(f32::to_le_bytes)).collect())], ..default() } }
                else { fc3_formats::fcb::Object::default() };
            data.entities.insert(id, Entry { id, name: format!("fixture{id}"), class: "CBasicShapeEntity".into(), pos: [0.0; 3], angles: [0.0; 3],
                layer: fc3_formats::world::DEFAULT_MISSION_LAYER, origin: Origin::Global,
                components: std::sync::Arc::new(fc3_formats::fcb::Object::default()), fields: std::sync::Arc::new(fields) });
        }
        let mut paths = StoryVehiclePaths::default();
        paths.native("CAPI_Vehicle.StartVehicleFollowPath", &["100".into(), "200".into(), "11".into()], &data);
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, data).unwrap();
        story.domino().lua().load(r#"
            arrivals = { n = 0, _type = { Finished = function(self) self.n = self.n + 1 end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("100", arrivals, "Finished", "FollowScriptedPathFinished")
        "#).exec().unwrap();
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).insert_resource(paths).init_resource::<crate::cinema::Cinema>()
            .init_resource::<avian3d::collider_tree::ColliderTrees>().insert_non_send(crate::story::StoryHost::new(story, None))
            .add_systems(Update, (drive_paths, story_paths).chain());
        let ve = app.world_mut().spawn((vehicle(), Transform::IDENTITY, LinearVelocity::ZERO, RigidBody::Dynamic)).id();
        let arrivals = |app: &App| app.world().non_send::<crate::story::StoryHost>().story.domino().lua().globals()
            .get::<mlua::Table>("arrivals").unwrap().get::<u32>("n").unwrap();
        app.update();
        assert_eq!(arrivals(&app), 0);
        assert_eq!(app.world().get::<Vehicle>(ve).unwrap().throttle, 0.0);
        app.world_mut().get_mut::<Vehicle>(ve).unwrap().seats[0].pawn = Some(300);
        app.world_mut().entity_mut(ve).insert(RigidBody::Kinematic);
        app.update(); assert_eq!(arrivals(&app), 0);
        app.world_mut().entity_mut(ve).insert(RigidBody::Dynamic);
        app.update();
        assert!(app.world().get::<Vehicle>(ve).unwrap().throttle > 0.0);
        for _ in 0..10 { app.update(); }
        assert_eq!(arrivals(&app), 0, "elapsed time cannot substitute for movement");
        app.world_mut().get_mut::<Transform>(ve).unwrap().translation.z = -9.0;
        app.update(); assert_eq!(arrivals(&app), 1);
        for _ in 0..10 { app.update(); }
        assert_eq!(arrivals(&app), 1);
        assert!(app.world().get::<Vehicle>(ve).unwrap().brake);
        app.world().non_send::<crate::story::StoryHost>().story.domino().lua().load(r#"CAPI_Entity.RemoveEntity("100")"#).exec().unwrap();
        app.update();
        assert!(!app.world().resource::<StoryVehiclePaths>().owns_controls(100));
        assert!(app.world().get::<Vehicle>(ve).unwrap().brake);
        assert_eq!(arrivals(&app), 1);
    }

    #[test]
    fn mission_vehicle_outlives_source_sector_and_distance_cleanup_but_not_its_layer() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<crate::mission_layers::MissionLayers>()
            .add_systems(Update, (despawn_vehicles, despawn_mission_vehicles).chain());
        app.world_mut().spawn((player(), Transform::IDENTITY));
        app.world_mut().resource_mut::<crate::mission_layers::MissionLayers>().present.insert(500);
        let sector = app.world_mut().spawn(Transform::IDENTITY).id();
        let mission = app.world_mut().spawn((vehicle(), MissionVehicle(500), Transform::from_xyz(1000.0, 0.0, 0.0))).id();
        let ordinary = app.world_mut().spawn((vehicle(), Transform::from_xyz(1000.0, 0.0, 0.0))).id();
        app.update();
        assert!(app.world().get::<Vehicle>(mission).is_some());
        assert!(app.world().get::<Vehicle>(ordinary).is_none());
        app.world_mut().despawn(sector);
        app.update();
        assert!(app.world().get::<Vehicle>(mission).is_some());
        app.world_mut().resource_mut::<crate::mission_layers::MissionLayers>().present.clear();
        app.update();
        assert!(app.world().get::<Vehicle>(mission).is_none());
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_mission_technical_uses_its_authored_model_and_seven_seat_bones() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let data = fc3_story::data::StoryData::load(&root).unwrap();
        let entry = data.entities.get(&2076001291963743153).unwrap();
        let layout = fc3_story::vehicles::Layout::from_entry(entry).unwrap();
        assert_eq!(layout.animation_type, 10);
        assert_eq!(layout.vehicle_type, 0);
        assert_eq!(layout.seats.len(), 7);
        assert_eq!(layout.seats.iter().map(|s| (s.kind, s.index)).collect::<Vec<_>>(), vec![(1, 0), (3, 0), (2, 0), (3, 1), (3, 2), (3, 3), (3, 4)]);
        let def = &DEFS[placed_definition(&cache::model_rel(&DEFS[1].model_path())).unwrap()];
        assert_eq!(def.name, "Technical");
        let model = cache::CachedModel::from_bytes(&std::fs::read(root.join(cache::model_rel(&def.model_path()))).unwrap()).unwrap();
        for s in &layout.seats { assert!(seat_bone_pose(&model.bones, s.bone).is_some(), "missing seat bone {:#x}", s.bone); }
    }

    #[test]
    fn scripted_driver_waits_for_streaming_transfers_once_and_releases_stale_seats() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).init_resource::<Driving>().init_resource::<StorySeatRequests>().init_resource::<crate::cinema::Cinema>()
            .add_systems(Update, (story_seats, seat_story_actors).chain());
        let mut data = fc3_story::data::StoryData::default();
        for id in [100, 101, 200] {
            data.entities.insert(id, fc3_story::data::Entry { id, name: id.to_string(), class: "CEntity".into(), pos: [0.0; 3], angles: [0.0; 3],
                layer: fc3_formats::world::DEFAULT_MISSION_LAYER, origin: fc3_story::data::Origin::Global,
                components: std::sync::Arc::new(fc3_formats::fcb::Object::default()), fields: std::sync::Arc::new(fc3_formats::fcb::Object::default()) });
        }
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, data).unwrap();
        story.domino().lua().load(r#"CAPI_Vehicle.SetIsSeatLocked("100", 1, 0, true)"#).exec().unwrap();
        app.insert_non_send(crate::story::StoryHost::new(story, None));
        let first = app.world_mut().spawn((vehicle(), Transform::IDENTITY)).id();
        let second = app.world_mut().spawn((Vehicle { spawn_id: 101, ..vehicle() }, Transform::from_xyz(10.0, 0.0, 0.0))).id();
        let board = |app: &mut App, id: u64| app.world_mut().resource_mut::<StorySeatRequests>().native("CAPI_Vehicle.VehicleUseTakeSeat",
            &[id.to_string(), "200".into(), "0".into(), "true".into(), "false".into(), "false".into(), "false".into(), "false".into()]);
        board(&mut app, 100);
        app.update();
        assert_eq!(app.world().resource::<StorySeatRequests>().0.len(), 1);
        assert_eq!(app.world().get::<Vehicle>(first).unwrap().seats[0].pawn, None);
        let actor = app.world_mut().spawn((crate::story_actors::StoryActor::new(200, None, false), Transform::IDENTITY)).id();
        app.update();
        assert!(app.world().resource::<StorySeatRequests>().0.is_empty());
        assert_eq!(app.world().get::<Vehicle>(first).unwrap().seats[0].pawn, Some(200));
        assert!(app.world().get::<crate::story_actors::StoryActor>(actor).unwrap().seated);
        assert!(app.world().get::<ColliderDisabled>(actor).is_some());
        assert_eq!(app.world().get::<Transform>(actor).unwrap().translation, Vec3::new(-0.6, 0.8, 0.0));
        board(&mut app, 101);
        app.update();
        assert_eq!(app.world().get::<Vehicle>(first).unwrap().seats[0].pawn, None);
        assert_eq!(app.world().get::<Vehicle>(second).unwrap().seats[0].pawn, Some(200));
        assert_eq!(app.world().get::<Transform>(actor).unwrap().translation, Vec3::new(9.4, 0.8, 0.0));
        app.world_mut().resource_mut::<StorySeatRequests>().native("CAPI_Vehicle.VehicleUseExit", &["101".into(), "200".into()]);
        app.update();
        assert!(!app.world().get::<crate::story_actors::StoryActor>(actor).unwrap().seated);
        assert!(app.world().get::<ColliderDisabled>(actor).is_none());
        board(&mut app, 101);
        app.update();
        app.world_mut().despawn(actor);
        app.update();
        assert_eq!(app.world().get::<Vehicle>(second).unwrap().seats[0].pawn, None);
        assert!(!app.world().get::<Vehicle>(second).unwrap().occupied);
        let player = app.world_mut().spawn((player(), Transform::IDENTITY)).id();
        app.world().non_send::<crate::story::StoryHost>().story.domino().lua()
            .load(r#"CAPI_Vehicle.SetIsSeatLocked("100", 3, 0, true); SendRegisteredEventToEntity("100", "CVehicle_SetAsUnusable", {})"#).exec().unwrap();
        app.world_mut().resource_mut::<StorySeatRequests>().force_player(&["1".into(), "3".into(), "0".into(), "100".into()]);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, Some(first));
        assert_eq!(app.world().get::<Vehicle>(first).unwrap().seats[1].pawn, Some(fc3_story::PLAYER));
        assert!(app.world().get::<Player>(player).unwrap().driving);
        app.world_mut().resource_mut::<StorySeatRequests>().force_player(&["1".into(), "3".into(), "0".into(), "101".into()]);
        app.update();
        assert_eq!(app.world().resource::<Driving>().vehicle, Some(second));
        assert_eq!(app.world().get::<Vehicle>(first).unwrap().seats[1].pawn, None);
        assert_eq!(app.world().get::<Vehicle>(second).unwrap().seats[1].pawn, Some(fc3_story::PLAYER));
    }
}

/// A vehicle part that turns: a wheel rolls with the ground and the front ones
/// steer; the steering wheel turns about its column.
#[derive(Component)]
struct TurningPart {
    kind: Turning,
    spin: f32,
}

#[derive(Clone, Copy)]
enum Turning {
    Wheel {
        front: bool,
        radius: f32,
    },
    /// The column's model-space orientation.
    Steering(Quat),
}

/// How far the front wheels and the steering wheel turn at full lock.
const WHEEL_LOCK: f32 = 0.5;
const STEERING_LOCK: f32 = 2.4;

fn turn_wheels(
    time: Res<Time>,
    vehicles: Query<(&Transform, &Vehicle, &LinearVelocity, &Children)>,
    mut parts: Query<(&mut Transform, &mut TurningPart), Without<Vehicle>>,
) {
    let dt = time.delta_secs();
    for (t, v, vel, children) in &vehicles {
        let speed = v.recorded_speed.unwrap_or_else(|| vel.0.dot(t.rotation * Vec3::NEG_Z));
        let steer = v.steer;
        for child in children.iter() {
            let Ok((mut pt, mut part)) = parts.get_mut(child) else { continue };
            match part.kind {
                Turning::Wheel { front, radius } => {
                    // Rolling forward turns the wheel's top forward (-Z): negative about +X.
                    part.spin = (part.spin - speed * dt / radius).rem_euclid(std::f32::consts::TAU);
                    // Steer is positive to the left, a turn about +Y.
                    let yaw = if front { steer * WHEEL_LOCK } else { 0.0 };
                    pt.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(part.spin);
                }
                Turning::Steering(column) => {
                    pt.rotation = column * Quat::from_rotation_z(steer * STEERING_LOCK) * column.inverse();
                }
            }
        }
    }
}

fn despawn_vehicles(
    players: Query<&Transform, With<Player>>,
    vehicles: Query<(Entity, &Transform, &Vehicle), (Without<crate::patrols::PatrolDriver>, Without<MissionVehicle>)>,
    mut commands: Commands,
) {
    let Ok(pt) = players.single() else { return };
    for (e, t, v) in &vehicles {
        if !v.occupied && t.translation.distance(pt.translation) > 320.0 {
            commands.entity(e).despawn();
        }
    }
}

fn despawn_mission_vehicles(
    layers: Res<crate::mission_layers::MissionLayers>,
    host: Option<NonSend<crate::story::StoryHost>>,
    vehicles: Query<(Entity, &Vehicle, &MissionVehicle)>,
    mut commands: Commands,
) {
    for (entity, vehicle, layer) in &vehicles {
        if !layers.present.contains(&layer.0) || host.as_ref().is_some_and(|h| !h.story.is_loaded(vehicle.spawn_id)) {
            commands.entity(entity).despawn();
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// The vehicle being repaired (E held at a damaged one), for the arms.
#[derive(Resource, Default)]
pub struct Repairing {
    pub vehicle: Option<Entity>,
    /// How long E has been held at a damaged vehicle.
    held: f32,
}

/// Holding E this long at a damaged vehicle repairs it (a tap still gets in).
const REPAIR_HOLD: f32 = 0.3;
/// Share of its armour the torch restores per second.
const REPAIR_RATE: f32 = 0.15;

#[allow(clippy::too_many_arguments)]
fn enter_exit(
    time: Res<Time>,
    mut repairing: ResMut<Repairing>,
    healths: Query<&Health, (With<Vehicle>, Without<Player>)>,
    mut actions: ResMut<ActionState>,
    (settings, text): (Res<crate::settings::Settings>, Option<Res<crate::hud::GameText>>),
    mut driving: ResMut<Driving>,
    mut hud: ResMut<crate::hud::HudState>,
    heights: Res<crate::terrain::Heightfield>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &Health), Without<Vehicle>>,
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle), Without<Player>>,
    velocities: Query<&LinearVelocity, With<Vehicle>>,
    wear: Query<&Wear>,
    crewed: Query<(), With<crate::patrols::PatrolDriver>>,
    host: Option<NonSend<crate::story::StoryHost>>,
    mut commands: Commands,
) {
    let Ok((pe, mut pt, mut p, h)) = players.single_mut() else { return };
    let key = actions.label(Action::Interact, &settings);
    // Repairing only while E stays held at the vehicle (set again below).
    let was_repairing = repairing.vehicle.take();
    if let Some(ve) = driving.vehicle {
        // Out of a burnt-out wreck whatever the key.
        let locked_exit = vehicles.get(ve).ok().is_some_and(|(_, _, v)| host.as_ref().is_some_and(|h| h.story.vehicle_rules(v.spawn_id).exit_disabled));
        let exit = vehicles.get(ve).is_err() || (actions.just_pressed(Action::Interact) && !locked_exit) || h.dead || wear.get(ve).is_ok_and(|w| w.wrecked);
        if !exit {
            return;
        }
        actions.consume(Action::Interact);
        let mut carried = Vec3::ZERO;
        if let Ok((_, vt, mut v)) = vehicles.get_mut(ve) {
            let driver = v.seats.get(driving.seat).is_some_and(|s| s.kind == 1);
            if let Some(s) = v.seats.get_mut(driving.seat) { s.pawn = None; }
            v.occupied = v.seats.iter().any(|s| s.pawn.is_some());
            if driver {
                v.parked = true;
                v.throttle = 0.0;
                v.steer = 0.0;
                v.handbrake = false;
            }
            // Authored passengers leave on their own side, not through the
            // driver. Ordinary bounds-based seats keep the existing left exit.
            let seat = v.seats.get(driving.seat);
            let right = v.authored_seats && seat.is_some_and(|s| s.pose.translation.x > (v.min.x + v.max.x) * 0.5);
            let side = vt.rotation * Vec3::new(if right { v.max.x + 1.2 } else { v.min.x - 1.2 }, 0.0,
                if v.authored_seats { seat.map_or(0.0, |s| s.pose.translation.z) } else { 0.0 });
            let mut at = vt.translation + Vec3::new(side.x, 0.0, side.z);
            let ground = heights.height_at(at).unwrap_or(at.y).max(crate::water::surface_at(at, &heights) - 1.0);
            at.y = ground + HALF_HEIGHT + 0.3;
            // Off a hang glider in flight: dropped from under its wing, still
            // moving with it.
            if v.kind() == Kind::Glider && vt.translation.y - ground > GLIDER_DROP {
                at = vt.translation - Vec3::Y * (HALF_HEIGHT + 1.0);
                carried = velocities.get(ve).map_or(Vec3::ZERO, |lv| lv.0);
            }
            pt.translation = at;
        }
        p.driving = false;
        p.velocity = carried;
        driving.vehicle = None;
        commands.entity(pe).remove::<ColliderDisabled>();
        return;
    }
    // On a ladder, a zip line or a mounted gun.
    if h.dead || (hud.prompt.is_some() && was_repairing.is_none()) || p.driving || p.mounted.is_some() {
        repairing.held = 0.0;
        return;
    }
    // (Not a wreck, nor one an enemy crew is still driving.)
    let best = vehicles
        .iter()
        .filter(|(e, _, _)| !wear.get(*e).is_ok_and(|w| w.wrecked) && !crewed.contains(*e))
        .filter(|(_, _, v)| v.available_seat(&host.as_ref().map(|h| h.story.vehicle_rules(v.spawn_id)).unwrap_or_default()).is_some())
        .map(|(e, t, v)| (e, t.translation.distance(pt.translation), v))
        .filter(|(_, d, v)| *d < (v.max - v.min).length() * 0.5 + 1.8)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((ve, _, v)) = best else {
        repairing.held = 0.0;
        return;
    };
    let name = DEFS[v.def].name.to_lowercase();
    let rules = host.as_ref().map(|h| h.story.vehicle_rules(v.spawn_id)).unwrap_or_default();
    let Some(seat) = v.available_seat(&rules) else { return };
    let verb = if v.seats[seat].kind == 1 { "drive" } else { "enter" };
    // A damaged one (not past saving): a tap gets in, holding repairs it.
    let damaged = healths.get(ve).is_ok_and(|h| !h.dead && h.current < h.max - 0.5);
    if damaged {
        if actions.pressed(Action::Interact) && (repairing.held > 0.0 || actions.just_pressed(Action::Interact)) {
            actions.consume(Action::Interact);
            repairing.held += time.delta_secs();
            if repairing.held >= REPAIR_HOLD {
                repairing.vehicle = Some(ve);
                hud.prompt = Some(format!("repairing the {name}"));
                return;
            }
            hud.prompt = Some(format!("{key}  {verb} {name}    hold {key}  repair"));
            return;
        }
        hud.prompt = Some(format!("{key}  {verb} {name}    hold {key}  repair"));
        let tapped = repairing.held > 0.0 && repairing.held < REPAIR_HOLD;
        repairing.held = 0.0;
        if !tapped {
            return;
        }
    } else {
        repairing.held = 0.0;
        let glider =
            (DEFS[v.def].kind == Kind::Glider).then(|| text.as_deref().and_then(|t| t.prompt(crate::hud::prompt::BOARD_GLIDER, &actions, &settings))).flatten();
        hud.prompt = Some(glider.unwrap_or_else(|| format!("{key}  {verb} {name}")));
        if !actions.just_pressed(Action::Interact) {
            return;
        }
    }
    actions.consume(Action::Interact);
    commands.entity(ve).remove::<crate::patrols::PatrolDriver>();
    if let Ok((_, vt, mut v)) = vehicles.get_mut(ve) {
        v.seats[seat].pawn = Some(fc3_story::PLAYER);
        v.occupied = true;
        driving.last_yaw = vt.rotation.to_euler(EulerRot::YXZ).0;
        // Start looking the way the vehicle points.
        p.yaw = driving.last_yaw;
        p.pitch = -0.2;
        info!("vehicle: player boarded {} seat type {} index {}", v.spawn_id, v.seats[seat].kind, v.seats[seat].index);
    }
    driving.vehicle = Some(ve);
    driving.seat = seat;
    if host.is_some() { driving.first_person = true; }
    p.driving = true;
    commands.entity(pe).insert(ColliderDisabled);
}

/// Keeps the player in the vehicle: a chase camera behind it, or the driver's
/// seat (V toggles). The view turns with the vehicle.
fn seat_player(
    mut driving: ResMut<Driving>,
    cinema: Res<crate::cinema::Cinema>,
    keys: Res<ButtonInput<KeyCode>>,
    actions: Res<ActionState>,
    spatial: SpatialQuery,
    mut players: Query<(&mut Transform, &mut Player), Without<Vehicle>>,
    mut vehicles: Query<(&Transform, &mut Vehicle), Without<Player>>,
    cameras: Option<Res<SeatCameras>>,
    paths: Option<Res<crate::vehicle_paths::StoryVehiclePaths>>,
) {
    let Some(ve) = driving.vehicle else {
        if let Ok((_, mut p)) = players.single_mut() {
            p.armed_passenger = false;
        }
        return;
    };
    if cinema.view.is_some() {
        return;
    }
    if keys.just_pressed(KeyCode::KeyV) {
        driving.first_person = !driving.first_person;
    }
    let (Ok((mut pt, mut p)), Ok((vt, mut v))) = (players.single_mut(), vehicles.get_mut(ve)) else { return };
    // Seated, he isn't flying about (a test's hold up high, kept through a
    // scene, would hide his weapon and keep it from firing).
    p.noclip = false;
    let yaw = vt.rotation.to_euler(EulerRot::YXZ).0;
    let turn = (yaw - driving.last_yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    driving.last_yaw = yaw;
    p.yaw += turn;
    // At the wheel the head turns only as far as the game lets it (its
    // angles count down and right positive, ours up and left).
    let driver = v.seats.get(driving.seat).is_some_and(|s| s.kind == 1);
    // A passenger (not at the wheel or a mounted gun) keeps his weapon, and
    // aims and fires from the seat in the seat's own view.
    p.armed_passenger = driving.first_person && driving.carried.is_none() && v.seats.get(driving.seat).is_some_and(|s| s.kind == 3);
    if let Some([pitch_lo, pitch_hi, yaw_lo, yaw_hi]) = handling_of(v.def).map(|h| h.look).filter(|_| driving.first_person && driver) {
        if pitch_hi > pitch_lo {
            p.pitch = p.pitch.clamp(-pitch_hi.to_radians(), -pitch_lo.to_radians());
        }
        if yaw_hi > yaw_lo {
            let ahead = (p.yaw - yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            p.yaw = yaw + ahead.clamp(-yaw_hi.to_radians(), -yaw_lo.to_radians());
        }
    }
    // (Carried, he looks where the gameplay has him look.)
    if let Some((_, look, w)) = driving.carried {
        let ahead = (yaw + (-look.x).atan2(-look.z) - p.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        p.yaw += ahead * w;
        p.pitch += (look.y.clamp(-1.0, 1.0).asin() - p.pitch) * w;
    }
    let eye = if driving.first_person {
        vt.transform_point(if !v.authored_seats { v.seat() } else {
            v.first_person_clip(driving.seat).and_then(|clip| cameras.as_ref()?.0.get(clip).copied())
                .unwrap_or_else(|| v.seats.get(driving.seat).map_or(Vec3::ZERO, |s| s.pose.translation) + Vec3::Y * 0.72)
        })
    } else {
        // Behind and above, following where the player looks.
        let size = (v.max - v.min).length();
        let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch.min(0.2));
        let centre = vt.transform_point((v.min + v.max) * 0.5);
        let want = centre - look * Vec3::NEG_Z * (size * 1.3 + 2.0) + Vec3::Y * (size * 0.35 + 1.0);
        // Pull in when something is between the vehicle and the camera.
        let to = want - centre;
        let filter = SpatialQueryFilter::from_excluded_entities([ve, v.body]);
        match Dir3::new(to).ok().and_then(|d| spatial.cast_ray(centre, d, to.length(), true, &filter)) {
            Some(hit) => centre + to.normalize() * (hit.distance - 0.4).max(0.5),
            None => want,
        }
    };
    let eye = match driving.carried {
        Some((at, _, w)) if driving.first_person => eye.lerp(vt.transform_point(at), w),
        _ => eye,
    };
    pt.rotation = if driving.first_person { vt.rotation * Quat::from_rotation_y(p.yaw - yaw) } else { Quat::from_rotation_y(p.yaw) };
    pt.translation = eye - pt.rotation * Vec3::Y * (p.eye - HALF_HEIGHT);
    p.velocity = Vec3::ZERO;
    // Controls: throttle and steering (the camera looks freely).
    if v.seats.get(driving.seat).is_some_and(|s| s.kind == 1) && paths.as_ref().is_none_or(|p| !p.owns_controls(v.spawn_id)) {
        v.throttle = actions.movement.y;
        v.steer = -actions.movement.x;
        v.handbrake = actions.pressed(Action::Jump);
    }
}

/// Vehicle forces: suspension, drive and grip for cars; buoyancy and thrust
/// for boats; glide for hang gliders.
#[allow(clippy::too_many_arguments)]
/// How far over a car's middle the water must come to drown its engine.
const FLOOD_DEPTH: f32 = 0.3;

#[allow(clippy::type_complexity)]
fn drive(
    time: Res<Time>,
    spatial: SpatialQuery,
    heights: Res<crate::terrain::Heightfield>,
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle, &RigidBody, Forces, &ComputedMass, Has<crate::patrols::PatrolDriver>)>,
    healths: Query<&Health, With<Vehicle>>,
    mut commands: Commands,
    mut aloft: Local<std::collections::HashMap<Entity, f32>>,
) {
    let dt = time.delta_secs().max(1e-4);
    aloft.retain(|e, _| vehicles.contains(*e));
    for (e, t, mut v, body, mut f, mass, patrol) in &mut vehicles {
        if *body != RigidBody::Dynamic { continue; }
        let d = &DEFS[v.def];
        let tune = tuning(v.def);
        let m = mass.value();
        let up = t.rotation * Vec3::Y;
        // Model front is local -Z.
        let fwd = t.rotation * Vec3::NEG_Z;
        let right = t.rotation * Vec3::X;
        let vel = f.linear_velocity();
        let speed = vel.dot(fwd);
        match d.kind {
            Kind::Car => {
                let driven = tune.wheels.iter().filter(|w| w.driving).count().max(1) as f32;
                let filter = SpatialQueryFilter::from_excluded_entities([e, v.body]);
                // Deep water (the sea, a river) drowns the engine and drags at
                // the body; fords are fine. (Patrols keep to their routes,
                // which the game runs through rivers in places.)
                let centre = t.transform_point(Vec3::new(0.0, (v.min.y + v.max.y) * 0.5, 0.0));
                let flooded = !patrol && centre.y + FLOOD_DEPTH < crate::water::surface_at(centre, &heights);
                if flooded {
                    f.apply_force(-vel * m * 1.2);
                }
                let mut grounded = 0;
                for (i, w) in v.wheels().into_iter().enumerate() {
                    let wheel = tune.wheels[i];
                    let mount = t.transform_point(w);
                    let Ok(down) = Dir3::new(-up) else { continue };
                    let Some(hit) = spatial.cast_ray(mount, down, wheel.reach, true, &filter) else { continue };
                    grounded += 1;
                    let compression = (wheel.reach - hit.distance).max(0.0);
                    let point_vel = f.velocity_at_point(mount);
                    // The spring, and the damper going in or coming out.
                    let rate = point_vel.dot(up);
                    let damping = if rate < 0.0 { wheel.compression } else { wheel.relaxation };
                    let spring = m * (wheel.strength * compression - damping * rate);
                    f.apply_force_at_point(up * spring.max(0.0), mount);
                    // Front wheels steer (less at speed).
                    let front = i < 2;
                    let steer = if front { v.steer * tune.lock(speed) } else { 0.0 };
                    let wheel_fwd = Quat::from_axis_angle(up, steer) * fwd;
                    let wheel_right = Quat::from_axis_angle(up, steer) * right;
                    let normal = spring.max(0.0);
                    // The handbrake locks its wheels (the rear ones), which
                    // then hold less of their grip.
                    let locked = v.handbrake && wheel.handbrake;
                    let limit = normal * wheel.grip * if locked { tune.lock_friction } else { 1.0 };
                    // Lateral grip: cancel sideways slip, up to the friction limit.
                    let lat = point_vel.dot(wheel_right);
                    let grip = (-lat * m / 4.0 / dt * 0.35).clamp(-limit, limit);
                    f.apply_force_at_point(wheel_right * grip, mount);
                    // Drive on the driven wheels; braking and rolling resistance.
                    let along = point_vel.dot(wheel_fwd);
                    let mut drive = if flooded || !wheel.driving { 0.0 } else { tune.pull(speed, v.throttle) / driven };
                    let parking = v.parked && !v.seats.iter().any(|s| s.kind == 1 && s.pawn.is_some());
                    if v.brake || locked || parking || (v.throttle * speed < -0.5 && v.throttle.abs() > 0.1) {
                        drive = (-along * m / 4.0 / dt * 0.25).clamp(-limit, limit);
                    }
                    let rolling = -along * 30.0;
                    f.apply_force_at_point(wheel_fwd * (drive + rolling), mount);
                }
                // In the air: heavier (eased in, above a speed), and the
                // stabiliser (if it has one) turns it to land on its wheels.
                let air = aloft.entry(e).or_default();
                *air = if grounded == 0 { *air + dt } else { 0.0 };
                if grounded == 0 {
                    let ease = if tune.airborne_fade > 0.0 { (*air / tune.airborne_fade).min(1.0) } else { 1.0 };
                    if vel.length() >= tune.airborne_min_speed {
                        f.apply_force(Vec3::NEG_Y * m * 9.81 * (tune.airborne_gravity - 1.0) * ease);
                    }
                    let tilt = up.cross(Vec3::Y);
                    f.apply_torque(tilt * m * tune.stabilizer);
                }
                // Extra gravity, lift (or downforce) and air drag.
                f.apply_force(Vec3::NEG_Y * m * tune.extra_gravity + up * vel.length_squared() * tune.lift);
                f.apply_force(-vel * vel.length() * tune.drag);
            }
            Kind::Boat => {
                let hull = [
                    Vec3::new(v.min.x, v.min.y + 0.3, v.min.z),
                    Vec3::new(v.max.x, v.min.y + 0.3, v.min.z),
                    Vec3::new(v.min.x, v.min.y + 0.3, v.max.z),
                    Vec3::new(v.max.x, v.min.y + 0.3, v.max.z),
                ];
                let mut wet = 0;
                let mut afloat = hull.len();
                match tune.boat.filter(|b| b.sphere_count > 0) {
                    // As the engine floats it: on its buoyancy spheres
                    // (dropping as it gets going, so the hull rises), each
                    // one's cap in the water; the water's weight that much
                    // less its mass, up at the centre of buoyancy; and, at
                    // each one in the water, friction on its rising and
                    // falling, and on going down into it.
                    Some(b) => {
                        afloat = b.sphere_count;
                        let drop = Vec3::Y * b.sphere_drop * vel.length();
                        let (mut volume, mut centre) = (0.0, Vec3::ZERO);
                        for &(at, r) in &b.spheres[..b.sphere_count] {
                            let c = t.transform_point(at + drop);
                            let water = crate::water::surface_at(c, &heights);
                            let h = (water - (c.y - r)).clamp(0.0, 2.0 * r);
                            if h <= 0.0 {
                                continue;
                            }
                            wet += 1;
                            let sub = std::f32::consts::PI * h * h * (3.0 * r - h) / 3.0;
                            volume += sub;
                            centre += Vec3::new(c.x, c.y - r + h * 0.5, c.z) * sub;
                            let vy = f.velocity_at_point(c).y;
                            let full = 4.0 / 3.0 * std::f32::consts::PI * r * r * r;
                            let into = if vy < 0.0 { b.down * (sub / full) * vy * vy } else { 0.0 };
                            f.apply_force_at_point(Vec3::Y * (-b.updown * vy + into) * m, c);
                        }
                        if volume > 0.0 {
                            f.apply_force_at_point(Vec3::Y * (WATER_DENSITY * volume - m).max(0.0), centre / volume);
                        }
                        let g = if wet > 0 { b.gravity } else { b.gravity_air };
                        f.apply_force(Vec3::NEG_Y * m * 9.81 * (g - 1.0));
                        if wet > 0 {
                            let spin = f.angular_velocity();
                            f.apply_torque(-up * spin.dot(up) * b.yaw_friction * wet as f32 * m);
                        }
                        // The stabiliser rights it past its angle.
                        if b.stabilizer > 0.0 && up.angle_between(Vec3::Y) > b.stabilizer_angle {
                            f.apply_torque(up.cross(Vec3::Y).normalize_or_zero() * m * b.stabilizer);
                        }
                    }
                    None => {
                        // On the sea or a river or lake.
                        let water = crate::water::surface_at(t.translation, &heights);
                        for h in hull {
                            let p = t.transform_point(h);
                            let depth = water - p.y;
                            if depth > 0.0 {
                                wet += 1;
                                let pv = f.velocity_at_point(p);
                                let lift = (depth.min(1.2) * m * 9.81 / 4.0 / 0.35) - pv.y * m * 0.6;
                                f.apply_force_at_point(Vec3::Y * lift.max(0.0), p);
                            }
                        }
                    }
                }
                // The motor's revs come up with the throttle (the game's rate),
                // and fall away without it.
                let revs_to = v.throttle.abs();
                let rate = tune.boat.map_or(0.0, |b| b.rev_time);
                v.revs = if rate > 0.0 { v.revs + (revs_to - v.revs).clamp(-dt / rate, dt / rate) } else { revs_to };
                if wet > 0 {
                    let flat_fwd = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
                    let lat = vel.dot(right);
                    match tune.boat {
                        // The game's motor (thrust per unit of mass at full
                        // revs), rudder (its angle for the speed) and water
                        // friction ahead and to the side.
                        Some(b) => {
                            // Against the way it goes, the motor brakes at
                            // its braking power; and the brake, if it has one.
                            let against = v.throttle * speed < 0.0 && speed.abs() > 0.5;
                            let thrust = if against {
                                b.braking
                            } else if v.throttle >= 0.0 {
                                b.thrust
                            } else {
                                b.reverse
                            };
                            f.apply_force(flat_fwd * v.throttle.signum() * thrust * v.revs * m);
                            if (v.brake || v.handbrake) && b.brake > 0.0 && speed.abs() > 0.1 {
                                f.apply_force(-flat_fwd * speed.signum() * b.brake * m);
                            }
                            let k = (speed.abs() / b.rudder_top).min(1.0);
                            let rudder = (b.rudder_low + (b.rudder_high - b.rudder_low) * k) / RUDDER_REFERENCE;
                            f.apply_torque(Vec3::Y * v.steer * m * 1.6 * rudder * (0.3 + k).min(1.3));
                            // (As much of the hull as is in the water.)
                            let share = wet as f32 / afloat as f32;
                            let drag = if speed >= 0.0 { b.forward_drag } else { b.reverse_drag };
                            f.apply_force((-right * lat * b.side_drag - flat_fwd * speed * drag) * m * share);
                        }
                        None => {
                            if speed.abs() < d.top_speed || v.throttle * speed < 0.0 {
                                f.apply_force(flat_fwd * v.throttle * d.engine);
                            }
                            // Rudder works with way on.
                            f.apply_torque(Vec3::Y * v.steer * m * 1.6 * (0.3 + speed.abs() / d.top_speed).min(1.3));
                            // Water drag: strong sideways, light lengthways.
                            f.apply_force(-right * lat * m * 1.5 - flat_fwd * speed * m * 0.08);
                        }
                    }
                    // (Ours floats on its corners, righted by hand.)
                    if tune.boat.is_none_or(|b| b.sphere_count == 0) {
                        let tilt = up.cross(Vec3::Y);
                        f.apply_torque(tilt * m * 6.0);
                    }
                }
            }
            Kind::Glider => {
                // Left in flight (the pilot let go), it glides on by itself
                // until it comes down.
                if !v.occupied && v.glide.flown == 0.0 {
                    continue;
                }
                if let Some(g) = tune.glider {
                    // Off the ground (or the water) below.
                    let filter = SpatialQueryFilter::from_excluded_entities([e, v.body]);
                    let over_water = t.translation.y - crate::water::surface_at(t.translation, &heights);
                    let below = (t.translation.y - heights.height_at(t.translation).unwrap_or(f32::MIN)).min(over_water);
                    let height = spatial.cast_ray(t.translation, Dir3::NEG_Y, below.clamp(0.0, 200.0) + 1.0, true, &filter).map_or(below, |hit| hit.distance);
                    let health = healths.get(e).map_or(1.0, Health::fraction);
                    // The take-off run is the game's set move off the launch
                    // (it hangs off the slope it stands on): straight out at
                    // its height, through whatever it brushes.
                    let running = v.glide.flown < g.takeoff;
                    if v.glide.flown == 0.0 {
                        // Launched as it faces, at its cruising speed or faster.
                        v.glide.yaw = t.rotation.to_euler(EulerRot::YXZ).0;
                        v.glide.speed = vel.length().max(g.base_speed);
                        commands.entity(v.body).insert(ColliderDisabled);
                    }
                    // Down on the ground after its take-off run (held back to
                    // a crawl), or on the water, it has landed: it stops
                    // flying, levels out and settles, until it goes over an
                    // edge again. Left to itself, it is done.
                    let wet = over_water < GLIDER_WATER;
                    let flying = v.glide.flown > g.takeoff;
                    let held = flying && vel.length() < v.glide.speed * 0.5;
                    v.glide.held = if held { v.glide.held + dt } else { 0.0 };
                    if flying && (wet || v.glide.held > GLIDER_HELD || (held && height < GLIDER_LANDED)) {
                        v.glide.down = true;
                    } else if v.glide.down && vel.y < -GLIDER_RELAUNCH && height > GLIDER_LANDED * 2.0 && !wet {
                        // Over an edge: flying again, at the speed it fell at.
                        v.glide.down = false;
                        v.glide.speed = vel.length();
                    }
                    let landed = v.glide.down;
                    if landed && !v.occupied {
                        commands.entity(v.body).remove::<ColliderDisabled>();
                        v.glide = Glide::default();
                        continue;
                    }
                    if landed {
                        v.glide.speed = 0.0;
                        v.glide.roll = 0.0;
                        v.glide.pitch = 0.0;
                        v.glide.roll_rate = 0.0;
                        v.glide.pitch_rate = 0.0;
                        v.glide.turn = 0.0;
                    } else {
                        let (stick, brake) = if v.occupied { (Vec2::new(v.steer, v.throttle), v.brake || v.handbrake) } else { (Vec2::ZERO, false) };
                        let sink = v.glide.step(&g, stick, brake, height, health, dt);
                        *f.linear_velocity_mut() = v.glide.velocity(if running { 0.0 } else { sink });
                        if running && v.glide.flown >= g.takeoff {
                            commands.entity(v.body).remove::<ColliderDisabled>();
                        }
                    }
                    // Turn the body to its attitude: the turn rate ahead, and
                    // a steady pull the rest of the way.
                    let mut to = v.glide.attitude() * t.rotation.inverse();
                    if to.w < 0.0 {
                        to = -to;
                    }
                    *f.angular_velocity_mut() = Vec3::Y * v.glide.turn + to.to_scaled_axis() * GLIDER_SETTLE;
                    continue;
                }
                if !v.occupied {
                    continue;
                }
                // Steady glide: forward speed with a gentle sink; pull back to
                // slow and climb briefly, push to dive.
                let flat_fwd = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(Vec3::NEG_Z);
                let pitch = v.throttle;
                let target = flat_fwd * (d.top_speed + pitch * 6.0) + Vec3::Y * (-2.2 - pitch * 2.5);
                *f.linear_velocity_mut() = vel.lerp(target, (dt * 1.5).min(1.0));
                let ang = Vec3::Y * v.steer * 0.7;
                *f.angular_velocity_mut() = ang;
            }
        }
    }
}

/// Fast-moving cars hurt whatever they hit.
fn run_over(
    driving: Res<Driving>,
    vehicles: Query<(&Transform, &Vehicle, &LinearVelocity)>,
    targets: Query<(Entity, &GlobalTransform, &Health), Or<(With<crate::npc::Npc>, With<crate::wildlife::WildAnimal>)>>,
    players: Query<Entity, With<Player>>,
    mut damage: MessageWriter<DamageEvent>,
    mut cooldown: Local<std::collections::HashMap<Entity, f32>>,
    time: Res<Time>,
) {
    let Some(ve) = driving.vehicle else { return };
    let Ok((vt, v, lv)) = vehicles.get(ve) else { return };
    if v.kind() == Kind::Glider {
        return;
    }
    let speed = lv.0.length();
    cooldown.retain(|_, t| {
        *t -= time.delta_secs();
        *t > 0.0
    });
    if speed < 5.0 {
        return;
    }
    let reach = (v.max - v.min).length() * 0.5;
    let player = players.single().ok();
    for (e, gt, h) in &targets {
        if h.dead || cooldown.contains_key(&e) {
            continue;
        }
        let d = gt.translation() - vt.translation;
        if Vec2::new(d.x, d.z).length() < reach && d.y.abs() < 2.0 {
            damage.write(DamageEvent {
                target: e,
                amount: speed * 14.0,
                kind: DamageKind::Explosion,
                source: Some(vt.translation),
                instigator: player,
                headshot: false,
                hit: crate::combat::Hit::Whole,
            });
            cooldown.insert(e, 1.0);
        }
    }
}

fn vehicle_hud(
    driving: Res<Driving>,
    vehicles: Query<(&Vehicle, &LinearVelocity)>,
    actions: Res<ActionState>,
    settings: Res<crate::settings::Settings>,
    mut hud: ResMut<crate::hud::HudState>,
    host: Option<NonSend<crate::story::StoryHost>>,
) {
    let Some((v, lv)) = driving.vehicle.and_then(|e| vehicles.get(e).ok()) else { return };
    let kmh = lv.0.length() * 3.6;
    let role = if v.seats.get(driving.seat).is_some_and(|s| s.kind == 3) { "PASSENGER  " } else { "" };
    let exit = if host.as_ref().is_some_and(|h| h.story.vehicle_rules(v.spawn_id).exit_disabled) { "V view".into() }
        else { format!("{} get out, V view", actions.label(Action::Interact, &settings)) };
    hud.status = Some(format!("{role}{}   {:.0} km/h   ({exit})", DEFS[v.def].name.to_uppercase(), kmh));
}

// ------------------------------------------------------------------ sound ---

/// A vehicle's sounds from its archetype's sound settings in the entity
/// library (`fc3tool vehicle-sounds`): the engine (a blend of looping layers
/// that follows RPM and throttle), and for AI drivers the cheaper engine the
/// game gives them, ignition and switch-off, and the suspension landing hard.
struct Sounds {
    engine: u32,
    ai_engine: u32,
    ignition: u32,
    off: u32,
    suspension: [u32; 3],
    /// Hitting something hard (the archetype's unnamed `8841202C`: a
    /// random set of short, decaying impacts); land vehicles only.
    crash: u32,
}

fn sounds(def: &Def) -> Option<Sounds> {
    const SUSPENSION: [u32; 3] = [0x0005_2528, 0x0005_2529, 0x0005_252B];
    let s = |engine, ai_engine, ignition, off| Sounds { engine, ai_engine, ignition, off, suspension: SUSPENSION, crash: 0 };
    Some(match def.prefix {
        "vehicle/land/jeep4x4" => Sounds { crash: 0x0010_35C8, ..s(0x0005_2524, 0x0018_81C1, 0x0007_4EEC, 0x0007_4CDD) },
        "vehicle/land/technical" => Sounds { crash: 0x0010_36D0, ..s(0x0010_36CC, 0x0018_8412, 0x0010_36CB, 0x0010_36C8) },
        "vehicle/land/atv" => {
            Sounds { suspension: [0x0017_794B, 0x0017_7949, 0x0017_794A], crash: 0x0005_2520, ..s(0x0010_3750, 0x0010_3750, 0x0010_374B, 0x0010_375E) }
        }
        "vehicle/land/japansedan" => Sounds { crash: 0x0010_35C8, ..s(0x0018_979F, 0x0018_F5FE, 0x0018_9795, 0x0018_9797) },
        "vehicle/land/germanhatch" => Sounds { crash: 0x0010_35C8, ..s(0x0018_983C, 0x0018_F5CC, 0x0018_9835, 0x0018_987E) },
        "vehicle/land/dunebuggy" => Sounds { crash: 0x0010_37F2, ..s(0x0010_37F8, 0x0010_37F8, 0x0010_37F3, 0x0010_381E) },
        "vehicle/land/cargotruck" => Sounds { crash: 0x0000_545F, ..s(0x0010_3666, 0x001A_4D2A, 0x0010_3670, 0x0010_3671) },
        "vehicle/sea/zodiac" => s(0x0017_308D, 0x0018_84AC, 0, 0),
        "vehicle/sea/jetski" => s(0x0017_2E75, 0x0017_2E75, 0, 0),
        "vehicle/sea/patrolboat" => s(0x0017_2E13, 0x001A_4CEA, 0, 0),
        _ => return None,
    })
}

/// Shared by every land vehicle: the tyres rolling (switched on the surface
/// under them), the brakes, and a burning, smoking or exploding engine.
mod snd {
    pub const ROLLING: u32 = 0x0007_4EC6;
    pub const BRAKE: u32 = 0x0002_07C0;
    pub const BURNING: u32 = 0x0002_6306;
    pub const SMOKING: u32 = 0x0009_F6EC;
    pub const EXPLOSION: u32 = 0x0002_079E;
    /// The engine sets switch between the player's (fuller) and AI drivers'.
    pub const ENGINE_GROUP: u32 = 0x0017_7A5C;
    pub const PLAYER_ENGINE: u32 = 0x0017_7A5D;
    pub const AI_ENGINE: u32 = 0x0017_7A5F;
    /// The rolling sound: one generic loop (far, AI) or per surface (near).
    pub const ROLL_GROUP: u32 = 0x001A_4B38;
    pub const ROLL_SIMPLE: u32 = 0x001A_4B39;
    pub const ROLL_SURFACE: u32 = 0x001A_4B3A;
    /// Game parameters the blends follow.
    pub const SPEED: u32 = 0x0044_0255;
    pub const RPM: u32 = 0x0044_0256;
    pub const SLIP: u32 = 0x0044_0257;
    pub const THROTTLE: u32 = 0x0044_025B;
    pub const DISTANCE: u32 = 0x0044_F4D0;
    pub const WEAR: u32 = 0x0007_4C81;
    pub const BOAT_RPM: u32 = 0x0002_62B7;
    pub const BOAT_LOAD: u32 = 0x0018_980E;
}

/// AI vehicles are heard within this range (m).
const ENGINE_RANGE: f32 = 120.0;
const IDLE_RPM: f32 = 900.0;

/// Engine speed for a road speed share (0..1) through four gears, or the
/// throttle revving it when standing.
fn target_rpm(share: f32, throttle: f32, boat: bool) -> f32 {
    if boat {
        return 1200.0 + throttle.abs() * 5200.0 + share * 1800.0;
    }
    const GEARS: [f32; 4] = [0.18, 0.4, 0.68, 1.0];
    let g = GEARS.iter().position(|&top| share <= top).unwrap_or(3);
    let low = if g == 0 { 0.0 } else { GEARS[g - 1] };
    let frac = ((share - low) / (GEARS[g] - low)).clamp(0.0, 1.0);
    let geared = if g == 0 { 1200.0 + frac * 3800.0 } else { 2300.0 + frac * 2700.0 };
    let revving = IDLE_RPM + throttle.abs() * 2600.0;
    if share < 0.03 {
        revving
    } else {
        geared.max(IDLE_RPM)
    }
}

#[derive(Default)]
struct EngineState {
    rpm: f32,
    fall: f32,
    braking: bool,
    /// Last frame's velocity, and seconds until another crash may sound.
    last_vel: Vec3,
    crash_wait: f32,
    /// The surface the tyres sound on, and one they've been on for a moment
    /// (switching restarts the loop, so brief patches are ignored).
    surface: u32,
    next_surface: (u32, f32),
}

/// Velocity lost in one step (m/s) that sounds as a crash.
const CRASH_JOLT: f32 = 4.0;

/// A new surface under the tyres takes over after this long (s).
const SURFACE_HOLD: f32 = 0.35;

/// Engines, tyres, brakes and bumps of the player's vehicle and the AI
/// vehicles nearby; burning and smoking engines; getting in and out.
#[allow(clippy::too_many_arguments)]
fn vehicle_sounds(
    time: Res<Time>,
    driving: Res<Driving>,
    mut was_driving: Local<Option<Entity>>,
    mut state: Local<std::collections::HashMap<Entity, EngineState>>,
    spatial: SpatialQuery,
    probe: crate::surfaces::SurfaceProbe,
    listener: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    vehicles: Query<(Entity, &Transform, &Vehicle, &LinearVelocity, &Health, &Wear, Has<crate::patrols::PatrolDriver>)>,
    mut loops: MessageWriter<crate::sound::LoopSfx>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    use crate::sound::{LoopSfx, PlaySfx};
    let dt = time.delta_secs();
    let ear = listener.single().map(|l| l.translation()).unwrap_or_default();
    // Getting in starts the engine, getting out switches it off.
    if *was_driving != driving.vehicle {
        if let Some((_, t, v, ..)) = was_driving.and_then(|e| vehicles.get(e).ok()) {
            if let Some(s) = sounds(&DEFS[v.def]).filter(|s| s.off != 0) {
                sfx.write(PlaySfx { event: s.off, at: Some(t.translation), volume: 0.8 });
            }
        }
        if let Some((_, _, v, ..)) = driving.vehicle.and_then(|e| vehicles.get(e).ok()) {
            if let Some(s) = sounds(&DEFS[v.def]).filter(|s| s.ignition != 0) {
                sfx.write(PlaySfx { event: s.ignition, at: None, volume: 0.8 });
            }
        }
        *was_driving = driving.vehicle;
    }
    state.retain(|e, _| vehicles.contains(*e));
    for (e, t, v, lv, h, w, patrol) in &vehicles {
        let d = t.translation.distance(ear);
        let mine = driving.vehicle == Some(e);
        let at = if mine { None } else { Some(t.translation) };
        // Burning and smoking engines, whoever is driving.
        if !w.wrecked && d < ENGINE_RANGE {
            let share = h.current / h.max.max(1.0);
            if w.fuse.is_some() {
                loops.write(LoopSfx { key: e.to_bits() ^ 0xB0, event: snd::BURNING, at: Some(t.translation), volume: 0.9, ..default() });
            } else if share < SMOKING {
                loops.write(LoopSfx { key: e.to_bits() ^ 0x5E, event: snd::SMOKING, at: Some(t.translation), volume: 0.6, ..default() });
            }
        }
        let def = &DEFS[v.def];
        // A glider has no engine: the wind rushing past (the game's falling wind).
        if mine && def.kind == Kind::Glider {
            let params = [(crate::player_audio::FALL_SPEED, lv.0.length() * 1.5), (0, 0.0), (0, 0.0), (0, 0.0)];
            loops.write(LoopSfx { key: e.to_bits() ^ 0x61, event: crate::player_audio::FALLING, at: None, volume: 0.7, params, ..default() });
            continue;
        }
        let Some(set) = sounds(def) else { continue };
        if w.wrecked || !(mine || patrol) || (!mine && d > ENGINE_RANGE) {
            state.remove(&e);
            continue;
        }
        let boat = def.kind == Kind::Boat;
        let speed = v.recorded_speed.map(f32::abs).unwrap_or_else(|| lv.0.length());
        let share = (speed / def.top_speed).clamp(0.0, 1.2);
        let st = state.entry(e).or_insert_with(|| EngineState { rpm: IDLE_RPM, last_vel: lv.0, ..default() });
        let target = target_rpm(share, v.throttle, boat);
        st.rpm += (target - st.rpm) * (dt * if target > st.rpm { 4.0 } else { 6.0 }).min(1.0);
        let distance = if mine { 0.0 } else { (d / ENGINE_RANGE).clamp(0.0, 1.0) };
        let wear = 1.0 - (h.current / h.max.max(1.0)).clamp(0.0, 1.0);
        let throttle = v.throttle.abs();
        let params = if boat {
            [(snd::BOAT_RPM, st.rpm), (snd::SPEED, speed), (snd::BOAT_LOAD, throttle), (snd::DISTANCE, distance)]
        } else {
            [(snd::RPM, st.rpm), (snd::THROTTLE, throttle * 100.0), (snd::DISTANCE, distance), (snd::WEAR, wear)]
        };
        let (engine, case) = if mine { (set.engine, snd::PLAYER_ENGINE) } else { (set.ai_engine, snd::AI_ENGINE) };
        let volume = if mine { 0.7 } else { 1.0 };
        loops.write(LoopSfx { key: e.to_bits() ^ 0xE1, event: engine, at, volume, switches: [(snd::ENGINE_GROUP, case), (0, 0)], params, tracks: None });
        if boat {
            continue;
        }
        // A hard knock (speed lost all at once, as no brakes do): the crash.
        st.crash_wait -= dt;
        let jolt = (lv.0 - st.last_vel).length();
        st.last_vel = lv.0;
        if set.crash != 0 && st.crash_wait <= 0.0 && jolt > CRASH_JOLT {
            st.crash_wait = 0.6;
            sfx.write(PlaySfx { event: set.crash, at, volume: ((jolt - CRASH_JOLT) / 10.0 + 0.5).min(1.2) });
        }
        // Tyres on whatever is under them, skidding when sliding sideways.
        let filter = SpatialQueryFilter::from_excluded_entities([e, v.body]);
        let below = spatial.cast_ray(t.translation + Vec3::Y * 0.5, Dir3::NEG_Y, (0.5 - v.min.y) + 0.6, true, &filter);
        if let Some(hit) = below.filter(|_| speed > 0.5) {
            let point = t.translation + Vec3::Y * (0.5 - hit.distance);
            let under = match probe.at(hit.entity, point, hit.normal) {
                0 => crate::surfaces::value::DEFAULT,
                s => s,
            };
            if st.surface == 0 || under == st.surface {
                st.surface = under;
                st.next_surface = (under, 0.0);
            } else if st.next_surface.0 == under {
                st.next_surface.1 += dt;
                if st.next_surface.1 > SURFACE_HOLD {
                    st.surface = under;
                }
            } else {
                st.next_surface = (under, 0.0);
            }
            let surface = st.surface;
            let sideways = lv.0.dot(*t.right()).abs();
            let slip = (sideways / 3.0 + if (v.brake || v.handbrake) && speed > 5.0 { 1.2 } else { 0.0 }).min(2.0);
            let roll = if mine || d < 30.0 { snd::ROLL_SURFACE } else { snd::ROLL_SIMPLE };
            loops.write(LoopSfx {
                key: e.to_bits() ^ 0x70,
                event: snd::ROLLING,
                at,
                volume: if mine { 0.8 } else { 1.0 },
                switches: [(crate::surfaces::SURFACE_GROUP, surface), (snd::ROLL_GROUP, roll)],
                params: [(snd::SPEED, speed), (snd::SLIP, slip), (0, 0.0), (0, 0.0)],
                tracks: None,
            });
        }
        // Landing hard after a jump or a drop.
        let vy = lv.0.y;
        if below.is_some() && st.fall > 3.0 {
            let i = if st.fall > 9.0 {
                2
            } else if st.fall > 5.5 {
                1
            } else {
                0
            };
            sfx.write(PlaySfx { event: set.suspension[i], at, volume: 0.9 });
            st.fall = 0.0;
        }
        st.fall = if below.is_none() { st.fall.max(-vy) } else { 0.0 };
        // Brakes (or the handbrake: the game names no sound of its own for
        // it) squealing at speed.
        let braking = (v.brake || v.handbrake) && speed > 8.0 && mine;
        if braking && !st.braking {
            sfx.write(PlaySfx { event: snd::BRAKE, at, volume: 0.6 });
        }
        st.braking = braking;
    }
}

/// `--drive-test`: boards the nearest vehicle, drives ahead then turns, logging.
#[allow(clippy::too_many_arguments)]
fn drive_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut start: Local<Option<f32>>,
    mut last_log: Local<f32>,
    mut shots: Local<usize>,
    mut driving: ResMut<Driving>,
    mut actions: ResMut<ActionState>,
    mut players: Query<(Entity, &Transform, &mut Player), Without<Vehicle>>,
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle, &LinearVelocity), (Without<Player>, Without<crate::patrols::PatrolDriver>)>,
    heights: Res<crate::terrain::Heightfield>,
    mut commands: Commands,
) {
    if !args.drive_test {
        return;
    }
    let Ok((pe, pt, mut p)) = players.single_mut() else { return };
    let now = time.elapsed_secs();
    match driving.vehicle {
        None if start.is_none() && !p.spawning => {
            let Some((ve, vt, mut v, _)) = vehicles
                .iter_mut()
                // FC3_DRIVE_KIND=boat|glider tests other kinds.
                .filter(|(_, t, v, _)| {
                    let want = match std::env::var("FC3_DRIVE_KIND").as_deref() {
                        Ok("boat") => Kind::Boat,
                        Ok("glider") => Kind::Glider,
                        _ => Kind::Car,
                    };
                    // (Not a car standing in the water: it would be flooded.)
                    let dry = want != Kind::Car || t.translation.y + 1.0 > crate::water::surface_at(t.translation, &heights);
                    t.translation.distance(pt.translation) < 200.0 && v.kind() == want && dry
                })
                .min_by(|a, b| a.1.translation.distance(pt.translation).total_cmp(&b.1.translation.distance(pt.translation)))
            else {
                // Nothing to board yet: say why, once, after a while.
                if now > 20.0 && *last_log == 0.0 {
                    *last_log = now;
                    if std::env::var("FC3_VEHICLE_DEBUG").is_ok() {
                        for (e, t, v, _) in vehicles.iter() {
                            info!("vehicle: {e:?} {} at {:.1?}", DEFS[v.def].name, t.translation);
                        }
                    }
                    let near: Vec<String> = vehicles
                        .iter()
                        .filter(|(_, t, _, _)| t.translation.distance(pt.translation) < 200.0)
                        .map(|(_, t, v, _)| {
                            let water = crate::water::surface_at(t.translation, &heights);
                            format!("{} {:.0} m, {:.1} m above water", DEFS[v.def].name, t.translation.distance(pt.translation), t.translation.y - water)
                        })
                        .collect();
                    info!("drive-test: nothing to board; vehicles near (patrols excluded): {near:?}");
                }
                return;
            };
            if args.vehicle_look {
                // Beside the vehicle on its +X side, looking at it.
                let right = vt.rotation * Vec3::X;
                let at = vt.translation + right * 7.0 + Vec3::Y * 1.5;
                let (vx, vz) = (vt.translation.x - at.x, vt.translation.z - at.z);
                commands.entity(pe).insert(Transform::from_translation(at));
                p.yaw = (-vx).atan2(-vz);
                p.pitch = -0.15;
                p.noclip = true;
                // Full left lock, to show the front wheels and steering wheel turned.
                v.steer = 1.0;
                *start = Some(now);
                info!("vehicle-look: {} rotation {:?}", DEFS[v.def].name, vt.rotation);
                return;
            }
            v.occupied = true;
            // FC3_DRIVE_SEAT=passenger: beside the driver's seat instead, as a
            // passenger (armed, in the seat's own view).
            let seat = if std::env::var("FC3_DRIVE_SEAT").as_deref() == Ok("passenger") {
                let pose = Transform::from_translation(v.seat() + Vec3::X * 0.8);
                let slot = v.seats.len() as i32;
                v.seats.push(SeatSlot { kind: 3, index: 0, slot, pose, pawn: None });
                v.seats.len() - 1
            } else {
                0
            };
            v.seats[seat].pawn = Some(fc3_story::PLAYER);
            driving.vehicle = Some(ve);
            driving.seat = seat;
            driving.first_person |= seat != 0;
            // FC3_DRIVE_VIEW=chase: from behind, to watch the wheels.
            if std::env::var("FC3_DRIVE_VIEW").as_deref() == Ok("chase") {
                driving.first_person = false;
            }
            driving.last_yaw = vt.rotation.to_euler(EulerRot::YXZ).0;
            p.driving = true;
            p.yaw = driving.last_yaw;
            p.pitch = -0.15;
            commands.entity(pe).insert(ColliderDisabled);
            *start = Some(now);
            info!("drive-test: in {} at {:?}, bounds {:.2?}..{:.2?}", DEFS[v.def].name, vt.translation, v.min, v.max);
            // The ground ahead of the nose and behind the tail.
            let ground = |d: f32| heights.height_at(vt.translation + vt.rotation * Vec3::NEG_Z * d).map_or(f32::NAN, |h| h - vt.translation.y);
            info!(
                "drive-test: ground ahead {:.1} {:.1} {:.1}, behind {:.1} {:.1} {:.1}",
                ground(10.0),
                ground(20.0),
                ground(40.0),
                ground(-10.0),
                ground(-20.0),
                ground(-40.0)
            );
        }
        Some(ve) => {
            let t = now - start.unwrap_or(now);
            let glider = vehicles.get(ve).is_ok_and(|(_, _, v, _)| v.kind() == Kind::Glider);
            // A glider: off its launch, a bank to the left, a dive, a pull up.
            actions.movement = if glider {
                match t {
                    t if t < 4.0 => Vec2::ZERO,
                    t if t < 7.0 => Vec2::new(-1.0, 0.0),
                    t if t < 9.0 => Vec2::new(0.0, 1.0),
                    t if t < 11.0 => Vec2::new(0.0, -1.0),
                    _ => Vec2::ZERO,
                }
            } else if t < 1.0 {
                Vec2::ZERO
            } else if t < 6.0 {
                Vec2::new(0.0, 1.0)
            } else if t < 9.0 {
                Vec2::new(0.6, 0.6)
            } else {
                Vec2::ZERO
            };
            // FC3_DRIVE_HANDBRAKE=secs: pull the handbrake for a second and a
            // half from then.
            if std::env::var("FC3_DRIVE_HANDBRAKE").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|at| (at..at + 1.5).contains(&t)) {
                actions.press(Action::Jump);
            }
            // FC3_DRIVE_EXIT=secs: get out that long into the drive.
            if std::env::var("FC3_DRIVE_EXIT").ok().and_then(|s| s.parse::<f32>().ok()).is_some_and(|at| t >= at) {
                info!("drive-test: getting out at t={t:.1}");
                actions.press(Action::Interact);
            }
            // FC3_TD_SHOT=2,4: screenshots that long into the drive.
            let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
            if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
                if t >= when {
                    let stem = path.strip_suffix(".png").unwrap_or(path);
                    crate::camera::capture(&mut commands, format!("{stem}_td{}.png", *shots));
                    *shots += 1;
                }
            }
            if now - *last_log >= 1.0 {
                *last_log = now;
                if let Ok((_, vt, _, lv)) = vehicles.get(ve) {
                    let up = vt.rotation * Vec3::Y;
                    let yaw = vt.rotation.to_euler(EulerRot::YXZ).0.to_degrees();
                    info!(
                        "drive-test t={t:.1} pos=({:.1},{:.1},{:.1}) speed={:.1} m/s up_y={:.2} yaw={yaw:.0} vel=({:.1},{:.1})",
                        vt.translation.x,
                        vt.translation.y,
                        vt.translation.z,
                        lv.0.length(),
                        up.y,
                        lv.0.x,
                        lv.0.z
                    );
                    let v = vehicles.get(ve).map(|(_, _, v, _)| v.glide).unwrap_or_default();
                    if v.flown > 0.0 {
                        let ground = heights.height_at(vt.translation).unwrap_or(0.0);
                        info!(
                            "drive-test glide: airspeed={:.1} roll={:.2} pitch={:.2} turn={:.2} landing={:.2} vy={:.1} above terrain={:.1}",
                            v.speed,
                            v.roll,
                            v.pitch,
                            v.turn,
                            v.landing,
                            lv.0.y,
                            vt.translation.y - ground
                        );
                    }
                }
            }
        }
        // Out again: where the player and the vehicles left are going.
        None if start.is_some() => {
            let t = now - start.unwrap_or(now);
            if now - *last_log >= 1.0 && t < 30.0 {
                *last_log = now;
                let ground = heights.height_at(pt.translation).unwrap_or(0.0);
                info!("drive-test out t={t:.1} player y={:.1} ({:.1} above terrain) vel={:.1}", pt.translation.y, pt.translation.y - ground, p.velocity);
                for (_, vt, v, lv) in vehicles.iter().filter(|(_, vt, v, _)| v.glide.flown > 0.0 && vt.translation.distance(pt.translation) < 300.0) {
                    info!("drive-test out: {} at {:.1} speed {:.1} landed {}", DEFS[v.def].name, vt.translation, lv.0.length(), v.glide.down);
                }
            }
        }
        None => {}
    }
}

/// An enemy's round that hit something other than its target.
#[derive(Message, Clone, Copy)]
pub struct StrayHit {
    pub entity: Entity,
    pub amount: f32,
    pub from: Vec3,
    pub instigator: Entity,
}

/// Rounds into a vehicle damage it, and some get through to the player at
/// the wheel (whose own collider is off while driving).
fn stray_hits(
    mut hits: MessageReader<StrayHit>,
    parents: Query<&ChildOf>,
    vehicles: Query<(), With<Vehicle>>,
    driving: Res<Driving>,
    players: Query<Entity, With<Player>>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut damage: MessageWriter<DamageEvent>,
) {
    let player = players.single().ok();
    for h in hits.read() {
        let Some(vehicle) = parents.get(h.entity).ok().map(|c| c.parent()).filter(|v| vehicles.contains(*v)) else { continue };
        damage.write(DamageEvent {
            target: vehicle,
            amount: h.amount,
            kind: DamageKind::Bullet,
            source: Some(h.from),
            instigator: Some(h.instigator),
            headshot: false,
            hit: crate::combat::Hit::Whole,
        });
        if driving.vehicle == Some(vehicle) && rng.f32() < 0.35 {
            if let Some(p) = player {
                damage.write(DamageEvent {
                    target: p,
                    amount: h.amount * 0.6,
                    kind: DamageKind::Bullet,
                    source: Some(h.from),
                    instigator: Some(h.instigator),
                    headshot: false,
                    hit: crate::combat::Hit::Whole,
                });
            }
        }
    }
}

/// A vehicle's state of repair: smoking, burning, burnt out.
#[derive(Component, Default)]
pub struct Wear {
    /// Seconds left before a burning vehicle blows up.
    fuse: Option<f32>,
    pub wrecked: bool,
    puff: f32,
    /// The game's engine fire it plays, and whether the burning stage's.
    fire: Option<(Entity, bool)>,
}

/// A vehicle's engine fire (its archetype's `EngineFire`): the smoking
/// stage's effect, or the burning one's (a boat's is the one).
fn engine_fire(prefix: &str, burning: bool) -> Option<&'static str> {
    let (smoke, fire) = match prefix {
        "vehicle/land/jeep4x4" | "vehicle/land/technical" => {
            ("fc3_gameplay_vehicule_land.land_vehicule_4x4.engine_fire", "fc3_gameplay_vehicule_land.land_vehicule_4x4.engine_fire_burning")
        }
        "vehicle/land/atv" => {
            ("fc3_gameplay_vehicule_land.land_vehicule_quad_bike.engine_fire", "fc3_gameplay_vehicule_land.land_vehicule_quad_bike.engine_fire_burning")
        }
        "vehicle/land/japansedan" | "vehicle/land/germanhatch" => {
            ("fc3_gameplay_vehicule_land.land_vehicule_japsedan.engine_fire", "fc3_gameplay_vehicule_land.land_vehicule_japsedan.engine_fire_burning")
        }
        "vehicle/land/dunebuggy" => {
            ("fc3_gameplay_vehicule_land.land_vehicule_buggy.engine_fire", "fc3_gameplay_vehicule_land.land_vehicule_buggy.engine_fire_burning")
        }
        "vehicle/land/cargotruck" => {
            ("fc3_gameplay_vehicule_land.land_vehicule_cargotruck.engine_fire", "fc3_gameplay_vehicule_land.land_vehicule_cargotruck.engine_fire_burning")
        }
        "vehicle/sea/zodiac" => ("fc3_gameplay_vehicule_sea.sea_vehicule_zodiac.engine_fire", "fc3_gameplay_vehicule_sea.sea_vehicule_zodiac.engine_fire"),
        "vehicle/sea/jetski" => ("fc3_gameplay_vehicule_sea.sea_vehicule_jetski.engine_fire", "fc3_gameplay_vehicule_sea.sea_vehicule_jetski.engine_fire"),
        "vehicle/sea/patrolboat" | "vehicle/sea/barge" => {
            ("fc3_gameplay_vehicule_sea.sea_vehicule_patrolboat.engine_fire", "fc3_gameplay_vehicule_sea.sea_vehicule_patrolboat.engine_fire")
        }
        _ => return None,
    };
    Some(if burning { fire } else { smoke })
}

/// Below this share of its armour a vehicle smokes, below the second it
/// catches fire and blows up a few seconds later.
const SMOKING: f32 = 0.5;
const BURNING: f32 = 0.25;
const FUSE: f32 = 4.5;

/// Damaged vehicles smoke, then burn and blow up, leaving a charred wreck
/// that can't be driven.
#[allow(clippy::too_many_arguments)]
fn vehicle_damage(
    time: Res<Time>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut particles: ResMut<crate::particles::Particles>,
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle, &mut Health, &mut Wear, &Children, Option<&WreckParts>)>,
    parts: Query<&Children>,
    whole_only: Query<(), With<WholeOnly>>,
    mut mats: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut originals: Query<Entity, With<MeshMaterial3d<crate::original_shaders::OriginalMaterial>>>,
    mut explosions: MessageWriter<crate::weapons::Explosion>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut charred: Local<Option<Handle<StandardMaterial>>>,
    mut commands: Commands,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    use crate::particles::{Frame, Particle, Sheet};
    let dt = time.delta_secs();
    let charred = charred
        .get_or_insert_with(|| materials.add(StandardMaterial { base_color: Color::srgb(0.06, 0.055, 0.05), perceptual_roughness: 0.95, ..default() }))
        .clone();
    for (e, t, mut v, mut h, mut w, children, wreck) in &mut vehicles {
        if w.wrecked {
            // A wreck smoulders.
            w.puff -= dt;
            if w.puff <= 0.0 {
                w.puff = 0.35;
                let top = t.transform_point(Vec3::new((v.min.x + v.max.x) * 0.5, v.max.y, (v.min.z + v.max.z) * 0.5));
                let mut s = Particle::new(Sheet::Smoke, top, 4.0, 1.2);
                s.grow = 2.5;
                s.vel = Vec3::Y * 1.2 + Vec3::new(rng.range(-0.3, 0.3), 0.0, rng.range(-0.3, 0.3));
                s.frame = Frame::Loop(12.0, (rng.f32() * 30.0) as u32);
                s.colour = Vec4::new(0.12, 0.11, 0.1, 0.45);
                particles.spawn(s);
            }
            continue;
        }
        let share = h.current / h.max.max(1.0);
        // The engine end: the front for cars, the back for boats.
        let front = matches!(DEFS[v.def].kind, Kind::Car);
        let engine_local = Vec3::new((v.min.x + v.max.x) * 0.5, v.max.y * 0.8 + v.min.y * 0.2, if front { v.min.z + 0.6 } else { v.max.z - 0.5 });
        let engine = t.transform_point(engine_local);
        // The game's own engine fire, smoking then burning (else ours).
        let stage = (share < SMOKING || h.dead).then_some(share < BURNING || h.dead);
        let authored = stage.and_then(|burning| engine_fire(DEFS[v.def].prefix, burning)).filter(|n| crate::authored_fx::available(n));
        if w.fire.map(|(_, b)| b) != stage.filter(|_| authored.is_some()) {
            if let Some((old, _)) = w.fire.take() {
                commands.entity(old).despawn();
            }
            if let (Some(name), Some(burning)) = (authored, stage) {
                // (At the vehicle's origin: its emitters place the hood's and the
                // cabin's fires themselves.)
                let fx = commands.spawn((Transform::default(), crate::authored_fx::AuthoredFx(name), ChildOf(e))).id();
                w.fire = Some((fx, burning));
            }
        }
        if authored.is_none() && (share < SMOKING || h.dead) {
            w.puff -= dt;
            if w.puff <= 0.0 {
                w.puff = if share < BURNING || h.dead { 0.08 } else { 0.25 };
                let black = (1.0 - share / SMOKING).clamp(0.0, 1.0);
                let mut s = Particle::new(Sheet::Smoke, engine, 2.5, 0.6);
                s.grow = 3.0;
                s.vel = Vec3::Y * 1.5 + Vec3::new(rng.range(-0.3, 0.3), 0.0, rng.range(-0.3, 0.3));
                s.frame = Frame::Loop(15.0, (rng.f32() * 30.0) as u32);
                let c = 0.7 - 0.55 * black;
                s.colour = Vec4::new(c, c, c, 0.35 + 0.25 * black);
                particles.spawn(s);
                if share < BURNING || h.dead {
                    let mut f = Particle::new(Sheet::Fire, engine + Vec3::Y * 0.2, rng.range(0.5, 0.8), rng.range(0.8, 1.3));
                    f.vel = Vec3::Y * 1.2;
                    particles.spawn(f);
                }
            }
        }
        if (share < BURNING || h.dead) && w.fuse.is_none() {
            w.fuse = Some(FUSE);
        }
        let Some(fuse) = w.fuse.as_mut() else { continue };
        *fuse -= dt;
        if *fuse > 0.0 {
            continue;
        }
        // It goes up: the blast, then a burnt-out shell.
        if let Some((fx, _)) = w.fire.take() {
            commands.entity(fx).despawn();
        }
        let centre = t.transform_point((v.min + v.max) * 0.5);
        info!("vehicle: {} ({}) wrecked at {centre:.0?}", DEFS[v.def].name, v.spawn_id);
        let effect = match DEFS[v.def].kind {
            Kind::Car => Some(crate::authored_fx::blast::CAR),
            Kind::Boat => Some(crate::authored_fx::blast::BOAT),
            Kind::Glider => None,
        };
        explosions.write(crate::weapons::Explosion { at: centre, radius: 9.0, damage: 220.0, instigator: None, effect });
        sfx.write(crate::sound::PlaySfx { event: snd::EXPLOSION, at: Some(centre), volume: 1.0 });
        w.wrecked = true;
        h.current = 0.0;
        h.dead = true;
        v.throttle = 0.0;
        v.steer = 0.0;
        v.brake = true;
        v.parked = true;
        for c in children.iter().chain(children.iter().flat_map(|c| parts.get(c).into_iter().flatten().copied())) {
            // What was there only whole goes; its wreck's own parts come.
            if whole_only.contains(c) {
                commands.entity(c).despawn();
                continue;
            }
            if let Ok(mut m) = mats.get_mut(c) {
                m.0 = charred.clone();
            }
            if originals.get_mut(c).is_ok() {
                commands
                    .entity(c)
                    .remove::<MeshMaterial3d<crate::original_shaders::OriginalMaterial>>()
                    .remove::<crate::original_shaders::OriginalSwap>()
                    .insert(MeshMaterial3d(charred.clone()));
            }
        }
        for (mesh, material) in wreck.map(|w| w.0.as_slice()).unwrap_or(&[]) {
            commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone()), crate::original_shaders::PropPart, ChildOf(e)));
        }
        commands.entity(e).remove::<crate::patrols::PatrolDriver>();
        info!("a {} blew up at {centre:.0?}", DEFS[v.def].name);
    }
}

/// The torch at work: the vehicle's armour comes back (a fire on it goes out
/// once it's above the burning mark), sparks where it's worked on.
fn repair_vehicles(
    time: Res<Time>,
    repairing: Res<Repairing>,
    mut vehicles: Query<(&Transform, &Vehicle, &mut Health, &mut Wear), Without<Player>>,
    players: Query<&Transform, (With<Player>, Without<Vehicle>)>,
    mut particles: ResMut<crate::particles::Particles>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut next: Local<f32>,
) {
    use crate::particles::{Frame, Particle, Sheet};
    let Some(ve) = repairing.vehicle else { return };
    let (Ok((t, v, mut h, mut w)), Ok(pt)) = (vehicles.get_mut(ve), players.single()) else { return };
    let dt = time.delta_secs();
    h.current = (h.current + REPAIR_RATE * h.max * dt).min(h.max);
    // The last scrape (repairs stop just short of full).
    if h.current > h.max - 1.0 {
        h.current = h.max;
    }
    if h.current / h.max.max(1.0) >= BURNING {
        w.fuse = None;
    }
    // On the side facing the player, at chest height.
    let chest = t.rotation.inverse() * (pt.translation + Vec3::Y * 0.2 - t.translation);
    let local = chest.clamp(Vec3::from(v.min), Vec3::from(v.max));
    let at = t.transform_point(local);
    *next -= dt;
    if *next > 0.0 {
        return;
    }
    *next = 0.04;
    let out = (pt.translation - at).normalize_or(Vec3::Y);
    for _ in 0..3 {
        let mut s = Particle::new(Sheet::Sparks, at, rng.range(0.15, 0.35), 0.05);
        let jitter = Vec3::new(rng.range(-0.7, 0.7), rng.range(-0.2, 0.9), rng.range(-0.7, 0.7));
        s.vel = (out + jitter).normalize_or(out) * rng.range(2.0, 5.0);
        s.gravity = 9.8;
        s.stretch = 0.03;
        s.frame = Frame::Fixed((rng.f32() * 4.0) as u32);
        s.colour = Vec4::new(1.0, 0.7, 0.35, 1.3);
        particles.spawn(s);
    }
    let mut glow = Particle::new(Sheet::Glow, at, 0.08, 0.25);
    glow.colour = Vec4::new(0.6, 0.75, 1.0, 1.0);
    particles.spawn(glow);
}

/// `FC3_REPAIR_TEST`: the nearest vehicle knocked down to 40% (smoking), the
/// player beside it holding E, logging its health each second.
#[allow(clippy::too_many_arguments)]
fn repair_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut commands: Commands,
    mut shots: Local<usize>,
    mut state: Local<(u32, f32, f32)>,
    mut actions: ResMut<ActionState>,
    mut players: Query<(&mut Transform, &mut Player), Without<Vehicle>>,
    mut vehicles: Query<(Entity, &Transform, &Vehicle, &mut Health), Without<Player>>,
    repairing: Res<Repairing>,
) {
    if std::env::var("FC3_REPAIR_TEST").is_err() {
        return;
    }
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    let (step, start, last) = &mut *state;
    let now = time.elapsed_secs();
    if *start == 0.0 {
        *start = now;
    }
    let t = now - *start;
    let Some((_, vt, v, mut h)) = vehicles
        .iter_mut()
        .filter(|(_, vt, _, _)| vt.translation.distance(pt.translation) < 80.0)
        .min_by(|a, b| a.1.translation.distance(pt.translation).total_cmp(&b.1.translation.distance(pt.translation)))
    else {
        return;
    };
    match *step {
        0 if t > 6.0 => {
            h.current = h.max * 0.4;
            // Beside it, facing it.
            let side = vt.rotation * Vec3::new(v.min.x - 1.0, 0.0, 0.0);
            pt.translation = vt.translation + Vec3::new(side.x, 0.0, side.z) + Vec3::Y * 0.4;
            let to = vt.translation - pt.translation;
            p.yaw = (-to.x).atan2(-to.z);
            p.pitch = -0.25;
            info!("repair-test: {} at 40%", DEFS[v.def].name);
            *step = 1;
        }
        1 if t > 8.0 => {
            actions.press(Action::Interact);
            *step = 2;
        }
        2 => {
            actions.hold(Action::Interact);
            // FC3_TD_SHOT=0.5,1.2: screenshots that long into the hold.
            let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
            if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
                if t - 8.0 >= when {
                    let stem = path.strip_suffix(".png").unwrap_or(path);
                    crate::camera::capture(&mut commands, format!("{stem}_td{}.png", *shots));
                    *shots += 1;
                }
            }
            if now - *last >= 1.0 {
                *last = now;
                info!("repair-test: health {:.0}/{:.0}, repairing {}", h.current, h.max, repairing.vehicle.is_some());
            }
            if h.current >= h.max {
                info!("repair-test: repaired in {:.1}s", t - 8.0);
                *step = 3;
            }
        }
        _ => {}
    }
}

/// `FC3_WRECK_TEST`: logs every vehicle's health and state each second (try
/// with `FC3_ARMED_TEST` and `FC3_EXPLOSIVES_TEST`, or shoot one).
fn wreck_test(time: Res<Time>, mut last: Local<f32>, vehicles: Query<(&Vehicle, &Health, &Wear, &Transform)>, players: Query<&Transform, With<Player>>) {
    if std::env::var("FC3_WRECK_TEST").is_err() || time.elapsed_secs() - *last < 1.0 {
        return;
    }
    *last = time.elapsed_secs();
    let Ok(pt) = players.single() else { return };
    for (v, h, w, t) in &vehicles {
        if t.translation.distance(pt.translation) < 40.0 {
            info!("wreck-test: {} health {:.0}/{:.0} burning {} wrecked {}", DEFS[v.def].name, h.current, h.max, w.fuse.is_some(), w.wrecked);
        }
    }
}

/// `FC3_WRECK_TEST=rocket`: a rocket's blast on the nearest vehicle 5 s in.
#[allow(clippy::type_complexity)]
fn wreck_rocket(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut done: Local<(bool, usize)>,
    vehicles: Query<&Transform, (With<Vehicle>, Without<Player>)>,
    mut players: Query<(&Transform, &mut Player)>,
    mut explosions: MessageWriter<crate::weapons::Explosion>,
    mut commands: Commands,
) {
    if std::env::var("FC3_WRECK_TEST").as_deref() != Ok("rocket") {
        return;
    }
    let Ok((pt, mut p)) = players.single_mut() else { return };
    let Some(v) = vehicles
        .iter()
        .filter(|t| t.translation.distance(pt.translation) < 40.0)
        .min_by(|a, b| a.translation.distance(pt.translation).total_cmp(&b.translation.distance(pt.translation)))
    else {
        return;
    };
    // Watching it.
    let to = v.translation - pt.translation;
    p.yaw = (-to.x).atan2(-to.z);
    p.pitch = -0.15;
    let (fired, shots) = &mut *done;
    // FC3_TD_SHOT: screenshots that long after the hit.
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (true, Some(&when), Some(path)) = (*fired, at.get(*shots), args.screenshot.as_ref()) {
        if time.elapsed_secs() - 25.0 >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    if *fired || time.elapsed_secs() < 25.0 {
        return;
    }
    *fired = true;
    explosions.write(crate::weapons::Explosion {
        at: v.translation + Vec3::Y * 0.5,
        radius: 7.0,
        damage: 260.0,
        instigator: None,
        effect: Some(crate::authored_fx::blast::ROCKET),
    });
    info!("wreck-test: a rocket hits at {:.0?}", v.translation);
}
