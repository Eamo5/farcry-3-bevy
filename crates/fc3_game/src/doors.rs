//! Swing doors (`CDoor`): a leaf hinged at its entity's origin and lying
//! along its model's x swings a quarter turn away from the player who walks
//! into it, its collision with it, and stays open; it sounds its
//! `sndPushDoor`. A script's `Door` box locks and unlocks one
//! (`CDoor_Disable`, `CDoor_Enable`). Doors are found in the story's entity
//! data, so they swing where the story runs.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::objects::{PlacedId, WorldObject};
use crate::player::{Player, CAPSULE_RADIUS, HALF_HEIGHT};

pub struct DoorsPlugin;

impl Plugin for DoorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Doors>().add_systems(Update, (find_doors, tag_doors, push_doors, swing_doors).chain());
    }
}

/// The world's swing doors and which a script has locked.
#[derive(Resource, Default)]
pub struct Doors {
    /// Door entities and their push sounds, once the story's data is in.
    known: Option<HashMap<u64, u32>>,
    /// Doors a script has disabled.
    pub locked: HashSet<u64>,
}

/// A placed swing door.
#[derive(Component)]
pub struct Door {
    id: u64,
    sound: u32,
}

/// A door swinging or swung: its closed turn and how far it has turned (about
/// the vertical, at its hinge), toward where.
#[derive(Component)]
struct Swing {
    closed: Quat,
    angle: f32,
    target: f32,
}

/// How fast a door swings (radians a second).
const SWING_SPEED: f32 = 4.0;

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
/// about the vertical that swings it away from them.
fn push_turn(hinge: Vec3, closed: Quat, width: f32, pos: Vec3, wish: Vec3) -> Option<f32> {
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
    [std::f32::consts::FRAC_PI_2, -std::f32::consts::FRAC_PI_2].into_iter().find(|turn| (Quat::from_rotation_y(*turn) * along).dot(across * side) < 0.0)
}

fn push_doors(
    mut commands: Commands,
    doors: Res<Doors>,
    models: Res<Assets<crate::cache_assets::Fc3Model>>,
    actions: Res<crate::controls::ActionState>,
    players: Query<(&Transform, &Player), Without<Door>>,
    placed: Query<(Entity, &Door, &WorldObject, &GlobalTransform, &Transform), Without<Swing>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
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
        if hinge.distance_squared(pt.translation) > 9.0 || doors.locked.contains(&door.id) {
            continue;
        }
        let width = models.get(&object.model).map_or(1.0, |m| m.aabb_max.x * t.scale.x).clamp(0.4, 3.0);
        let closed = global.to_scale_rotation_translation().1;
        let Some(turn) = push_turn(hinge, closed, width, pt.translation, wish) else { continue };
        debug!("doors: {} swings {:.0} degrees", door.id, turn.to_degrees());
        commands.entity(e).insert(Swing { closed: t.rotation, angle: 0.0, target: turn });
        if door.sound != 0 {
            sfx.write(crate::sound::PlaySfx { event: door.sound, at: Some(hinge), volume: 1.0 });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A door along x from the origin; walked into from either side, it
    /// swings away from the walker, and not when they walk past or away.
    #[test]
    fn a_door_swings_away_from_whoever_pushes_it() {
        let at = |x: f32, z: f32| Vec3::new(x, HALF_HEIGHT, z);
        let from_front = push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 0.5), Vec3::new(0.0, 0.0, -2.0)).expect("pushed");
        // The leaf ends up on the far side (negative z).
        assert!((Quat::from_rotation_y(from_front) * Vec3::X).z < -0.99);
        let from_back = push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, -0.5), Vec3::new(0.0, 0.0, 2.0)).expect("pushed");
        assert!((Quat::from_rotation_y(from_back) * Vec3::X).z > 0.99);
        // Walking along it, or away, or too far off.
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 0.5), Vec3::new(2.0, 0.0, 0.0)).is_none());
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 0.5), Vec3::new(0.0, 0.0, 2.0)).is_none());
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(0.6, 1.5), Vec3::new(0.0, 0.0, -2.0)).is_none());
        // Past its free end.
        assert!(push_turn(Vec3::ZERO, Quat::IDENTITY, 1.2, at(2.2, 0.5), Vec3::new(0.0, 0.0, -2.0)).is_none());
    }
}
