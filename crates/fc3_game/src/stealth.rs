//! Stealth: camera tagging, takedowns and on-screen detection markers.
//!
//! * Holding the tag key raises the camera (then a zoomed view through it); an
//!   enemy kept under the crosshair for a moment is tagged and stays marked,
//!   through walls and on the minimap.
//! * Close behind (or beside) an enemy that hasn't spotted the player, the
//!   melee key performs a silent takedown.
//! * Enemies that are noticing the player show a marker over their head that
//!   fills as they become aware (yellow suspicious, orange searching, red in
//!   combat).
//! * Thrown rocks clatter where they land, drawing nearby guards to look.

use std::collections::HashMap;

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::combat::{DamageEvent, DamageKind, Health};
use crate::controls::{Action, ActionState};
use crate::npc::{Npc, NpcState};
use crate::player::{Player, PlayerCamera};
use crate::sky::SEA_LEVEL;
use crate::weapons::{Inventory, WeaponClass};

pub struct StealthPlugin;

impl Plugin for StealthPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Binoculars>()
            .init_resource::<CameraTargets>()
            .add_message::<CameraSaw>()
            .init_resource::<RockAimTest>()
            .init_resource::<Takedown>()
            .add_systems(PreUpdate, aim_test_rock.after(crate::controls::update_actions).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PreUpdate, (takedown_test, below_test, takedown).chain().after(crate::controls::update_actions))
            .add_systems(Startup, load_rock)
            .add_systems(
                Update,
                ((tag_test, binoculars, camera_shot).chain(), takedown_prompt.before(crate::hud::HudSet), detection_markers, throw_rock, fly_rocks)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            );
    }
}

/// An enemy the player has tagged.
#[derive(Component)]
pub struct Tagged;

/// Binocular zoom (0..1 blend, read by the camera FOV and view model).
#[derive(Resource)]
pub struct Binoculars {
    pub blend: f32,
    /// The camera's magnification (`CAMERA_ZOOM`), the wheel's to change
    /// while it is up.
    pub zoom: f32,
    /// Something to tag or take in is under the camera's centre.
    pub on_target: bool,
    /// Seconds the tag key has been held (the camera comes up first).
    pub held: f32,
    dwell: HashMap<Entity, f32>,
}

/// The camera rested on one of the story's characters (alive or dead) long
/// enough to take it in: scripts hear it discovered (and a live one, foe or
/// not, tagged: a mission may want a captive picked out). Or on a thing the
/// scripts wait to see through it (WIL_010's packages' barcodes).
#[derive(Message, Clone, Copy)]
pub struct CameraSaw {
    pub id: u64,
    pub tagged: bool,
}

/// The things (not characters) scripts wait to see through the camera
/// (`Entity_Discovered`), by entity id (the story keeps it), and what the
/// camera has taken in (one script may go on listening for all of them).
#[derive(Resource, Default)]
pub struct CameraTargets {
    pub awaited: Vec<u64>,
    /// Where the story placed them: for one with no placed object of its
    /// own in the world (a base-world prop: JAS_200's meeting, a guard rail
    /// on the catwalk it is held on).
    pub spots: Vec<(u64, Vec3)>,
    pub taken: std::collections::HashSet<u64>,
}

/// The camera's raise (its draw clip) before the view goes through it.
const CAMERA_RAISE: f32 = 0.5;

/// The camera's magnification: least and most (its viewfinder reads 1.0x to
/// 7.5x), as first raised, and a step of the wheel.
pub const CAMERA_ZOOM: [f32; 2] = [1.0, 7.5];
const CAMERA_START_ZOOM: f32 = 4.0;
const CAMERA_ZOOM_STEP: f32 = 1.25;

impl Default for Binoculars {
    fn default() -> Self {
        Binoculars { blend: 0.0, zoom: CAMERA_START_ZOOM, on_target: false, held: 0.0, dwell: HashMap::new() }
    }
}

/// The vertical field of view (degrees) through the camera at a
/// magnification, from the view's own.
pub fn camera_fov(view: f32, zoom: f32) -> f32 {
    2.0 * ((view.to_radians() * 0.5).tan() / zoom.max(1.0)).atan().to_degrees()
}

