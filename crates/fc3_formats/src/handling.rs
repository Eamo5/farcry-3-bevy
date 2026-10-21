//! How the game's vehicles handle, from their archetypes in the entity
//! library. Cars (`CVehicleWheeledPhysComponent.WheeledParams`): mass, the
//! engine's power (horsepower) and its RPMs, the gearbox (each gear's ratio
//! and the km/h it holds to, and the speed in m/s the gearing reaches at the
//! engine's top RPM), the engine's cut-off speeds (m/s), steering lock at
//! rest and at top speed, the tyres' grip and the aerodynamic drag (units
//! as the engine's own field notes give them), and each wheel's grip and
//! suspension and whether the handbrake holds it. Boats (`BoatParams`): mass,
//! the motor's thrust (an acceleration, scaled by the RPM), how fast its RPM
//! rises, the water's friction ahead and to the side, and steering. Hang
//! gliders (`ParagliderParams`) and the wingsuit (`WingsuitParams`): how far
//! and how fast they bank and pitch, the turn a bank gives, the cruising,
//! braking and top speeds, the sink, and the landing mode near the ground;
//! and the wingsuit's parachute. And for every vehicle how its
//! driving camera widens with speed (`FOV`) and how far its driver can look
//! about (`CFCXVehicle`), and its lights (`VehicleLightSettings`, with what
//! its light switch puts on its slots). Keyed by the folder of the
//! vehicle's descriptor (`land\4x4`), as vehicles are placed by model.

use crate::fcb::Object;

pub const HANDLING_REL: &str = "vehicles/handling.txt";
const MAGIC: &str = "fc3-handling 13";

/// Whether a cached table is this version's.
pub fn is_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MAGIC)
}

/// A car's drivetrain and chassis.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Car {
    /// Horsepower, at the optimal RPM; the top RPM the gearing is worked
    /// out to; the share of the torque given near the top RPM.
    pub power: f32,
    pub optimal_rpm: f32,
    pub max_rpm: f32,
    pub torque_factor: f32,
    /// The speed (m/s) the top gear reaches at the engine's top RPM.
    pub gearbox_top: f32,
    /// Each gear's ratio and the speed (km/h) it holds to.
    pub gears: Vec<(f32, f32)>,
    pub reverse_ratio: f32,
    /// The engine cuts off above these speeds (m/s), ahead and in reverse.
    pub cutoff: f32,
    pub reverse_cutoff: f32,
    /// Steering lock (degrees) at rest and at top speed.
    pub steer_low: f32,
    pub steer_high: f32,
    /// The driven wheels' grip.
    pub friction: f32,
    /// Aerodynamic drag: its coefficient and the frontal area (m²).
    pub drag: f32,
    pub area: f32,
    /// The wheels, front first (left, right), as the data lists them.
    pub wheels: Vec<Wheel>,
    /// The share of its grip a wheel keeps when locked.
    pub lock_friction: f32,
    /// The chassis: aerodynamic lift (its coefficient; negative presses it
    /// down), extra gravity (m/s²), gravity's factor with no wheel on the
    /// ground (eased in over `airborne_fade` s, above `airborne_min_speed`
    /// m/s), the angular damping on the ground, and the stabiliser's turn
    /// rate in the air (rad/s; 0 none).
    pub lift: f32,
    pub extra_gravity: f32,
    pub airborne_gravity: f32,
    pub airborne_fade: f32,
    pub airborne_min_speed: f32,
    pub angular_damping: f32,
    pub stabilizer: f32,
}

/// A wheel's grip and suspension. The suspension figures are Havok's (its
/// notes say N/m and N/(m/s)), which are per kilogram of the chassis: the
/// spring pushes `strength` times the chassis mass per metre it is pressed
/// in, the dampers `compression` or `relaxation` times it per m/s.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Wheel {
    pub friction: f32,
    pub strength: f32,
    /// How far (m) the wheel hangs below its mount.
    pub length: f32,
    pub compression: f32,
    pub relaxation: f32,
    /// Held by the handbrake; driven by the engine.
    pub handbrake: bool,
    pub driving: bool,
}

