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
            .add_systems(OnExit(crate::menu::GameState::Loading), load_vehicles)
            .add_systems(
                Update,
                (
                    spawn_vehicles,
                    despawn_vehicles,
                    (repair_test, enter_exit, repair_vehicles).chain().before(crate::hud::HudSet).after(crate::turrets::man_turrets),
                    run_over,
                    vehicle_sounds,
                    vehicle_hud.before(crate::hud::HudSet),
                    drive_test,
                )
                    .run_if(in_state(crate::menu::GameState::Playing)),
            )
            .add_systems(FixedUpdate, drive)
            .add_message::<StrayHit>()
            .add_systems(Update, (turn_wheels, wreck_rocket, stray_hits, vehicle_damage, wreck_test).chain().run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PostUpdate, seat_player.before(TransformSystems::Propagate));
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

const VEHICLES: &str = r"graphics\__fc3_graphics\_common\vehicles\";

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
];

fn def_for(name: &str) -> Option<usize> {
    let n = name.to_ascii_lowercase();
    DEFS.iter().position(|d| n.starts_with(d.prefix))
}

/// The vehicle definition with this display name (e.g. "Technical").
pub fn def_named(name: &str) -> Option<usize> {
    DEFS.iter().position(|d| d.name == name)
}

#[derive(Resource)]
pub struct VehicleAssets {
    pub models: Vec<Handle<Fc3Model>>,
    /// The mounted gun's base on an armed technical, and the ring on a boat.
    pub turret_base: Handle<Fc3Model>,
    pub boat_ring: Handle<Fc3Model>,
}

fn load_vehicles(mut commands: Commands, server: Res<AssetServer>) {
    let models = DEFS.iter().map(|d| server.load(cache_path(&cache::model_rel(&format!("{VEHICLES}{}", d.model))))).collect();
    let turret_base = server.load(cache_path(&cache::model_rel(&format!("{VEHICLES}{TURRET_BASE}"))));
    let boat_ring = server.load(cache_path(&cache::model_rel(&format!("{VEHICLES}{BOAT_RING}"))));
    commands.insert_resource(VehicleAssets { models, turret_base, boat_ring });
}

const TURRET_BASE: &str = r"land\4x4technical\4x4techturretbase.xbg";
const BOAT_RING: &str = r"sea\speedboat_01\zodiac_mountedring_01.xbg";
/// The gun's pivot above the base's mount node (the technical), and above
/// the gunner's feet (any mount).
const GUN_ON_BASE: Vec3 = Vec3::new(0.0, 0.85, -0.02);
const GUN_ABOVE_GUNNER: f32 = 1.37;

/// A vehicle with a mounted gun: the gun's pivot and its gunner's feet (in
/// the vehicle's model space).
#[derive(Component)]
pub struct ArmedVehicle {
    pub pivot: Vec3,
    pub gunner: Vec3,
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
    commands.entity(root).insert(ArmedVehicle { pivot, gunner });
    true
}

#[derive(Component)]
pub struct Vehicle {
    def: usize,
    /// World entity id of the spawn it came from.
    spawn_id: u64,
    throttle: f32,
    steer: f32,
    brake: bool,
    /// Local-space bounds of the model.
    min: Vec3,
    max: Vec3,
    occupied: bool,
    /// Nobody has driven it (or the player got out): the parking brake is on.
    parked: bool,
    /// The body collider (a child), excluded from the wheel rays.
    body: Entity,
}

impl Vehicle {
    pub fn kind(&self) -> Kind {
        DEFS[self.def].kind
    }