/// Takedown in progress: restores the previous weapon afterwards.
#[derive(Resource, Default)]
pub struct Takedown {
    /// Enemy that can be taken down right now, and how.
    pub candidate: Option<Entity>,
    kind: TakedownKind,
    timer: f32,
    previous: Option<usize>,
    /// Seconds left to chain a thrown knife after a takedown.
    chain_window: f32,
    /// Grenade takedown: where the group the victim will be kicked into is.
    grenade_at: Option<Vec3>,
    /// The enemy just taken down (not a chain target).
    last: Option<Entity>,
    /// A second enemy in view that the candidate's knife would be thrown at.
    chain_target: Option<Entity>,
    /// The enemies in view his pistol would be turned on.
    pistol_targets: [Option<Entity>; 3],
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum TakedownKind {
    #[default]
    Ground,
    Above,
    /// From above onto two.
    Double,
    /// Pulling him off a ledge from below.
    Below,
    Chain,
}

/// Chained knife throws: how long after a takedown, and how far.
const CHAIN_WINDOW: f32 = 1.8;
const CHAIN_RANGE: f32 = 15.0;
/// How far the pistol takedown reaches.
const PISTOL_RANGE: f32 = 30.0;
/// How near the first a second enemy must stand for a double takedown.
const DOUBLE_REACH: f32 = 4.5;
/// How far ahead the grenade takedown's kick lands, and how near it two
/// others must stand.
const GRENADE_KICK: f32 = 6.0;
const GRENADE_GROUP: f32 = 6.0;

impl Takedown {
    pub fn active(&self) -> bool {
        self.timer > 0.0
    }
}

const TAKEDOWN_RANGE: f32 = 2.3;
/// The game's thrown-rock model.
const ROCK_MODEL: &str = r"graphics\__fc3_graphics\multiplayer\gameplay\thrownrock_01.xbg";
/// Guards within this distance hear a rock land.
const ROCK_HEARD: f32 = 16.0;

#[derive(Resource)]
struct RockModel(Handle<crate::cache_assets::Fc3Model>);

/// A thrown rock in flight (or lying where it landed).
#[derive(Component)]
struct Rock {
    velocity: Vec3,
    landed: bool,
    age: f32,
}

fn load_rock(mut commands: Commands, server: Res<AssetServer>) {
    commands.insert_resource(RockModel(server.load(crate::cache_assets::cache_path(&fc3_formats::cache::model_rel(ROCK_MODEL)))));
}

/// Autopilot requests a landing target; the normal input/throw/physics path
/// must produce the impact. No noise or mission event is fabricated here.
#[derive(Resource, Default)]
pub(crate) struct RockAimTest(pub Option<Vec3>);

fn rock_aim(from: Vec3, target: Vec3) -> Option<Vec3> {
    let horizontal = (target - from).with_y(0.0);
    let distance = horizontal.length();
    if distance < 0.6 { return None; }
    let heading = horizontal / distance;
    let mut best = None;
    let mut error = f32::MAX;
    // Include the actual hand offset and upward launch bias. Prefer the low
    // arc, which avoids roofs; physical collisions still decide the impact.
    for i in 0..=1300 {
        let angle = -1.2 + i as f32 * 0.002;
        let f = heading * angle.cos() + Vec3::Y * angle.sin();
        let velocity = (f + Vec3::Y * 0.12).normalize() * 17.0;
        let flight = (distance - angle.cos() * 0.5) / velocity.with_y(0.0).length();
        let y = from.y + f.y * 0.5 - 0.1 + velocity.y * flight - 4.905 * flight * flight;
        let miss = (y - target.y).abs();
        if miss < error { error = miss; best = Some(f); }
        if miss < 0.025 { return Some(f); }
    }
    best.filter(|_| error < 0.15)
}

fn aim_test_rock(
    mut target: ResMut<RockAimTest>,
    mut players: Query<(Entity, &mut Player)>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut actions: ResMut<ActionState>,
    spatial: SpatialQuery,
) {
    let Some(mut at) = target.0 else { return };
    let (Ok((entity, mut p)), Ok(cam)) = (players.single_mut(), cameras.single()) else { return };
    if p.spawning || p.velocity.length() > 0.2 { return; }
    // Trigger centres are often above a sloped floor. Aim at the actual
    // collision surface, otherwise the rock sails through the centre and
    // lands outside the volume the investigating guard must enter.
    let origin = at + Vec3::Y * 2.0;
    if let Some(hit) = spatial.cast_ray(origin, Dir3::NEG_Y, 8.0, true, &SpatialQueryFilter::from_excluded_entities([entity])) {
        at.y = origin.y - hit.distance + 0.02;
    }
    let Some(f) = rock_aim(cam.translation(), at) else {
        warn!("story rock test: target {at:?} is out of throwing range from {:?}", cam.translation());
        target.0 = None;
        return;
    };
    p.yaw = (-f.x).atan2(-f.z);
    p.pitch = f.y.asin();
    actions.look = Vec2::ZERO;
    if cam.forward().dot(f) > 0.9999 { actions.press(Action::ThrowRock); }
}

/// Throws a rock along the view (unlimited, a short pause between throws).
fn throw_rock(
    time: Res<Time>,
    actions: Res<ActionState>,
    mut cooldown: Local<f32>,
    players: Query<(&Player, &Health)>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    mut commands: Commands,
    mut fp: ResMut<crate::fp::FpState>,
    busy: Res<crate::fp::HandsBusy>,
    scripted: Res<crate::story::ScriptedPlayer>,
    test_target: Option<ResMut<RockAimTest>>,
    mut stats: MessageWriter<crate::progression::StatGain>,
) {
    *cooldown -= time.delta_secs();
    let Ok((p, h)) = players.single() else { return };
    if !actions.just_pressed(Action::ThrowRock)
        || scripted.ignored.contains("throw_rock")
        || !scripted.has_gadget("gadgets:FC3/Rock")
        || *cooldown > 0.0
        || h.dead
        || p.swimming
        || p.driving
        || p.noclip
        || p.mounted.is_some()
        || busy.busy()
    {
        return;
    }
    let Ok(cam) = camera.single() else { return };
    *cooldown = 0.7;
    fp.gesture = Some("rock");
    stats.write(crate::progression::StatGain { stat: crate::statistics::stat::ROCKS_THROWN, amount: 1 });
    let f = cam.forward().as_vec3();
    if let Some(mut target) = test_target {
        if let Some(at) = target.0.take() { info!("story rock test: physical throw from {:?} toward {at:?}", cam.translation()); }
    }
    commands.spawn((
        Rock { velocity: (f + Vec3::Y * 0.12).normalize() * 17.0 + p.velocity * 0.5, landed: false, age: 0.0 },
        Transform::from_translation(cam.translation() + f * 0.5 - Vec3::Y * 0.1),
        Visibility::Visible,
    ));
}

/// Flies rocks; where one lands, nearby guards hear it and come to look.
#[allow(clippy::too_many_arguments)]
fn fly_rocks(
    time: Res<Time>,
    spatial: SpatialQuery,
    model: Option<Res<RockModel>>,
    models: Res<Assets<crate::cache_assets::Fc3Model>>,
    players: Query<Entity, With<Player>>,
    mut rocks: Query<(Entity, &mut Rock, &mut Transform, Option<&Children>)>,
    mut noise: MessageWriter<crate::npc::Noise>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
    probe: crate::surfaces::SurfaceProbe,
) {
    let dt = time.delta_secs();
    let player = players.single().ok();
    for (e, mut rock, mut t, children) in &mut rocks {
        if children.is_none() {
            if let Some(lod) = model.as_ref().and_then(|m| models.get(&m.0)).and_then(|m| m.lods.first()) {
                for part in &lod.parts {
                    commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), ChildOf(e)));
                }
            }
        }
        rock.age += dt;
        if rock.landed {
            if rock.age > 20.0 {
                commands.entity(e).despawn();
            }
            continue;
        }
        rock.velocity.y -= 9.81 * dt;
        let step = rock.velocity * dt;
        let filter = SpatialQueryFilter::from_excluded_entities(player.into_iter().chain([e]));
        let hit = Dir3::new(step).ok().and_then(|d| spatial.cast_ray(t.translation, d, step.length(), true, &filter));
        match hit {
            Some(h) => {
                t.translation += step.normalize_or_zero() * h.distance + h.normal * 0.05;
                rock.landed = true;
                rock.age = 0.0;
                noise.write(crate::npc::Noise { at: t.translation, radius: ROCK_HEARD, kind: crate::npc::NoiseKind::Interesting, aside: false });
                debug!("rock impact at {:?}", t.translation);
                let event = crate::sound::switched(crate::sound::ev::ROCK_IMPACT, probe.at(h.entity, t.translation, h.normal), 0);
                sfx.write(crate::sound::PlaySfx { event, at: Some(t.translation), volume: 0.45 });
            }
            None => {
                t.translation += step;
                t.rotate_local_x(dt * 9.0);
            }
        }
        // Lost (out over the sea or the void).
        if rock.age > 10.0 {
            commands.entity(e).despawn();
        }
    }
}
const TAG_RANGE: f32 = 180.0;

fn camera_available(actions: &ActionState, player: &Player, health: &Health, scripted: &crate::story::ScriptedPlayer) -> bool {
    (actions.pressed(Action::Tag) || scripted.camera_drawn) && scripted.has_camera() && !health.dead && !player.swimming && !player.noclip && !player.driving
}

