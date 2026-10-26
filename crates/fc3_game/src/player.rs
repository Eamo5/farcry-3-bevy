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
/// Keep the same feet/reference origin for animation, stairs and save poses;
/// shorten only the top of the capsule when crouched.
fn stance_collider(crouched: bool) -> Collider {
    let bottom = -CAPSULE_LENGTH * 0.5;
    let top = if crouched { CROUCH_EYE_HEIGHT + 0.1 - HALF_HEIGHT - CAPSULE_RADIUS } else { CAPSULE_LENGTH * 0.5 };
    Collider::capsule_endpoints(CAPSULE_RADIUS, Vec3::Y * bottom, Vec3::Y * top)
}

fn standing_clearance(spatial: &SpatialQuery, centre: Vec3, filter: &SpatialQueryFilter) -> bool {
    // A small skin excludes the floor contact, while testing the full volume
    // that standing would occupy, including overhanging edges.
    let standing = Collider::capsule(CAPSULE_RADIUS - 0.01, CAPSULE_LENGTH);
    spatial.shape_intersections(&standing, centre, Quat::IDENTITY, filter).is_empty()
}

/// Whether a contact under the capsule is something to stand on: a walkable
/// face, or the lip of one (a step, a threshold), whose contact on the
/// capsule's rounded bottom reads as steep though the top past it is flat.
/// (A steep slope has no flat top just past the contact.)
fn stands_on(spatial: &SpatialQuery, point: Vec3, normal: Vec3, filter: &SpatialQueryFilter) -> bool {
    if normal.y > MIN_GROUND_NORMAL_Y {
        return true;
    }
    // (Only contacts on the bottom half: a lip, not a wall's face.)
    if normal.y < 0.1 {
        return false;
    }
    let inward = Vec3::new(-normal.x, 0.0, -normal.z).normalize_or_zero();
    let from = point + inward * EDGE_PROBE + Vec3::Y * EDGE_PROBE;
    spatial.cast_ray(from, Dir3::NEG_Y, EDGE_PROBE * 2.0, true, filter).is_some_and(|h| h.distance > 0.0 && h.normal.y > MIN_GROUND_NORMAL_Y)
}

/// How far past a lip, and above it, the top is looked for (m).
const EDGE_PROBE: f32 = 0.05;

#[cfg(test)]
mod stance_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn crouched_capsule_passes_low_roof_and_cannot_stand_inside_it() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin, PhysicsPlugins::default()))
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(20)));
        app.world_mut().spawn((RigidBody::Static, Collider::cuboid(20.0, 0.2, 20.0), Transform::from_xyz(0.0, -0.1, 0.0)));
        app.world_mut().spawn((RigidBody::Static, Collider::cuboid(2.0, 0.2, 3.0), Transform::from_xyz(2.5, 1.4, 0.0)));
        app.finish();
        app.cleanup();
        for _ in 0..3 { app.update(); }
        app.world_mut().run_system_once(|spatial: SpatialQuery| {
            let filter = SpatialQueryFilter::default();
            let start = Vec3::new(0.0, HALF_HEIGHT + 0.02, 0.0);
            let config = ShapeCastConfig::from_max_distance(5.0);
            assert!(spatial.cast_shape(&stance_collider(false), start, Quat::IDENTITY, Dir3::X, &config, &filter).is_some());
            assert!(spatial.cast_shape(&stance_collider(true), start, Quat::IDENTITY, Dir3::X, &config, &filter).is_none());
            assert!(standing_clearance(&spatial, start, &filter));
            assert!(!standing_clearance(&spatial, start + Vec3::X * 2.5, &filter));
            for crouched in [false, true] {
                let floor = spatial.cast_shape(&stance_collider(crouched), start, Quat::IDENTITY, Dir3::NEG_Y,
                    &ShapeCastConfig::from_max_distance(0.1), &filter).unwrap();
                assert!((floor.distance - 0.02).abs() < 0.002, "stance changed feet height: {}", floor.distance);
            }
        }).unwrap();
    }

    /// A capsule on a step's lip stands on it; on a steep ramp, it doesn't.
    #[test]
    fn a_lip_is_ground_and_a_steep_ramp_is_not() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin,
            PhysicsPlugins::default(),
        ))
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(20)));
        // A step 0.2 m high from x = 0, and well off to the side, a ramp at
        // 60 degrees.
        app.world_mut().spawn((RigidBody::Static, Collider::cuboid(2.0, 0.2, 2.0), Transform::from_xyz(1.0, 0.1, 0.0)));
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(4.0, 0.2, 2.0),
            Transform::from_xyz(0.0, 0.0, 10.0).with_rotation(Quat::from_rotation_z(60f32.to_radians())),
        ));
        app.finish();
        app.cleanup();
        for _ in 0..3 {
            app.update();
        }
        app.world_mut()
            .run_system_once(|spatial: SpatialQuery| {
                let filter = SpatialQueryFilter::default();
                let collider = stance_collider(false);
                let down = ShapeCastConfig::from_max_distance(5.0);
                // Most of the capsule's radius back off the lip: its bottom
                // meets the edge well round its curve.
                let on_lip = spatial.cast_shape(&collider, Vec3::new(-CAPSULE_RADIUS * 0.85, 2.0, 0.0), Quat::IDENTITY, Dir3::NEG_Y, &down, &filter).unwrap();
                let normal = -on_lip.normal2;
                assert!(normal.y < MIN_GROUND_NORMAL_Y, "the lip should read as steep: {normal:?}");
                assert!(stands_on(&spatial, on_lip.point1, normal, &filter));
                let on_ramp = spatial.cast_shape(&collider, Vec3::new(0.0, 4.0, 10.0), Quat::IDENTITY, Dir3::NEG_Y, &down, &filter).unwrap();
                assert!(!stands_on(&spatial, on_ramp.point1, -on_ramp.normal2, &filter));
            })
            .unwrap();
    }

    #[test]
    fn an_authored_ledge_is_climbed_from_in_front_within_reach() {
        use fc3_formats::traversal::Climb;
        // A ledge 2.5 m up along z at x = 1, its anchor a metre beyond.
        let ledge = Climb { mission_layer: 0, a: [1.0, 2.5, -1.0], b: [1.0, 2.5, 1.0], anchor: [2.0, 2.5, 0.0] };
        let standing = Vec3::new(0.4, HALF_HEIGHT, 0.3);
        let (edge, anchor) = authored_ledge([&ledge], standing, Vec3::X, (0.3, 3.0)).expect("in reach");
        assert!(edge.distance(Vec3::new(1.0, 2.5, 0.3)) < 1e-4 && anchor == Vec3::new(2.0, 2.5, 0.0), "{edge:?}");
        // Pressed to a face a little past the line, still.
        assert!(authored_ledge([&ledge], Vec3::new(1.1, HALF_HEIGHT, 0.3), Vec3::X, (0.3, 3.0)).is_some());
        // Not when facing away, from beyond it, too low a reach or too far.
        assert!(authored_ledge([&ledge], standing, Vec3::NEG_X, (0.3, 3.0)).is_none());
        assert!(authored_ledge([&ledge], Vec3::new(1.6, HALF_HEIGHT, 0.0), Vec3::NEG_X, (0.3, 3.0)).is_none());
        assert!(authored_ledge([&ledge], standing, Vec3::X, (0.3, 2.0)).is_none());
        assert!(authored_ledge([&ledge], Vec3::new(-1.0, HALF_HEIGHT, 0.0), Vec3::X, (0.3, 3.0)).is_none());
    }

    #[test]
    fn a_tall_rock_is_out_of_reach_standing_but_caught_in_a_jump() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin,
            PhysicsPlugins::default(),
        ));
        // A block 3.2 m tall from x = 1, its top at 3.2.
        app.world_mut().spawn((RigidBody::Static, Collider::cuboid(4.0, 3.2, 4.0), Transform::from_xyz(3.0, 1.6, 0.0)));
        app.finish();
        app.cleanup();
        for _ in 0..3 {
            app.update();
        }
        app.world_mut()
            .run_system_once(|spatial: SpatialQuery| {
                let filter = SpatialQueryFilter::default();
                let standing = Vec3::new(0.5, HALF_HEIGHT, 0.0);
                // Standing, made-up reaches: 0.3 to 1.8 m over the feet.
                assert!(find_ledge(&spatial, &filter, standing, Vec3::X, (0.3, 1.8)).is_none());
                // A metre up in a jump, a reach round the head finds its top.
                let head = 2.0 * HALF_HEIGHT;
                let (top, h, vault) = find_ledge(&spatial, &filter, standing + Vec3::Y, Vec3::X, (head - 2.0, head + 0.5)).expect("caught");
                assert!((top.y - 3.2).abs() < 0.05 && (h - 2.2).abs() < 0.05 && !vault, "{top:?} {h}");
            })
            .unwrap();
    }
}
/// The slide: eye height, the sprint speed it needs, the push it gives,
/// how fast it slows (per second) and its longest run.
const SLIDE_EYE_HEIGHT: f32 = 0.75;
/// How far above the ground (or the sea) a place a teleport keeps the height
/// of must be to leave the player in the air there.
const AIRBORNE_SPAWN: f32 = 40.0;
const SLIDE_MIN_SPEED: f32 = 5.5;
const SLIDE_BOOST: f32 = 2.0;
const SLIDE_FRICTION: f32 = 1.1;
const SLIDE_TIME: f32 = 1.1;
/// Walking while dragging a body.
const DRAG_SPEED: f32 = 1.5;
const WALK_SPEED: f32 = 4.2;
const SPRINT_SPEED: f32 = 7.5;
const CROUCH_SPEED: f32 = 2.0;
const CROUCH_RUN_SPEED: f32 = 3.4;
const SWIM_SPEED: f32 = 3.2;
const JUMP_SPEED: f32 = 5.0;
const GRAVITY: f32 = 19.0;
const GROUND_ACCEL: f32 = 60.0;
const AIR_ACCEL: f32 = 6.0;
const STEP_HEIGHT: f32 = 0.45;
/// Steepest walkable slope (cosine of the angle from vertical).
pub(crate) const MIN_GROUND_NORMAL_Y: f32 = 0.64;