/// A boat's motor, hull and rudder.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Boat {
    /// The motor's thrust ahead and astern (accelerations, at full RPM).
    pub forward: f32,
    pub reverse: f32,
    /// How fast the RPM rises (per second) to its top.
    pub rev_up: f32,
    pub max_rpm: f32,
    /// The water's friction ahead and to the side (independent of mass).
    pub forward_friction: f32,
    pub side_friction: f32,
    /// Steering (degrees) at rest and at the speed counted as its top (m/s).
    pub steer_low: f32,
    pub steer_high: f32,
    pub steer_top: f32,
    /// The motor's push against the way it is going (an acceleration, like
    /// its thrust), the brake (m/s²; 0 none) and the hull's angular damping.
    pub engine_braking: f32,
    pub brake: f32,
    pub angular_damping: f32,
    /// The water's friction going astern.
    pub reverse_friction: f32,
    /// The buoyancy spheres it floats on (those of any size: Dunia offsets
    /// from its centre of mass, radius). The water's friction acts at each
    /// one in the water, so its frictions add up that many times over.
    pub spheres: Vec<([f32; 3], f32)>,
    /// Its centre of mass (Dunia, from its origin).
    pub com: [f32; 3],
    /// The spheres' drop (m, negative: down) at `z_offset_speed` (m/s), in
    /// proportion to the speed: the hull rises as it gets going.
    pub z_offset: f32,
    pub z_offset_speed: f32,
    /// Gravity's factor with any sphere in the water, and with none.
    pub gravity: f32,
    pub gravity_air: f32,
    /// The water's friction on its rising and falling (per sphere, with the
    /// speed), and going down into it (with the speed squared, as deep as
    /// the sphere is in).
    pub updown_friction: f32,
    pub down_friction: f32,
    /// Friction on its turning about its yaw, and on its rolling and
    /// pitching going down into the water (per sphere).
    pub up_rotate_friction: f32,
    pub down_rotate_friction: f32,
    /// The stabiliser's turn rate righting it, past its angle (degrees).
    pub stabilizer: f32,
    pub stabilizer_angle: f32,
}

/// A hang glider's flight (angles in radians, speeds in m/s, rates per
/// second; as the engine's field notes describe them).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Glider {
    /// Banking: the largest bank; the bank still to go times `roll_rate`
    /// is the wanted roll speed (up to `roll_speed`), reached at `roll_acc`.
    pub max_roll: f32,
    pub roll_rate: f32,
    pub roll_speed: f32,
    pub roll_acc: f32,
    /// Pitching, likewise.
    pub max_pitch: f32,
    pub pitch_rate: f32,
    pub pitch_speed: f32,
    pub pitch_acc: f32,
    /// Speed gained per radian of pitch (negative: nose down speeds up).
    pub pitch_to_speed: f32,
    /// The turn rate per radian of bank, reached at `turn_acc`, in full
    /// from `turn_full_speed` (none at a standstill).
    pub roll_to_turn: f32,
    pub turn_acc: f32,
    pub turn_full_speed: f32,
    /// Climbing, the speed it fades to before the nose drops back.
    pub stall_speed: f32,
    /// The cruising speed; the sink always applied (negative: down).
    pub base_speed: f32,
    pub sink: f32,
    /// Below this share of health it sinks at `broken_sink`.
    pub broken_health: f32,
    pub broken_sink: f32,
    /// Below this height it sinks at `low_sink` instead.
    pub low_height: f32,
    pub low_sink: f32,
    /// Braking towards `brake_floor`, and back to cruising after.
    pub braking: f32,
    pub brake_restore: f32,
    pub brake_floor: f32,
    pub max_speed: f32,
    /// The take-off run (seconds without controls).
    pub takeoff: f32,
    /// Landing mode below `landing_height`: its cruising speed, banks,
    /// pitches, top speed and sink, eased into and out of (seconds).
    pub landing_height: f32,
    pub landing_speed: f32,
    pub landing_roll: f32,
    pub landing_pitch: f32,
    pub landing_max_speed: f32,
    pub landing_sink: f32,
    pub to_landing: f32,
    pub from_landing: f32,
}

const GLIDER_FIELDS: [&str; 32] = [
    "fMaxRollAngle",
    "fRollVelScale",
    "fRollVelMax",
    "fRollAcc",
    "fMaxPitchAngle",
    "fPitchVelScale",
    "fPitchVelMax",
    "fPitchAcc",
    "fPitchSpeedScale",
    "fRollAngleToTurnSpeed",
    "fTurnAcc",
    "fSpeedTurnMax",
    "fUpPitchAngleSpeedFade",
    "fBaseSpeed",
    "fDownSpeed",
    "fMalfunctionHealthThreshold",
    "fMalfunctionDownSpeed",
    "fLowAltitudeGlideHeight",
    "fLowAltitudeDownSpeed",
    "fBrakingRate",
    "fBrakeRestoreSpeedRate",
    "fMinBrakeSpeed",
    "fMaxSpeed",
    "fTakeOffRunTime",
    "fAltitudeToTriggerLandingMode",
    "fLandingBaseSpeed",
    "fLandingMaxRollAngle",
    "fLandingMaxPitchAngle",
    "fLandingMaxSpeed",
    "fLandingDownSpeed",
    "fToLandingModeTransitionTime",
    "fFromLandingModeTransitionTime",
];