#[allow(clippy::too_many_arguments)]
fn binoculars(
    time: Res<Time>,
    actions: Res<ActionState>,
    spatial: SpatialQuery,
    mut bino: ResMut<Binoculars>,
    players: Query<(Entity, &Player, &Health)>,
    camera: Query<&GlobalTransform, With<PlayerCamera>>,
    npcs: Query<(Entity, &GlobalTransform, &Health, &Npc), Without<Tagged>>,
    mut commands: Commands,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    perks: Res<crate::progression::Perks>,
    (mut scripted, hud): (ResMut<crate::story::ScriptedPlayer>, Res<crate::map::HudParams>),
    (actors, mut saw, mut reported, mut story_dwell, placed, mut targets): (
        Query<(Entity, &GlobalTransform, &crate::story_actors::StoryActor)>,
        MessageWriter<CameraSaw>,
        Local<std::collections::HashSet<u64>>,
        Local<HashMap<u64, f32>>,
        Query<(Entity, &GlobalTransform, &crate::objects::PlacedId)>,
        ResMut<CameraTargets>,
    ),
) {
    let dt = time.delta_secs();
    let Ok((pe, p, h)) = players.single() else { return };
    // The game's tagging range (else ours).
    let range = if hud.0.tag_range > 0.0 { hud.0.tag_range } else { TAG_RANGE };
    // A camera a script drew goes down when the player presses for it (the
    // press then holds it up as usual).
    if scripted.camera_drawn && actions.just_pressed(Action::Tag) {
        scripted.camera_drawn = false;
    }
    let active = camera_available(&actions, p, h, &scripted);
    if actions.pressed(Action::Tag) && std::env::var("FC3_CAMERA_DEBUG").is_ok() && time.elapsed_secs() as u32 != (time.elapsed_secs() - dt) as u32 {
        info!(
            "camera asked for: available {active} (camera {}, dead {}, swimming {}, noclip {}, driving {}), held {:.2}, raised {:.2}",
            scripted.has_camera(),
            h.dead,
            p.swimming,
            p.noclip,
            p.driving,
            bino.held,
            bino.blend
        );
    }
    bino.held = if active { bino.held + dt } else { 0.0 };
    let target = if bino.held >= CAMERA_RAISE { 1.0 } else { 0.0 };
    bino.blend += (target - bino.blend) * (dt * 10.0).min(1.0);
    let up = bino.blend >= 0.8;
    if scripted.camera_up != up {
        scripted.camera_up = up;
    }
    // The wheel zooms the camera while it is up (up is in).
    if up && actions.cycle != 0 {
        bino.zoom = (bino.zoom * CAMERA_ZOOM_STEP.powi(-actions.cycle.signum())).clamp(CAMERA_ZOOM[0], CAMERA_ZOOM[1]);
    }
    if bino.blend < 0.8 {
        bino.dwell.clear();
        bino.on_target = false;
        return;
    }
    if std::env::var("FC3_CAMERA_DEBUG").is_ok() && time.elapsed_secs() as u32 != (time.elapsed_secs() - dt) as u32 {
        let Ok(cam) = camera.single() else { return };
        let near = actors
            .iter()
            .map(|(_, gt, a)| (a.id, a.dead, gt.translation() - cam.translation()))
            .min_by(|a, b| a.2.length().total_cmp(&b.2.length()))
            .map(|(id, dead, to)| (id, dead, to.length(), to.normalize_or_zero().dot(cam.forward().as_vec3()).clamp(-1.0, 1.0).acos().to_degrees()));
        info!("camera up; nearest story character (id, dead, m, degrees off): {near:?}");
        // The things awaited where the story placed them: how far, how far
        // off the view, and what a ray to them hits first, how far short.
        let filter = SpatialQueryFilter::from_excluded_entities([pe]);
        for &(id, spot) in &targets.spots {
            let to = spot - cam.translation();
            let off = to.normalize_or_zero().dot(cam.forward().as_vec3()).clamp(-1.0, 1.0).acos().to_degrees();
            let hit = Dir3::new(to).ok().and_then(|dir| spatial.cast_ray(cam.translation(), dir, to.length(), true, &filter));
            info!(
                "camera up; awaited thing {id}: {:.1} m, {off:.1} degrees off, ray hits {:?}",
                to.length(),
                hit.map(|h| (h.entity, to.length() - h.distance))
            );
        }
    }
    let Ok(cam) = camera.single() else { return };
    let eye = cam.translation();
    let fwd = cam.forward().as_vec3();
    let mut seen = Vec::new();
    for (e, gt, nh, npc) in &npcs {
        if nh.dead || !npc.faction.hostile_to_player() {
            continue;
        }
        let chest = gt.translation() + Vec3::Y * 0.3;
        let to = chest - eye;
        let dist = to.length();
        if dist > range * perks.tag_range || to.normalize_or_zero().dot(fwd) < 4f32.to_radians().cos() {
            continue;
        }
        let Ok(dir) = Dir3::new(to) else { continue };
        let filter = SpatialQueryFilter::from_excluded_entities([pe]);
        let clear = spatial.cast_ray(eye, dir, dist, true, &filter).is_none_or(|hit| hit.entity == e || hit.distance > dist - 0.8);
        if clear {
            seen.push(e);
        }
    }
    // The story's characters, the dead too (a body to photograph).
    let filter = SpatialQueryFilter::from_excluded_entities([pe]);
    let mut story_seen = Vec::new();
    for (e, gt, actor) in &actors {
        if reported.contains(&actor.id) {
            continue;
        }
        let at = gt.translation() + Vec3::Y * if actor.dead { -0.6 } else { 0.3 };
        let to = at - eye;
        let dist = to.length();
        if dist > range * perks.tag_range || to.normalize_or_zero().dot(fwd) < 4f32.to_radians().cos() {
            continue;
        }
        let Ok(dir) = Dir3::new(to) else { continue };
        if spatial.cast_ray(eye, dir, dist, true, &filter).is_none_or(|hit| hit.entity == e || hit.distance > dist - 0.8) {
            story_seen.push((actor.id, !actor.dead));
        }
    }
    // Things the scripts wait to see (not characters): their middle.
    if !targets.awaited.is_empty() {
        let mut in_world = Vec::new();
        for (e, gt, id) in &placed {
            if !targets.awaited.contains(&id.0) {
                continue;
            }
            in_world.push(id.0);
            if reported.contains(&id.0) {
                continue;
            }
            let to = gt.translation() - eye;
            let dist = to.length();
            if dist > range * perks.tag_range || to.normalize_or_zero().dot(fwd) < 4f32.to_radians().cos() {
                continue;
            }
            let Ok(dir) = Dir3::new(to) else { continue };
            if spatial.cast_ray(eye, dir, dist, true, &filter).is_none_or(|hit| hit.entity == e || hit.distance > dist - 0.8) {
                story_seen.push((id.0, false));
            }
        }
        // (One not placed in the world as a thing of its own: where the story
        // placed it, nothing but itself in the way.)
        for &(id, spot) in &targets.spots {
            if in_world.contains(&id) || reported.contains(&id) {
                continue;
            }
            let to = spot - eye;
            let dist = to.length();
            if dist > range * perks.tag_range || to.normalize_or_zero().dot(fwd) < 4f32.to_radians().cos() {
                continue;
            }
            let Ok(dir) = Dir3::new(to) else { continue };
            if spatial.cast_ray(eye, dir, dist, true, &filter).is_none_or(|hit| hit.distance > dist - 1.0) {
                story_seen.push((id, false));
            }
        }
    }
    if !story_seen.is_empty() {
        trace!("camera: {} of the story's characters in view", story_seen.len());
    }
    bino.on_target = !seen.is_empty() || !story_seen.is_empty();
    story_dwell.retain(|id, _| story_seen.iter().any(|s| s.0 == *id));
    for (id, hostile) in story_seen {
        let t = story_dwell.entry(id).or_insert(0.0);
        *t += dt;
        if *t >= 0.35 && reported.insert(id) {
            saw.write(CameraSaw { id, tagged: hostile });
            targets.taken.insert(id);
        }
    }
    bino.dwell.retain(|e, _| seen.contains(e));
    for e in seen {
        let t = bino.dwell.entry(e).or_insert(0.0);
        *t += dt;
        if *t >= 0.35 {
            commands.entity(e).insert(Tagged);
            sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::UI_CLICK, at: None, volume: 0.5 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story::ScriptedPlayer;

    #[test]
    fn aimed_rock_reaches_near_far_and_raised_targets_under_projectile_gravity() {
        let from = Vec3::new(0.0, 2.0, 0.0);
        for target in [Vec3::new(4.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -18.0), Vec3::new(9.0, 4.0, -9.0)] {
            let direction = rock_aim(from, target).expect("reachable target");
            let mut at = from + direction * 0.5 - Vec3::Y * 0.1;
            let mut velocity = (direction + Vec3::Y * 0.12).normalize() * 17.0;
            let heading = (target - from).with_y(0.0).normalize();
            let mut nearest = f32::MAX;
            for _ in 0..3000 {
                velocity.y -= 9.81 * 0.001;
                at += velocity * 0.001;
                nearest = nearest.min(at.distance(target));
                if (at - target).dot(heading) >= 0.0 { break; }
            }
            assert!(nearest < 0.08, "missed {target:?} by {nearest}");
        }
        assert!(rock_aim(from, Vec3::new(100.0, 0.0, 0.0)).is_none());
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

    #[test]
    fn rock_throws_require_the_mission_gadget_and_obey_signal_locks() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<ActionState>()
            .init_resource::<crate::fp::FpState>()
            .init_resource::<crate::fp::HandsBusy>()
            .init_resource::<ScriptedPlayer>()
            .add_message::<crate::progression::StatGain>()
            .add_systems(Update, throw_rock);
        app.world_mut().spawn((player(), Health::new(100.0)));
        app.world_mut().spawn((PlayerCamera, GlobalTransform::default()));
        app.world_mut().resource_mut::<ActionState>().press(Action::ThrowRock);
        app.world_mut().resource_mut::<ScriptedPlayer>().gadgets.clear();
        app.update();
        let count = |app: &mut App| app.world_mut().query::<&Rock>().iter(app.world()).count();
        assert_eq!(count(&mut app), 0);
        app.world_mut().resource_mut::<ScriptedPlayer>().gadgets.insert("gadgets:fc3/rock".into());
        app.world_mut().resource_mut::<ScriptedPlayer>().ignored.insert("throw_rock".into());
        app.update();
        assert_eq!(count(&mut app), 0);
        app.world_mut().resource_mut::<ScriptedPlayer>().ignored.clear();
        app.update();
        assert_eq!(count(&mut app), 1);
        assert_eq!(app.world().resource::<crate::fp::FpState>().gesture, Some("rock"));
    }

    #[test]
    fn removing_the_camera_disables_tagging_while_the_button_is_held() {
        let mut scripted = ScriptedPlayer::default();
        let mut actions = ActionState::default();
        actions.hold(Action::Tag);
        let mut player = player();
        let health = Health::new(100.0);
        assert!(camera_available(&actions, &player, &health, &scripted));
        scripted.gadgets.clear();
        assert!(!camera_available(&actions, &player, &health, &scripted));
        scripted.gadgets.insert("gadgets:equipped/tablet".into());
        assert!(camera_available(&actions, &player, &health, &scripted));
        scripted.gadgets.clear();
        scripted.gadgets.insert("gadgets:Equipped/Binoculars".into());
        assert!(camera_available(&actions, &player, &health, &scripted));
        player.driving = true;
        assert!(!camera_available(&actions, &player, &health, &scripted));
        // Drawn by a script, the camera comes up with no button held.
        player.driving = false;
        let idle = ActionState::default();
        assert!(!camera_available(&idle, &player, &health, &scripted));
        scripted.camera_drawn = true;
        assert!(camera_available(&idle, &player, &health, &scripted));
    }

    #[test]
    fn the_camera_narrows_the_view_by_its_magnification() {
        assert!((camera_fov(70.0, 1.0) - 70.0).abs() < 1e-3);
        let narrow = camera_fov(70.0, 4.0);
        assert!(((narrow.to_radians() * 0.5).tan() * 4.0 - 35f32.to_radians().tan()).abs() < 1e-4);
        assert!(camera_fov(70.0, CAMERA_ZOOM[1]) < narrow);
    }
}

/// `FC3_TAG_TEST` (with `--npc-test guards`): holds the tag key from 6 s in,
/// logging what gets tagged; `FC3_TD_SHOT` screenshots that long into it.
fn tag_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    mut state: Local<(f32, usize, bool)>,
    mut actions: ResMut<ActionState>,
    tagged: Query<(), With<Tagged>>,
    mut commands: Commands,
) {
    if std::env::var("FC3_TAG_TEST").is_err() {
        return;
    }
    let (start, shots, logged) = &mut *state;
    let now = time.elapsed_secs();
    if *start == 0.0 {
        *start = now + 6.0;
    }
    if now < *start {
        return;
    }
    actions.hold(Action::Tag);
    let t = now - *start;
    let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
        if t >= when {
            let stem = path.strip_suffix(".png").unwrap_or(path);
            crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
            *shots += 1;
        }
    }
    if t > 2.0 && !*logged {
        *logged = true;
        info!("tag-test: {} tagged", tagged.iter().count());
    }
}