pub struct PlayerPlugin;

/// Startup set that spawns the player and camera; systems that decorate the
/// player (health, inventory, view model) run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlayerSpawn;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputCaptured>()
            .init_resource::<SceneHeadLook>()
            .init_resource::<ViewLimits>()
            .init_resource::<PlayerBody>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_body)
            .add_systems(Startup, spawn_player.in_set(PlayerSpawn))
            .add_systems(Update, (player_look, place_on_ground, climb_course, player_move, scripted_walk_log, footsteps).chain())
            .add_systems(PreUpdate, slide_test.after(crate::controls::update_actions))
            .add_systems(Update, teleport_test.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// How the player moves by the game's data (`fc3_formats::body`): its
/// walking, crouching, sprinting and swimming speeds, jump and gravity; the
/// tuned constants stand in without it.
#[derive(Resource)]
pub struct PlayerBody(pub fc3_formats::body::Body);

impl Default for PlayerBody {
    fn default() -> Self {
        PlayerBody(fc3_formats::body::Body {
            walk: WALK_SPEED,
            crouch: CROUCH_SPEED,
            sprint: SPRINT_SPEED,
            crouch_sprint: CROUCH_RUN_SPEED,
            // (No stamina: the sprint keeps its speed.)
            sprint_tired: SPRINT_SPEED,
            crouch_sprint_tired: CROUCH_RUN_SPEED,
            stamina: 0.0,
            stamina_drain: 0.0,
            stamina_regen: 0.0,
            stamina_fast: 0.0,
            swim: SWIM_SPEED,
            jump_height: JUMP_SPEED * JUMP_SPEED / (2.0 * GRAVITY),
            gravity: GRAVITY,
            fall: None,
            breath_hold: None,
            oxygen: None,
            swim_sprint: 1.0,
            climb: None,
        })
    }
}

fn load_body(info: Option<Res<crate::data::CacheInfo>>, mut body: ResMut<PlayerBody>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::body::BODY_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::body::from_text(&t)) {
        Ok(b) if b.walk > 0.0 && b.gravity > 0.0 => {
            info!("player body: walk {} m/s, sprint {} m/s, jump {} m", b.walk, b.sprint, b.jump_height);
            body.0 = b;
        }
        Ok(_) => warn!("player body: an empty table; re-run the converter"),
        Err(e) => warn!("player body: no table ({e:#}); re-run the converter"),
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
    /// The spawn keeps to the height it was given (a story teleport into a
    /// cave lands on the floor below, not on the rock above).
    pub keep_height: bool,
    /// Downward speed at the moment of the last landing (for fall damage).
    pub last_landing_speed: f32,
    pub sprinting: bool,
    /// Time the player last landed after (re)spawning; enemies hold off briefly after.
    pub spawned_at: f32,
    /// In a vehicle (positioned by the vehicle, not by walking).
    pub driving: bool,
    /// Riding as a passenger, weapon in hand (not at the wheel or a mounted
    /// gun): he aims and fires from the seat.
    pub armed_passenger: bool,
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

/// A scripted gameplay's hold on the view (a sniping challenge's), if one
/// has it.
#[derive(Resource, Default)]
pub struct ViewLimits(pub Option<ViewArc>);

/// How far the view may turn from a centre (radians): either side (left
/// positive) and down and up.
#[derive(Clone, Copy, Debug)]
pub struct ViewArc {
    pub yaw: f32,
    pub pitch: f32,
    pub yaw_range: [f32; 2],
    pub pitch_range: [f32; 2],
}

impl ViewArc {
    pub fn clamp(&self, yaw: f32, pitch: f32) -> (f32, f32) {
        let off = (yaw - self.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        (self.yaw + off.clamp(self.yaw_range[0], self.yaw_range[1]), pitch.clamp(self.pitch + self.pitch_range[0], self.pitch + self.pitch_range[1]))
    }
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
/// How high over the feet a ledge can be climbed standing, without the
/// game's figures.
const GROUND_CLIMB: (f32, f32) = (0.5, 2.3);

/// A ledge ahead whose top is `reach` (lowest, highest) metres over the feet:
/// its top, its height and whether to vault it.
fn find_ledge(sq: &SpatialQuery, filter: &SpatialQueryFilter, pos: Vec3, fwd: Vec3, reach: (f32, f32)) -> Option<(Vec3, f32, bool)> {
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
    // Its top, just beyond the face (thin fences have tops too), or a little
    // further in past a rounded lip (a boulder's).
    let (top, h) = [0.15, 0.45, 0.8].into_iter().find_map(|beyond| {
        let probe = feet + fwd * (face + beyond) + Vec3::Y * (reach.1 + 0.3);
        let Some(down) = sq.cast_ray(probe, Dir3::NEG_Y, reach.1 + 0.3 - reach.0.min(0.0), true, filter) else {
            debug!("ledge: face at {face:.2} but nothing below {probe:.1?}");
            return None;
        };
        let top = probe - Vec3::Y * down.distance;
        let h = top.y - feet.y;
        if down.normal.y < 0.7 || !(reach.0..=reach.1).contains(&h) {
            debug!("ledge: face at {face:.2}, top {beyond} m in {h:.2} m up, normal {:.2}", down.normal.y);
            return None;
        }
        Some((top, h))
    })?;
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

/// The nearest authored ledge (`traversal::Climb`) ahead of a player
/// (capsule centre `pos`, facing `fwd`), `reach` (lowest, highest) metres
/// over the feet, faced from the side away from its anchor: the point on the
/// ledge, and the anchor the climb ends at. (A line can sit on a rock's lip
/// a little outside its collision, so a climber pressed to the face may
/// stand a little past it.)
fn authored_ledge<'a>(climbs: impl IntoIterator<Item = &'a fc3_formats::traversal::Climb>, pos: Vec3, fwd: Vec3, reach: (f32, f32)) -> Option<(Vec3, Vec3)> {
    let feet = pos.y - HALF_HEIGHT;
    climbs
        .into_iter()
        .filter_map(|c| {
            let (a, b, anchor) = (Vec3::from(c.a), Vec3::from(c.b), Vec3::from(c.anchor));
            let ab = (b - a).with_y(0.0);
            let s = if ab.length_squared() > 1e-6 { ((pos - a).with_y(0.0).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            let edge = a.lerp(b, s);
            // Across the line towards the anchor; the climber's distance
            // short of it (negative) or past it, and to one side of its ends.
            let mut across = Vec3::new(-ab.z, 0.0, ab.x).normalize_or((anchor - edge).with_y(0.0).normalize_or(fwd));
            if across.dot(anchor - edge) < 0.0 {
                across = -across;
            }
            let off = (pos - edge).with_y(0.0);
            let short = across.dot(off);
            let aside = (off - across * short).length();
            let near = short > -(CAPSULE_RADIUS + 0.8) && short < CAPSULE_RADIUS && aside < 0.3 && fwd.dot(across) > 0.3;
            (near && (reach.0..=reach.1).contains(&(edge.y - feet))).then_some((short.abs() + aside, edge, anchor))
        })
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map(|(_, edge, anchor)| (edge, anchor))
}

/// The wingsuit or parachute in flight, by the game's figures: the suit
/// flies as its hang glider cousin does (`vehicles::Glide`), the movement
/// keys banking it and pitching its nose down (it has no nose-up: pulling
/// back brakes); the canopy drifts on its heading at its own speeds and
/// sink. The view turns with either, within its look limits.
#[derive(Default)]
pub(crate) struct Flight {
    glide: Option<crate::vehicles::Glide>,
    mode: AirMode,
    /// How long the ground has been closer than the auto-parachute's time.
    close: f32,
}

impl Flight {
    /// Flies a frame; false when there is nothing for it to fly (on foot,
    /// in free fall, or without the game's figures).
    fn fly(&mut self, p: &mut Player, h: &fc3_formats::handling::Handling, axis: Vec2, clearance: f32, chute_owned: bool, dt: f32) -> bool {
        let (Some(g), Some(c)) = (h.glider, h.chute) else { return false };
        if p.air == AirMode::None {
            *self = Flight::default();
            return false;
        }
        let fresh = self.mode != p.air;
        self.mode = p.air;
        let horizontal = Vec2::new(p.velocity.x, p.velocity.z).length();
        // Into the suit as it falls: its nose down the fall, at the fall's
        // speed (or its cruising speed).
        let glide = self.glide.get_or_insert_with(|| {
            crate::vehicles::Glide::launch(p.yaw, p.velocity.y.atan2(horizontal.max(0.1)).clamp(-g.max_pitch, 0.0), p.velocity.length().max(g.base_speed))
        });
        if fresh {
            glide.flown = 0.0;
        }
        let before = glide.yaw;
        // The stick: left positive, nose down positive.
        let stick = Vec2::new(-axis.x, axis.y);
        match p.air {
            AirMode::Wingsuit => {
                let nose = if c.pitch_up { stick.y } else { stick.y.max(0.0) };
                let brake = !c.pitch_up && stick.y < -0.1;
                let sink = glide.step(&g, Vec2::new(stick.x, nose), brake, clearance, 1.0, dt);
                p.velocity = glide.velocity(sink);
                // The canopy opens by itself just short of the ground.
                let eta = clearance / (-p.velocity.y).max(0.1);
                self.close = if eta < c.auto_eta { self.close + dt } else { 0.0 };
                if chute_owned && self.close > c.auto_hold {
                    p.air = AirMode::Chute;
                }
            }
            AirMode::Chute => {
                glide.flown += dt;
                glide.roll += (stick.x * c.max_roll - glide.roll) * (c.side_rate * dt).min(1.0);
                glide.turn = glide.roll * g.roll_to_turn.abs();
                glide.yaw += glide.turn * dt;
                let ahead = (c.base_speed + stick.y * (c.max_speed - c.base_speed)).clamp(0.0, c.max_speed);
                glide.speed += (ahead - glide.speed) * (c.front_rate * dt).min(1.0);
                let heading = Quat::from_rotation_y(glide.yaw) * Vec3::NEG_Z * glide.speed;
                let flat = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
                let flat = flat + (heading - flat) * (c.front_rate * dt).min(1.0);
                // Opening, its pull brakes the fall (its impulse over its
                // time, on the suit's mass); then it settles to its sink.
                let mut vy = p.velocity.y + (c.sink - p.velocity.y) * (dt * 2.0).min(1.0);
                if glide.flown < c.deploy_time && p.velocity.y < c.sink && h.mass > 0.0 {
                    let pull = c.deploy_impulse / h.mass / c.deploy_time.max(0.01);
                    vy = (p.velocity.y + pull * dt).min(c.sink).max(vy);
                }
                p.velocity = Vec3::new(flat.x, vy, flat.z);
            }
            AirMode::None => {}
        }
        // The view turns with it, and looks about only so far.
        p.yaw += glide.yaw - before;
        let [pitch_lo, pitch_hi, yaw_lo, yaw_hi] = h.look;
        if pitch_hi > pitch_lo {
            p.pitch = p.pitch.clamp((-pitch_hi.to_radians()).max(-1.5), (-pitch_lo.to_radians()).min(1.5));
        }
        if yaw_hi > yaw_lo {
            let ahead = (p.yaw - glide.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            p.yaw = glide.yaw + ahead.clamp(-yaw_hi.to_radians(), -yaw_lo.to_radians());
        }
        true
    }
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

/// The player's head turned within a first-person scene (radians), and
/// seconds since he last turned it.
#[derive(Resource, Default)]
pub struct SceneHeadLook {
    pub yaw: f32,
    pub pitch: f32,
    idle: f32,
}

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
                keep_height: false,
                last_landing_speed: 0.0,
                sprinting: false,
                spawned_at: 0.0,
                driving: false,
                armed_passenger: false,
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

pub(crate) fn player_look(
    actions: Res<crate::controls::ActionState>,
    state: Res<State<crate::menu::GameState>>,
    cinematic: Res<crate::fp::Cinematic>,
    sway: Res<crate::weapons::ScopeSway>,
    wheel: Res<crate::weapon_wheel::WeaponWheel>,
    shake: Res<crate::shake::ViewShake>,
    mut player: Query<(&mut Player, &mut Transform, Option<&crate::weapons::Inventory>), Without<PlayerCamera>>,
    mut camera: Query<&mut Transform, With<PlayerCamera>>,
    (time, captured, mut next_log): (Res<Time>, Res<InputCaptured>, Local<f32>),
    (cinema, fp, mut head): (Res<crate::cinema::Cinema>, Res<crate::fp::FpState>, ResMut<SceneHeadLook>),
    assist: Option<Res<crate::aim_assist::AimAssist>>,
    mut stuck: Local<LookWatch>,
    limits: Res<ViewLimits>,
    drunk: Res<crate::intoxication::Intoxication>,
) {
    // A first-person scene lets the head turn within its clip's limits,
    // turning back once left alone.
    let in_view = cinema.view.is_some() && *state.get() == crate::menu::GameState::Playing;
    match (cinema.free_aim.filter(|_| in_view), cinema.head_look.filter(|_| in_view)) {
        // (Aiming from a scene's view, the head turns as far as the gameplay
        // lets it, staying where it is put.)
        (Some(reach), _) => {
            let look = actions.look * aimed_look(player.single().ok().and_then(|(_, _, inv)| inv), actions.gamepad_active);
            head.yaw = (head.yaw - look.x).clamp(-reach.x, reach.x);
            head.pitch = (head.pitch + look.y).clamp(-reach.y, reach.y);
        }
        (None, Some(h)) => {
            let dt = time.delta_secs();
            let turn = actions.look * h.speed.max(0.1) * 0.5;
            if turn.length_squared() > 0.0 {
                head.idle = 0.0;
                head.yaw = (head.yaw - turn.x).clamp(h.min[2].to_radians(), h.max[2].to_radians());
                head.pitch = (head.pitch + turn.y).clamp(h.min[0].to_radians(), h.max[0].to_radians());
            } else {
                head.idle += dt;
                if head.idle > h.return_delay {
                    let back = (dt * h.return_speed.max(0.1)).min(1.0);
                    head.yaw *= 1.0 - back;
                    head.pitch *= 1.0 - back;
                }
            }
        }
        (None, None) => *head = SceneHeadLook::default(),
    }
    let Ok((mut p, mut t, inv)) = player.single_mut() else { return };
    // (Where the view was left last frame, after anything else had its say.)
    let (yaw0, pitch0) = (p.yaw, p.pitch);
    // FC3_LOOK_DEBUG: what holds the view, twice a second.
    if std::env::var_os("FC3_LOOK_DEBUG").is_some() && time.elapsed_secs() >= *next_log {
        *next_log = time.elapsed_secs() + 0.5;
        info!(
            "look: state {:?} captured {} look {:.4?} pad {} takedown {:.2} wheel {} yaw {:.3} pitch {:.3} mounted {} cutscene {} scene_view {} fp_takedown {} head {:.2} {:.2} limits {}",
            state.get(),
            captured.0,
            actions.look,
            actions.gamepad_active,
            cinematic.0,
            wheel.open,
            p.yaw,
            p.pitch,
            p.mounted.is_some(),
            cinema.cutscene(),
            cinema.view.is_some(),
            fp.in_takedown(),
            head.yaw,
            head.pitch,
            cinema.head_look.is_some()
        );
    }
    // A takedown or a scene's camera holds the view, and the weapon wheel
    // takes the look input. A scene playing without a camera of its own
    // (the player's own view, as when following Grant out of the camp)
    // leaves the view the player's to turn.
    let own_view_in_scene = cinema.cutscene() && cinema.view.is_none() && !fp.in_takedown();
    let held = cinematic.0 > 0.0 && !own_view_in_scene;
    if *state.get() == crate::menu::GameState::Playing && !held && !wheel.open {
        let mut look = actions.look * aimed_look(inv, actions.gamepad_active);
        // (A pad's turn eased over a near target.)
        if actions.gamepad_active {
            if let Some(a) = assist.as_ref() {
                look = a.ease_stick(look);
            }
        }
        p.yaw -= look.x;
        p.pitch = (p.pitch + look.y).clamp(-1.5, 1.5);
    }
    if let Some(arcs) = p.mounted {
        (p.yaw, p.pitch) = arcs.clamp(p.yaw, p.pitch);
    }
    if let Some(arc) = limits.0 {
        (p.yaw, p.pitch) = arc.clamp(p.yaw, p.pitch);
    }
    // The mouse moving for a while in play with the view not turning (no
    // scene camera holding it): say once what holds it.
    let now = time.elapsed_secs();
    // (At the wheel the head stops at the vehicle's limits. Only a captured
    // mouse or a pad counts: test automation turns the view itself.)
    let input = actions.look.length() > 1e-4
        && (captured.0 || actions.gamepad_active)
        && *state.get() == crate::menu::GameState::Playing
        && cinema.view.is_none()
        && !p.driving;
    if stuck.note(now, input, yaw0, pitch0) {
        warn!(
                "look: the mouse is moving but the view isn't turning (held {held}: scene hold {:.2}, cutscene {}, takedown {}; weapon wheel {}; on a gun {}; input captured {}; pad {})",
                cinematic.0,
                cinema.cutscene(),
                fp.in_takedown(),
                wheel.open,
                p.mounted.is_some(),
                captured.0,
                actions.gamepad_active
            );
    }
    t.rotation = Quat::from_rotation_y(p.yaw);
    if let Ok(mut ct) = camera.single_mut() {
        // A scope drifting with the breath, the view shaken, and swaying
        // drunk (`intoxication`).
        ct.rotation = Quat::from_rotation_y(sway.offset.x + shake.yaw + drunk.offset.x)
            * Quat::from_rotation_x(p.pitch + sway.offset.y + shake.pitch + drunk.offset.y)
            * Quat::from_rotation_z(shake.roll + drunk.roll);
        ct.translation.y = p.eye - HALF_HEIGHT;
    }
}

/// Watching the mouse against the view (see `player_look`): since when the
/// view has held still under it, from where, and whether that was said.
#[derive(Default)]
pub(crate) struct LookWatch {
    since: f32,
    yaw: f32,
    pitch: f32,
    last_input: f32,
    warned: bool,
}

impl LookWatch {
    /// Notes a frame: whether the mouse (or stick) is turning, and where the
    /// view stood as the frame began. True once, when the view has held still
    /// under a turning mouse for two seconds (half a second without input
    /// starts the watch afresh).
    fn note(&mut self, now: f32, input: bool, yaw: f32, pitch: f32) -> bool {
        if !input {
            return false;
        }
        let fresh = now - self.last_input > 0.5 || (yaw - self.yaw).abs() + (pitch - self.pitch).abs() > 0.01;
        self.last_input = now;
        if fresh {
            *self = LookWatch { since: now, yaw, pitch, last_input: now, warned: false };
            return false;
        }
        if now - self.since > 2.0 && !self.warned {
            self.warned = true;
            return true;
        }
        false
    }
}

/// How much slower the view turns down the sights (across, and up and
/// down): the weapon's own aimed sensitivity, a pad's or the mouse's, as far
/// as the sights are up.
fn aimed_look(inv: Option<&crate::weapons::Inventory>, pad: bool) -> Vec2 {
    let Some((a, blend)) = inv.filter(|i| !i.slots.is_empty()).and_then(|i| i.def().aim.map(|a| (a, i.aim_blend))) else { return Vec2::ONE };
    let across = 1.0 + ((if pad { a.look_pad } else { a.look_mouse }) - 1.0) * blend;
    Vec2::new(across, across * (1.0 + (a.look_vertical - 1.0) * blend))
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
        let volume = if p.crouching {
            0.15
        } else if p.sprinting {
            0.5
        } else {
            0.3
        };
        let surface = surface();
        let gait = if p.crouching {
            2
        } else if p.sprinting {
            3
        } else {
            1
        };
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
    objects: Query<(&crate::objects::WorldObject, &ChildOf, Has<crate::physics::ObjectCollisionReady>)>,
    object_roots: Query<&crate::objects::SectorObjects>,
    streamer: Res<crate::objects::ObjectStreamer>,
    mut collision_ready: Local<bool>,
    server: Res<AssetServer>,
    args: Res<Args>,
    vegetation: Res<crate::vegetation::VegStreamer>,
) {
    let Ok((entity, mut p, mut t)) = player.single_mut() else { return };
    if !p.spawning {
        *collision_ready = false;
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
    // Kept to its height: the surface just below the point given.
    if p.keep_height {
        // Terrain commonly arrives before the building containing the save.
        // Grounding at that point puts the player underneath a late floor.
        // (So does the vegetation's rock it stands on: WIL_050's rifle is on
        // one above the ground.)
        let rocks = vegetation.solid_root(here);
        let ready = streamer.centre() == Some((here, true))
            && rocks.is_ok()
            && objects.iter().all(|(_, parent, ready)| {
                ready || (object_roots.get(parent.parent()).map_or(true, |root| root.coord != here) && rocks != Ok(Some(parent.parent())))
            });
        if !ready {
            *collision_ready = false;
            return;
        }
        // Let the physics spatial index observe newly inserted colliders.
        if !*collision_ready {
            *collision_ready = true;
            return;
        }
        let filter = SpatialQueryFilter::from_excluded_entities([entity]);
        // Probe near the requested feet, not above the head: an overhead
        // doorway/roof is not the floor of an indoor checkpoint.
        let origin = saved_height_probe(t.translation);
        if let Some(h) = spatial.cast_ray(origin, Dir3::NEG_Y, 10.0, true, &filter) {
            t.translation.y = origin.y - h.distance + HALF_HEIGHT + 0.05;
            p.velocity = Vec3::ZERO;
            p.spawning = false;
            p.keep_height = false;
            p.spawned_at = time.elapsed_secs();
            info!("player placed at its height at {:?}", t.translation);
            return;
        }
        p.keep_height = false;
        // High in the air with nothing under it (JAS_170's drop, 300 m over
        // the sea): left there to fall, as the original leaves him to open
        // the wingsuit, not set down on the seabed.
        if t.translation.y - HALF_HEIGHT > ground.max(SEA_LEVEL) + AIRBORNE_SPAWN {
            p.velocity = Vec3::ZERO;
            p.spawning = false;
            p.spawned_at = time.elapsed_secs();
            info!("player left in the air at {:?}", t.translation);
            return;
        }
    }
    // Land on the highest surface below a point well above the terrain (roofs, rocks...).
    let top = Vec3::new(t.translation.x, ground.max(SEA_LEVEL) + 60.0, t.translation.z);
    // Exclude the player's own capsule from the probe.
    let hit = spatial.cast_ray(top, Dir3::NEG_Y, 200.0, true, &SpatialQueryFilter::from_excluded_entities([entity]));
    if let Some(h) = &hit {
        let what = objects.get(h.entity).ok().and_then(|(o, _, _)| server.get_path(&o.model)).map(|p| p.to_string()).unwrap_or_else(|| "terrain".into());
        info!("spawn ray hit {what} at y={:.2} (terrain {ground:.2})", top.y - h.distance);
    }
    let surface = hit.map(|h| top.y - h.distance).unwrap_or(ground).max(ground);
    t.translation.y = surface + HALF_HEIGHT + 0.05;
    p.velocity = Vec3::ZERO;
    p.spawning = false;
    p.spawned_at = time.elapsed_secs();
    info!("player placed on ground at {:?}", t.translation);
}

fn saved_height_probe(centre: Vec3) -> Vec3 {
    centre - Vec3::Y * (HALF_HEIGHT - 0.2)
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    #[test]
    fn indoor_floor_probe_does_not_select_the_roof() {
        // Two real collision slabs reproduce the indoor/roof ambiguity seen
        // at the opening mission's locked-house doorway.
        let slab = Collider::cuboid(8.0, 0.2, 8.0);
        let floor = Vec3::new(0.0, 192.5, 0.0);
        let roof = Vec3::new(0.0, 195.7, 0.0);
        for centre_y in [193.4, 194.735] {
            let origin = saved_height_probe(Vec3::new(0.0, centre_y, 0.0));
            assert!(slab.cast_ray(roof, Quat::IDENTITY, origin, Vec3::NEG_Y, 10.0, true).is_none());
            let (distance, _) = slab.cast_ray(floor, Quat::IDENTITY, origin, Vec3::NEG_Y, 10.0, true).unwrap();
            assert!((origin.y - distance - 192.6).abs() < 0.001);
        }
        // A save actually made on the roof must still land on that roof.
        let origin = saved_height_probe(Vec3::new(0.0, 195.8 + HALF_HEIGHT, 0.0));
        let (distance, _) = slab.cast_ray(roof, Quat::IDENTITY, origin, Vec3::NEG_Y, 10.0, true).unwrap();
        assert!((origin.y - distance - 195.8).abs() < 0.001);
    }
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

pub(crate) fn player_move(
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
    mut player: Query<(Entity, &mut Player, &mut Transform, &Collider, Option<&crate::weapons::Inventory>)>,
    progress: Res<crate::progression::Progress>,
    mut actions_out: MessageWriter<crate::fp::FpAction>,
    sway: Res<crate::weapons::ScopeSway>,
    (scripted, collision_ids, mut next_collision_log, mut commands, body, mut stamina, mut flight, ways, layers, drunk): (
        Res<crate::story::ScriptedPlayer>,
        Query<(Option<&crate::objects::PlacedId>, Option<&Name>, Option<&ChildOf>)>,
        Local<f32>,
        Commands,
        Res<PlayerBody>,
        Local<Option<f32>>,
        Local<Flight>,
        Res<crate::traversal::Ways>,
        Res<crate::mission_layers::MissionLayers>,
        Res<crate::intoxication::Intoxication>,
    ),
) {
    let body = body.0;
    let Ok((entity, mut p, mut t, collider, inv)) = player.single_mut() else { return };
    let mut collider = collider.clone();
    // Down the sights, the weapon's slower pace on foot.
    let aimed_pace = inv.filter(|i| !i.slots.is_empty()).and_then(|i| i.def().aim.map(|a| 1.0 + (a.move_speed - 1.0) * i.aim_blend)).unwrap_or(1.0);
    let dt = time.delta_secs().min(0.05);
    // FC3_PLAYER_TRACE (tests): where the player is and how he's held,
    // twice a second.
    static TRACE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("FC3_PLAYER_TRACE").is_some());
    let now = time.elapsed_secs();
    if *TRACE && (now * 2.0) as u32 != ((now - time.delta_secs()) * 2.0) as u32 {
        info!(
            "player trace: {:.1?} spawning {} noclip {} grounded {} swimming {} keep height {} velocity {:.1?}",
            crate::data::to_dunia(t.translation),
            p.spawning,
            p.noclip,
            p.grounded,
            p.swimming,
            p.keep_height,
            p.velocity
        );
    }
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
    // Signals a mission ignores (no jumping or sprinting through a scene).
    let ignored = |a: Action| match a {
        Action::Jump => scripted.ignored.contains("jump"),
        Action::Sprint => scripted.ignored.contains("sprint") || scripted.ignored.contains("lock_sprint"),
        _ => false,
    };
    let key = |a: Action| {
        !ignored(a)
            && ((input_ok && actions.pressed(a)) || (a == Action::Forward && s_fwd) || (a == Action::Sprint && s_sprint) || (a == Action::Jump && s_jump))
    };
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let right = Vec3::new(forward.z * -1.0, 0.0, forward.x);
    // Analogue on a gamepad; scripted runs walk forward.
    let mut axis = if input_ok { actions.movement } else { Vec2::ZERO };
    if s_fwd {
        axis.y = 1.0;
    }
    // (Drunk, the walk wanders off where it is steered.)
    let wish = Quat::from_rotation_y(drunk.drift) * (forward * axis.y + right * axis.x).clamp_length_max(1.0) * scripted.movement;

    if p.noclip {
        let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch) * Vec3::NEG_Z;
        let mut v = (look * wish.dot(forward) + right * wish.dot(right)).normalize_or_zero();
        // (Up and down by the keys themselves, not the actions: a script's
        // held crouch, BAM_020's on its ship, sank a player the autopilot
        // held up at a trigger through the deck and the world below.)
        if input_ok && (keys.pressed(KeyCode::Space) || keys.pressed(KeyCode::KeyE)) {
            v += Vec3::Y;
        }
        if input_ok && (keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::KeyQ)) {
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
    let head = 2.0 * HALF_HEIGHT;
    let climbs = || {
        ways.data.climbs.iter().filter(|c| c.mission_layer == fc3_formats::world::DEFAULT_MISSION_LAYER || layers.present.contains(&c.mission_layer))
    };
    // A ledge authored to be climbed: up onto it and over to its anchor,
    // standing (with the jump the climb starts with) or caught in the air.
    let authored = |reach: (f32, f32)| authored_ledge(climbs(), t.translation, forward, reach);
    let jump_pressed = input_ok && actions.just_pressed(Action::Jump);
    let ledge = if jump_pressed && p.grounded && !p.swimming && p.air == AirMode::None {
        let low = body.climb.map_or(GROUND_CLIMB.0, |c| c.ground[0].max(0.3));
        authored((low, head + body.climb.map_or(0.5, |c| c.jump[1]) + body.jump_height))
    } else if !p.grounded && p.velocity.y.abs() > 0.5 && p.air == AirMode::None && !p.swimming && axis.y > 0.3 {
        body.climb.and_then(|c| authored(((head + c.jump[0]).max(GROUND_CLIMB.0), head + c.jump[1])))
    } else {
        None
    };
    if let Some((edge, anchor)) = ledge {
        let filter = SpatialQueryFilter::from_excluded_entities([entity]);
        let from = t.translation;
        let toward = (edge - from).with_y(0.0).normalize_or(forward);
        let peak = Vec3::new(from.x, edge.y + HALF_HEIGHT + 0.1, from.z) + toward * 0.15;
        // On whatever is under the anchor, looked for from over the ledge
        // (some anchors sit inside the rock, below its top).
        let over = anchor.with_y(anchor.y.max(edge.y) + 1.0);
        let stand = move_and_slide
            .spatial_query
            .cast_ray(over, Dir3::NEG_Y, over.y - anchor.y + 1.5, true, &filter)
            .filter(|g| g.normal.y > 0.5)
            .map_or(anchor, |g| over - Vec3::Y * g.distance);
        let to = stand + Vec3::Y * (HALF_HEIGHT + 0.02);
        let h = edge.y - (from.y - HALF_HEIGHT);
        let dur = 0.45 + h.max(0.0) * 0.3;
        p.mantle = Some(Mantle { from, peak, to, t: 0.0, dur });
        info!("climb: the authored ledge {h:.2} m over the feet, from {from:.1?} to {to:.1?}");
        actions_out.write(crate::fp::FpAction::Climb { high: h > 1.3, secs: dur });
        return;
    }
    if (jump_pressed || climb_test) && !p.swimming && p.air == AirMode::None && (p.grounded || p.velocity.y.abs() < 3.0) {
        let filter = SpatialQueryFilter::from_excluded_entities([entity]);
        // (The player's own reach standing: `fGroundClimbMin/MaxHeight`.)
        let reach = body.climb.map_or(GROUND_CLIMB, |c| (c.ground[0].max(0.3), c.ground[1]));
        if let Some((top, h, vault)) = find_ledge(&move_and_slide.spatial_query, &filter, t.translation, forward, reach) {
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
    // Jumping or falling, pushing on, a high ledge within reach of the head:
    // caught and climbed (the game's jump climb). It has to stand
    // `jump_above_ground` over the ground below, from `jump[0]` to `jump[1]`
    // metres off the head, and (ours) over the feet.
    if let Some(c) = body.climb.filter(|c| c.jump_above_ground > 0.0) {
        if !p.grounded && p.velocity.y.abs() > 0.5 && p.mantle.is_none() && p.air == AirMode::None && !p.swimming && axis.y > 0.3 {
            let filter = SpatialQueryFilter::from_excluded_entities([entity]);
            let reach = ((head + c.jump[0]).max(GROUND_CLIMB.0), head + c.jump[1]);
            let ground = move_and_slide.spatial_query.cast_ray(t.translation, Dir3::NEG_Y, 50.0, true, &filter).map(|g| t.translation.y - g.distance);
            if let Some((top, h, _)) = find_ledge(&move_and_slide.spatial_query, &filter, t.translation, forward, reach)
                .filter(|(top, _, _)| ground.is_none_or(|g| top.y - g >= c.jump_above_ground))
            {
                let from = t.translation;
                let peak = Vec3::new(from.x, top.y + HALF_HEIGHT + 0.1, from.z) + forward * 0.15;
                let to = top + forward * 0.45 + Vec3::Y * (HALF_HEIGHT + 0.02);
                let dur = 0.45 + h.max(0.0) * 0.3;
                p.mantle = Some(Mantle { from, peak, to, t: 0.0, dur });
                info!("climb: caught a ledge {h:.2} m over the feet in the air, from {from:.1?} to {to:.1?}");
                actions_out.write(crate::fp::FpAction::Climb { high: true, secs: dur });
                return;
            }
        }
    }
    let feet = t.translation.y - HALF_HEIGHT;
    let water = crate::water::surface_at(t.translation, &heights);
    p.swimming = crate::water::submerged(t.translation.with_y(feet), &heights, 1.3);
    // Crouching at a sprint: a slide, low and fast, slowing to a crouch.
    let flat_speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    // (Once `SP_Slide` is learned.)
    if p.sliding.is_none() && perks.slide && p.sprinting && p.grounded && flat_speed > SLIDE_MIN_SPEED && input_ok && actions.just_pressed(Action::Crouch) {
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
    let requested_crouch = (key(Action::Crouch) || p.sliding.is_some()) && !p.swimming && !p.dragging;
    let crouching = requested_crouch || (p.crouching && !standing_clearance(
        &move_and_slide.spatial_query, t.translation, &SpatialQueryFilter::from_excluded_entities([entity])));
    if p.crouching != crouching {
        collider = stance_collider(crouching);
        commands.entity(entity).insert(collider.clone());
        p.crouching = crouching;
    }
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

    // Stamina: sprinting drains it, anything else restores it; the fast
    // sprint lasts while there is enough (`SP_LongerSprintBurst`: always).
    let left = stamina.get_or_insert(body.stamina);
    if p.sprinting && !perks.endless_sprint {
        *left = (*left - body.stamina_drain * dt).max(0.0);
    } else {
        *left = (*left + body.stamina_regen * dt).min(body.stamina);
    }
    let fast = perks.endless_sprint || *left >= body.stamina_fast;
    let speed = if p.dragging {
        DRAG_SPEED
    } else if p.swimming {
        // (The sprint key swims faster, as the game has it.)
        let stroke = if key(Action::Sprint) { body.swim_sprint.max(1.0) } else { 1.0 };
        body.swim * stroke * perks.swim * if boosts.active(crate::syringes::Syringe::SprintBurst) { 1.3 } else { 1.0 }
    } else if p.crouching {
        // (`SP_CrouchRun`: the sprint key hurries a crouch along.)
        if perks.crouch_run && key(Action::Sprint) {
            if fast {
                body.crouch_sprint
            } else {
                body.crouch_sprint_tired
            }
        } else {
            body.crouch
        }
    } else if p.sprinting {
        let sprint = if fast { body.sprint } else { body.sprint_tired };
        sprint * perks.sprint * if boosts.active(crate::syringes::Syringe::SprintBurst) { 1.3 } else { 1.0 }
    } else {
        body.walk
    };
    let speed = speed * if p.swimming || p.dragging { 1.0 } else { aimed_pace };
    // Sliding: momentum carries on (friction slows it, a little steering),
    // instead of accelerating toward the wished velocity.
    if was_sliding && p.sliding.is_some() {
        let horiz = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
        let slowed = horiz * (1.0 - SLIDE_FRICTION * dt).max(0.0) + right * axis.x * 2.0 * dt;
        p.velocity.x = slowed.x;
        p.velocity.z = slowed.z;
    }
    // Horizontal acceleration toward the wished velocity.
    let accel = if p.sliding.is_some() {
        0.0
    } else if p.grounded || p.swimming {
        GROUND_ACCEL
    } else {
        AIR_ACCEL
    };
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
            p.velocity.y = body.jump_speed();
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
        // The wingsuit and its parachute fly by the game's figures when it
        // has them (`Flight::fly`), else by ours below.
        let clearance = feet - heights.height_at(t.translation).unwrap_or(f32::MIN);
        let chute_owned = progress.upgrade(crate::freefall::PARACHUTE) > 0;
        let flown = crate::vehicles::wingsuit().is_some_and(|h| flight.fly(&mut p, h, axis, clearance, chute_owned, dt));
        match p.air {
            _ if flown => {}
            AirMode::None => p.velocity.y -= body.gravity * dt,
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
    let trace = std::env::var_os("FC3_COLLISION_DEBUG").is_some() && time.elapsed_secs() >= *next_collision_log;
    let mut contacts = Vec::new();
    let out = move_and_slide.move_and_slide(&collider, start, Quat::IDENTITY, p.velocity, std::time::Duration::from_secs_f32(dt), &config, &filter, |hit| {
        if trace { contacts.push((hit.entity, hit.point, *hit.normal, hit.distance)); }
        if stands_on(&move_and_slide.spatial_query, hit.point, hit.normal.as_vec3(), &filter) {
            ground_hit = true;
        }
        MoveAndSlideHitResponse::Accept
    });
    let mut new_pos = out.position;
    let mut new_vel = out.projected_velocity;

    // Step up: if horizontal motion was blocked, retry from a raised position.
    let wanted = Vec3::new(p.velocity.x, 0.0, p.velocity.z) * dt;
    let achieved = Vec3::new(new_pos.x - start.x, 0.0, new_pos.z - start.z);
    if trace && wanted.length() > 0.001 && achieved.length() < wanted.length() * 0.5 {
        *next_collision_log = time.elapsed_secs() + 1.0;
        info!("player collision: blocked at {start:?}, wanted {wanted:?}, achieved {achieved:?}, grounded {was_grounded}");
        for (entity, point, normal, distance) in contacts {
            let identity = collision_ids.get(entity).ok();
            info!("player collision: hit {entity:?} id {:?} name {:?} parent {:?}, point {point:?}, normal {normal:?}, distance {distance}",
                identity.and_then(|v| v.0).map(|v| v.0), identity.and_then(|v| v.1).map(|v| v.as_str()),
                identity.and_then(|v| v.2).map(|v| v.parent()));
            if identity.is_some_and(|v| v.0.is_none() && v.1.is_none()) {
                let mut parent = identity.and_then(|v| v.2).map(|v| v.parent());
                for _ in 0..8 {
                    let Some(ancestor) = parent else { break };
                    let Ok((id, name, next)) = collision_ids.get(ancestor) else { break };
                    if id.is_some() || name.is_some() {
                        info!("player collision: ancestor {ancestor:?} id {:?} name {:?}", id.map(|v| v.0), name.map(|v| v.as_str()));
                        break;
                    }
                    parent = next.map(|v| v.parent());
                }
            }
        }
    }
    if was_grounded && wanted.length() > 0.001 && achieved.length() < wanted.length() * 0.5 {
        let up = Vec3::Y * STEP_HEIGHT;
        let raised = start + up;
        let clear =
            move_and_slide.spatial_query.cast_shape(&collider, start, Quat::IDENTITY, Dir3::Y, &ShapeCastConfig::from_max_distance(STEP_HEIGHT), &filter);
        if trace {
            info!("player collision: step clearance {:?}", clear.as_ref().map(|h| (h.entity,
                collision_ids.get(h.entity).ok().and_then(|v| v.0).map(|v| v.0), h.distance, h.normal1)));
        }
        if clear.is_none() {
            let step_out = move_and_slide.move_and_slide(
                &collider,
                raised,
                Quat::IDENTITY,
                Vec3::new(p.velocity.x, 0.0, p.velocity.z),
                std::time::Duration::from_secs_f32(dt),
                &config,
                &filter,
                |_| MoveAndSlideHitResponse::Accept,
            );
            let moved = Vec3::new(step_out.position.x - start.x, 0.0, step_out.position.z - start.z);
            if trace { info!("player collision: raised step achieved {moved:?}"); }
            if step_advances(wanted, achieved, moved) {
                // Settle back down onto the step.
                let down = move_and_slide.spatial_query.cast_shape(
                    &collider,
                    step_out.position,
                    Quat::IDENTITY,
                    Dir3::NEG_Y,
                    &ShapeCastConfig::from_max_distance(STEP_HEIGHT + 0.05),
                    &filter,
                );
                if let Some(d) = down.filter(|d| d.normal1.y > MIN_GROUND_NORMAL_Y || stands_on(&move_and_slide.spatial_query, d.point1, -d.normal2, &filter)) {
                    new_pos = step_out.position - Vec3::Y * (d.distance - 0.01).max(0.0);
                    new_vel = Vec3::new(step_out.projected_velocity.x, 0.0, step_out.projected_velocity.z);
                    ground_hit = true;
                } else if let Some(d) = down.filter(|d| d.distance < STEP_HEIGHT - 0.02) {
                    // Onto the rounded edge of a lip (a threshold's), whose
                    // contact reads steep: kept, though not as ground, so
                    // the next steps carry on over it rather than the
                    // capsule sticking against it.
                    new_pos = step_out.position - Vec3::Y * (d.distance - 0.01).max(0.0);
                    new_vel = Vec3::new(step_out.projected_velocity.x, 0.0, step_out.projected_velocity.z);
                }
            }
        }
    }

    // Ground probe keeps us glued to slopes when walking downhill.
    let probe = move_and_slide.spatial_query.cast_shape(
        &collider,
        new_pos,
        Quat::IDENTITY,
        Dir3::NEG_Y,
        &ShapeCastConfig::from_max_distance(if was_grounded { 0.25 } else { 0.05 }),
        &filter,
    );
    let probe_ground =
        probe.as_ref().is_some_and(|h| h.normal1.y > MIN_GROUND_NORMAL_Y || stands_on(&move_and_slide.spatial_query, h.point1, -h.normal2, &filter));
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

/// Compare progress along the requested direction, using a relative margin.
/// A fixed centimetre-per-frame threshold made low steps impossible at high
/// frame rates, while sideways sliding could incorrectly qualify as progress.
fn step_advances(wanted: Vec3, achieved: Vec3, stepped: Vec3) -> bool {
    let distance = wanted.length();
    if !distance.is_finite() || distance <= 0.001 { return false; }
    let direction = wanted / distance;
    stepped.dot(direction) > achieved.dot(direction).max(0.0) + distance * 0.05
}

#[cfg(test)]
mod step_tests {
    use super::*;

    /// A wingsuit and parachute with round figures of the kind the game's
    /// have.
    fn suit() -> fc3_formats::handling::Handling {
        let g = fc3_formats::handling::Glider {
            max_roll: 0.5,
            roll_rate: 2.0,
            roll_speed: 6.0,
            roll_acc: 3.0,
            max_pitch: 0.8,
            pitch_rate: 1.5,
            pitch_speed: 6.0,
            pitch_acc: 1.5,
            pitch_to_speed: -8.0,
            roll_to_turn: -2.0,
            turn_acc: 1.5,
            turn_full_speed: 10.0,
            base_speed: 12.0,
            sink: -4.0,
            low_height: 10.0,
            low_sink: -0.5,
            braking: 0.5,
            brake_restore: 2.0,
            brake_floor: 3.0,
            max_speed: 50.0,
            ..default()
        };
        let c = fc3_formats::handling::Chute {
            base_speed: 5.0,
            max_speed: 8.0,
            sink: -6.0,
            max_roll: 0.3,
            side_rate: 3.0,
            front_rate: 3.0,
            deploy_impulse: 2000.0,
            deploy_time: 0.5,
            auto_eta: 0.3,
            auto_hold: 0.1,
            pitch_up: false,
        };
        fc3_formats::handling::Handling { mass: 200.0, glider: Some(g), chute: Some(c), look: [-30.0, 90.0, -80.0, 80.0], ..default() }
    }

    fn fly(f: &mut Flight, p: &mut Player, h: &fc3_formats::handling::Handling, axis: Vec2, clearance: f32, seconds: f32) {
        let dt = 1.0 / 60.0;
        for _ in 0..(seconds / dt) as usize {
            assert!(f.fly(p, h, axis, clearance, true, dt));
        }
    }

    #[test]
    fn the_wingsuit_flies_on_the_keys_and_the_view_follows_it() {
        let h = suit();
        let mut f = Flight::default();
        let mut p = Player { grounded: false, air: AirMode::Wingsuit, velocity: Vec3::new(0.0, -20.0, 0.0), ..test_player() };
        // Out of the fall into a glide, settling to cruising.
        fly(&mut f, &mut p, &h, Vec2::ZERO, 200.0, 8.0);
        let flat = Vec2::new(p.velocity.x, p.velocity.z).length();
        assert!((flat - 12.0).abs() < 1.0 && (p.velocity.y + 4.0).abs() < 1.0, "{:?}", p.velocity);
        // No nose up: pulling back brakes instead.
        fly(&mut f, &mut p, &h, Vec2::new(0.0, -1.0), 200.0, 3.0);
        assert!(p.velocity.y < 0.0 && Vec2::new(p.velocity.x, p.velocity.z).length() < 11.0, "{:?}", p.velocity);
        // Left on the keys turns it left, and the view with it.
        let yaw = p.yaw;
        fly(&mut f, &mut p, &h, Vec2::new(-1.0, 0.0), 200.0, 2.0);
        assert!(p.yaw > yaw + 0.5, "{} -> {}", yaw, p.yaw);
        // The view looks only so far from its heading, and not far up.
        p.yaw += 3.0;
        p.pitch = 1.2;
        fly(&mut f, &mut p, &h, Vec2::ZERO, 200.0, 0.1);
        let heading = f.glide.unwrap().yaw;
        assert!((p.yaw - heading).abs() <= 80f32.to_radians() + 1e-3 && p.pitch <= 30f32.to_radians() + 1e-3);
    }

    #[test]
    fn the_parachute_opens_by_itself_short_of_the_ground_and_drifts_down() {
        let h = suit();
        let mut f = Flight::default();
        let mut p = Player { grounded: false, air: AirMode::Wingsuit, velocity: Vec3::new(0.0, -10.0, -12.0), ..test_player() };
        fly(&mut f, &mut p, &h, Vec2::new(0.0, 1.0), 1.0, 0.5);
        assert_eq!(p.air, AirMode::Chute);
        // Opening, it pulls the fall up hard; then it drifts at its speeds.
        let mut p = Player { grounded: false, air: AirMode::Chute, velocity: Vec3::new(0.0, -30.0, 0.0), ..test_player() };
        let mut f = Flight::default();
        fly(&mut f, &mut p, &h, Vec2::ZERO, 200.0, 0.5);
        assert!(p.velocity.y > -15.0, "{:?}", p.velocity);
        fly(&mut f, &mut p, &h, Vec2::new(0.0, 1.0), 200.0, 5.0);
        let flat = Vec2::new(p.velocity.x, p.velocity.z).length();
        assert!((p.velocity.y + 6.0).abs() < 0.1 && (flat - 8.0).abs() < 0.2, "{:?}", p.velocity);
    }

    #[test]
    fn a_view_held_still_under_a_turning_mouse_is_reported_once() {
        let mut w = LookWatch::default();
        // Turning freely: never.
        for i in 0..300 {
            assert!(!w.note(i as f32 * 0.016, true, i as f32 * 0.01, 0.0));
        }
        // Held still while the mouse keeps moving: once, after two seconds.
        let mut said = 0;
        for i in 0..300 {
            if w.note(5.0 + i as f32 * 0.016, true, 1.0, 0.0) {
                said += 1;
                assert!(i as f32 * 0.016 > 2.0);
            }
        }
        assert_eq!(said, 1);
        // No input: nothing, and a pause starts the watch afresh (so it can
        // be said again).
        assert!(!w.note(20.0, false, 1.0, 0.0));
        let again = (0..30).filter(|i| w.note(21.0 + *i as f32 * 0.1, true, 1.0, 0.0)).count();
        assert_eq!(again, 1);
    }

    #[test]
    fn step_progress_is_consistent_across_frame_rates_and_rejects_sideways_motion() {
        for fps in [30.0, 60.0, 144.0, 360.0, 1000.0] {
            let wanted = Vec3::NEG_Z * (4.0 / fps);
            assert!(step_advances(wanted, wanted * 0.1, wanted * 0.9), "{fps} FPS");
            assert!(!step_advances(wanted, wanted * 0.1, wanted * 0.12));
            assert!(!step_advances(wanted, Vec3::ZERO, Vec3::X * wanted.length()));
            assert!(!step_advances(wanted, Vec3::ZERO, -wanted));
        }
        assert!(!step_advances(Vec3::ZERO, Vec3::ZERO, Vec3::X));
        assert!(!step_advances(Vec3::splat(f32::NAN), Vec3::ZERO, Vec3::X));
    }
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
        info!(
            "slide-test t={t:.2} speed {:.1} eye {:.2} sliding {:?} crouching {} sprinting {}",
            Vec2::new(p.velocity.x, p.velocity.z).length(),
            p.eye,
            p.sliding.map(|s| (s * 100.0).round() / 100.0),
            p.crouching,
            p.sprinting
        );
    }
}

/// A player standing still on the ground (tests).
#[cfg(test)]
pub(crate) fn test_player() -> Player {
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
        eye: EYE_HEIGHT,
        air: AirMode::None,
        mantle: None,
        mounted: None,
        sliding: None,
        dragging: false,
    }
}

/// `FC3_TELEPORT=<s>,<x>,<y>,<z>` (tests): that many seconds into play the
/// player is put there (Dunia's coordinates, the feet), once: out of a
/// mission's area, say.
fn teleport_test(time: Res<Time>, mut done: Local<bool>, mut players: Query<(&mut Transform, &mut Player)>) {
    if *done {
        return;
    }
    let Some(v) = std::env::var("FC3_TELEPORT").ok().map(|v| v.split(',').filter_map(|n| n.trim().parse::<f32>().ok()).collect::<Vec<f32>>()) else {
        *done = true;
        return;
    };
    let (Some(&at), Ok((mut t, mut p))) = (v.first(), players.single_mut()) else { return };
    if v.len() < 4 || time.elapsed_secs() < at {
        return;
    }
    t.translation = crate::data::to_bevy([v[1], v[2], v[3]]) + Vec3::Y * HALF_HEIGHT;
    p.velocity = Vec3::ZERO;
    *done = true;
    info!("teleport test: the player put at {:?}", [v[1], v[2], v[3]]);
}
