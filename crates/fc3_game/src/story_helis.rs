//! Reinforcements that come by helicopter: a reinforcement point whose
//! vehicle is a helicopter (`CVehicleHelicopterPhysComponent`, the
//! Blackhawk) has it fly the point's vehicle path (`entVehiclePathId`) at
//! the point's speed with its crew aboard. At the path's end it hovers,
//! facing the player, its crewman at the belly gun firing while it can see
//! him; or, where the point says so (`bVehicleLandOnArrival`), it sets down
//! and its crew get out to search the battlefield.
//!
//! Its own data gives its health and what rounds and rockets do to it
//! (`fMaxHealth`, `fPierceDamageMultiplier`, `fExplosionRPGDamageMultiplier`:
//! a rocket brings one down, a rifle takes a long while). Brought down, it
//! falls spinning and smoking, its crew killed with it (by whoever brought
//! it down), and blows up where it lands, a burnt wreck; the scripts hear it
//! destroyed. G3's Trial of the Rakyat is fought so: the player's rocket
//! launcher against wave on wave of them.

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::cache_assets::Fc3Model;
use crate::combat::{DamageEvent, DamageKind, Health};
use crate::player::{Player, HALF_HEIGHT};
use crate::story_actors::StoryActor;

pub struct StoryHelisPlugin;

