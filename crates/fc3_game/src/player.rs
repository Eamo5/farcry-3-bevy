//! First-person player: look, walk/sprint/crouch/jump, swimming, and noclip.
//!
//! Movement is kinematic: velocities are integrated here and resolved against
//! static colliders with avian's `MoveAndSlide` (collide-and-slide with
//! depenetration), plus a small step-up so curbs and stairs don't block.

use avian3d::character_controller::prelude::*;
use avian3d::prelude::*;
use bevy::prelude::*;

use crate::sky::SEA_LEVEL;
use crate::terrain::{Heightfield, StreamingAnchor};
use crate::Args;

pub const CAPSULE_RADIUS: f32 = 0.35;
pub const CAPSULE_LENGTH: f32 = 1.1;
/// Distance from the capsule centre down to the feet.
pub const HALF_HEIGHT: f32 = CAPSULE_RADIUS + CAPSULE_LENGTH * 0.5;
pub const EYE_HEIGHT: f32 = 1.62;
pub const CROUCH_EYE_HEIGHT: f32 = 1.0;
/// The slide: eye height, the sprint speed it needs, the push it gives,
/// how fast it slows (per second) and its longest run.
const SLIDE_EYE_HEIGHT: f32 = 0.75;
const SLIDE_MIN_SPEED: f32 = 5.5;
const SLIDE_BOOST: f32 = 2.0;
const SLIDE_FRICTION: f32 = 1.1;
const SLIDE_TIME: f32 = 1.1;
/// Walking while dragging a body.
const DRAG_SPEED: f32 = 1.5;
const WALK_SPEED: f32 = 4.2;
const SPRINT_SPEED: f32 = 7.5;
const CROUCH_SPEED: f32 = 2.0;
const SWIM_SPEED: f32 = 3.2;
const JUMP_SPEED: f32 = 5.0;
const GRAVITY: f32 = 19.0;
const GROUND_ACCEL: f32 = 60.0;
const AIR_ACCEL: f32 = 6.0;
const STEP_HEIGHT: f32 = 0.45;
/// Steepest walkable slope (cosine of the angle from vertical).
const MIN_GROUND_NORMAL_Y: f32 = 0.64;

pub struct PlayerPlugin;

/// Startup set that spawns the player and camera; systems that decorate the
/// player (health, inventory, view model) run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlayerSpawn;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputCaptured>()
            .add_systems(Startup, spawn_player.in_set(PlayerSpawn))
            .add_systems(Update, (player_look, place_on_ground, climb_course, player_move, scripted_walk_log, footsteps).chain())
            .add_systems(PreUpdate, slide_test.after(crate::controls::update_actions));
    }
}

#[derive(Component)]
pub struct Player {
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
    pub crouching: bool,
    pub swimming: bool,
    pub noclip: bool,
    /// Waiting for the ground under the spawn point to stream in.
    pub spawning: bool,
    /// Downward speed at the moment of the last landing (for fall damage).
    pub last_landing_speed: f32,
    pub sprinting: bool,
    /// Time the player last landed after (re)spawning; enemies hold off briefly after.
    pub spawned_at: f32,
    /// In a vehicle (positioned by the vehicle, not by walking).
    pub driving: bool,
    pub eye: f32,
    /// Flying the wingsuit or under the parachute.
    pub air: AirMode,
    /// Pulling up onto a ledge or vaulting over something.
    pub mantle: Option<Mantle>,
    /// On a mounted gun: the view turns only as far as it does.
    pub mounted: Option<GunArcs>,
    /// Seconds into a slide (crouching out of a sprint), while it lasts.
    pub sliding: Option<f32>,
    /// Dragging a body (slow, no sprinting or jumping).
    pub dragging: bool,
}

/// How far a mounted gun turns: either side of its facing, and down and up.
#[derive(Clone, Copy, Debug)]
pub struct GunArcs {
    pub turret: Entity,
    pub base_yaw: f32,
    pub arc: f32,
    pub pitch: (f32, f32),
}

impl GunArcs {
    /// A view direction brought within the gun's arcs.
    pub fn clamp(&self, yaw: f32, pitch: f32) -> (f32, f32) {
        let rel = crate::turrets::wrap(yaw - self.base_yaw);
        let rel = if self.arc >= std::f32::consts::PI - 1e-3 { rel } else { rel.clamp(-self.arc, self.arc) };
        (self.base_yaw + rel, pitch.clamp(self.pitch.0, self.pitch.1))
    }
}

/// A climb or vault: from, over the edge, to; seconds in and in all.
#[derive(Clone, Copy, Debug)]
pub struct Mantle {
    from: Vec3,
    peak: Vec3,
    to: Vec3,
    t: f32,
    dur: f32,
}