impl Glider {
    fn values(&self) -> [f32; 32] {
        [
            self.max_roll,
            self.roll_rate,
            self.roll_speed,
            self.roll_acc,
            self.max_pitch,
            self.pitch_rate,
            self.pitch_speed,
            self.pitch_acc,
            self.pitch_to_speed,
            self.roll_to_turn,
            self.turn_acc,
            self.turn_full_speed,
            self.stall_speed,
            self.base_speed,
            self.sink,
            self.broken_health,
            self.broken_sink,
            self.low_height,
            self.low_sink,
            self.braking,
            self.brake_restore,
            self.brake_floor,
            self.max_speed,
            self.takeoff,
            self.landing_height,
            self.landing_speed,
            self.landing_roll,
            self.landing_pitch,
            self.landing_max_speed,
            self.landing_sink,
            self.to_landing,
            self.from_landing,
        ]
    }

    fn from_values(v: &[f32]) -> Option<Glider> {
        let v: &[f32; 32] = v.get(..32)?.try_into().ok()?;
        Some(Glider {
            max_roll: v[0],
            roll_rate: v[1],
            roll_speed: v[2],
            roll_acc: v[3],
            max_pitch: v[4],
            pitch_rate: v[5],
            pitch_speed: v[6],
            pitch_acc: v[7],
            pitch_to_speed: v[8],
            roll_to_turn: v[9],
            turn_acc: v[10],
            turn_full_speed: v[11],
            stall_speed: v[12],
            base_speed: v[13],
            sink: v[14],
            broken_health: v[15],
            broken_sink: v[16],
            low_height: v[17],
            low_sink: v[18],
            braking: v[19],
            brake_restore: v[20],
            brake_floor: v[21],
            max_speed: v[22],
            takeoff: v[23],
            landing_height: v[24],
            landing_speed: v[25],
            landing_roll: v[26],
            landing_pitch: v[27],
            landing_max_speed: v[28],
            landing_sink: v[29],
            to_landing: v[30],
            from_landing: v[31],
        })
    }
}

/// The wingsuit's parachute (`WingsuitParams.ParachuteControl` and its
/// landing logic): speeds in m/s, the sink negative.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Chute {
    /// Drifting ahead: at rest, and pushed on.
    pub base_speed: f32,
    pub max_speed: f32,
    pub sink: f32,
    /// Its largest bank (radians), and how quickly (per second) it answers
    /// left and right, ahead and back.
    pub max_roll: f32,
    pub side_rate: f32,
    pub front_rate: f32,
    /// Opening it: its pull (N·s) and over how long (s).
    pub deploy_impulse: f32,
    pub deploy_time: f32,
    /// It opens by itself this long (s) before the ground, held that long.
    pub auto_eta: f32,
    pub auto_hold: f32,
    /// Whether the wingsuit may pitch its nose up.
    pub pitch_up: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Handling {
    /// The descriptor's folder under `vehicles\` (lower case, `land\4x4`).
    pub folder: String,
    /// The archetype it was read from (`Land/4x4`).
    pub archetype: String,
    pub mass: f32,
    pub car: Option<Car>,
    pub boat: Option<Boat>,
    pub glider: Option<Glider>,
    pub chute: Option<Chute>,
    /// The driver's camera: its view angle at the low speed and at the high
    /// one (`[low speed, high speed, angle at low, angle at high]`; speeds
    /// in km/h, as the engine notes them). 0s: none.
    pub fov: [f32; 4],
    /// How far the driver can look from straight ahead, as the game counts
    /// it (degrees: `[least pitch, most pitch, least yaw, most yaw]`, pitch
    /// down positive: an open 4x4 looks far up and not down, closed cars
    /// barely up, the hang glider and wingsuit far down). 0s: none.
    pub look: [f32; 4],
    pub lights: Option<Lights>,
}

/// A vehicle's lights (`CFCXVehicle.VehicleLightSettings`): its dynamic light
/// (one spot, at the model's `DynamicLight` node), the hours they're on, and
/// what its light switch (`CGraphicComponent.MaterialLightSwitch`) puts on
/// its slots then (the lamps' glass, lit).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lights {
    /// On from this hour, off at that one.
    pub on_hour: f32,
    pub off_hour: f32,
    /// The spot's reach (m) and where its fall begins.
    pub range: f32,
    pub fade_start: f32,
    /// Its cone's full angles (degrees), inner and outer.
    pub inner_angle: f32,
    pub outer_angle: f32,
    pub intensity: f32,
    /// The brake lights' share while not braking.
    pub brake_dim: f32,
    /// The slots switched (the model's material name) and what they wear.
    pub swaps: Vec<(String, String)>,
}