/// `FC3_CAMERA_SHOT=<seconds>` (with `--screenshot`): a screenshot the
/// first time the camera has been up that long (`<stem>_camera.png`).
fn camera_shot(time: Res<Time>, args: Res<crate::Args>, scripted: Res<crate::story::ScriptedPlayer>, mut up_for: Local<Option<f32>>, mut commands: Commands) {
    let Some(after) = std::env::var("FC3_CAMERA_SHOT").ok().and_then(|v| v.parse::<f32>().ok()) else { return };
    let Some(path) = args.screenshot.as_ref() else { return };
    let Some(t) = up_for.as_mut().filter(|t| **t >= 0.0) else {
        if up_for.is_none() && scripted.camera_up {
            *up_for = Some(0.0);
        }
        return;
    };
    *t = if scripted.camera_up { *t + time.delta_secs() } else { 0.0 };
    if *t >= after {
        let stem = path.strip_suffix(".png").unwrap_or(path);
        crate::camera::capture(&mut commands, format!("{stem}_camera.png"));
        // (Once.)
        *up_for = Some(-1.0);
    }
}

/// Finds a takedown target and performs the takedown on the melee key:
/// on the ground from behind (or the front with the skill), dropping onto an
/// enemy from above, or, right after one, a thrown knife at another.
#[allow(clippy::too_many_arguments)]
fn takedown(
    time: Res<Time>,
    state: Res<State<crate::menu::GameState>>,
    spatial: SpatialQuery,
    mut actions: ResMut<ActionState>,
    mut td: ResMut<Takedown>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &Health, &mut Inventory), Without<Npc>>,
    npcs: Query<(Entity, &Transform, &Npc, &Health), Without<Player>>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    perks: Res<crate::progression::Perks>,
    (mut particles, mut rng, busy): (ResMut<crate::particles::Particles>, ResMut<crate::weapons::Rng>, Res<crate::fp::HandsBusy>),
    mut anims: MessageWriter<crate::fp::TakedownAnim>,
    (mut mount, mut commands, heights): (ResMut<crate::traversal::Mount>, Commands, Res<crate::terrain::Heightfield>),
    (mut stats, mut rewards): (MessageWriter<crate::progression::StatGain>, ResMut<crate::progression::TakedownRewards>),
) {
    let Ok((pe, mut pt, mut p, h, mut inv)) = players.single_mut() else { return };
    let dt = time.delta_secs();
    td.chain_window = (td.chain_window - dt).max(0.0);
    td.candidate = None;
    td.chain_target = None;
    // Finish a takedown: back to the previous weapon.
    if td.timer > 0.0 {
        td.timer -= dt;
        if td.timer <= 0.0 {
            if let Some(prev) = td.previous.take() {
                if prev < inv.slots.len() {
                    inv.current = prev;
                    inv.switching = 0.35;
                }
            }
        }
    }
    // On a zip line only the drop onto someone below is open.
    let zipping = matches!(*mount, crate::traversal::Mount::Zip { .. });
    // In the water only the pull off a dock (Death From Below) is open.
    if *state.get() != crate::menu::GameState::Playing || h.dead || !perks.takedown || (p.swimming && !perks.death_from_below) || p.noclip || (p.driving && !zipping) {
        return;
    }
    let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let takeable = |npc: &Npc, nh: &Health| nh.dead || !npc.faction.hostile_to_player() || (npc.role.armoured() && !perks.heavy_takedown);

    // Chain: a knife thrown at another enemy in view, once the takedown is done.
    if td.chain_window > 0.0 && perks.chain_takedown && td.timer < 0.3 && !p.swimming {
        let eye = pt.translation + Vec3::Y * (crate::player::EYE_HEIGHT - crate::player::HALF_HEIGHT);
        let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch) * Vec3::NEG_Z;
        let best = npcs
            .iter()
            .filter(|(e, _, npc, nh)| Some(*e) != td.last && !takeable(npc, nh))
            .filter_map(|(e, t, _, _)| {
                let to = t.translation + Vec3::Y * 0.4 - eye;
                let d = to.length();
                let aim = to.normalize_or_zero().dot(look);
                (d < CHAIN_RANGE && aim > 0.94).then_some((e, d, to, aim))
            })
            .filter(|(e, d, to, _)| {
                let filter = SpatialQueryFilter::from_excluded_entities([pe]);
                Dir3::new(*to).ok().is_some_and(|dir| spatial.cast_ray(eye, dir, *d, true, &filter).is_none_or(|h| h.entity == *e || h.distance > d - 0.6))
            })
            .max_by(|a, b| a.3.total_cmp(&b.3));
        if let Some((target, _, to, _)) = best {
            td.candidate = Some(target);
            td.kind = TakedownKind::Chain;
            if actions.just_pressed(Action::Melee) {
                actions.consume(Action::Melee);
                td.chain_window = 0.0;
                rewards.0.insert(target, "TakedownKnifeThrow");
                stats.write(crate::progression::StatGain { stat: crate::statistics::stat::KNIFE_THROW_TAKEDOWNS, amount: 1 });
                damage.write(DamageEvent {
                    target,
                    amount: 10_000.0,
                    kind: DamageKind::Melee,
                    source: Some(pt.translation),
                    instigator: Some(pe),
                    headshot: false,
                    hit: crate::combat::Hit::Whole,
                });
                crate::particles::fx::blood(&mut particles, &mut rng, eye + to, to.normalize_or_zero());
                sfx.write(crate::sound::PlaySfx {
                    event: crate::sound::switched(crate::sound::ev::MELEE_IMPACT, crate::surfaces::value::FLESH, 0),
                    at: Some(eye + to),
                    volume: 0.8,
                });
            }
            return;
        }
    }
    if td.timer > 0.0 || busy.busy() {
        return;
    }

    // From above: falling (or sliding down a zip line) onto an enemy below.
    if perks.death_from_above && ((!p.grounded && !p.swimming && p.velocity.y < -1.0) || zipping) {
        // (A zip line may run higher over him than a ledge.)
        let drop = if zipping { 12.0 } else { 8.0 };
        let below = npcs
            .iter()
            .filter(|(_, _, npc, nh)| !takeable(npc, nh))
            .map(|(e, t, _, _)| (e, t.translation - pt.translation))
            .filter(|(_, to)| Vec2::new(to.x, to.z).length() < 2.6 && (-drop..-0.8).contains(&to.y))
            .min_by(|a, b| a.1.length().total_cmp(&b.1.length()));
        if let Some((target, to)) = below {
            td.candidate = Some(target);
            td.kind = TakedownKind::Above;
            // With the skill, a second enemy right beside him goes down too
            // (the regular build only: the clips don't cover heavies).
            let first = npcs.get(target).ok().map(|(_, t, n, _)| (t.translation, n.role.armoured()));
            let second = first.filter(|(_, heavy)| perks.double_takedown && !heavy).and_then(|(at, _)| {
                npcs.iter()
                    .filter(|(e, t, n, h)| {
                        *e != target
                            && !takeable(n, h)
                            && !n.role.armoured()
                            && Vec2::new(t.translation.x - at.x, t.translation.z - at.z).length() < DOUBLE_REACH
                            && (t.translation.y - at.y).abs() < 2.5
                    })
                    .min_by(|a, b| a.1.translation.distance(at).total_cmp(&b.1.translation.distance(at)))
                    .map(|(e, t, _, _)| {
                        let flat = Vec3::new(to.x, 0.0, to.z).normalize_or(fwd);
                        // Which side of the first he stands, from where we come down.
                        let left = (t.translation - at).dot(flat.cross(Vec3::Y)) < 0.0;
                        (e, left)
                    })
            });
            if second.is_some() {
                td.kind = TakedownKind::Double;
            }
            if actions.just_pressed(Action::Melee) {
                actions.consume(Action::Melee);
                // Off the zip line and down onto him.
                if zipping {
                    *mount = crate::traversal::Mount::None;
                    p.driving = false;
                    commands.entity(pe).remove::<avian3d::prelude::ColliderDisabled>();
                }
                // Land beside him, the fall broken by the takedown, facing
                // him; the clips cover his front, back and sides.
                let flat = Vec3::new(to.x, 0.0, to.z).normalize_or(fwd);
                pt.translation += to - flat * 0.9;
                // (Not into a slope rising behind him.)
                if let Some(g) = heights.height_at(pt.translation) {
                    pt.translation.y = pt.translation.y.max(g + crate::player::HALF_HEIGHT);
                }
                p.yaw = (-flat.x).atan2(-flat.z);
                p.velocity = Vec3::ZERO;
                p.last_landing_speed = 0.0;
                let from = npcs.get(target).map(|(_, _, n, _)| crate::fp::Quarter::of(n.yaw, -flat)).unwrap_or(crate::fp::Quarter::Back);
                strike(&mut td, &mut inv, target, pe, pt.translation, &mut damage, &mut sfx);
                stats.write(crate::progression::StatGain { stat: crate::progression::stat::TAKEDOWNS_FROM_ABOVE, amount: 1 });
                let reward = if second.is_some() { "TakedownFromAboveDouble" } else { "TakedownFromAbove" };
                rewards.0.insert(target, reward);
                if let Some((e2, _)) = second {
                    rewards.0.insert(e2, reward);
                    damage.write(DamageEvent {
                        target: e2,
                        amount: 10_000.0,
                        kind: DamageKind::Melee,
                        source: Some(pt.translation),
                        instigator: Some(pe),
                        headshot: false,
                        hit: crate::combat::Hit::Whole,
                    });
                    td.timer = crate::fp::DOUBLE_TIME + 0.1;
                }
                anims.write(crate::fp::TakedownAnim {
                    victim: target,
                    style: crate::fp::TakedownStyle::Above { from },
                    chain: None,
                    grenade: None,
                    drag: false,
                    second,
                    pistol: [None; 3],
                });
            }
            return;
        }
    }

    // From below: an unaware enemy at the edge of a ledge just above (or of a
    // dock, from the water) gets pulled down.
    if perks.death_from_below && (p.grounded || p.swimming) {
        let feet = pt.translation.y - crate::player::HALF_HEIGHT;
        let (lo, hi) = if p.swimming { (0.6, 2.4) } else { (1.3, 2.8) };
        let above = npcs
            .iter()
            .filter(|(_, _, npc, nh)| !takeable(npc, nh) && npc.state != NpcState::Combat && npc.awareness < 1.0)
            .map(|(e, t, n, _)| (e, t.translation, n.yaw))
            .filter(|(_, at, _)| {
                let up = at.y - crate::player::HALF_HEIGHT - feet;
                Vec2::new(at.x - pt.translation.x, at.z - pt.translation.z).length() < 1.8 && (lo..hi).contains(&up)
            })
            .min_by(|a, b| a.1.distance(pt.translation).total_cmp(&b.1.distance(pt.translation)));
        if let Some((target, at, yaw)) = above {
            td.candidate = Some(target);
            td.kind = TakedownKind::Below;
            if actions.just_pressed(Action::Melee) {
                actions.consume(Action::Melee);
                // Square up under him, a step back from the edge.
                let flat = Vec3::new(at.x - pt.translation.x, 0.0, at.z - pt.translation.z).normalize_or(fwd);
                pt.translation = Vec3::new(at.x, pt.translation.y, at.z) - flat * 1.0;
                p.yaw = (-flat.x).atan2(-flat.z);
                p.pitch = 0.3;
                p.velocity = Vec3::ZERO;
                let facing = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
                let front = facing.dot(flat) < -0.3;
                strike(&mut td, &mut inv, target, pe, at, &mut damage, &mut sfx);
                stats.write(crate::progression::StatGain { stat: crate::progression::stat::TAKEDOWNS_FROM_BELOW, amount: 1 });
                let below = if p.swimming { crate::statistics::stat::WATER_TAKEDOWNS } else { crate::statistics::stat::LEDGE_TAKEDOWNS };
                stats.write(crate::progression::StatGain { stat: below, amount: 1 });
                rewards.0.insert(target, if p.swimming { "TakedownFromWater" } else { "TakedownFromLedge" });
                debug!("takedown from below ({} m up, front {front}, water {})", at.y - crate::player::HALF_HEIGHT - feet, p.swimming);
                anims.write(crate::fp::TakedownAnim {
                    victim: target,
                    style: crate::fp::TakedownStyle::Below { front, water: p.swimming },
                    chain: None,
                    grenade: None,
                    drag: false,
                    second: None,
                    pistol: [None; 3],
                });
            }
            return;
        }
    }
    if p.swimming {
        return;
    }

    // On the ground: from behind or the side (the front with the skill).
    let mut best: Option<(f32, Entity)> = None;
    for (e, t, npc, nh) in &npcs {
        if takeable(npc, nh) || npc.state == NpcState::Combat || npc.awareness >= 1.0 {
            continue;
        }
        let to = t.translation - pt.translation;
        let flat = Vec3::new(to.x, 0.0, to.z);
        let d = flat.length();
        if d > TAKEDOWN_RANGE || to.y.abs() > 1.2 || flat.normalize_or_zero().dot(fwd) < 0.5 {
            continue;
        }
        let facing = Vec3::new(-npc.yaw.sin(), 0.0, -npc.yaw.cos());
        if !perks.front_takedown && facing.dot(-flat.normalize_or_zero()) > 0.5 {
            continue;
        }
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, e));
        }
    }
    let Some((_, target)) = best else { return };
    td.candidate = Some(target);
    td.kind = TakedownKind::Ground;
    // With the grenade skill, a group of others where the victim will land
    // (a few metres ahead) gets him, his grenade's pin pulled.
    td.grenade_at = None;
    if perks.grenade_takedown && npcs.get(target).is_ok_and(|(_, _, n, _)| !n.role.armoured()) {
        let landing = pt.translation + fwd * GRENADE_KICK;
        let near: Vec<Vec3> = npcs
            .iter()
            .filter(|(e, t, npc, nh)| {
                *e != target
                    && !nh.dead
                    && npc.faction.hostile_to_player()
                    && Vec2::new(t.translation.x - landing.x, t.translation.z - landing.z).length() < GRENADE_GROUP
            })
            .map(|(_, t, _, _)| t.translation)
            .collect();
        if near.len() >= 2 {
            td.grenade_at = Some(near.iter().copied().sum::<Vec3>() / near.len() as f32);
        }
    }
    // Other enemies in view (in clear sight, within `range`), nearest first.
    let regular = npcs.get(target).is_ok_and(|(_, _, n, _)| !n.role.armoured());
    let eye = pt.translation + Vec3::Y * (crate::player::EYE_HEIGHT - crate::player::HALF_HEIGHT);
    let filter = SpatialQueryFilter::from_excluded_entities([pe, target]);
    let in_view = |range: f32| -> Vec<Entity> {
        let mut seen: Vec<(Entity, f32)> = npcs
            .iter()
            .filter(|(e, _, npc, nh)| *e != target && !takeable(npc, nh))
            .filter_map(|(e, t, _, _)| {
                let to = t.translation + Vec3::Y * 0.4 - eye;
                let d = to.length();
                (d < range && Vec3::new(to.x, 0.0, to.z).normalize_or_zero().dot(fwd) > 0.4).then_some((e, d, to))
            })
            .filter(|(e, d, to)| {
                Dir3::new(*to).ok().is_some_and(|dir| spatial.cast_ray(eye, dir, *d, true, &filter).is_none_or(|h| h.entity == *e || h.distance > d - 0.6))
            })
            .map(|(e, d, _)| (e, d))
            .collect();
        seen.sort_by(|a, b| a.1.total_cmp(&b.1));
        seen.into_iter().map(|(e, _)| e).collect()
    };
    // With the skill, the nearest other enemy in view gets the victim's knife
    // (the game's paired clips cover the regular build, not heavies).
    td.chain_target = None;
    if td.grenade_at.is_none() && perks.chain_takedown && regular {
        td.chain_target = in_view(CHAIN_RANGE).first().copied();
    }
    // With the pistol skill, two or more in view (or one, without the knife
    // throw) get his pistol turned on them, up to three.
    td.pistol_targets = [None; 3];
    if td.grenade_at.is_none() && perks.pistol_takedown && regular {
        let seen = in_view(PISTOL_RANGE);
        if seen.len() >= 2 || (seen.len() == 1 && td.chain_target.is_none()) {
            for (slot, e) in td.pistol_targets.iter_mut().zip(seen) {
                *slot = Some(e);
            }
            td.chain_target = None;
        }
    }
    // E with the skill: take him down and drag the body away.
    let drag = perks.takedown_drag && actions.just_pressed(Action::Interact);
    if !actions.just_pressed(Action::Melee) && !drag {
        return;
    }
    let chain = td.chain_target.filter(|_| td.grenade_at.is_none() && !drag);
    let grenade = td.grenade_at.filter(|_| !drag);
    let pistol = if drag { [None; 3] } else { td.pistol_targets };
    let shots = pistol.iter().flatten().count();
    actions.consume(Action::Melee);
    actions.consume(Action::Interact);
    let Ok((_, t, _, _)) = npcs.get(target) else { return };
    // Close in and strike with the machete.
    let to = t.translation - pt.translation;
    let step = Vec3::new(to.x, 0.0, to.z).normalize_or_zero() * (to.length() - 1.0).max(0.0);
    pt.translation += step;
    // Face him for the takedown; from his front if he's looking our way.
    let flat = Vec3::new(to.x, 0.0, to.z).normalize_or(fwd);
    p.yaw = (-flat.x).atan2(-flat.z);
    let npc_facing = npcs.get(target).map(|(_, _, n, _)| Vec3::new(-n.yaw.sin(), 0.0, -n.yaw.cos())).unwrap_or(flat);
    let style = if npc_facing.dot(flat) < -0.3 { crate::fp::TakedownStyle::Front } else { crate::fp::TakedownStyle::Back };
    strike(&mut td, &mut inv, target, pe, t.translation, &mut damage, &mut sfx);
    // (The XP each victim is worth, by the takedown that kills him; the
    // takedowns counted by kind.)
    let heavy = npcs.get(target).is_ok_and(|(_, _, n, _)| n.role.armoured());
    {
        use crate::statistics::stat;
        let kind = if heavy {
            stat::HEAVY_TAKEDOWNS
        } else if grenade.is_some() {
            stat::GRENADE_TAKEDOWNS
        } else if drag {
            stat::DRAG_TAKEDOWNS
        } else {
            stat::STANDARD_TAKEDOWNS
        };
        let chained = if chain.is_some() { 1 } else { 0 };
        for (stat, amount) in [(kind, 1), (stat::CHAIN_TAKEDOWNS, chained), (stat::PISTOL_TAKEDOWNS, shots as u32)] {
            if amount > 0 {
                stats.write(crate::progression::StatGain { stat, amount });
            }
        }
    }
    rewards.0.insert(target, if heavy { "HeavyTakedown" } else if grenade.is_some() { "TakedownGrenadePinPull" } else { "StandardTakedown" });
    if let Some(c) = chain {
        rewards.0.insert(c, "TakedownChain");
    }
    for e in pistol.iter().flatten() {
        rewards.0.insert(*e, "TakedownPistolMarksman");
    }
    anims.write(crate::fp::TakedownAnim { victim: target, style, chain, grenade, drag, second: None, pistol });
    // The knife throw (or the kick) is part of the takedown: no separate
    // window, a longer hold.
    if chain.is_some() {
        td.chain_window = 0.0;
        td.timer = crate::fp::CHAIN_TIME + 0.1;
    }
    if grenade.is_some() {
        td.chain_window = 0.0;
        td.timer = crate::fp::GRENADE_TIME + 0.1;
    }
    if shots > 0 {
        td.chain_window = 0.0;
        td.timer = crate::fp::pistol_time(shots) + 0.1;
    }
}