/// Something to climb onto in front of the player (capsule centre `pos`):
/// the top's point, its height above the feet, and whether to vault over.
fn find_ledge(sq: &SpatialQuery, filter: &SpatialQueryFilter, pos: Vec3, fwd: Vec3) -> Option<(Vec3, f32, bool)> {
    let feet = pos - Vec3::Y * HALF_HEIGHT;
    let dir = Dir3::new(fwd).ok()?;
    // A steep face in front (not just rising ground), somewhere between the
    // knees and the head.
    let face = [0.45, 0.95, 1.5]
        .iter()
        .filter_map(|h| sq.cast_ray(feet + Vec3::Y * *h, dir, 0.9, true, filter))
        .filter(|h| h.normal.y.abs() < 0.35)
        .map(|h| h.distance)
        .fold(f32::MAX, f32::min);
    if face == f32::MAX {
        return None;
    }
    // Its top, just beyond the face (thin fences have tops too).
    let probe = feet + fwd * (face + 0.15) + Vec3::Y * 2.6;
    let Some(down) = sq.cast_ray(probe, Dir3::NEG_Y, 2.6, true, filter) else {
        debug!("ledge: face at {face:.2} but nothing below {probe:.1?}");
        return None;
    };
    let top = probe - Vec3::Y * down.distance;
    let h = top.y - feet.y;
    if down.normal.y < 0.7 || !(0.5..=2.3).contains(&h) {
        debug!("ledge: face at {face:.2}, top {h:.2} m up, normal {:.2}", down.normal.y);
        return None;
    }
    // Room to stand up there.
    if sq.cast_ray(top + Vec3::Y * 0.05, Dir3::Y, 1.8, true, filter).is_some() {
        debug!("ledge: no headroom over {h:.2} m");
        return None;
    }
    // Low and with a drop beyond (a fence, a crate): over it rather than onto it.
    let clear_ahead = sq.cast_ray(top + Vec3::Y * 0.4, dir, 1.2, true, filter).is_none();
    let drop = sq.cast_ray(top + fwd * 0.6 + Vec3::Y * 0.2, Dir3::NEG_Y, 3.0, true, filter).is_none_or(|d| d.distance > 0.45);
    Some((top, h, h < 1.3 && clear_ahead && drop))
}

/// Falling with gear (see `freefall`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AirMode {
    #[default]
    None,
    Wingsuit,
    Chute,
}

#[derive(Component)]
pub struct PlayerCamera;

/// Whether gameplay input (mouse look, firing) is active.
#[derive(Resource, Default)]
pub struct InputCaptured(pub bool);

fn spawn_player(mut commands: Commands, args: Res<Args>) {
    let player = commands
        .spawn((
            Player {
                velocity: Vec3::ZERO,
                yaw: args.yaw,
                pitch: args.pitch,
                grounded: false,
                crouching: false,
                swimming: false,
                noclip: args.noclip,
                spawning: !args.noclip,
                last_landing_speed: 0.0,
                sprinting: false,
                spawned_at: 0.0,
                driving: false,
                eye: EYE_HEIGHT,
                air: AirMode::None,
                mantle: None,
                mounted: None,
                sliding: None,
                dragging: false,
            },
            Collider::capsule(CAPSULE_RADIUS, CAPSULE_LENGTH),
            Transform::from_translation(args.start).with_rotation(Quat::from_rotation_y(args.yaw)),
            Visibility::Visible,
            StreamingAnchor,
            Name::new("player"),
        ))
        .id();
    commands.spawn((
        PlayerCamera,
        Camera3d::default(),
        // Cleared transparent: the water tells sky from scene in its copy of
        // the view by that (see `water.wgsl`).
        Camera { clear_color: ClearColorConfig::Custom(Color::NONE), ..default() },
        Projection::Perspective(PerspectiveProjection { fov: 75f32.to_radians(), near: 0.01, far: 20_000.0, ..default() }),
        Transform::from_xyz(0.0, EYE_HEIGHT - HALF_HEIGHT, 0.0).with_rotation(Quat::from_rotation_x(args.pitch)),
        crate::sky::camera_sky_bundle(),
        bevy::camera::Exposure::SUNLIGHT,
        bevy::core_pipeline::tonemapping::Tonemapping::AgX,
        Msaa::Off,
        ChildOf(player),
        Name::new("player camera"),
    ));
}