impl Plugin for StoryHelisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Flights>()
            .add_systems(Update, (note_hits, fly).chain().after(crate::story_actors::ActorSet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PostUpdate, (spin_rotors, carry_crew).before(TransformSystems::Propagate))
            .add_systems(Update, look_test.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The model a wrecked Blackhawk leaves.
const WRECK_MODEL: &str = r"graphics\__fc3_graphics\_common\vehicles\air\blackhawk\blackhawk_destroyed.xbg";

/// What a helicopter archetype's own data gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct HeliData {
    pub model: String,
    pub health: f32,
    /// What rounds and rockets do to it (of their damage).
    pub bullets: f32,
    pub rockets: f32,
}

/// A helicopter archetype's data; None for anything else.
pub fn heli_data(archetype: &fc3_story::data::Entry) -> Option<HeliData> {
    let phys = archetype.component("CVehicleHelicopterPhysComponent")?;
    let model = crate::story_rides::model_of(archetype)?;
    let first = |name: &str| {
        let mut v = None;
        phys.walk(&mut |o, _| {
            if v.is_none() {
                v = o.f32(name);
            }
        });
        v
    };
    Some(HeliData {
        model,
        health: first("fMaxHealth").filter(|h| *h > 0.0).unwrap_or(200.0),
        bullets: first("fPierceDamageMultiplier").filter(|m| *m > 0.0).unwrap_or(0.1),
        rockets: first("fExplosionRPGDamageMultiplier").filter(|m| *m > 0.0).unwrap_or(10.0),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Its model loading.
    Placing,
    Flying,
    Hovering,
    Descending,
    Landed,
    Falling,
    Wrecked,
}

/// A helicopter bringing a reinforcement point's mercs.
pub struct Flight {
    /// Its id (the story's).
    pub vehicle: u64,
    data: HeliData,
    /// Its way in (Bevy space), from its start.
    points: Vec<Vec3>,
    speed: f32,
    land: bool,
    /// The mercs, the pilot first, then the gunner.
    pub crew: Vec<u64>,
    /// Where they head once set down.
    field: Option<Vec3>,
    stage: Stage,
    since: f32,
    model: Handle<Fc3Model>,
    wreck: Handle<Fc3Model>,
    entity: Option<Entity>,
    /// Model-space seats (by crew place) and the belly gun's muzzle.
    seats: Vec<Vec3>,
    muzzle: Vec3,
    leg: usize,
    pos: Vec3,
    vel: Vec3,
    yaw: f32,
    /// Its fall's spin (radians a second).
    spin: f32,
    gun: Gun,
    /// Smoke puffs' timer.
    puff: f32,
}

#[derive(Default, Clone, Copy)]
struct Gun {
    /// Seconds left of the burst (below zero, of the pause before the next).
    burst: f32,
    cooldown: f32,
    /// Seconds the player has been in its sights (aim settles in).
    engaged: f32,
}

impl Flight {
    pub fn new(server: &AssetServer, vehicle: u64, data: HeliData, points: Vec<Vec3>, speed: f32, land: bool, crew: Vec<u64>, field: Option<Vec3>) -> Self {
        let ahead = points.get(1).map_or(Vec3::NEG_Z, |p| (*p - points[0]).with_y(0.0));
        let yaw = if ahead.length() > 0.1 { (-ahead.x).atan2(-ahead.z) } else { 0.0 };
        Self {
            vehicle,
            model: crate::weapons::model_handle(server, &data.model),
            wreck: crate::weapons::model_handle(server, WRECK_MODEL),
            data,
            pos: points[0],
            points,
            speed,
            land,
            crew,
            field,
            stage: Stage::Placing,
            since: 0.0,
            entity: None,
            seats: Vec::new(),
            muzzle: Vec3::new(0.0, 0.35, -2.5),
            leg: 1,
            vel: Vec3::ZERO,
            yaw,
            spin: 0.0,
            gun: Gun::default(),
            puff: 0.0,
        }
    }

    fn enter(&mut self, stage: Stage) {
        debug!("story heli {}: {:?} -> {stage:?} after {:.1} s at {:.0}", self.vehicle, self.stage, self.since, self.pos);
        self.stage = stage;
        self.since = 0.0;
    }

    fn airborne(&self) -> bool {
        matches!(self.stage, Stage::Flying | Stage::Hovering | Stage::Descending)
    }
}

/// Helicopters on their way in, overhead, or down.
#[derive(Resource, Default)]
pub struct Flights(pub Vec<Flight>);

/// A story helicopter in the world (its story id), and who last hurt it.
#[derive(Component)]
pub struct Heli {
    pub vehicle: u64,
    by: Option<Entity>,
    /// How fast its rotors turn (0 stopped .. 1 flying).
    power: f32,
}

/// A rotor, turning about its axis (model space).
#[derive(Component)]
struct Rotor {
    axis: Vec3,
    /// Radians a second at full power.
    rate: f32,
    angle: f32,
}

/// A crewman riding in a helicopter, held at his seat (model space).
#[derive(Component)]
struct Aboard {
    heli: Entity,
    seat: Vec3,
}

/// Main and tail rotor speeds (radians a second): the main about four
/// turns a second, the tail five times as fast.
const MAIN_ROTOR: f32 = 26.0;
const TAIL_ROTOR: f32 = 130.0;
/// Within this of a point of its way (m) it makes for the next.
const NEXT_POINT: f32 = 6.0;
/// How quickly it changes speed (m/s²) and turns (radians a second).
const ACCEL: f32 = 5.0;
const TURN: f32 = 0.9;
/// Its least height over the ground on the way (m).
const CLEARANCE: f32 = 8.0;
/// How fast it sets down (m/s).
const DESCENT: f32 = 3.0;
/// The belly gun: how far it fires (m), its bursts (s on, s off), a
/// round's damage.
const GUN_REACH: f32 = 160.0;
const BURST: (f32, f32) = (1.2, 2.0);
const GUN_DAMAGE: f32 = 10.0;
/// A falling one: gravity's pull and its spin's build-up.
const FALL_GRAVITY: f32 = 9.8;
const FALL_SPIN: f32 = 2.5;
/// Its crash's blast.
const CRASH: (f32, f32) = (9.0, 220.0);
const EXPLOSION_SFX: u32 = 0x0002_079E;
/// A wreck burns this long (s), then goes.
const WRECK_LIFE: f32 = 90.0;

/// A node's position in the model (its bind pose, Bevy space).
fn node_at(model: &Fc3Model, name: &str) -> Option<Vec3> {
    let mut index = model.bones.iter().position(|b| b.name.eq_ignore_ascii_case(name))?;
    let mut pose = Transform::IDENTITY;
    for _ in 0..model.bones.len() {
        let b = model.bones.get(index)?;
        pose = Transform { translation: Vec3::from_array(b.translation), rotation: Quat::from_array(b.rotation), scale: Vec3::from_array(b.scale) } * pose;
        if b.parent < 0 {
            return Some(pose.translation);
        }
        index = b.parent as usize;
    }
    None
}

/// Its body for rounds and rockets: the cabin and the tail boom (model
/// space: up Y, the nose at -Z).
fn hull() -> Collider {
    Collider::compound(vec![
        (Vec3::new(0.0, 1.7, -0.6), Quat::IDENTITY, Collider::cuboid(3.0, 2.8, 9.6)),
        (Vec3::new(0.0, 2.7, 8.4), Quat::IDENTITY, Collider::cuboid(1.0, 1.4, 8.0)),
    ])
}

/// Its parts under `root`: the rotors' on their own turning nodes.
fn spawn_parts(commands: &mut Commands, root: Entity, model: &Fc3Model) {
    let mut pivots: Vec<(String, Entity)> = Vec::new();
    for part in model.lods.first().map(|l| l.parts.as_slice()).unwrap_or(&[]) {
        let rotor = part.pivot.as_ref().filter(|p| p.name.eq_ignore_ascii_case("ROTOR") || p.name.eq_ignore_ascii_case("TAILROTOR"));
        let (parent, offset) = match rotor {
            Some(p) => {
                let at = Vec3::from_array(p.at);
                let e = match pivots.iter().find(|(n, _)| *n == p.name) {
                    Some((_, e)) => *e,
                    None => {
                        let tail = p.name.eq_ignore_ascii_case("TAILROTOR");
                        let rotor =
                            if tail { Rotor { axis: Vec3::X, rate: TAIL_ROTOR, angle: 0.0 } } else { Rotor { axis: Vec3::Y, rate: MAIN_ROTOR, angle: 0.0 } };
                        let e = commands.spawn((Transform::from_translation(at), Visibility::Inherited, rotor, ChildOf(root))).id();
                        pivots.push((p.name.clone(), e));
                        e
                    }
                };
                (e, -at)
            }
            None => (root, Vec3::ZERO),
        };
        commands.spawn((
            Mesh3d(part.mesh.clone()),
            MeshMaterial3d(part.material.clone()),
            crate::original_shaders::PropPart,
            Transform::from_translation(offset),
            ChildOf(parent),
        ));
    }
    debug!("helicopter: {} turning rotor(s)", pivots.len());
}

/// Who last hurt each helicopter (whose kill it is when it comes down).
fn note_hits(mut hurts: MessageReader<DamageEvent>, mut helis: Query<&mut Heli>) {
    for d in hurts.read() {
        if let (Ok(mut h), Some(by)) = (helis.get_mut(d.target), d.instigator) {
            h.by = Some(by);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fly(
    time: Res<Time>,
    mut flights: ResMut<Flights>,
    host: Option<NonSendMut<crate::story::StoryHost>>,
    (models, heights, spatial): (Res<Assets<Fc3Model>>, Res<crate::terrain::Heightfield>, SpatialQuery),
    mut helis: Query<(&mut Transform, &Health, &mut Heli), (Without<StoryActor>, Without<Player>)>,
    mut actors: Query<(Entity, &mut StoryActor, &Transform, Option<&mut crate::npc::Npc>, Has<Aboard>), (Without<Heli>, Without<Player>)>,
    players: Query<(Entity, &Transform), (With<Player>, Without<Heli>, Without<StoryActor>)>,
    mut arrivals: ResMut<crate::story_actors::Arrivals>,
    (mut damage, mut explosions, mut sfx, mut loops, mut noises): (
        MessageWriter<DamageEvent>,
        MessageWriter<crate::weapons::Explosion>,
        MessageWriter<crate::sound::PlaySfx>,
        MessageWriter<crate::sound::LoopSfx>,
        MessageWriter<crate::npc::Noise>,
    ),
    (fx, mut rng, mut shot_fx): (Option<Res<crate::weapons::EffectAssets>>, ResMut<crate::weapons::Rng>, crate::particles::ShotFx),
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    if flights.0.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    let Ok((pe, pt)) = players.single() else { return };
    let player = pt.translation;
    let ground = |at: Vec3, skip: Option<Entity>| {
        let terrain = heights.height_at(at).unwrap_or(at.y - 50.0);
        let water = crate::water::surface_at(at, &heights);
        let filter = SpatialQueryFilter::from_excluded_entities(skip);
        let hit = spatial.cast_ray(at + Vec3::Y * 2.0, Dir3::NEG_Y, 120.0, true, &filter).map(|h| at.y + 2.0 - h.distance);
        hit.unwrap_or(terrain).max(terrain).max(water)
    };
    flights.0.retain_mut(|f| {
        f.since += dt;
        // Gone (cleared away: a trial's end, a checkpoint), it's done.
        if f.entity.is_some_and(|e| helis.get(e).is_err()) {
            debug!("story heli {}: gone", f.vehicle);
            return false;
        }
        // The crew not in the world yet come with it.
        for (e, _) in arrivals.0.iter_mut().filter(|(e, _)| f.crew.contains(&e.id)) {
            e.pos = crate::data::to_dunia(f.pos);
        }
        if f.stage == Stage::Placing {
            let (Some(model), Some(_)) = (models.get(&f.model), models.get(&f.wreck)) else {
                if f.since > 30.0 {
                    warn!("story heli {}: its model {} didn't load", f.vehicle, f.data.model);
                    return false;
                }
                return true;
            };
            let names = ["DRIVER_SITPOINT", "PASSENGER_SITPOINT", "SITPOINT_R1", "SITPOINT_L1", "SITPOINT_R2", "SITPOINT_L2"];
            f.seats = names.iter().map(|n| node_at(model, n).unwrap_or(Vec3::new(0.0, 1.2, 0.0))).collect();
            if let Some(m) = node_at(model, "TURRET_BELLY") {
                f.muzzle = m;
            }
            let at = Transform::from_translation(f.pos).with_rotation(Quat::from_rotation_y(f.yaw));
            let root = commands
                .spawn((
                    at,
                    Visibility::Visible,
                    RigidBody::Kinematic,
                    hull(),
                    crate::vehicles::vehicle_layers(),
                    Health::new(f.data.health),
                    crate::combat::DamageScale { bullet: f.data.bullets, explosion: f.data.rockets },
                    Heli { vehicle: f.vehicle, by: None, power: 1.0 },
                    Name::new(format!("helicopter {}", f.vehicle)),
                ))
                .id();
            spawn_parts(&mut commands, root, model);
            f.entity = Some(root);
            info!("story heli {}: made at {:.0}, {} aboard, {} m of way at {:.0} m/s", f.vehicle, f.pos, f.crew.len(), path_length(&f.points), f.speed);
            f.enter(Stage::Flying);
            return true;
        }
        let Some(root) = f.entity else { return false };
        let Ok((mut tf, health, mut heli)) = helis.get_mut(root) else { return false };
        // The crew in the world board (hidden in their seats); the dead stay
        // put where they fell.
        if f.airborne() {
            for (i, id) in f.crew.iter().enumerate() {
                let Some((e, mut a, _, npc, aboard)) = actors.iter_mut().find(|a| a.1.id == *id) else { continue };
                if aboard || a.dead {
                    continue;
                }
                a.seated = true;
                if let Some(mut npc) = npc {
                    npc.velocity = Vec3::ZERO;
                }
                let seat = f.seats.get(i.min(f.seats.len().saturating_sub(1))).copied().unwrap_or(Vec3::Y);
                commands.entity(e).insert((Aboard { heli: root, seat }, ColliderDisabled, Visibility::Hidden, crate::npc::Scripted));
            }
        }
        // FC3_HELI_DOWN=<s> (tests): one hovering that long is brought
        // down by the player.
        let test_down = std::env::var("FC3_HELI_DOWN").ok().and_then(|v| v.parse::<f32>().ok());
        if test_down.is_some_and(|t| f.stage == Stage::Hovering && f.since > t && !health.dead && heli.by.is_none()) {
            info!("story heli {}: FC3_HELI_DOWN: the player brings it down", f.vehicle);
            heli.by = Some(pe);
            damage.write(DamageEvent {
                target: root,
                amount: f.data.health * 2.0,
                kind: DamageKind::Explosion,
                source: Some(player),
                instigator: Some(pe),
                headshot: false,
                hit: crate::combat::Hit::Whole,
            });
        }
        // Brought down: (the pilot killed brings it down too).
        let pilot_dead = f.crew.first().is_some_and(|id| host.story.is_dead(*id));
        if f.airborne() && (health.dead || pilot_dead) {
            let by = heli.by;
            let whom = if by == Some(pe) { "the player".to_string() } else { format!("{by:?}") };
            info!("story heli {}: brought down at {:.0} by {whom} ({:.0} health left)", f.vehicle, f.pos, health.current);
            for id in &f.crew {
                match actors.iter().find(|a| a.1.id == *id) {
                    Some((e, a, ..)) if !a.dead => {
                        damage.write(DamageEvent {
                            target: e,
                            amount: 10_000.0,
                            kind: DamageKind::Explosion,
                            source: Some(f.pos),
                            instigator: by,
                            headshot: false,
                            hit: crate::combat::Hit::Whole,
                        });
                    }
                    Some(_) => {}
                    None if !host.story.is_dead(*id) => {
                        host.story.killed(*id);
                        arrivals.0.retain(|(e, _)| e.id != *id);
                    }
                    None => {}
                }
            }
            f.spin = 0.3;
            f.enter(Stage::Falling);
        }
        match f.stage {
            Stage::Flying => {
                let to = f.points[f.leg.min(f.points.len() - 1)];
                let last = f.leg + 1 >= f.points.len();
                let off = to - f.pos;
                if !last && off.length() < NEXT_POINT {
                    f.leg += 1;
                }
                // Slowing for the end of its way.
                let stop = f.speed * f.speed / (2.0 * ACCEL);
                let want = if last { (off.length() / stop.max(1.0)).min(1.0) * f.speed } else { f.speed };
                let desired = off.normalize_or_zero() * want;
                f.vel += (desired - f.vel).clamp_length_max(ACCEL * dt);
                if last && off.length() < 1.5 && f.vel.length() < 1.0 {
                    f.enter(if f.land { Stage::Descending } else { Stage::Hovering });
                }
                // Facing where it goes.
                let flat = f.vel.with_y(0.0);
                if flat.length() > 2.0 {
                    let want_yaw = (-flat.x).atan2(-flat.z);
                    f.yaw += crate::turrets::wrap(want_yaw - f.yaw).clamp(-TURN * dt, TURN * dt);
                }
            }
            Stage::Hovering => {
                f.vel *= (1.0 - 2.0 * dt).max(0.0);
                let to = player - f.pos;
                let want_yaw = (-to.x).atan2(-to.z);
                f.yaw += crate::turrets::wrap(want_yaw - f.yaw).clamp(-TURN * 0.6 * dt, TURN * 0.6 * dt);
            }
            Stage::Descending => {
                let floor = ground(f.pos, Some(root));
                f.vel = Vec3::new(0.0, -DESCENT.min((f.pos.y - floor).max(0.3)), 0.0);
                if f.pos.y - floor < 0.15 {
                    f.vel = Vec3::ZERO;
                    f.pos.y = floor;
                    f.enter(Stage::Landed);
                    let field = f.field.unwrap_or(player);
                    let doors = [Vec3::new(2.4, 0.0, 0.4), Vec3::new(-2.4, 0.0, 0.4), Vec3::new(2.4, 0.0, -1.0), Vec3::new(-2.4, 0.0, -1.0)];
                    for (i, id) in f.crew.iter().enumerate() {
                        let Some((e, mut a, _, npc, _)) = actors.iter_mut().find(|a| a.1.id == *id) else { continue };
                        let out = Transform::from_translation(f.pos).with_rotation(Quat::from_rotation_y(f.yaw)).transform_point(doors[i % doors.len()]);
                        let alive = !a.dead;
                        unboard(&mut commands, e, &mut a, alive);
                        commands.entity(e).insert(Transform::from_translation(out + Vec3::Y * HALF_HEIGHT));
                        if let Some(mut npc) = npc.filter(|_| !a.dead) {
                            npc.state = crate::npc::NpcState::Search;
                            npc.last_known = Some(field);
                            npc.post = out;
                        }
                    }
                    info!("story heli {}: set down at {:.0}; the crew get out", f.vehicle, f.pos);
                }
            }
            Stage::Falling => {
                f.vel.y -= FALL_GRAVITY * dt;
                f.vel.x *= (1.0 - 0.4 * dt).max(0.0);
                f.vel.z *= (1.0 - 0.4 * dt).max(0.0);
                f.spin = (f.spin + FALL_SPIN * dt).min(4.0);
                f.yaw += f.spin * dt;
                heli.power = (heli.power - 0.3 * dt).max(0.2);
                let floor = ground(f.pos, Some(root));
                if f.pos.y + f.vel.y * dt <= floor + 0.2 || f.since > 12.0 {
                    f.pos.y = floor.max(f.pos.y.min(floor + 0.2));
                    crash(f, root, &mut commands, &models, &mut explosions, &mut sfx);
                    heli.power = 0.0;
                    // The crew's bodies by the wreck.
                    for (i, id) in f.crew.iter().enumerate() {
                        let Some((e, mut a, ..)) = actors.iter_mut().find(|a| a.1.id == *id) else { continue };
                        let a_side = Vec3::new(if i % 2 == 0 { 3.0 } else { -3.0 }, 0.0, i as f32 * 1.5 - 1.5);
                        let at = f.pos + Quat::from_rotation_y(f.yaw) * a_side;
                        let at = at.with_y(ground(at, Some(root)));
                        unboard(&mut commands, e, &mut a, false);
                        commands.entity(e).insert(Transform::from_translation(at + Vec3::Y * HALF_HEIGHT));
                    }
                    host.story.destroyed(f.vehicle);
                    f.enter(Stage::Wrecked);
                }
            }
            Stage::Landed | Stage::Placing => {}
            Stage::Wrecked => {
                if f.since > WRECK_LIFE && f.pos.distance(player) > 60.0 {
                    commands.entity(root).despawn();
                    return false;
                }
            }
        }
        if !matches!(f.stage, Stage::Landed | Stage::Wrecked) {
            // Not into the ground on its way.
            if f.stage == Stage::Flying {
                let floor = ground(f.pos, Some(root)) + CLEARANCE;
                if f.pos.y < floor {
                    f.vel.y = f.vel.y.max((floor - f.pos.y) * 2.0);
                }
            }
            f.pos += f.vel * dt;
        }
        // A gentle bob while it hangs there; banking into its way.
        let bob = if f.stage == Stage::Hovering { (f.since * 1.3).sin() * 0.25 } else { 0.0 };
        let local = Quat::from_rotation_y(f.yaw).inverse() * f.vel;
        let pitch = if f.stage == Stage::Falling { 0.15 } else { (local.z / 25.0).clamp(-0.25, 0.25) };
        let roll = if f.stage == Stage::Falling { 0.2 * (f.since * 3.0).sin() } else { (-local.x / 30.0).clamp(-0.2, 0.2) };
        tf.translation = f.pos + Vec3::Y * bob;
        tf.rotation = Quat::from_rotation_y(f.yaw) * Quat::from_rotation_x(pitch) * Quat::from_rotation_z(roll);
        // Smoke once it's hurt; fire as it falls.
        let share = health.current / health.max.max(1.0);
        if (share < 0.6 || f.stage == Stage::Falling || f.stage == Stage::Wrecked) && f.stage != Stage::Placing {
            f.puff -= dt;
            if f.puff <= 0.0 {
                f.puff = if f.stage == Stage::Falling { 0.05 } else { 0.2 };
                let engine = tf.transform_point(Vec3::new(0.0, 3.2, 1.5));
                let mut s = crate::particles::Particle::new(crate::particles::Sheet::Smoke, engine, 3.0, 1.5);
                s.grow = 3.0;
                s.vel = Vec3::Y * 1.5 - f.vel * 0.3;
                s.frame = crate::particles::Frame::Loop(15.0, (rng.f32() * 30.0) as u32);
                s.colour = Vec4::new(0.12, 0.11, 0.1, 0.5);
                shot_fx.particles.spawn(s);
                if f.stage == Stage::Falling || (f.stage == Stage::Wrecked && f.since < 30.0) {
                    let mut fire = crate::particles::Particle::new(crate::particles::Sheet::Fire, engine, rng.range(0.4, 0.8), rng.range(1.0, 1.8));
                    fire.vel = Vec3::Y * 1.5;
                    shot_fx.particles.spawn(fire);
                }
            }
        }
        // The belly gun on the player.
        let gunner = f.crew.get(1).and_then(|id| actors.iter().find(|a| a.1.id == *id)).filter(|a| !a.1.dead);
        if let (Some(fx), Some((ge, ..)), true) = (fx.as_ref(), gunner, matches!(f.stage, Stage::Flying | Stage::Hovering)) {
            let muzzle = tf.transform_point(f.muzzle);
            let target = player + Vec3::Y * 0.3;
            let sight = target - muzzle;
            let sees = sight.length() < GUN_REACH
                && Dir3::new(sight).ok().is_some_and(|d| {
                    spatial
                        .cast_ray(muzzle, d, sight.length(), true, &SpatialQueryFilter::from_excluded_entities([root, ge]))
                        .is_none_or(|h| h.entity == pe || h.distance > sight.length() - 0.6)
                });
            let g = &mut f.gun;
            g.burst -= dt;
            if g.burst < -BURST.1 {
                g.burst = BURST.0;
            }
            g.cooldown -= dt;
            g.engaged = if sees { g.engaged + dt } else { 0.0 };
            if sees && g.burst > 0.0 && g.cooldown <= 0.0 {
                g.cooldown = 0.1;
                let dist = sight.length();
                let settle = (g.engaged / 2.5).clamp(0.25, 1.0);
                let hit_chance = (0.2 * settle * (1.0 - dist / GUN_REACH)).clamp(0.02, 0.2);
                let aim = if rng.f32() < hit_chance {
                    target + Vec3::Y * rng.range(-0.3, 0.4)
                } else {
                    target + Vec3::new(rng.range(-1.5, 1.5), rng.range(-0.8, 1.2), rng.range(-1.5, 1.5)) * (1.0 + dist / 50.0)
                };
                let dir = (aim - muzzle).normalize_or(Vec3::NEG_Y);
                if let Ok(d3) = Dir3::new(dir) {
                    let hit = spatial.cast_ray(muzzle, d3, 400.0, true, &SpatialQueryFilter::from_excluded_entities([root, ge]));
                    let end = hit.map(|h| muzzle + dir * h.distance).unwrap_or(muzzle + dir * 400.0);
                    match hit {
                        Some(h) if h.entity == pe => {
                            damage.write(DamageEvent {
                                target: pe,
                                amount: GUN_DAMAGE,
                                kind: DamageKind::Bullet,
                                source: Some(muzzle),
                                instigator: Some(ge),
                                headshot: false,
                                hit: crate::combat::Hit::Whole,
                            });
                        }
                        _ if end.distance(player) < 50.0 => shot_fx.bullet_end(&mut rng, muzzle, dir, 400.0, hit.map(|h| (h.entity, h.distance, h.normal))),
                        _ => {}
                    }
                    let len = muzzle.distance(end);
                    if len > 1.0 && rng.f32() < 0.4 {
                        commands.spawn((
                            Mesh3d(fx.tracer_mesh.clone()),
                            MeshMaterial3d(fx.tracer_mat.clone()),
                            Transform::from_translation((muzzle + end) * 0.5).looking_at(end, Vec3::Y).with_scale(Vec3::new(1.0, 1.0, len)),
                            crate::weapons::Effect { age: 0.0, life: 0.05, grow: 0.0, light: false },
                            bevy::light::NotShadowCaster,
                        ));
                    }
                    crate::particles::fx::muzzle_flash(&mut shot_fx.particles, &mut rng, muzzle, dir, 1.5);
                    noises.write(crate::npc::Noise { at: muzzle, radius: 120.0, kind: crate::npc::NoiseKind::Threatening, aside: false });
                    loops.write(crate::sound::LoopSfx { key: root.to_bits(), event: crate::sound::ev::LMG_LOOP, at: Some(muzzle), volume: 1.0, ..default() });
                }
            }
        }
        true
    });
}

/// `FC3_HELI_LOOK` (tests): the view turned onto the nearest helicopter;
/// `FC3_HELI_SHOTS=<dir>`: the first captured hovering, falling and
/// wrecked (a second into each).
fn look_test(
    flights: Res<Flights>,
    helis: Query<&GlobalTransform, With<Heli>>,
    mut players: Query<(&Transform, &mut Player)>,
    mut shot: Local<[bool; 3]>,
    mut commands: Commands,
) {
    if std::env::var_os("FC3_HELI_LOOK").is_some() {
        let Ok((t, mut p)) = players.single_mut() else { return };
        let eye = t.translation + Vec3::Y * 0.7;
        let nearest = flights
            .0
            .iter()
            .filter(|f| f.stage != Stage::Wrecked)
            .filter_map(|f| f.entity.and_then(|e| helis.get(e).ok()))
            .map(|g| g.translation() + Vec3::Y * 2.0)
            .min_by(|a, b| a.distance(eye).total_cmp(&b.distance(eye)));
        if let Some(at) = nearest {
            let to = at - eye;
            p.yaw = (-to.x).atan2(-to.z);
            p.pitch = to.y.atan2(Vec2::new(to.x, to.z).length());
        }
    }
    let Ok(dir) = std::env::var("FC3_HELI_SHOTS") else { return };
    for (i, (stage, name)) in [(Stage::Hovering, "hover"), (Stage::Falling, "fall"), (Stage::Wrecked, "wreck")].iter().enumerate() {
        if !shot[i] && flights.0.iter().any(|f| f.stage == *stage && f.since > 1.0) {
            shot[i] = true;
            crate::camera::capture(&mut commands, format!("{dir}/heli_{name}.png"));
        }
    }
}

/// The length of a way (m).
fn path_length(points: &[Vec3]) -> f32 {
    points.windows(2).map(|w| w[0].distance(w[1])).sum()
}

/// A crewman out of his seat: shown, solid, and (alive) his own man again.
fn unboard(commands: &mut Commands, e: Entity, a: &mut StoryActor, alive: bool) {
    a.seated = false;
    commands.entity(e).remove::<(Aboard, ColliderDisabled)>().insert(Visibility::Inherited);
    if alive {
        commands.entity(e).remove::<crate::npc::Scripted>();
    }
}

/// It hits the ground: the blast, and the wreck in its place.
fn crash(
    f: &Flight,
    root: Entity,
    commands: &mut Commands,
    models: &Assets<Fc3Model>,
    explosions: &mut MessageWriter<crate::weapons::Explosion>,
    sfx: &mut MessageWriter<crate::sound::PlaySfx>,
) {
    let centre = f.pos + Vec3::Y * 1.5;
    info!("story heli {}: crashed at {:.0}", f.vehicle, f.pos);
    explosions.write(crate::weapons::Explosion { at: centre, radius: CRASH.0, damage: CRASH.1, instigator: None });
    sfx.write(crate::sound::PlaySfx { event: EXPLOSION_SFX, at: Some(centre), volume: 1.0 });
    commands.entity(root).despawn_related::<Children>();
    if let Some(wreck) = models.get(&f.wreck) {
        spawn_parts(commands, root, wreck);
    }
}

/// Rotors turn with the power.
fn spin_rotors(time: Res<Time>, helis: Query<&Heli>, mut rotors: Query<(&mut Rotor, &mut Transform, &ChildOf)>) {
    let dt = time.delta_secs();
    for (mut r, mut t, parent) in &mut rotors {
        let power = helis.get(parent.parent()).map_or(0.0, |h| h.power);
        r.angle = (r.angle + r.rate * power * dt).rem_euclid(std::f32::consts::TAU);
        t.rotation = Quat::from_axis_angle(r.axis, r.angle);
    }
}

/// The crew held in their seats.
fn carry_crew(helis: Query<&Transform, With<Heli>>, mut crew: Query<(&Aboard, &mut Transform), Without<Heli>>) {
    for (a, mut t) in &mut crew {
        if let Ok(h) = helis.get(a.heli) {
            t.translation = h.transform_point(a.seat) + Vec3::Y * HALF_HEIGHT;
            t.rotation = h.rotation;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::fcb::Object;
    use fc3_formats::hash::name_hash;
    use std::sync::Arc;

    fn object(name: &str, fields: Vec<(&str, Vec<u8>)>, children: Vec<Object>) -> Object {
        Object {
            name_hash: name_hash(name),
            fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(),
            children: children.into_iter().map(Arc::new).collect(),
        }
    }

    fn archetype(components: Vec<Object>) -> fc3_story::data::Entry {
        fc3_story::data::Entry {
            id: 1,
            name: String::new(),
            class: String::new(),
            pos: [0.0; 3],
            angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: fc3_story::data::Origin::Global,
            components: Arc::new(object("Components", vec![], components)),
            fields: Arc::new(Object::default()),
        }
    }

    fn graphic(model: &str) -> Object {
        let mut path = model.as_bytes().to_vec();
        path.push(0);
        // (The mesh's file, by its field's hash.)
        let mut mesh = object("Mesh", vec![], vec![]);
        mesh.fields.push((0x3182_766C, path));
        object("CGraphicComponent", vec![], vec![mesh])
    }

    /// A helicopter's health and what rounds and rockets do to it come from
    /// its physics' part; a car's archetype is no helicopter.
    #[test]
    fn a_helicopter_takes_its_health_and_damage_from_its_data() {
        let f = |v: f32| v.to_le_bytes().to_vec();
        let part = object(
            "Part",
            vec![("fMaxHealth", f(200.0))],
            vec![object("Damage", vec![("fPierceDamageMultiplier", f(0.1)), ("fExplosionRPGDamageMultiplier", f(10.0))], vec![])],
        );
        let model = r"graphics\__fc3_graphics\_common\vehicles\air\blackhawk\vehicle_helicopter.xbg";
        let heli = archetype(vec![graphic(model), object("CVehicleHelicopterPhysComponent", vec![], vec![object("Parts", vec![], vec![part])])]);
        let data = heli_data(&heli);
        assert!(data.is_some(), "{data:?}");
        let data = data.unwrap();
        assert_eq!((data.health, data.bullets, data.rockets), (200.0, 0.1, 10.0));
        assert_eq!(data.model, model);
        let car = archetype(vec![graphic(model), object("CVehicleWheeledPhysComponent", vec![], vec![])]);
        assert_eq!(heli_data(&car), None);
    }

    /// A way's length; a flight starts at its first point, facing its first
    /// leg, its rotors' tail faster than its main.
    #[test]
    fn a_flight_starts_at_its_way_facing_its_first_leg() {
        assert_eq!(path_length(&[Vec3::ZERO, Vec3::new(3.0, 4.0, 0.0), Vec3::new(3.0, 4.0, 10.0)]), 15.0);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default())).init_asset::<Fc3Model>();
        let server = app.world().resource::<AssetServer>().clone();
        let data = HeliData { model: "x.xbg".into(), health: 200.0, bullets: 0.1, rockets: 10.0 };
        let f = Flight::new(&server, 1, data, vec![Vec3::new(0.0, 50.0, 0.0), Vec3::new(10.0, 50.0, 0.0)], 15.0, false, vec![2, 3], None);
        assert_eq!(f.pos, Vec3::new(0.0, 50.0, 0.0));
        // Facing +X: Bevy's forward is -Z, so a quarter turn clockwise.
        assert!((f.yaw + std::f32::consts::FRAC_PI_2).abs() < 1e-4, "{}", f.yaw);
        assert!(MAIN_ROTOR < TAIL_ROTOR);
    }
}