/// The machete blow itself; opens the window for a chained knife throw.
fn strike(
    td: &mut Takedown,
    inv: &mut Inventory,
    target: Entity,
    player: Entity,
    at: Vec3,
    damage: &mut MessageWriter<DamageEvent>,
    sfx: &mut MessageWriter<crate::sound::PlaySfx>,
) {
    if let Some(machete) = inv.slots.iter().position(|s| inv.defs[s.def].class == WeaponClass::Melee) {
        if inv.current != machete {
            td.previous = Some(inv.current);
            inv.current = machete;
        }
    }
    inv.recoil_kick = 1.0;
    // The arms swing the machete (or play the takedown through).
    inv.since_shot = 0.0;
    td.timer = 2.2;
    td.chain_window = td.timer + CHAIN_WINDOW;
    td.last = Some(target);
    damage.write(DamageEvent {
        target,
        amount: 10_000.0,
        kind: DamageKind::Melee,
        source: Some(at),
        instigator: Some(player),
        headshot: false,
        hit: crate::combat::Hit::Whole,
    });
    sfx.write(crate::sound::PlaySfx {
        event: crate::sound::switched(crate::sound::ev::MELEE_IMPACT, crate::surfaces::value::FLESH, 0),
        at: Some(at),
        volume: 0.8,
    });
}