fn player_look(
    actions: Res<crate::controls::ActionState>,
    state: Res<State<crate::menu::GameState>>,
    cinematic: Res<crate::fp::Cinematic>,
    sway: Res<crate::weapons::ScopeSway>,
    wheel: Res<crate::weapon_wheel::WeaponWheel>,
    mut player: Query<(&mut Player, &mut Transform), Without<PlayerCamera>>,
    mut camera: Query<&mut Transform, With<PlayerCamera>>,
) {
    let Ok((mut p, mut t)) = player.single_mut() else { return };
    // A takedown holds the view, and the weapon wheel takes the look input.
    if *state.get() == crate::menu::GameState::Playing && cinematic.0 <= 0.0 && !wheel.open {
        p.yaw -= actions.look.x;
        p.pitch = (p.pitch + actions.look.y).clamp(-1.5, 1.5);
    }
    if let Some(arcs) = p.mounted {
        (p.yaw, p.pitch) = arcs.clamp(p.yaw, p.pitch);
    }
    t.rotation = Quat::from_rotation_y(p.yaw);
    if let Ok(mut ct) = camera.single_mut() {
        // A scope drifting with the breath.
        ct.rotation = Quat::from_rotation_y(sway.offset.x) * Quat::from_rotation_x(p.pitch + sway.offset.y);
        ct.translation.y = p.eye - HALF_HEIGHT;
    }
}

/// Footstep sounds paced by ground speed, on whatever is underfoot (wading
/// splashes), at the pace's gait; a thump on landing from a fall and a scrape
/// when a slide starts.
#[allow(clippy::too_many_arguments)]
fn footsteps(
    time: Res<Time>,
    mut timer: Local<f32>,
    mut last: Local<Option<Vec3>>,
    mut was: Local<(bool, bool, f32)>,
    players: Query<(Entity, &Player, &Transform)>,
    spatial: SpatialQuery,
    probe: crate::surfaces::SurfaceProbe,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let Ok((entity, p, t)) = players.single() else { return };
    let feet = t.translation - Vec3::Y * HALF_HEIGHT;
    let surface = || {
        if feet.y < probe.water(feet) - 0.05 {
            return crate::surfaces::value::SHALLOW_WATER;
        }
        let filter = SpatialQueryFilter::from_excluded_entities([entity]);
        match spatial.cast_ray(feet + Vec3::Y * 0.3, Dir3::NEG_Y, 1.2, true, &filter) {
            Some(hit) => probe.at(hit.entity, feet + Vec3::Y * (0.3 - hit.distance), hit.normal),
            None => probe.terrain(feet),
        }
    };
    let (was_grounded, was_sliding, fall) = *was;
    let sliding = p.sliding.is_some();
    *was = (p.grounded, sliding, if p.grounded { 0.0 } else { fall.max(-p.velocity.y) });
    if p.noclip {
        return;
    }
    if p.grounded && !was_grounded && fall > LANDING_HEARD {
        let volume = (fall / 12.0).clamp(0.25, 0.8);
        sfx.write(crate::sound::PlaySfx { event: crate::sound::switched(crate::sound::ev::LANDING, surface(), 0), at: None, volume });
    }
    if sliding && !was_sliding {
        sfx.write(crate::sound::PlaySfx { event: crate::sound::switched(crate::sound::ev::SLIDE, surface(), 0), at: None, volume: 0.5 });
    }
    // Paced by how far the feet actually go, so pushing against a wall is quiet.
    let moved = last.replace(t.translation).map_or(0.0, |l| Vec2::new(t.translation.x - l.x, t.translation.z - l.z).length());
    let speed = moved / time.delta_secs().max(1e-4);
    if !p.grounded || sliding || speed < 0.6 {
        *timer = 0.0;
        return;
    }
    *timer -= time.delta_secs() * speed;
    if *timer <= 0.0 {
        *timer = 1.9;
        let volume = if p.crouching { 0.15 } else if p.sprinting { 0.5 } else { 0.3 };
        let surface = surface();
        let gait = if p.crouching { 2 } else if p.sprinting { 3 } else { 1 };
        if std::env::var("FC3_SURFACE_DEBUG").is_ok() {
            info!("footstep on surface {surface:08x} gait {gait} at {:?}", crate::data::to_dunia(feet));
        }
        sfx.write(crate::sound::PlaySfx { event: crate::sound::switched(crate::sound::ev::FOOTSTEP, surface, gait), at: None, volume });
    }
}

/// Falls faster than this (m/s) land with a thump.
const LANDING_HEARD: f32 = 4.0;