    /// Model-space bounds.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        (self.min, self.max)
    }

    pub fn body(&self) -> Entity {
        self.body
    }

    /// Driving inputs for an AI driver: throttle -1..1, steer -1..1 (left
    /// positive), and the brake.
    pub fn control(&mut self, throttle: f32, steer: f32, brake: bool) {
        self.parked = false;
        self.throttle = throttle.clamp(-1.0, 1.0);
        self.steer = steer.clamp(-1.0, 1.0);
        self.brake = brake;
    }

    fn wheels(&self) -> [Vec3; 4] {
        let inset = Vec3::new(0.3, 0.0, 0.8);
        let y = self.min.y + 0.6;
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
    last_yaw: f32,
    /// First-person seat instead of the chase camera (V toggles).
    pub first_person: bool,
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

/// Spawns a drivable vehicle of a definition at a pose; returns its root.
pub fn build_vehicle(commands: &mut Commands, def: usize, model: &Fc3Model, at: Vec3, rotation: Quat, spawn_id: u64) -> Entity {
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
            AngularDamping(0.6),
            Transform::from_translation(at).with_rotation(rotation),
            Visibility::Visible,
            Name::new(d.name),
        ))
        .id();
    let body = commands
        .spawn((
            Collider::cuboid(half.x * 2.0, half.y * 2.0, half.z * 2.0),
            ColliderDensity(d.mass / volume),
            Transform::from_translation(centre),
            ChildOf(root),
        ))
        .id();
    commands.entity(root).insert((
        Vehicle { def, spawn_id, throttle: 0.0, steer: 0.0, brake: false, min, max, occupied: false, parked: true, body },
        Health::new(d.armour),
        Wear::default(),
        crate::combat::DamageScale { bullet: 0.45, explosion: 1.8 },
    ));
    // Cars carry their weight low (engine, axles, fuel), about at the axles,
    // not in the middle of the body box: it keeps them on their wheels in turns.
    if d.kind == Kind::Car {
        commands.entity(root).insert((CenterOfMass(Vec3::new(centre.x, min.y + 0.4, centre.z)), NoAutoCenterOfMass));
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
        commands.spawn((
            Mesh3d(part.mesh.clone()),
            MeshMaterial3d(part.material.clone()),
            crate::original_shaders::PropPart,
            Transform::from_translation(offset),
            ChildOf(parent),
        ));
    }
    root
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
        let speed = vel.0.dot(t.rotation * Vec3::NEG_Z);
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
    vehicles: Query<(Entity, &Transform, &Vehicle), Without<crate::patrols::PatrolDriver>>,
    mut commands: Commands,
) {
    let Ok(pt) = players.single() else { return };
    for (e, t, v) in &vehicles {
        if !v.occupied && t.translation.distance(pt.translation) > 320.0 {
            commands.entity(e).despawn();
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
    settings: Res<crate::settings::Settings>,
    mut driving: ResMut<Driving>,
    mut hud: ResMut<crate::hud::HudState>,
    heights: Res<crate::terrain::Heightfield>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &Health), Without<Vehicle>>,
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle), Without<Player>>,
    wear: Query<&Wear>,
    crewed: Query<(), With<crate::patrols::PatrolDriver>>,
    mut commands: Commands,
) {
    let Ok((pe, mut pt, mut p, h)) = players.single_mut() else { return };
    let key = actions.label(Action::Interact, &settings);
    // Repairing only while E stays held at the vehicle (set again below).
    let was_repairing = repairing.vehicle.take();
    if let Some(ve) = driving.vehicle {
        // Out of a burnt-out wreck whatever the key.
        let exit = actions.just_pressed(Action::Interact) || h.dead || wear.get(ve).is_ok_and(|w| w.wrecked);
        if !exit {
            return;
        }
        actions.consume(Action::Interact);
        if let Ok((_, vt, mut v)) = vehicles.get_mut(ve) {
            v.occupied = false;
            v.parked = true;
            v.throttle = 0.0;
            v.steer = 0.0;
            // Step out on the driver's side (or above the water for boats).
            let side = vt.rotation * Vec3::new(v.min.x - 1.2, 0.0, 0.0);
            let mut at = vt.translation + Vec3::new(side.x, 0.0, side.z);
            let ground = heights.height_at(at).unwrap_or(at.y).max(crate::water::surface_at(at, &heights) - 1.0);
            at.y = ground + HALF_HEIGHT + 0.3;
            pt.translation = at;
        }
        p.driving = false;
        p.velocity = Vec3::ZERO;
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
        .map(|(e, t, v)| (e, t.translation.distance(pt.translation), v))
        .filter(|(_, d, v)| *d < (v.max - v.min).length() * 0.5 + 1.8)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((ve, _, v)) = best else {
        repairing.held = 0.0;
        return;
    };
    let name = DEFS[v.def].name.to_lowercase();
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
            hud.prompt = Some(format!("{key}  drive {name}    hold {key}  repair"));
            return;
        }
        hud.prompt = Some(format!("{key}  drive {name}    hold {key}  repair"));
        let tapped = repairing.held > 0.0 && repairing.held < REPAIR_HOLD;
        repairing.held = 0.0;
        if !tapped {
            return;
        }
    } else {
        repairing.held = 0.0;
        hud.prompt = Some(format!("{key}  drive {name}"));
        if !actions.just_pressed(Action::Interact) {
            return;
        }
    }
    actions.consume(Action::Interact);
    commands.entity(ve).remove::<crate::patrols::PatrolDriver>();
    if let Ok((_, vt, mut v)) = vehicles.get_mut(ve) {
        v.occupied = true;
        driving.last_yaw = vt.rotation.to_euler(EulerRot::YXZ).0;
        // Start looking the way the vehicle points.
        p.yaw = driving.last_yaw;
        p.pitch = -0.2;
    }
    driving.vehicle = Some(ve);
    p.driving = true;
    commands.entity(pe).insert(ColliderDisabled);
}