fn takedown_prompt(
    td: Res<Takedown>,
    actions: Res<ActionState>,
    settings: Res<crate::settings::Settings>,
    perks: Res<crate::progression::Perks>,
    text: Option<Res<crate::hud::GameText>>,
    mut hud: ResMut<crate::hud::HudState>,
) {
    if td.candidate.is_some() && hud.prompt.is_none() {
        // The game's words where it has them (a plain takedown, one from a
        // ledge below, a double); the grenade, pistol and chain ones are ours.
        use crate::hud::prompt as p;
        let plain = td.grenade_at.is_none() && td.pistol_targets[0].is_none() && td.chain_target.is_none();
        let game = match td.kind {
            TakedownKind::Ground | TakedownKind::Above if plain => Some(p::TAKEDOWN),
            TakedownKind::Below => Some(p::LEDGE_TAKEDOWN),
            TakedownKind::Double => Some(p::DOUBLE_TAKEDOWN),
            _ => None,
        };
        let what = match td.kind {
            TakedownKind::Ground if td.grenade_at.is_some() => "grenade takedown",
            TakedownKind::Ground if td.pistol_targets[0].is_some() => "pistol takedown",
            TakedownKind::Ground if td.chain_target.is_some() => "chain takedown",
            TakedownKind::Ground => "takedown",
            TakedownKind::Above => "takedown from above",
            TakedownKind::Double => "double takedown",
            TakedownKind::Below => "takedown from below",
            TakedownKind::Chain => "throw knife",
        };
        let mut line = game
            .and_then(|id| text.as_deref()?.prompt(id, &actions, &settings))
            .unwrap_or_else(|| format!("{}  {what}", actions.label(Action::Melee, &settings)));
        if td.kind == TakedownKind::Ground && perks.takedown_drag {
            line += &format!("      {}  drag", actions.label(Action::Interact, &settings));
        }
        hud.prompt = Some(line);
    }
}