/// Drops the player onto the ground once the terrain under them has loaded.
fn place_on_ground(
    time: Res<Time>,
    mut player: Query<(Entity, &mut Player, &mut Transform)>,
    heights: Res<Heightfield>,
    spatial: SpatialQuery,
    chunks: Query<(&crate::terrain::TerrainChunk, &Children)>,
    colliders: Query<(), With<crate::physics::TerrainCollider>>,
    objects: Query<&crate::objects::WorldObject>,
    server: Res<AssetServer>,
    args: Res<Args>,
) {
    let Ok((entity, mut p, mut t)) = player.single_mut() else { return };
    if !p.spawning {
        return;
    }
    // --underground: the floor just below the requested height (a cave), once
    // something is there to stand on.
    if args.underground && p.spawned_at == 0.0 {
        let filter = SpatialQueryFilter::from_excluded_entities([entity]);
        let Some(h) = spatial.cast_ray(t.translation + Vec3::Y, Dir3::NEG_Y, 6.0, true, &filter) else { return };
        t.translation.y += 1.0 - h.distance + HALF_HEIGHT + 0.05;
        p.velocity = Vec3::ZERO;
        p.spawning = false;
        p.spawned_at = time.elapsed_secs();
        info!("player placed underground at {:?}", t.translation);
        return;
    }
    let Some(ground) = heights.height_at(t.translation) else { return };
    // Wait until this sector's terrain collision exists so we can't fall through
    // (other sectors may already have colliders after a teleport).
    let d = crate::data::to_dunia(t.translation);
    let here = fc3_formats::terrain::SectorCoord::containing(d[0], d[1]);
    if !chunks.iter().any(|(c, kids)| c.coord == here && kids.iter().any(|k| colliders.contains(k))) {
        return;
    }
    // Land on the highest surface below a point well above the terrain (roofs, rocks...).
    let top = Vec3::new(t.translation.x, ground.max(SEA_LEVEL) + 60.0, t.translation.z);
    // Exclude the player's own capsule from the probe.
    let hit = spatial.cast_ray(top, Dir3::NEG_Y, 200.0, true, &SpatialQueryFilter::from_excluded_entities([entity]));
    if let Some(h) = &hit {
        let what = objects
            .get(h.entity)
            .ok()
            .and_then(|o| server.get_path(&o.model))
            .map(|p| p.to_string())
            .unwrap_or_else(|| "terrain".into());
        info!("spawn ray hit {what} at y={:.2} (terrain {ground:.2})", top.y - h.distance);
    }
    let surface = hit.map(|h| top.y - h.distance).unwrap_or(ground).max(ground);
    t.translation.y = surface + HALF_HEIGHT + 0.05;
    p.velocity = Vec3::ZERO;
    p.spawning = false;
    p.spawned_at = time.elapsed_secs();
    info!("player placed on ground at {:?}", t.translation);
}

/// `FC3_CLIMB_TEST=boxes` (with --walk): a crate to climb, a fence to vault
/// and a wall to scale, set out ahead of where the player lands.
fn climb_course(
    mut done: Local<bool>,
    mut players: Query<(&Player, &mut Transform)>,
    heights: Res<Heightfield>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    if *done || std::env::var("FC3_CLIMB_TEST").as_deref() != Ok("boxes") {
        return;
    }
    let Ok((p, mut t)) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    *done = true;
    let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let rotation = Quat::from_rotation_y(p.yaw);
    let start = t.translation - Vec3::Y * HALF_HEIGHT;
    // A level deck above the highest ground along the way, the player on it.
    let top = (0..=24).filter_map(|i| heights.height_at(start + fwd * i as f32)).fold(start.y, f32::max) + 0.5;
    let wood = materials.add(StandardMaterial { base_color: Color::srgb(0.55, 0.45, 0.3), perceptual_roughness: 0.9, ..default() });
    let mut block = |centre: Vec3, size: Vec3| {
        commands.spawn((
            RigidBody::Static,
            Collider::cuboid(size.x, size.y, size.z),
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(wood.clone()),
            Transform::from_translation(centre).with_rotation(rotation),
        ));
    };
    block(start + fwd * 11.0 + Vec3::Y * (top - start.y - 0.5), Vec3::new(5.0, 1.0, 26.0));
    // (distance ahead, height, depth): a crate, a fence, a wall.
    for (ahead, h, depth) in [(3.0, 1.0, 2.0), (9.0, 0.9, 0.3), (14.0, 2.0, 3.0)] {
        let base = start + fwd * (ahead + depth * 0.5);
        block(Vec3::new(base.x, top + h * 0.5, base.z), Vec3::new(4.0, h, depth));
    }
    t.translation = Vec3::new(start.x, top + HALF_HEIGHT + 0.05, start.z) - fwd * 1.5;
    info!("climb course set out ahead on a deck at {top:.1}");
}