/// Keeps the player in the vehicle: a chase camera behind it, or the driver's
/// seat (V toggles). The view turns with the vehicle.
fn seat_player(
    mut driving: ResMut<Driving>,
    keys: Res<ButtonInput<KeyCode>>,
    actions: Res<ActionState>,
    spatial: SpatialQuery,
    mut players: Query<(&mut Transform, &mut Player), Without<Vehicle>>,
    mut vehicles: Query<(&Transform, &mut Vehicle), Without<Player>>,
) {
    let Some(ve) = driving.vehicle else { return };
    if keys.just_pressed(KeyCode::KeyV) {
        driving.first_person = !driving.first_person;
    }
    let (Ok((mut pt, mut p)), Ok((vt, mut v))) = (players.single_mut(), vehicles.get_mut(ve)) else { return };
    let yaw = vt.rotation.to_euler(EulerRot::YXZ).0;
    let turn = (yaw - driving.last_yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    driving.last_yaw = yaw;
    p.yaw += turn;
    let eye = if driving.first_person {
        vt.transform_point(v.seat())
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
    pt.translation = eye - Vec3::Y * (crate::player::EYE_HEIGHT - HALF_HEIGHT);
    pt.rotation = Quat::from_rotation_y(p.yaw);
    p.velocity = Vec3::ZERO;
    // Controls: throttle and steering (the camera looks freely).
    v.throttle = actions.movement.y;
    v.steer = -actions.movement.x;
    v.brake = actions.pressed(Action::Jump);
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
    mut vehicles: Query<(Entity, &Transform, &Vehicle, Forces, &ComputedMass, Has<crate::patrols::PatrolDriver>)>,
) {
    let dt = time.delta_secs().max(1e-4);
    for (e, t, v, mut f, mass, patrol) in &mut vehicles {
        let d = &DEFS[v.def];
        let m = mass.value();
        let up = t.rotation * Vec3::Y;
        // Model front is local -Z.
        let fwd = t.rotation * Vec3::NEG_Z;
        let right = t.rotation * Vec3::X;
        let vel = f.linear_velocity();
        let speed = vel.dot(fwd);
        match d.kind {
            Kind::Car => {
                let rest = 0.55;
                let radius = 0.42;
                let k = m * 9.81 / 4.0 / 0.22;
                let c = 2.0 * (k * m / 4.0).sqrt() * 0.45;
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
                    let mount = t.transform_point(w);
                    let Ok(down) = Dir3::new(-up) else { continue };
                    let Some(hit) = spatial.cast_ray(mount, down, rest + radius, true, &filter) else { continue };
                    grounded += 1;
                    let compression = (rest + radius - hit.distance).max(0.0);
                    let point_vel = f.velocity_at_point(mount);
                    let spring = k * compression - c * point_vel.dot(up);
                    f.apply_force_at_point(up * spring.max(0.0), mount);
                    // Front wheels steer (less at speed).
                    let front = i < 2;
                    let steer = if front { v.steer * 0.55 / (1.0 + speed.abs() * 0.06) } else { 0.0 };
                    let wheel_fwd = Quat::from_axis_angle(up, steer) * fwd;
                    let wheel_right = Quat::from_axis_angle(up, steer) * right;
                    let normal = spring.max(0.0);
                    // Lateral grip: cancel sideways slip, up to the friction limit.
                    let lat = point_vel.dot(wheel_right);
                    let grip = (-lat * m / 4.0 / dt * 0.35).clamp(-normal * 1.3, normal * 1.3);
                    f.apply_force_at_point(wheel_right * grip, mount);
                    // Drive on all wheels; braking and rolling resistance.
                    let along = point_vel.dot(wheel_fwd);
                    let mut drive = if flooded { 0.0 } else { v.throttle * d.engine / 4.0 };
                    if speed.abs() > d.top_speed && drive * speed > 0.0 {
                        drive = 0.0;
                    }
                    let parking = v.parked && !v.occupied;
                    if v.brake || parking || (v.throttle * speed < -0.5 && v.throttle.abs() > 0.1) {
                        drive = (-along * m / 4.0 / dt * 0.25).clamp(-normal, normal);
                    }
                    let rolling = -along * 30.0;
                    f.apply_force_at_point(wheel_fwd * (drive + rolling), mount);
                }
                if grounded == 0 {
                    // Airborne: a little self-righting so cars land on their wheels.
                    let tilt = up.cross(Vec3::Y);
                    f.apply_torque(tilt * m * 4.0);
                }
                // Air drag.
                f.apply_force(-vel * vel.length() * 0.9);
            }
            Kind::Boat => {
                let hull = [
                    Vec3::new(v.min.x, v.min.y + 0.3, v.min.z),
                    Vec3::new(v.max.x, v.min.y + 0.3, v.min.z),
                    Vec3::new(v.min.x, v.min.y + 0.3, v.max.z),
                    Vec3::new(v.max.x, v.min.y + 0.3, v.max.z),
                ];
                let mut wet = 0;
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
                if wet > 0 {
                    let flat_fwd = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
                    if speed.abs() < d.top_speed || v.throttle * speed < 0.0 {
                        f.apply_force(flat_fwd * v.throttle * d.engine);
                    }
                    // Rudder works with way on.
                    f.apply_torque(Vec3::Y * v.steer * m * 1.6 * (0.3 + speed.abs() / d.top_speed).min(1.3));
                    // Water drag: strong sideways, light lengthways.
                    let lat = vel.dot(right);
                    f.apply_force(-right * lat * m * 1.5 - flat_fwd * speed * m * 0.08);
                    let tilt = up.cross(Vec3::Y);
                    f.apply_torque(tilt * m * 6.0);
                }
            }
            Kind::Glider => {
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
) {
    let Some((v, lv)) = driving.vehicle.and_then(|e| vehicles.get(e).ok()) else { return };
    let kmh = lv.0.length() * 3.6;
    hud.status = Some(format!("{}   {:.0} km/h   ({} get out, V view)", DEFS[v.def].name.to_uppercase(), kmh, actions.label(Action::Interact, &settings)));
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
        let speed = lv.0.length();
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
        loops.write(LoopSfx { key: e.to_bits() ^ 0xE1, event: engine, at, volume, switches: [(snd::ENGINE_GROUP, case), (0, 0)], params });
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
            let slip = (sideways / 3.0 + if v.brake && speed > 5.0 { 1.2 } else { 0.0 }).min(2.0);
            let roll = if mine || d < 30.0 { snd::ROLL_SURFACE } else { snd::ROLL_SIMPLE };
            loops.write(LoopSfx {
                key: e.to_bits() ^ 0x70,
                event: snd::ROLLING,
                at,
                volume: if mine { 0.8 } else { 1.0 },
                switches: [(crate::surfaces::SURFACE_GROUP, surface), (snd::ROLL_GROUP, roll)],
                params: [(snd::SPEED, speed), (snd::SLIP, slip), (0, 0.0), (0, 0.0)],
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
        // Brakes squealing at speed.
        let braking = v.brake && speed > 8.0 && mine;
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
            driving.vehicle = Some(ve);
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
            info!("drive-test: in {} at {:?}", DEFS[v.def].name, vt.translation);
        }
        Some(ve) => {
            let t = now - start.unwrap_or(now);
            actions.movement = if t < 1.0 {
                Vec2::ZERO
            } else if t < 6.0 {
                Vec2::new(0.0, 1.0)
            } else if t < 9.0 {
                Vec2::new(0.6, 0.6)
            } else {
                Vec2::ZERO
            };
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
                }
            }
        }
        _ => {}
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
    mut vehicles: Query<(Entity, &Transform, &mut Vehicle, &mut Health, &mut Wear, &Children)>,
    parts: Query<&Children>,
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
    for (e, t, mut v, mut h, mut w, children) in &mut vehicles {
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
        let engine = t.transform_point(Vec3::new((v.min.x + v.max.x) * 0.5, v.max.y * 0.8 + v.min.y * 0.2, if front { v.min.z + 0.6 } else { v.max.z - 0.5 }));
        if share < SMOKING || h.dead {
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
        let centre = t.transform_point((v.min + v.max) * 0.5);
        explosions.write(crate::weapons::Explosion { at: centre, radius: 9.0, damage: 220.0, instigator: None });
        sfx.write(crate::sound::PlaySfx { event: snd::EXPLOSION, at: Some(centre), volume: 1.0 });
        w.wrecked = true;
        h.current = 0.0;
        h.dead = true;
        v.throttle = 0.0;
        v.steer = 0.0;
        v.brake = true;
        v.parked = true;
        for c in children.iter().chain(children.iter().flat_map(|c| parts.get(c).into_iter().flatten().copied())) {
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
    explosions.write(crate::weapons::Explosion { at: v.translation + Vec3::Y * 0.5, radius: 7.0, damage: 260.0, instigator: None });
    info!("wreck-test: a rocket hits at {:.0?}", v.translation);
}