/// A marker over an enemy's head.
#[derive(Component)]
struct DetectionMarker;

fn detection_markers(
    cameras: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    npcs: Query<(&GlobalTransform, &Npc, &Health, Has<Tagged>)>,
    markers: Query<Entity, With<DetectionMarker>>,
    mut commands: Commands,
    scale: Res<UiScale>,
) {
    for e in &markers {
        commands.entity(e).despawn();
    }
    let Ok((cam, cam_t)) = cameras.single() else { return };
    for (gt, npc, h, tagged) in &npcs {
        if h.dead || !npc.faction.hostile_to_player() {
            continue;
        }
        let noticing = npc.awareness > 0.05 || npc.state != NpcState::Idle;
        if !noticing && !tagged {
            continue;
        }
        let head = gt.translation() + Vec3::Y * 1.25;
        if cam_t.translation().distance(head) > 250.0 {
            continue;
        }
        let Ok(px) = cam.world_to_viewport(cam_t, head) else { continue };
        let px = crate::ui_nav::viewport_to_ui(px, scale.0);
        let colour = match npc.state {
            NpcState::Combat => Color::srgb(0.95, 0.15, 0.1),
            NpcState::Search => Color::srgb(1.0, 0.55, 0.12),
            _ if noticing => Color::srgb(1.0, 0.85, 0.25),
            _ => Color::srgb(0.95, 0.95, 0.9),
        };
        let fill = if npc.state == NpcState::Combat { 1.0 } else { npc.awareness.clamp(0.0, 1.0) };
        let root = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(px.x - 9.0),
                    top: Val::Px(px.y - 18.0),
                    width: Val::Px(18.0),
                    height: Val::Px(18.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    ..default()
                },
                GlobalZIndex(1),
                DetectionMarker,
            ))
            .id();
        // Tagged enemies: a red diamond; noticing ones: a coloured bar filling up.
        if tagged {
            commands.spawn((
                Node { width: Val::Px(9.0), height: Val::Px(9.0), border: UiRect::all(Val::Px(1.0)), ..default() },
                BackgroundColor(Color::srgb(0.85, 0.12, 0.1)),
                BorderColor::all(Color::srgb(0.05, 0.05, 0.05)),
                UiTransform { rotation: Rot2::degrees(45.0), ..default() },
                ChildOf(root),
            ));
        }
        if noticing {
            let bar = commands
                .spawn((
                    Node { width: Val::Px(22.0), height: Val::Px(4.0), margin: UiRect::top(Val::Px(3.0)), ..default() },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
                    ChildOf(root),
                ))
                .id();
            commands.spawn((Node { width: Val::Percent(fill * 100.0), height: Val::Percent(100.0), ..default() }, BackgroundColor(colour), ChildOf(bar)));
        }
    }
}

/// `--takedown-test`: steps up to the test enemy, presses melee and logs the
/// result. `FC3_TAKEDOWN_MODE=above` drops onto it instead; `chain` (with two
/// guards, `--npc-test guards`) takes one down and throws at the other.
fn takedown_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    mut step: Local<(u32, f32)>,
    mut actions: ResMut<ActionState>,
    mut players: Query<(&mut Transform, &mut Player), Without<Npc>>,
    npcs: Query<(&Transform, &Health, &Npc), Without<Player>>,
    mut shots: Local<(Option<f32>, usize)>,
    mut commands: Commands,
    (td, mount): (Res<Takedown>, Res<crate::traversal::Mount>),
) {
    // `FC3_TRAVERSAL_TEST=zip_takedown`: drop off the zip line onto the enemy
    // waiting under it, once over him.
    if std::env::var("FC3_TRAVERSAL_TEST").is_ok_and(|m| m == "zip_takedown") && matches!(*mount, crate::traversal::Mount::Zip { .. }) && td.candidate.is_some()
    {
        info!("zip test: dropping onto the enemy below");
        actions.press(Action::Melee);
    }
    let (n, wait) = &mut *step;
    if !args.takedown_test || std::env::var("FC3_TAKEDOWN_MODE").is_ok_and(|m| m.starts_with("below")) {
        return;
    }
    // FC3_TD_SHOT=0.5,1.2: screenshots that long after the strike (with --screenshot and a long --settle).
    let (since, taken) = &mut *shots;
    if let (Some(since), Some(path)) = (since.as_mut(), args.screenshot.as_ref()) {
        *since += time.delta_secs();
        let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
        if let Some(&t) = at.get(*taken) {
            if *since >= t {
                let stem = path.strip_suffix(".png").unwrap_or(path);
                crate::camera::capture(&mut commands, format!("{stem}_td{taken}.png"));
                *taken += 1;
            }
        }
    }
    if *n > 40 {
        return;
    }
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    let mut guards: Vec<(&Transform, &Health, &Npc)> = npcs.iter().filter(|(_, _, n)| n.marker_id >= u64::MAX - 4).collect();
    guards.sort_by_key(|(_, _, n)| std::cmp::Reverse(n.marker_id));
    let Some(&(nt, nh, _)) = guards.first() else { return };
    if p.spawning {
        return;
    }
    // Dragging (the `drag` mode): walking backwards with the body.
    if std::env::var("FC3_TAKEDOWN_MODE").as_deref() == Ok("drag") && (10..=13).contains(n) {
        actions.movement = Vec2::new(0.0, -1.0);
        // Looking down at him.
        if let Ok(pitch) = std::env::var("FC3_DRAG_PITCH").map(|v| v.parse::<f32>().unwrap_or(-0.5)) {
            p.pitch = pitch;
        }
    }
    *wait -= time.delta_secs();
    if *wait > 0.0 {
        return;
    }
    let mode = std::env::var("FC3_TAKEDOWN_MODE").unwrap_or_default();
    let face = |p: &mut Player, from: Vec3, at: Vec3| {
        let to = at - from;
        p.yaw = (-to.x).atan2(-to.z);
        p.pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
    };
    match (*n, mode.as_str()) {
        (0, "double") => {
            // Above the first guard, off its far shoulder from the second.
            let second = guards.get(1).map(|g| g.0.translation).unwrap_or(nt.translation);
            let away = (nt.translation - second).normalize_or(Vec3::X);
            pt.translation = nt.translation + away * 0.6 + Vec3::Y * 4.0;
            let to = second - nt.translation;
            p.yaw = (-to.z).atan2(to.x);
            p.velocity = Vec3::new(0.0, -2.0, 0.0);
            info!("takedown-test: dropping onto two");
            *n = 1;
        }
        (1..=20, "double") => {
            actions.press(Action::Melee);
            shots.0.get_or_insert(0.0);
            *n += 1;
            if guards.iter().all(|(_, h, _)| h.dead) {
                info!("takedown-test: both dead");
                *n = 21;
                *wait = 3.0;
            }
        }
        (21, "double") => {
            let at: Vec<String> = guards.iter().map(|(t, h, _)| format!("{:.1?} dead {}", t.translation, h.dead)).collect();
            info!("takedown-test: after: {at:?}");
            *n = 99;
        }
        (0, m) if m.starts_with("above") => {
            // Four metres up, just off its shoulder, already falling
            // (`above_front|back|left|right`: off that side of him).
            let facing = nt.rotation * Vec3::NEG_Z;
            let side = match m {
                "above_front" => facing,
                "above_back" => -facing,
                "above_right" => facing.cross(Vec3::Y),
                "above_left" => -facing.cross(Vec3::Y),
                _ => Vec3::X,
            };
            pt.translation = nt.translation + Vec3::new(side.x, 0.0, side.z).normalize_or(Vec3::X) * 0.6 + Vec3::Y * 4.0;
            p.velocity = Vec3::new(0.0, -2.0, 0.0);
            info!("takedown-test: dropping from above");
            *n = 1;
        }
        (1..=20, m) if m.starts_with("above") => {
            actions.press(Action::Melee);
            shots.0.get_or_insert(0.0);
            *n += 1;
            if nh.dead {
                info!("takedown-test: enemy dead = true (from above, landing speed {:.1})", p.last_landing_speed);
                *n = 99;
            }
        }
        (0, _) => {
            // Behind it, facing its back.
            let back = Vec3::new(nt.rotation.mul_vec3(Vec3::Z).x, 0.0, nt.rotation.mul_vec3(Vec3::Z).z).normalize_or(Vec3::Z);
            // `front`: face to face instead (needs the skill).
            let side = if mode == "front" { -1.0 } else { 1.0 };
            pt.translation = nt.translation + back * side * 1.6;
            let at = nt.translation;
            face(&mut p, pt.translation, at);
            info!("takedown-test: positioned");
            *n = 1;
        }
        (1, "drag") => {
            actions.press(Action::Interact);
            shots.0 = Some(0.0);
            *n = 10;
            *wait = 3.2;
        }
        // Dragging: walk back for three seconds, then let go.
        (10..=12, "drag") => {
            info!("takedown-test: dragging, player at {:.1?} yaw {:.2} pitch {:.2}, body at {:.1?}", pt.translation, p.yaw, p.pitch, nt.translation);
            *n += 1;
            *wait = 1.0;
        }
        (13, "drag") => {
            actions.press(Action::Interact);
            *n = 14;
            *wait = 2.5;
        }
        (14, "drag") => {
            info!("takedown-test: dropped; body at {:.1?}, dead {}", nt.translation, nh.dead);
            *n = 99;
        }
        (1, _) => {
            actions.press(Action::Melee);
            shots.0 = Some(0.0);
            *n = 2;
            // Through the takedown (`grenade`: and the blast).
            *wait = match mode.as_str() {
                "grenade" => crate::fp::GRENADE_TIME + 1.0,
                "pistol" => crate::fp::pistol_time(3) + 1.0,
                _ => 2.3,
            };
        }
        (2, "chain") => {
            // Turn to the second guard and throw.
            if let Some(&(t2, _, _)) = guards.get(1) {
                let eye = pt.translation + Vec3::Y * (crate::player::EYE_HEIGHT - crate::player::HALF_HEIGHT);
                face(&mut p, eye, t2.translation + Vec3::Y * 0.4);
            }
            *n = 3;
            *wait = 0.1;
        }
        (3, "chain") => {
            actions.press(Action::Melee);
            *n = 4;
            *wait = 0.5;
        }
        _ => {
            let dead: Vec<bool> = guards.iter().map(|(_, h, _)| h.dead).collect();
            info!("takedown-test: dead = {dead:?}");
            *n = 99;
        }
    }
}