/// Elapsed seconds since the player landed, for scripted tests.
#[derive(Default)]
struct WalkClock {
    start: Option<f32>,
    last_log: f32,
}

fn scripted_walk_log(args: Res<Args>, time: Res<Time>, mut clock: Local<WalkClock>, player: Query<(&Player, &Transform)>) {
    if args.walk_secs <= 0.0 {
        return;
    }
    let Ok((p, t)) = player.single() else { return };
    if p.spawning {
        return;
    }
    let now = time.elapsed_secs();
    let start = *clock.start.get_or_insert(now);
    if now - clock.last_log >= 1.0 && now - start <= args.walk_secs + 1.0 {
        clock.last_log = now;
        let ground = crate::data::to_dunia(t.translation);
        info!(
            "walk t={:.1}s pos=({:.1},{:.1},{:.2}) grounded={} swimming={} vel={:.2}",
            now - start,
            ground[0],
            ground[1],
            t.translation.y - HALF_HEIGHT,
            p.grounded,
            p.swimming,
            p.velocity.length()
        );
    }
}

/// Keys held by the scripted walk test (`--walk`).
fn scripted_keys(args: &Args, time: &Time, start: &mut Option<f32>) -> (bool, bool, bool) {
    if args.walk_secs <= 0.0 {
        return (false, false, false);
    }
    let now = time.elapsed_secs();
    let t = now - *start.get_or_insert(now);
    if t > args.walk_secs {
        return (false, false, false);
    }
    // forward always, sprint in the second half, jump once a few seconds in
    (true, t > args.walk_secs * 0.5, (t % 4.0) < 0.05 && t > 1.0)
}

