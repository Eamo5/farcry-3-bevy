//! Swing doors (`CDoor`): a leaf hinged at its entity's origin and lying
//! along its model's x swings a quarter turn away from the player who walks
//! into it, its collision with it; it sounds its `sndPushDoor` and tells the
//! story it was pushed (from its front) or pulled (from behind), which a
//! script's `Door` box hears. It swings shut again once the player is clear
//! of it (`CLOSE_AFTER`), or at once when a script locks it, and tells the
//! story it `Closed` (BAM_060 waits for Buck's door to shut behind the
//! player). The box locks and unlocks one (`CDoor_Disable`, `CDoor_Enable`).
//! Doors are found in the story's entity data, so they swing where the
//! story runs.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::objects::{PlacedId, WorldObject};
use crate::player::{Player, CAPSULE_RADIUS, HALF_HEIGHT};

pub struct DoorsPlugin;

impl Plugin for DoorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Doors>().add_systems(Update, (find_doors, tag_doors, push_doors, swing_doors, close_doors).chain()).add_systems(
            PreUpdate,
            autopilot_doors.after(crate::controls::update_actions).before(crate::controls::apply_action_maps).run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

/// The world's swing doors and which a script has locked.
#[derive(Resource, Default)]
pub struct Doors {
    /// Door entities and their push sounds, once the story's data is in.
    known: Option<HashMap<u64, u32>>,
    /// Doors a script has disabled.
    pub locked: HashSet<u64>,
    /// Tests (`FC3_STORY_AUTOPILOT`): doors the player was walked into.
    pub walked: HashSet<u64>,
}

/// A placed swing door.
#[derive(Component)]
pub struct Door {
    id: u64,
    sound: u32,
}

/// A door swinging or swung: its closed turn and how far it has turned (about
/// the vertical, at its hinge), toward where, and how long it has stood
/// open.
#[derive(Component)]
struct Swing {
    closed: Quat,
    angle: f32,
    target: f32,
    open_for: f32,
}

/// How fast a door swings (radians a second).
const SWING_SPEED: f32 = 4.0;

/// How long a door stands open before it swings shut (once the player is
/// clear of its leaf).
const CLOSE_AFTER: f32 = 3.0;

fn find_doors(host: Option<NonSend<crate::story::StoryHost>>, mut doors: ResMut<Doors>) {
    if doors.known.is_some() {
        return;
    }
    let Some(host) = host else { return };
    let data = host.story.data();
    let known: HashMap<u64, u32> = data
        .entities
        .values()
        .filter_map(|e| {
            let door = e.component("CDoor")?;
            let sound = door.str("sndPushDoor").and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
            Some((e.id, sound))
        })
        .collect();
    info!("doors: {} swing doors", known.len());
    doors.known = Some(known);
}

/// Marks placed doors (new ones, and all of them once the doors are known).
fn tag_doors(
    mut commands: Commands,
    doors: Res<Doors>,
    added: Query<(Entity, &PlacedId), Added<PlacedId>>,
    all: Query<(Entity, &PlacedId), Without<Door>>,
    mut swept: Local<bool>,
) {
    let Some(known) = &doors.known else { return };
    let mut tag = |e: Entity, id: &PlacedId| {
        if let Some(&sound) = known.get(&id.0) {
            commands.entity(e).insert(Door { id: id.0, sound });
        }
    };
    if !*swept {
        *swept = true;
        for (e, id) in &all {
            tag(e, id);
        }
    } else {
        for (e, id) in &added {
            tag(e, id);
        }
    }
}

/// The leaf of a door at `hinge` turned `closed`, `width` long: whether a
/// player at `pos` (capsule centre) heading `wish` (the way they're trying
/// to go: a closed door stops them dead) walks into it, and if so the turn
/// about the vertical that swings it away from them, and whether they came
/// at it from its front (its +z).
fn push_turn(hinge: Vec3, closed: Quat, width: f32, pos: Vec3, wish: Vec3) -> Option<(f32, bool)> {
    let along = closed * Vec3::X;
    let across = closed * Vec3::Z;
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
    let off = flat(pos - hinge);
    let nearest = along * off.dot(along).clamp(0.0, width);
    let to_leaf = nearest - off;
    let feet = pos.y - HALF_HEIGHT;
    let reach = to_leaf.length() < CAPSULE_RADIUS + 0.25 && feet > hinge.y - 0.5 && feet < hinge.y + 1.5;
    let pushing = flat(wish).normalize_or_zero().dot(to_leaf.normalize_or_zero()) > 0.5;
    if !reach || !pushing {
        return None;
    }
    // Away from the side the player stands on.
    let side = off.dot(across).signum();
    [std::f32::consts::FRAC_PI_2, -std::f32::consts::FRAC_PI_2]
        .into_iter()
        .find(|turn| (Quat::from_rotation_y(*turn) * along).dot(across * side) < 0.0)
        .map(|turn| (turn, side > 0.0))
}

fn push_doors(
    mut commands: Commands,
    doors: Res<Doors>,
    models: Res<Assets<crate::cache_assets::Fc3Model>>,
    actions: Res<crate::controls::ActionState>,
    players: Query<(&Transform, &Player), Without<Door>>,
    placed: Query<(Entity, &Door, &WorldObject, &GlobalTransform, &Transform), Without<Swing>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut host: Option<NonSendMut<crate::story::StoryHost>>,
    time: Res<Time>,
    mut since: Local<f32>,
) {
    let Ok((pt, p)) = players.single() else { return };
    if p.driving || p.noclip {
        return;
    }
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let wish = forward * actions.movement.y + right * actions.movement.x;
    for (e, door, object, global, t) in &placed {
        let hinge = global.translation();
        if hinge.distance_squared(pt.translation) > 9.0 {
            continue;
        }
        let width = models.get(&object.model).map_or(1.0, |m| m.aabb_max.x * t.scale.x).clamp(0.4, 3.0);
        let closed = global.to_scale_rotation_translation().1;
        *since += time.delta_secs();
        if *since > 0.5 {
            *since = 0.0;
            let off = pt.translation - hinge;
            debug!(
                "doors: {} near: locked {}, along {:.2} across {:.2} (width {width:.2}), feet {:.2} above the hinge, wish {:.2?}",
                door.id,
                doors.locked.contains(&door.id),
                off.dot(closed * Vec3::X),
                off.dot(closed * Vec3::Z),
                off.y - HALF_HEIGHT,
                wish
            );
        }
        if doors.locked.contains(&door.id) {
            continue;
        }
        let Some((turn, front)) = push_turn(hinge, closed, width, pt.translation, wish) else { continue };
        debug!("doors: {} swings {:.0} degrees", door.id, turn.to_degrees());
        if let Some(host) = host.as_mut() {
            host.story.entity_event(door.id, if front { "Push" } else { "Pull" }, vec![]);
        }
        commands.entity(e).insert(Swing { closed: t.rotation, angle: 0.0, target: turn, open_for: 0.0 });
        if door.sound != 0 {
            sfx.write(crate::sound::PlaySfx { event: door.sound, at: Some(hinge), volume: 1.0 });
        }
    }
}

/// Tests (`FC3_STORY_AUTOPILOT`): a door a script waits to be pushed or
/// pulled (a hut's, to leave by), once the story has gone quiet: the player
/// set down before it, facing it, walks into it. He comes at it from the
/// side away from the triggers the scripts watch by it (those are for
/// walking through it into: BAM_060's inside Buck's hut locks the door
/// behind him), else from the side he is on.
#[allow(clippy::type_complexity)]
fn autopilot_doors(
    host: Option<NonSendMut<crate::story::StoryHost>>,
    time: Res<Time>,
    models: Res<Assets<crate::cache_assets::Fc3Model>>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut players: Query<(&mut Transform, &mut Player), Without<Door>>,
    placed: Query<(&Door, &WorldObject, &GlobalTransform, &Transform), (Without<Swing>, Without<Player>)>,
    mut doors: ResMut<Doors>,
    mut walking: Local<Option<f32>>,
    mut far_tried: Local<HashSet<u64>>,
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() {
        return;
    }
    let Some(mut host) = host else { return };
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    if let Some(left) = walking.as_mut() {
        if !p.spawning {
            actions.movement = Vec2::Y;
        }
        // (Set down, he may be a moment finding his feet.)
        *left -= time.delta_secs();
        if *left <= 0.0 {
            *walking = None;
        }
        return;
    }
    if host.quiet() < 6.0 {
        return;
    }
    let awaited = host.story.awaited_doors();
    let Some((door, object, global, t)) = placed.iter().find(|(d, ..)| awaited.contains(&d.id) && !doors.walked.contains(&d.id)) else {
        // One not in the world near him (BAM_060's, across the island from
        // the last mission): beside it first, so that it streams in.
        let data = host.story.data();
        let far = awaited.iter().filter(|id| !far_tried.contains(id)).find_map(|id| data.entities.get(id).map(|e| (*id, e.pos, e.angles)));
        if let Some((id, pos, angles)) = far {
            let (at, across) = (crate::data::to_bevy(pos), Quat::from_array(fc3_formats::world::angles_to_bevy_quat(angles)) * Vec3::Z);
            let side = approach_side(&host, at, across, pt.translation);
            info!("story autopilot: go to door {id}");
            crate::story::teleport(&mut pt, &mut p, crate::data::to_dunia(at + across * side * 2.5), 0.0);
            far_tried.insert(id);
            host.reset_quiet();
        }
        return;
    };
    let (_, closed, hinge) = global.to_scale_rotation_translation();
    let width = models.get(&object.model).map_or(1.0, |m| m.aabb_max.x * t.scale.x).clamp(0.4, 3.0);
    let along = closed * Vec3::X;
    let across = closed * Vec3::Z * approach_side(&host, hinge, closed * Vec3::Z, pt.translation);
    let stand = hinge + along * width * 0.5 + across * 1.2;
    info!("story autopilot: walk into door {}", door.id);
    debug!("doors: hinge {hinge:.2?}, width {width:.2}, along {along:.2?}, from {stand:.2?}");
    crate::story::teleport(&mut pt, &mut p, crate::data::to_dunia(stand), 0.0);
    // Facing back across it (forward is -z turned by the yaw).
    p.yaw = across.x.atan2(across.z);
    doors.walked.insert(door.id);
    *walking = Some(2.0);
    host.reset_quiet();
}

/// Which side of a door at `at` (its front `across`) to come at it from: 1
/// its front, -1 behind. The side standing in fewer of the triggers the
/// scripts watch (those past the door are for walking through it into),
/// else the side the player is on.
fn approach_side(host: &crate::story::StoryHost, at: Vec3, across: Vec3, player: Vec3) -> f32 {
    let data = host.story.data();
    let watched: Vec<fc3_story::triggers::Shape> = host
        .story
        .watched_triggers()
        .into_iter()
        .filter(|w| w.1 == "Enter")
        .filter_map(|w| data.entities.get(&w.0).and_then(fc3_story::triggers::shape_of))
        .collect();
    let inside = |side: f32| {
        let p = crate::data::to_dunia(at + across * side * 1.5 + Vec3::Y);
        watched.iter().filter(|s| s.contains(p)).count()
    };
    match (inside(1.0), inside(-1.0)) {
        (front, back) if front < back => 1.0,
        (front, back) if back < front => -1.0,
        _ if (player - at).dot(across) < 0.0 => -1.0,
        _ => 1.0,
    }
}

fn swing_doors(time: Res<Time>, mut swings: Query<(&mut Swing, &mut Transform)>) {
    let step = SWING_SPEED * time.delta_secs();
    for (mut s, mut t) in &mut swings {
        if s.angle == s.target {
            continue;
        }
        s.angle += (s.target - s.angle).clamp(-step, step);
        t.rotation = Quat::from_rotation_y(s.angle) * s.closed;
    }
}

/// An open door swings shut after a while once the player is clear of its
/// leaf (at once when a script has locked it), and tells the story it
/// `Closed`; shut, it can be pushed again.
fn close_doors(
    mut commands: Commands,
    time: Res<Time>,
    doors: Res<Doors>,
    models: Res<Assets<crate::cache_assets::Fc3Model>>,
    players: Query<&Transform, (With<Player>, Without<Door>)>,
    mut swings: Query<(Entity, &Door, &mut Swing, &WorldObject, &GlobalTransform, &Transform)>,
    mut host: Option<NonSendMut<crate::story::StoryHost>>,
) {
    let player = players.single().ok().map(|t| t.translation);
    for (e, door, mut s, object, global, t) in &mut swings {
        if s.target != 0.0 && s.angle == s.target {
            s.open_for += time.delta_secs();
            let width = models.get(&object.model).map_or(1.0, |m| m.aabb_max.x * t.scale.x).clamp(0.4, 3.0);
            let clear = player.is_none_or(|p| (p - global.translation()).with_y(0.0).length() > width + CAPSULE_RADIUS + 0.5);
            if doors.locked.contains(&door.id) || (s.open_for >= CLOSE_AFTER && clear) {
                s.target = 0.0;
            }
        } else if s.target == 0.0 && s.angle == 0.0 {
            commands.entity(e).remove::<Swing>();
            if let Some(host) = host.as_mut() {
                host.story.entity_event(door.id, "Closed", vec![]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A door along x from the origin; walked into from either side, it
    /// swings away from the walker, and not when they walk past or away.
    #[test]
    fn a_door_swings_away_from_whoever_pushes_it() {
        let at = |x: f32, z: f32| Vec3::new(x, HALF_HEIGHT, z);
        let (from_front, front) = push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 0.5), Vec3::new(0.0, 0.0, -2.0)).expect("pushed");
        // The leaf ends up on the far side (negative z).
        assert!((Quat::from_rotation_y(from_front) * Vec3::X).z < -0.99);
        assert!(front);
        let (from_back, front) = push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, -0.5), Vec3::new(0.0, 0.0, 2.0)).expect("pulled");
        assert!((Quat::from_rotation_y(from_back) * Vec3::X).z > 0.99);
        assert!(!front);
        // Walking along it, or away, or too far off.
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 0.5), Vec3::new(2.0, 0.0, 0.0)).is_none());
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 0.5), Vec3::new(0.0, 0.0, 2.0)).is_none());
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 1.5), Vec3::new(0.0, 0.0, -2.0)).is_none());
        // Past its free end.
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(2.2, 0.5), Vec3::new(0.0, 0.0, -2.0)).is_none());
    }
}