/// `--takedown-test` with `FC3_TAKEDOWN_MODE=below` (and `FC3_CLIMB_TEST=boxes`,
/// `--npc-test guards`): a guard up on the course's 2 m wall, facing away, the
/// player at its foot; then the takedown from below. `below_water` (with
/// `--npc-test guards`): the player swimming in the nearest deep water, a
/// guard standing just above the surface (as if on a dock).
#[allow(clippy::too_many_arguments)]
pub fn below_test(
    args: Res<crate::Args>,
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    mut state: Local<(u32, f32, usize)>,
    mut actions: ResMut<ActionState>,
    mut players: Query<(&mut Transform, &mut Player), Without<Npc>>,
    mut npcs: Query<(Entity, &mut Transform, &Health, &mut Npc), Without<Player>>,
    mut commands: Commands,
) {
    let Ok(mode) = std::env::var("FC3_TAKEDOWN_MODE") else { return };
    if !args.takedown_test || !mode.starts_with("below") {
        return;
    }
    let water = mode == "below_water";
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    if p.spawning {
        return;
    }
    let (step, start, shots) = &mut *state;
    let now = time.elapsed_secs();
    if *start == 0.0 {
        *start = now;
    }
    let t = now - *start;
    let fwd = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    match *step {
        0 if t > 4.0 && water => {
            let feet = pt.translation - Vec3::Y * crate::player::HALF_HEIGHT;
            let deep = (1..40)
                .flat_map(|r| (0..24).map(move |k| (r as f32 * 5.0, k as f32 * std::f32::consts::TAU / 24.0)))
                .map(|(r, a)| feet + Vec3::new(a.cos(), 0.0, a.sin()) * r)
                .find(|at| heights.height_at(*at).is_some_and(|h| h < SEA_LEVEL - 4.0));
            let Some(spot) = deep else {
                info!("takedown-test: no deep water near");
                *step = 3;
                return;
            };
            let Some((e, mut nt, _, mut npc)) = npcs.iter_mut().filter(|(_, _, _, n)| n.marker_id >= u64::MAX - 4).max_by_key(|(_, _, _, n)| n.marker_id)
            else {
                return;
            };
            let swim = Vec3::new(spot.x, SEA_LEVEL - 1.45 + crate::player::HALF_HEIGHT, spot.z);
            nt.translation = Vec3::new(spot.x, SEA_LEVEL + 0.3 + crate::player::HALF_HEIGHT, spot.z) + fwd * 1.0;
            npc.yaw = (-fwd.x).atan2(-fwd.z);
            nt.rotation = Quat::from_rotation_y(npc.yaw);
            commands.entity(e).insert(crate::npc::Frozen);
            pt.translation = swim;
            p.pitch = 0.4;
            info!("takedown-test: swimming at {spot:.0?}, a guard on the dock");
            *step = 1;
        }
        // The course is out by now: the wall's face is 15.5 m ahead.
        0 if t > 4.0 => {
            let feet = pt.translation - Vec3::Y * crate::player::HALF_HEIGHT;
            let Some((e, mut nt, _, mut npc)) = npcs.iter_mut().filter(|(_, _, _, n)| n.marker_id >= u64::MAX - 4).max_by_key(|(_, _, _, n)| n.marker_id)
            else {
                return;
            };
            nt.translation = feet + fwd * 15.65 + Vec3::Y * (2.0 + crate::player::HALF_HEIGHT);
            npc.yaw = (-fwd.x).atan2(-fwd.z);
            nt.rotation = Quat::from_rotation_y(npc.yaw);
            commands.entity(e).insert(crate::npc::Frozen);
            pt.translation = feet + fwd * 14.3 + Vec3::Y * crate::player::HALF_HEIGHT;
            p.pitch = 0.4;
            info!("takedown-test: a guard up on the wall");
            *step = 1;
        }
        1 if t > 5.5 => {
            info!("takedown-test: player at {:.1?} grounded {} swimming {}", pt.translation, p.grounded, p.swimming);
            actions.press(Action::Melee);
            *step = 2;
        }
        // FC3_TD_SHOT=0.5,1.2: screenshots that long after the strike.
        2 if t <= 8.5 => {
            let at: Vec<f32> = std::env::var("FC3_TD_SHOT").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
            if let (Some(&when), Some(path)) = (at.get(*shots), args.screenshot.as_ref()) {
                if t - 5.5 >= when {
                    let stem = path.strip_suffix(".png").unwrap_or(path);
                    crate::camera::capture(&mut commands, format!("{stem}_td{shots}.png"));
                    *shots += 1;
                }
            }
        }
        2 => {
            let dead: Vec<String> =
                npcs.iter().filter(|(_, _, _, n)| n.marker_id >= u64::MAX - 4).map(|(_, t, h, _)| format!("{:.1?} dead {}", t.translation, h.dead)).collect();
            info!("takedown-test: after {dead:?}, player at {:.1?}", pt.translation);
            *step = 3;
        }
        // Then a look at where he landed (the final screenshot).
        3 => {
            let eye = pt.translation + Vec3::Y * 0.7;
            if let Some((_, t, _, _)) = npcs.iter().find(|(_, _, h, n)| h.dead && n.marker_id >= u64::MAX - 4) {
                let to = t.translation - eye;
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
            }
        }
        _ => {}
    }
}