fn player_move(
    time: Res<Time>,
    args: Res<Args>,
    mut script_start: Local<Option<f32>>,
    keys: Res<ButtonInput<KeyCode>>,
    actions: Res<crate::controls::ActionState>,
    state: Res<State<crate::menu::GameState>>,
    perks: Res<crate::progression::Perks>,
    boosts: Res<crate::syringes::Boosts>,
    cinematic: Res<crate::fp::Cinematic>,
    move_and_slide: MoveAndSlide,
    heights: Res<Heightfield>,
    mut player: Query<(Entity, &mut Player, &mut Transform, &Collider)>,
    progress: Res<crate::progression::Progress>,
    mut actions_out: MessageWriter<crate::fp::FpAction>,
    sway: Res<crate::weapons::ScopeSway>,
) {
    let Ok((entity, mut p, mut t, collider)) = player.single_mut() else { return };
    let dt = time.delta_secs().min(0.05);
    if dt <= 0.0 || p.spawning || p.driving || p.mounted.is_some() {
        return;
    }
    if keys.just_pressed(KeyCode::F2) {
        p.noclip = !p.noclip;
        p.velocity = Vec3::ZERO;
    }
    use crate::controls::Action;
    let input_ok = *state.get() == crate::menu::GameState::Playing && cinematic.0 <= 0.0;
    let (s_fwd, s_sprint, s_jump) = scripted_keys(&args, &time, &mut script_start);
    let key = |a: Action| {
        (input_ok && actions.pressed(a)) || (a == Action::Forward && s_fwd) || (a == Action::Sprint && s_sprint) || (a == Action::Jump && s_jump)
    };
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let right = Vec3::new(forward.z * -1.0, 0.0, forward.x);
    // Analogue on a gamepad; scripted runs walk forward.
    let mut axis = if input_ok { actions.movement } else { Vec2::ZERO };
    if s_fwd {
        axis.y = 1.0;
    }
    let wish = (forward * axis.y + right * axis.x).clamp_length_max(1.0);

    if p.noclip {
        let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch) * Vec3::NEG_Z;
        let mut v = (look * wish.dot(forward) + right * wish.dot(right)).normalize_or_zero();
        if key(Action::Jump) || keys.pressed(KeyCode::KeyE) {
            v += Vec3::Y;
        }
        if key(Action::Crouch) || keys.pressed(KeyCode::KeyQ) {
            v -= Vec3::Y;
        }
        let speed = if key(Action::Sprint) { 120.0 } else { 25.0 };
        t.translation += v.normalize_or_zero() * speed * dt;
        return;
    }

    // Mid-climb: up to the edge, then over it.
    if let Some(mut m) = p.mantle {
        m.t += dt;
        let f = (m.t / m.dur).min(1.0);
        let ease = |x: f32| x * x * (3.0 - 2.0 * x);
        t.translation = if f < 0.6 { m.from.lerp(m.peak, ease(f / 0.6)) } else { m.peak.lerp(m.to, ease((f - 0.6) / 0.4)) };
        p.velocity = Vec3::ZERO;
        p.mantle = (f < 1.0).then_some(m);
        p.grounded = f >= 1.0;
        return;
    }
    // Jump at a ledge: climb it (or vault it when low with a drop beyond).
    // FC3_CLIMB_TEST (with --walk): climb whatever is met.
    // (Not for the takedown from below, staged at the course's wall.)
    let climb_test = std::env::var("FC3_CLIMB_TEST").is_ok() && std::env::var("FC3_TAKEDOWN_MODE").as_deref() != Ok("below") && p.grounded;
    if (input_ok && actions.just_pressed(Action::Jump) || climb_test) && !p.swimming && p.air == AirMode::None && (p.grounded || p.velocity.y.abs() < 3.0) {
        let filter = SpatialQueryFilter::from_excluded_entities([entity]);
        if let Some((top, h, vault)) = find_ledge(&move_and_slide.spatial_query, &filter, t.translation, forward) {
            let from = t.translation;
            let peak = Vec3::new(from.x, top.y + HALF_HEIGHT + 0.1, from.z) + forward * 0.15;
            let to = if vault { top + forward * 1.4 + Vec3::Y * HALF_HEIGHT } else { top + forward * 0.45 + Vec3::Y * (HALF_HEIGHT + 0.02) };
            let dur = if vault { 0.55 } else { 0.45 + h * 0.3 };
            p.mantle = Some(Mantle { from, peak, to, t: 0.0, dur });
            info!("climb: {:.2} m up, vault {vault}, from {from:.1?} to {to:.1?}", h);
            actions_out.write(if vault { crate::fp::FpAction::Vault { secs: dur } } else { crate::fp::FpAction::Climb { high: h > 1.3, secs: dur } });
            return;
        }
    }
    let feet = t.translation.y - HALF_HEIGHT;
    let water = crate::water::surface_at(t.translation, &heights);
    p.swimming = feet < water - 1.3;
    // Crouching at a sprint: a slide, low and fast, slowing to a crouch.
    let flat_speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    if p.sliding.is_none() && p.sprinting && p.grounded && flat_speed > SLIDE_MIN_SPEED && input_ok && actions.just_pressed(Action::Crouch) {
        p.sliding = Some(0.0);
        let boost = Vec3::new(p.velocity.x, 0.0, p.velocity.z).normalize_or_zero() * SLIDE_BOOST;
        p.velocity += boost;
    }
    let was_sliding = p.sliding.is_some();
    if let Some(s) = p.sliding {
        let s = s + dt;
        let speed_now = Vec2::new(p.velocity.x, p.velocity.z).length();
        // Over when it has run its course, slowed right down, left the
        // ground, or a jump breaks out of it.
        let over = s > SLIDE_TIME || speed_now < 2.0 || !p.grounded || (input_ok && actions.just_pressed(Action::Jump)) || p.swimming;
        p.sliding = (!over).then_some(s);
    }
    p.crouching = (key(Action::Crouch) || p.sliding.is_some()) && !p.swimming && !p.dragging;
    // Through a scope the sprint key holds the breath instead.
    p.sprinting = key(Action::Sprint) && !p.crouching && !p.swimming && !p.dragging && !sway.scoped && wish.dot(forward) > 0.5;
    let target_eye = if p.sliding.is_some() {
        SLIDE_EYE_HEIGHT
    } else if p.crouching {
        CROUCH_EYE_HEIGHT
    } else {
        EYE_HEIGHT
    };
    p.eye += (target_eye - p.eye) * (dt * 12.0).min(1.0);

    let speed = if p.dragging {
        DRAG_SPEED
    } else if p.swimming {
        SWIM_SPEED * perks.swim * if boosts.active(crate::syringes::Syringe::Diver) { 1.3 } else { 1.0 }
    } else if p.crouching {
        CROUCH_SPEED
    } else if p.sprinting {
        SPRINT_SPEED * perks.sprint * if boosts.active(crate::syringes::Syringe::Explorer) { 1.3 } else { 1.0 }
    } else {
        WALK_SPEED
    };
    // Sliding: momentum carries on (friction slows it, a little steering),
    // instead of accelerating toward the wished velocity.
    if was_sliding && p.sliding.is_some() {
        let horiz = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
        let slowed = horiz * (1.0 - SLIDE_FRICTION * dt).max(0.0) + right * axis.x * 2.0 * dt;
        p.velocity.x = slowed.x;
        p.velocity.z = slowed.z;
    }
    // Horizontal acceleration toward the wished velocity.
    let accel = if p.sliding.is_some() { 0.0 } else if p.grounded || p.swimming { GROUND_ACCEL } else { AIR_ACCEL };
    let target = wish * speed;
    let horiz = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
    let delta = target - horiz;
    let step = delta.clamp_length_max(accel * dt);
    // (The wingsuit and parachute fly by their own rules below.)
    if p.air == AirMode::None {
        p.velocity.x += step.x;
        p.velocity.z += step.z;
    }

    if p.swimming {
        // Float toward the surface, with manual up/down.
        let surface_feet = water - 1.45;
        let mut vy = (surface_feet - feet) * 2.0;
        if key(Action::Jump) {
            vy = 2.5;
        }
        if key(Action::SwimDown) || key(Action::Crouch) {
            vy = -2.5;
        }
        p.velocity.y += (vy - p.velocity.y) * (dt * 4.0).min(1.0);
    } else {
        if p.grounded && key(Action::Jump) && !p.dragging {
            p.velocity.y = JUMP_SPEED;
            p.grounded = false;
        }
        // Jump in the air: the wingsuit, then the parachute.
        if !p.grounded && input_ok && actions.just_pressed(Action::Jump) {
            let clearance = feet - heights.height_at(t.translation).unwrap_or(f32::MIN);
            let (chute, suit) = (progress.upgrade(crate::freefall::PARACHUTE) > 0, progress.upgrade(crate::freefall::WINGSUIT) > 0);
            p.air = match p.air {
                AirMode::None if clearance > crate::freefall::MIN_CLEARANCE && p.velocity.y < -4.0 && suit => AirMode::Wingsuit,
                AirMode::None | AirMode::Wingsuit if clearance > crate::freefall::MIN_CLEARANCE * 0.5 && p.velocity.y < -2.0 && chute => AirMode::Chute,
                m => m,
            };
        }
        match p.air {
            AirMode::None => p.velocity.y -= GRAVITY * dt,
            AirMode::Wingsuit => {
                // Gliding along the view: diving trades height for speed,
                // pulling up bleeds speed for a flatter glide.
                let dive = (-p.pitch).clamp(-0.6, 1.2);
                let along = (26.0 + dive * 18.0).clamp(14.0, 45.0);
                let sink = (6.0 + dive * 14.0).clamp(2.5, 30.0);
                let target = forward * along + right * axis.x * 4.0 - Vec3::Y * sink;
                let v = p.velocity;
                p.velocity = v + (target - v) * (dt * 1.6).min(1.0);
            }
            AirMode::Chute => {
                // A slow, steerable drift.
                let target = forward * (6.5 + axis.y * 2.5) + right * axis.x * 2.5 - Vec3::Y * 4.2;
                let v = p.velocity;
                p.velocity = v + (target - v) * (dt * 2.0).min(1.0);
            }
        }
    }

    let filter = SpatialQueryFilter::from_excluded_entities([entity]);
    let config = MoveAndSlideConfig::default();
    let was_grounded = p.grounded;
    // Flying into the ground in the wingsuit hurts like a fall at that speed.
    let fall_speed = if p.air == AirMode::Wingsuit { p.velocity.length() } else { -p.velocity.y };
    let mut ground_hit = false;
    let start = t.translation;
    let out = move_and_slide.move_and_slide(
        collider,
        start,
        Quat::IDENTITY,
        p.velocity,
        std::time::Duration::from_secs_f32(dt),
        &config,
        &filter,
        |hit| {
            if hit.normal.y > MIN_GROUND_NORMAL_Y {
                ground_hit = true;
            }
            MoveAndSlideHitResponse::Accept
        },
    );
    let mut new_pos = out.position;
    let mut new_vel = out.projected_velocity;

    // Step up: if horizontal motion was blocked, retry from a raised position.
    let wanted = Vec3::new(p.velocity.x, 0.0, p.velocity.z) * dt;
    let achieved = Vec3::new(new_pos.x - start.x, 0.0, new_pos.z - start.z);
    if was_grounded && wanted.length() > 0.001 && achieved.length() < wanted.length() * 0.5 {
        let up = Vec3::Y * STEP_HEIGHT;
        let raised = start + up;
        let clear = move_and_slide.spatial_query.cast_shape(
            collider,
            start,
            Quat::IDENTITY,
            Dir3::Y,
            &ShapeCastConfig::from_max_distance(STEP_HEIGHT),
            &filter,
        );
        if clear.is_none() {
            let step_out = move_and_slide.move_and_slide(
                collider,
                raised,
                Quat::IDENTITY,
                Vec3::new(p.velocity.x, 0.0, p.velocity.z),
                std::time::Duration::from_secs_f32(dt),
                &config,
                &filter,
                |_| MoveAndSlideHitResponse::Accept,
            );
            let moved = Vec3::new(step_out.position.x - start.x, 0.0, step_out.position.z - start.z);
            if moved.length() > achieved.length() + 0.01 {
                // Settle back down onto the step.
                let down = move_and_slide.spatial_query.cast_shape(
                    collider,
                    step_out.position,
                    Quat::IDENTITY,
                    Dir3::NEG_Y,
                    &ShapeCastConfig::from_max_distance(STEP_HEIGHT + 0.05),
                    &filter,
                );
                if let Some(d) = down {
                    new_pos = step_out.position - Vec3::Y * (d.distance - 0.01).max(0.0);
                    new_vel = Vec3::new(step_out.projected_velocity.x, 0.0, step_out.projected_velocity.z);
                    ground_hit = true;
                }
            }
        }
    }

    // Ground probe keeps us glued to slopes when walking downhill.
    let probe = move_and_slide.spatial_query.cast_shape(
        collider,
        new_pos,
        Quat::IDENTITY,
        Dir3::NEG_Y,
        &ShapeCastConfig::from_max_distance(if was_grounded { 0.25 } else { 0.05 }),
        &filter,
    );
    let probe_ground = probe.as_ref().is_some_and(|h| h.normal1.y > MIN_GROUND_NORMAL_Y || -h.normal2.y > MIN_GROUND_NORMAL_Y);
    if was_grounded && !ground_hit && new_vel.y <= 0.0 {
        if let Some(h) = &probe {
            if probe_ground {
                new_pos.y -= (h.distance - 0.01).max(0.0);
                ground_hit = true;
            }
        }
    }
    p.grounded = (ground_hit || probe_ground) && !p.swimming;
    // Landing (or swimming) puts the gear away; a wingsuit hitting a wall at
    // speed is a crash.
    let lost = (p.velocity - new_vel).length();
    if p.air == AirMode::Wingsuit && lost > 12.0 && !p.grounded {
        p.last_landing_speed = lost;
        p.air = AirMode::None;
    }
    if p.grounded || p.swimming {
        p.air = AirMode::None;
    }
    if p.grounded {
        if !was_grounded {
            p.last_landing_speed = fall_speed;
        }
        new_vel.y = new_vel.y.max(-1.0).min(if new_vel.y > 0.5 { new_vel.y } else { 0.0 });
    }

    // Never sink below the terrain surface, even if a collider hasn't streamed in yet.
    // Well below it (in a cave, or under an overhang) the colliders decide.
    if let Some(ground) = heights.height_at(new_pos) {
        let min_center = ground + HALF_HEIGHT - 0.02;
        if new_pos.y < min_center && min_center - new_pos.y < 1.2 {
            new_pos.y = min_center;
            if new_vel.y < 0.0 {
                if !was_grounded {
                    p.last_landing_speed = fall_speed;
                }
                new_vel.y = 0.0;
            }
            p.grounded = !p.swimming;
        }
    }
    t.translation = new_pos;
    p.velocity = new_vel;
}