fn lights(components: &Object) -> Option<Lights> {
    let s = components.child("CFCXVehicle")?.child("VehicleLightSettings")?;
    let f = |n: &str| float(s, n).unwrap_or(0.0);
    let mut swaps = Vec::new();
    if let Some(switch) = components.child("CGraphicComponent").and_then(|g| g.child("MaterialLightSwitch")) {
        for slots in &switch.children {
            for m in &slots.children {
                let (Some(name), Some(file)) = (m.str_h(0x8BAC_1025), m.str_h(0x9301_DFBD)) else { continue };
                if !name.is_empty() && file.to_ascii_lowercase().ends_with(".material.bin") {
                    swaps.push((name.to_ascii_lowercase(), file.to_ascii_lowercase()));
                }
            }
        }
    }
    let l = Lights {
        on_hour: f("fLightsOnTimeOfDay"),
        off_hour: f("fLightsOffTimeOfDay"),
        range: f("fDynamicLightRange"),
        fade_start: f("fDynamicLightFadeStartRange"),
        inner_angle: f("fDynamicLightInnerAngle"),
        outer_angle: f("fDynamicLightOuterAngle"),
        intensity: f("fLightIntensity"),
        brake_dim: float(s, "fBrakeLightDimmedFactor").unwrap_or(1.0),
        swaps,
    };
    (l.range > 0.0 && l.intensity > 0.0).then_some(l)
}

impl Lights {
    /// Whether they're on at an hour of the day.
    pub fn on_at(&self, hour: f32) -> bool {
        let h = hour.rem_euclid(24.0);
        if self.on_hour <= self.off_hour {
            (self.on_hour..self.off_hour).contains(&h)
        } else {
            h >= self.on_hour || h < self.off_hour
        }
    }
}

fn float(o: &Object, name: &str) -> Option<f32> {
    let b = o.field(name)?;
    match b.len() {
        4 => Some(f32::from_le_bytes(b.try_into().ok()?)),
        1 => Some(b[0] as f32),
        _ => None,
    }
}

fn vec3(o: &Object, name: &str) -> Option<[f32; 3]> {
    let b = o.field(name)?;
    let f = |i: usize| Some(f32::from_le_bytes(b.get(i * 4..i * 4 + 4)?.try_into().ok()?));
    Some([f(0)?, f(1)?, f(2)?])
}