/// `FC3_SLIDE_TEST`: sprints for two seconds, then crouches into a slide,
/// logging speed and eye height (`FC3_TD_SHOT` screenshots that long after
/// the crouch).
fn slide_test(
    time: Res<Time>,
    args: Res<Args>,
    mut state: Local<(Option<f32>, f32, usize)>,
    mut actions: ResMut<crate::controls::ActionState>,
    players: Query<&Player>,
    mut commands: Commands,
) {
    use crate::controls::Action;
    if std::env::var("FC3_SLIDE_TEST").is_err() {
        return;
    }
    let Ok(p) = players.single() else { return };
    if p.spawning {
        return;
    }
    let (start, last, shots) = &mut *state;
    let now = time.elapsed_secs();
    let t = now - *start.get_or_insert(now);
    if t < 4.5 {
        actions.movement = Vec2::Y;
        actions.press(Action::Sprint);
    }
    if (2.0..2.05).contains(&t) {
        actions.press(Action::Crouch);
    } else if (2.05..4.5).contains(&t) {
        // Held, not pressed again.
        actions.press(Action::Crouch);
        actions.consume(Action::Crouch);
    }
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
        if t - 2.0 >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    if now - *last >= 0.25 && t < 5.0 {
        *last = now;
        info!("slide-test t={t:.2} speed {:.1} eye {:.2} sliding {:?} crouching {} sprinting {}", Vec2::new(p.velocity.x, p.velocity.z).length(), p.eye, p.sliding.map(|s| (s * 100.0).round() / 100.0), p.crouching, p.sprinting);
    }
}