fn car(w: &Object) -> Option<Car> {
    let f = |n: &str| float(w, n).unwrap_or(0.0);
    let gears: Vec<(f32, f32)> = w
        .child("GearsConfig")
        .map(|g| g.children_named("Gear").filter_map(|g| Some((float(g, "GearRatio")?, float(g, "MaxSpeed")?))).filter(|(r, s)| *r > 0.0 && *s > 0.0).collect())
        .unwrap_or_default();
    let steering = w.child("Steering");
    let car = Car {
        power: f("fEnginePower"),
        optimal_rpm: f("fEngineOptimalRPM"),
        max_rpm: f("fEngineMaxRPM"),
        torque_factor: f("fEngineTorqueFactorAtMaxRPM"),
        gearbox_top: f("fGearBoxTopSpeed"),
        gears,
        reverse_ratio: f("fReverseGearRatio"),
        cutoff: f("fEngineCutOffSpeed"),
        reverse_cutoff: f("fEngineReverseCutOffSpeed"),
        steer_low: steering.and_then(|s| float(s, "fLowMaxAngle")).unwrap_or(0.0),
        steer_high: steering.and_then(|s| float(s, "fHighMaxAngle")).unwrap_or(0.0),
        friction: w.child("Wheels").and_then(|ws| ws.children_named("Wheel").find_map(|wh| float(wh, "fFriction"))).unwrap_or(0.0),
        drag: f("fAeroDragCoefficient"),
        area: f("fAeroFrontalArea"),
        wheels: w
            .child("Wheels")
            .map(|ws| {
                ws.children_named("Wheel")
                    .map(|wh| {
                        let f = |n: &str| float(wh, n).unwrap_or(0.0);
                        Wheel {
                            friction: f("fFriction"),
                            strength: f("fSuspStrength"),
                            length: f("fSuspLength"),
                            compression: f("fSuspDampingCompression"),
                            relaxation: f("fSuspDampingRelaxation"),
                            handbrake: f("bHandBrake") > 0.0,
                            driving: f("bDriving") > 0.0,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        lock_friction: float(w, "WheelLockFrictionScale").unwrap_or(1.0),
        lift: f("fAeroLiftCoefficient"),
        extra_gravity: f("fAeroExtraGravity"),
        airborne_gravity: w.child("WheelAirborne").and_then(|a| float(a, "WheelAirborneGravityModifier")).unwrap_or(1.0),
        airborne_fade: w.child("WheelAirborne").and_then(|a| float(a, "WheelAirborneGravityModifierFadeInTime")).unwrap_or(0.0),
        airborne_min_speed: w.child("WheelAirborne").and_then(|a| float(a, "WheelAirborneGravityModifierMinSpeed")).unwrap_or(0.0),
        angular_damping: f("fChassisAngularDamping"),
        stabilizer: w.child("WheelAirborne").and_then(|a| float(a, "fStabilizerAccel")).unwrap_or(0.0),
    };
    (car.power > 0.0 && car.optimal_rpm > 0.0 && car.max_rpm > 0.0 && car.gearbox_top > 0.0 && car.cutoff > 0.0 && !car.gears.is_empty()).then_some(car)
}

fn boat(b: &Object) -> Option<Boat> {
    let f = |n: &str| float(b, n).unwrap_or(0.0);
    let boat = Boat {
        forward: f("fForwardEnginePower"),
        reverse: f("fReverseEnginePower"),
        rev_up: f("fMotorRevUpPerSec"),
        max_rpm: f("fMotorMaxRPM"),
        forward_friction: f("fForwardFriction"),
        side_friction: f("fSideFriction"),
        steer_low: f("fSteerAngleLowSpeed"),
        steer_high: f("fSteerAngleHighSpeed"),
        steer_top: f("fSteerMaxSpeed"),
        engine_braking: f("fEngineBrakingPower"),
        brake: f("Brake"),
        angular_damping: f("fAngularDamping"),
        reverse_friction: f("fReverseFriction"),
        spheres: b
            .child("BuoyancySphere")
            .map(|s| s.children_named("Spheres").filter_map(|s| Some((vec3(s, "LocalPosition")?, float(s, "Radius")?))).filter(|(_, r)| *r > 0.0).collect())
            .unwrap_or_default(),
        com: vec3(b, "vectorCenterOfMassOffset").unwrap_or_default(),
        z_offset: f("fSpheresZOffsetMax"),
        z_offset_speed: f("fSpheresZOffsetMaxSpeed"),
        gravity: float(b, "GravityModifier").unwrap_or(1.0),
        gravity_air: float(b, "GravityModifierAirborne").unwrap_or(1.0),
        updown_friction: f("fUpDownFriction"),
        down_friction: f("fDownFriction"),
        up_rotate_friction: f("fUpRotateFriction"),
        down_rotate_friction: f("fDownRotateFriction"),
        stabilizer: f("StabilizerAccel"),
        stabilizer_angle: float(b, "fStabilizerRollAngleActivation").unwrap_or(45.0),
    };
    (boat.forward > 0.0 && boat.forward_friction > 0.0).then_some(boat)
}

fn glider(p: &Object) -> Option<Glider> {
    let v: Vec<f32> = GLIDER_FIELDS.iter().map(|n| float(p, n).unwrap_or(0.0)).collect();
    Glider::from_values(&v).filter(|g| g.base_speed > 0.0 && g.max_speed > 0.0)
}

fn chute(w: &Object) -> Option<Chute> {
    let c = w.child("ParachuteControl")?;
    let f = |n: &str| float(c, n).unwrap_or(0.0);
    let impulse = w.child("Impulses").and_then(|i| i.child("DeployParachuteImpulse"));
    let logic = w.child("ParachuteLandingLogic");
    let chute = Chute {
        base_speed: f("fParachuteBaseSpeed"),
        max_speed: f("fParachuteMaxSpeed"),
        sink: f("fParachuteDownSpeed"),
        max_roll: f("fParachuteMaxRollAngle"),
        side_rate: f("fLeftRightFactor"),
        front_rate: f("fFrontBackFactor"),
        deploy_impulse: impulse.and_then(|i| float(i, "fAmount")).unwrap_or(0.0),
        deploy_time: impulse.and_then(|i| float(i, "fTime")).unwrap_or(0.0),
        auto_eta: logic.and_then(|l| float(l, "fETAForAutoParachute")).unwrap_or(0.0),
        auto_hold: logic.and_then(|l| float(l, "fTimeForAutoParachute")).unwrap_or(0.0),
        pitch_up: w.child("PitchUp").and_then(|p| float(p, "bAllowPitchUp")).is_none_or(|b| b > 0.0),
    };
    (chute.sink < 0.0).then_some(chute)
}

/// The vehicles' handling, one per descriptor folder (the plainest
/// archetype: the shortest name).
pub fn handling(library: &Object) -> Vec<Handling> {
    let mut out: Vec<Handling> = Vec::new();
    library.walk(&mut |o, _| {
        if !o.is("EntityPrototype") {
            return;
        }
        let Some(e) = o.child("Entity") else { return };
        let Some(name) = e.str("hidName").and_then(|n| n.strip_prefix("FC3_Vehicles:").map(str::to_string)) else { return };
        let Some(components) = e.child("Components") else { return };
        let Some(path) = components.child("CFileDescriptorComponent").and_then(|d| d.str_h(0x2A7B_CA49)).map(|p| p.to_ascii_lowercase()) else { return };
        let Some(folder) = path.split_once(r"\vehicles\").and_then(|(_, rest)| rest.rsplit_once('\\')).map(|(f, _)| f.to_string()) else { return };
        // The driver's camera (the first seat's), the wheels or the hull.
        let mut fov = [0.0f32; 4];
        let (mut wheeled, mut hull, mut wing) = (None, None, None);
        // The driver's look: pitch (x) and yaw (z).
        let look = components
            .child("CFCXVehicle")
            .and_then(|v| Some((vec3(v, "vehicleMinLookAngle")?, vec3(v, "vehicleMaxLookAngle")?)))
            .map_or([0.0; 4], |(lo, hi)| [lo[0], hi[0], lo[2], hi[2]]);
        components.walk(&mut |o, _| {
            if fov[1] <= 0.0 && o.is("FOV") {
                let f = |n: &str| float(o, n).unwrap_or(0.0);
                fov = [f("VehicleMinSpeed"), f("VehicleMaxSpeed"), f("FOVAngleAtMinSpeed"), f("FOVAngleAtMaxSpeed")];
            }
            if wheeled.is_none() && o.is("WheeledParams") {
                wheeled = Some(o);
            }
            if hull.is_none() && o.is("BoatParams") {
                hull = Some(o);
            }
            if wing.is_none() && (o.is("ParagliderParams") || o.is("WingsuitParams")) {
                wing = Some(o);
            }
        });
        let h = Handling {
            folder,
            archetype: name,
            mass: wheeled.or(hull).or(wing).and_then(|o| float(o, "fMass")).unwrap_or(0.0),
            car: wheeled.and_then(car),
            boat: hull.and_then(boat),
            glider: wing.and_then(glider),
            chute: wing.and_then(chute),
            fov,
            look,
            lights: lights(components),
        };
        if h.car.is_none() && h.boat.is_none() && h.glider.is_none() && h.fov[1] <= 0.0 {
            return;
        }
        match out.iter_mut().find(|o| o.folder == h.folder) {
            Some(o) if h.archetype.len() < o.archetype.len() => *o = h,
            Some(_) => {}
            None => out.push(h),
        }
    });
    out.sort_by(|a, b| a.folder.cmp(&b.folder));
    out
}

fn nums(v: &[f32]) -> String {
    v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
}

pub fn to_text(list: &[Handling]) -> String {
    let mut s = format!("{MAGIC}\n");
    for h in list {
        let car = h.car.as_ref().map_or("-".to_string(), |c| {
            let gears: Vec<String> = c.gears.iter().map(|(r, k)| format!("{r}:{k}")).collect();
            let v = [
                c.power,
                c.optimal_rpm,
                c.max_rpm,
                c.torque_factor,
                c.gearbox_top,
                c.reverse_ratio,
                c.cutoff,
                c.reverse_cutoff,
                c.steer_low,
                c.steer_high,
                c.friction,
                c.drag,
                c.area,
                c.lock_friction,
                c.lift,
                c.extra_gravity,
                c.airborne_gravity,
                c.airborne_fade,
                c.airborne_min_speed,
                c.angular_damping,
                c.stabilizer,
            ];
            let wheels: Vec<String> = c
                .wheels
                .iter()
                .map(|w| {
                    format!("{}:{}:{}:{}:{}:{}:{}", w.friction, w.strength, w.length, w.compression, w.relaxation, u8::from(w.handbrake), u8::from(w.driving))
                })
                .collect();
            format!("{};{};{}", nums(&v), gears.join(" "), wheels.join(" "))
        });
        let boat = h.boat.as_ref().map_or("-".to_string(), |b| {
            nums(
                &[
                    b.forward,
                    b.reverse,
                    b.rev_up,
                    b.max_rpm,
                    b.forward_friction,
                    b.side_friction,
                    b.steer_low,
                    b.steer_high,
                    b.steer_top,
                    b.engine_braking,
                    b.brake,
                    b.angular_damping,
                    b.spheres.len() as f32,
                    b.reverse_friction,
                    b.com[0],
                    b.com[1],
                    b.com[2],
                    b.z_offset,
                    b.z_offset_speed,
                    b.gravity,
                    b.gravity_air,
                    b.updown_friction,
                    b.down_friction,
                    b.up_rotate_friction,
                    b.down_rotate_friction,
                    b.stabilizer,
                    b.stabilizer_angle,
                ]
                .into_iter()
                .chain(b.spheres.iter().flat_map(|(p, r)| [p[0], p[1], p[2], *r]))
                .collect::<Vec<_>>(),
            )
        });
        let glider = h.glider.as_ref().map_or("-".to_string(), |g| nums(&g.values()));
        let chute = h.chute.as_ref().map_or("-".to_string(), |c| {
            nums(&[
                c.base_speed,
                c.max_speed,
                c.sink,
                c.max_roll,
                c.side_rate,
                c.front_rate,
                c.deploy_impulse,
                c.deploy_time,
                c.auto_eta,
                c.auto_hold,
                f32::from(u8::from(c.pitch_up)),
            ])
        });
        let lights = h.lights.as_ref().map_or("-".to_string(), |l| {
            let swaps: Vec<String> = l.swaps.iter().map(|(slot, file)| format!("{slot}={file}")).collect();
            format!(
                "{};{}",
                nums(&[l.on_hour, l.off_hour, l.range, l.fade_start, l.inner_angle, l.outer_angle, l.intensity, l.brake_dim]),
                swaps.join("|")
            )
        });
        s += &format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            h.folder,
            h.archetype,
            h.mass,
            car,
            boat,
            nums(&h.fov),
            glider,
            nums(&h.look),
            chute,
            lights
        );
    }
    s
}

pub fn from_text(text: &str) -> anyhow::Result<Vec<Handling>> {
    let mut lines = text.lines();
    if !lines.next().is_some_and(is_current) {
        anyhow::bail!("not a current handling table");
    }
    let floats = |s: &str| s.split(',').filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<f32>>();
    Ok(lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 9 {
                return None;
            }
            let car = f[3].split_once(';').and_then(|(n, rest)| {
                let v = floats(n);
                let (g, w) = rest.split_once(';').unwrap_or((rest, ""));
                let gears = g.split(' ').filter_map(|x| x.split_once(':')).filter_map(|(r, k)| Some((r.parse().ok()?, k.parse().ok()?))).collect();
                let wheels = w
                    .split(' ')
                    .filter_map(|x| {
                        let n: Vec<f32> = x.split(':').filter_map(|n| n.parse().ok()).collect();
                        (n.len() == 7).then(|| Wheel {
                            friction: n[0],
                            strength: n[1],
                            length: n[2],
                            compression: n[3],
                            relaxation: n[4],
                            handbrake: n[5] > 0.0,
                            driving: n[6] > 0.0,
                        })
                    })
                    .collect();
                (v.len() >= 21).then(|| Car {
                    power: v[0],
                    optimal_rpm: v[1],
                    max_rpm: v[2],
                    torque_factor: v[3],
                    gearbox_top: v[4],
                    gears,
                    reverse_ratio: v[5],
                    cutoff: v[6],
                    reverse_cutoff: v[7],
                    steer_low: v[8],
                    steer_high: v[9],
                    friction: v[10],
                    drag: v[11],
                    area: v[12],
                    wheels,
                    lock_friction: v[13],
                    lift: v[14],
                    extra_gravity: v[15],
                    airborne_gravity: v[16],
                    airborne_fade: v[17],
                    airborne_min_speed: v[18],
                    angular_damping: v[19],
                    stabilizer: v[20],
                })
            });
            let boat = (f[4] != "-").then(|| floats(f[4])).filter(|v| v.len() >= 27 + 4 * v[12] as usize).map(|v| Boat {
                forward: v[0],
                reverse: v[1],
                rev_up: v[2],
                max_rpm: v[3],
                forward_friction: v[4],
                side_friction: v[5],
                steer_low: v[6],
                steer_high: v[7],
                steer_top: v[8],
                engine_braking: v[9],
                brake: v[10],
                angular_damping: v[11],
                reverse_friction: v[13],
                spheres: v[27..27 + 4 * v[12] as usize].chunks(4).map(|c| ([c[0], c[1], c[2]], c[3])).collect(),
                com: [v[14], v[15], v[16]],
                z_offset: v[17],
                z_offset_speed: v[18],
                gravity: v[19],
                gravity_air: v[20],
                updown_friction: v[21],
                down_friction: v[22],
                up_rotate_friction: v[23],
                down_rotate_friction: v[24],
                stabilizer: v[25],
                stabilizer_angle: v[26],
            });
            let fov = floats(f[5]);
            let glider = (f[6] != "-").then(|| floats(f[6])).and_then(|v| Glider::from_values(&v));
            let look = floats(f[7]);
            let chute = (f[8] != "-").then(|| floats(f[8])).filter(|v| v.len() >= 11).map(|v| Chute {
                base_speed: v[0],
                max_speed: v[1],
                sink: v[2],
                max_roll: v[3],
                side_rate: v[4],
                front_rate: v[5],
                deploy_impulse: v[6],
                deploy_time: v[7],
                auto_eta: v[8],
                auto_hold: v[9],
                pitch_up: v[10] > 0.0,
            });
            let lights = f.get(9).filter(|l| **l != "-").and_then(|l| {
                let (n, swaps) = l.split_once(';').unwrap_or((l, ""));
                let v = floats(n);
                (v.len() >= 8).then(|| Lights {
                    on_hour: v[0],
                    off_hour: v[1],
                    range: v[2],
                    fade_start: v[3],
                    inner_angle: v[4],
                    outer_angle: v[5],
                    intensity: v[6],
                    brake_dim: v[7],
                    swaps: swaps.split('|').filter_map(|s| s.split_once('=')).map(|(a, b)| (a.to_string(), b.to_string())).collect(),
                })
            });
            Some(Handling {
                folder: f[0].to_string(),
                archetype: f[1].to_string(),
                mass: f[2].parse().ok()?,
                car,
                boat,
                glider,
                chute,
                fov: [*fov.first()?, *fov.get(1)?, *fov.get(2)?, *fov.get(3)?],
                look: [*look.first()?, *look.get(1)?, *look.get(2)?, *look.get(3)?],
                lights,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handling_text_round_trips() {
        let list = vec![
            Handling {
                folder: r"land\truck".into(),
                archetype: "Land/Truck".into(),
                mass: 1500.0,
                car: Some(Car {
                    power: 300.0,
                    optimal_rpm: 3000.0,
                    max_rpm: 6500.0,
                    torque_factor: 0.5,
                    gearbox_top: 60.0,
                    gears: vec![(2.5, 40.0), (1.5, 60.0), (1.0, 80.0)],
                    reverse_ratio: 2.5,
                    cutoff: 22.0,
                    reverse_cutoff: 15.0,
                    steer_low: 35.0,
                    steer_high: 30.0,
                    friction: 1.2,
                    drag: 0.4,
                    area: 2.5,
                    wheels: vec![
                        Wheel { friction: 1.2, strength: 30.0, length: 0.3, compression: 3.0, relaxation: 2.0, handbrake: false, driving: false },
                        Wheel { friction: 1.1, strength: 25.0, length: 0.2, compression: 3.0, relaxation: 2.0, handbrake: true, driving: true },
                    ],
                    lock_friction: 0.7,
                    lift: -1.5,
                    extra_gravity: 2.0,
                    airborne_gravity: 1.5,
                    airborne_fade: 0.3,
                    airborne_min_speed: 2.0,
                    angular_damping: 0.1,
                    stabilizer: 3.0,
                }),
                boat: None,
                glider: None,
                chute: None,
                fov: [1.0, 55.0, 85.0, 115.0],
                look: [-70.0, 5.0, -80.0, 80.0],
                lights: Some(Lights {
                    on_hour: 17.75,
                    off_hour: 6.5,
                    range: 30.0,
                    fade_start: 15.0,
                    inner_angle: 30.0,
                    outer_angle: 120.0,
                    intensity: 1.0,
                    brake_dim: 0.5,
                    swaps: vec![("fmadore-m-2011030340291388".into(), r"graphics\_materials\mckoung-m-20120508173554.material.bin".into())],
                }),
            },
            Handling {
                folder: r"sea\raft".into(),
                archetype: "Sea/Raft".into(),
                mass: 300.0,
                car: None,
                boat: Some(Boat {
                    forward: 12.0,
                    reverse: 3.0,
                    rev_up: 4000.0,
                    max_rpm: 7000.0,
                    forward_friction: 0.25,
                    side_friction: 0.75,
                    steer_low: 20.0,
                    steer_high: 35.0,
                    steer_top: 12.0,
                    engine_braking: 4.0,
                    brake: 3.0,
                    angular_damping: 0.25,
                    reverse_friction: 0.5,
                    spheres: vec![([1.0, 1.5, -0.25], 1.25), ([-1.0, 1.5, -0.25], 1.25), ([0.0, -1.5, 0.0], 1.0)],
                    com: [0.0, -0.5, -0.25],
                    z_offset: -0.5,
                    z_offset_speed: 5.0,
                    gravity: 1.5,
                    gravity_air: 1.25,
                    updown_friction: 1.25,
                    down_friction: 0.5,
                    up_rotate_friction: 0.75,
                    down_rotate_friction: 4.0,
                    stabilizer: 3.0,
                    stabilizer_angle: 40.0,
                }),
                glider: None,
                chute: None,
                fov: [1.0, 55.0, 85.0, 105.0],
                look: [0.0; 4],
                lights: None,
            },
            Handling {
                folder: r"air\glider".into(),
                archetype: "Air/Paraglider".into(),
                mass: 450.0,
                car: None,
                boat: None,
                glider: Some(Glider {
                    max_roll: 0.7,
                    roll_rate: 1.5,
                    base_speed: 12.0,
                    sink: -1.5,
                    max_speed: 25.0,
                    landing_sink: -0.75,
                    ..Default::default()
                }),
                chute: Some(Chute {
                    base_speed: 5.0,
                    max_speed: 8.0,
                    sink: -5.0,
                    max_roll: 0.25,
                    side_rate: 3.0,
                    front_rate: 2.0,
                    deploy_impulse: 2000.0,
                    deploy_time: 0.5,
                    auto_eta: 0.3,
                    auto_hold: 0.2,
                    pitch_up: false,
                }),
                fov: [30.0, 45.0, 80.0, 95.0],
                look: [-60.0, 80.0, -50.0, 50.0],
                lights: Some(Lights { on_hour: 18.0, off_hour: 6.0, range: 20.0, intensity: 0.5, brake_dim: 1.0, ..Default::default() }),
            },
        ];
        assert_eq!(from_text(&to_text(&list)).unwrap(), list);
    }

    #[test]
    fn lights_come_on_overnight() {
        let l = Lights { on_hour: 17.75, off_hour: 6.5, ..Default::default() };
        assert!(l.on_at(18.0) && l.on_at(23.9) && l.on_at(0.0) && l.on_at(6.4) && l.on_at(30.0));
        assert!(!l.on_at(6.5) && !l.on_at(12.0) && !l.on_at(17.7));
        let day = Lights { on_hour: 8.0, off_hour: 9.0, ..Default::default() };
        assert!(day.on_at(8.5) && !day.on_at(9.5));
    }
}
