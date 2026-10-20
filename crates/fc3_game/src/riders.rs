//! Characters riding in a vehicle sit in its seats on the game's own moves:
//! the seated loops of `com_inside_vehicle` (a technical's bed, a boat's
//! benches), and at the wheel the driving pose the player's own seat plays
//! (its body's whole, hands on the wheel). These clips are made in the
//! vehicle's frame, their root at the seat, so a rider is placed by the
//! clip's root on the vehicle. They get out by `com_exit_vehicle`'s moves,
//! whose root moves from the seat out to the ground beside it.

use bevy::prelude::*;

use crate::cache_assets::{cache_path, Fc3Clip, Fc3Skeleton};
use crate::characters::ClipOverride;
use crate::combat::Health;
use crate::npc::{Npc, NpcState, Riding};
use crate::player::HALF_HEIGHT;
use crate::vehicles::Vehicle;

pub struct RidersPlugin;

impl Plugin for RidersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_clips)
            .add_systems(PostUpdate, (sit_riders, get_out).chain().after(crate::patrols::carry_crew).before(bevy::transform::TransformSystems::Propagate))
            .add_systems(PostUpdate, seat_story_riders.after(crate::vehicles::seat_story_actors).before(bevy::transform::TransformSystems::Propagate));
    }
}

/// A place in a vehicle: its seated loop, and the move out of it (none for
/// a boat's crew, who stay aboard), under `animations\vehicles\`.
pub struct Place {
    pub sit: &'static str,
    pub exit: Option<&'static str>,
}

/// The technical's: the wheel, the seat beside it, the bed's four (its own
/// cab has no moves out, the 4x4's are taken; their cabs are alike).
const TECHNICAL: [Place; 6] = [
    Place {
        sit: r"4x4_technical\1stge_fulb_driving_nodir_nowep_4x4tech.mab",
        exit: Some(r"truck\4x4\alert\exits\3rdge_fulbst_alert_driverexit4x4_v00_-090lt_ak74.mab"),
    },
    Place {
        sit: r"4x4_technical\1stge_fulb_passenger_nodir_nowep_4x4tech.mab",
        exit: Some(r"truck\4x4\alert\exits\3rdge_fulbst_alert_passengerexit4x4_v00_+090rt_ak74.mab"),
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionl1_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionl1_4x4tech_exit_+000fw_ak74.mab"),
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionr1_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionr1_4x4tech_exit_+000fw_ak74.mab"),
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionl2_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionl2_4x4tech_exit_+000fw_ak74.mab"),
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionr2_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionr2_4x4tech_exit_+000fw_ak74.mab"),
    },
];

/// The zodiac's: its driver, and the benches on its right.
const BOAT: [Place; 3] = [
    Place { sit: r"boat\zodiac\alert\idles\3rdge_fulbst_driver_idlealert_zodiac_+000fw_nowep.mab", exit: None },
    Place { sit: r"boat\zodiac\alert\idles\3rdge_fulbst_passright01_idlealert_zodiac_+000fw_nowep.mab", exit: None },
    Place { sit: r"boat\zodiac\alert\idles\3rdge_fulbst_passright02_idlealert_zodiac_+000fw_nowep.mab", exit: None },
];

/// A vehicle's places by its name (ours, `vehicles::DEFS`).
pub fn places(vehicle: &str) -> &'static [Place] {
    match vehicle {
        "Technical" => &TECHNICAL,
        "Boat" => &BOAT,
        _ => &[],
    }
}

/// How far (m) one out of a vehicle stands clear of its sides (its body
/// had overlapped the vehicle, which the two pushed apart, flipping it).
const CLEAR: f32 = 0.5;

/// A point in a vehicle's frame moved out of its footprint (its bounds
/// `CLEAR` wider) by the nearest side, if it is in it.
fn clear_of(local: Vec3, (min, max): (Vec3, Vec3)) -> Vec3 {
    let (lo, hi) = (min - Vec3::splat(CLEAR), max + Vec3::splat(CLEAR));
    if local.x <= lo.x || local.x >= hi.x || local.z <= lo.z || local.z >= hi.z {
        return local;
    }
    let ways = [Vec3::X * (lo.x - local.x), Vec3::X * (hi.x - local.x), Vec3::Z * (lo.z - local.z), Vec3::Z * (hi.z - local.z)];
    local + ways.into_iter().min_by(|a, b| a.length().total_cmp(&b.length())).unwrap_or(Vec3::ZERO)
}

const PREFIX: &str = r"graphics\characters\_common\animations\vehicles\";
const MASTER_SKELETON: &str = r"graphics\actors_\_skeleton_master\hips_ref.skeleton";

/// The places' clips, by their paths under `animations\vehicles\`, and the
/// skeleton they're made for.
#[derive(Resource)]
pub struct RiderClips {
    skeleton: Handle<Fc3Skeleton>,
    clips: std::collections::HashMap<&'static str, Handle<Fc3Clip>>,
}

fn load_clips(mut commands: Commands, server: Res<AssetServer>) {
    let mut clips = std::collections::HashMap::new();
    for place in TECHNICAL.iter().chain(BOAT.iter()) {
        for path in std::iter::once(place.sit).chain(place.exit) {
            clips.entry(path).or_insert_with(|| server.load(cache_path(&fc3_formats::cache::anim_rel(&format!("{PREFIX}{path}")))));
        }
    }
    let skeleton = server.load(cache_path(&fc3_formats::cache::skeleton_rel(MASTER_SKELETON)));
    commands.insert_resource(RiderClips { skeleton, clips });
}

/// Getting out of a vehicle on its place's move: its seat in the vehicle's
/// frame (where the move starts), the move, and how far into it.
#[derive(Component)]
pub struct GettingOut {
    vehicle: Entity,
    seat: Transform,
    clip: Handle<Fc3Clip>,
    t: f32,
}

/// Sitting on its place's loop (its rig's clip set aside while it does).
#[derive(Component)]
pub struct Sitting;

/// A rider's place in its vehicle, if the vehicle has one by that number.
fn place_of(vehicle: &Vehicle, ride: &Riding) -> Option<&'static Place> {
    places(vehicle.name()).get(ride.place? as usize)
}

/// The capsule's place for a root (feet) in the world.
fn capsule(root: Transform) -> Transform {
    Transform { translation: root.translation + root.rotation * Vec3::Y * HALF_HEIGHT, rotation: root.rotation, scale: Vec3::ONE }
}

/// Riders with a place sit in it on its loop, each a little out of step.
#[allow(clippy::type_complexity)]
fn sit_riders(
    time: Res<Time>,
    rider_clips: Option<Res<RiderClips>>,
    clips: Res<Assets<Fc3Clip>>,
    vehicles: Query<(&Transform, &Vehicle), Without<Riding>>,
    mut riders: Query<(Entity, &Riding, &mut Transform, &Npc, &Health, &mut Visibility, Has<Sitting>), Without<GettingOut>>,
    mut commands: Commands,
) {
    let Some(rider_clips) = rider_clips else { return };
    for (e, ride, mut at, npc, health, mut vis, sitting) in &mut riders {
        let Ok((vt, v)) = vehicles.get(ride.vehicle) else { continue };
        let Some(place) = place_of(v, ride) else { continue };
        // Killed in the seat: its own fall again.
        if health.dead {
            if sitting {
                if let Some(rig) = npc.rig {
                    commands.entity(rig).remove::<ClipOverride>();
                }
                commands.entity(e).remove::<Sitting>();
            }
            continue;
        }
        let Some(handle) = rider_clips.clips.get(place.sit) else { continue };
        let Some(clip) = clips.get(handle) else { continue };
        let length = clip.0.duration.max(0.1);
        let t = (time.elapsed_secs() + (e.to_bits() % 16) as f32 * 0.37).rem_euclid(length);
        *at = capsule(*vt * crate::cinema::clip_root(&clip.0, t));
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
        if let Some(rig) = npc.rig {
            commands.entity(rig).insert(ClipOverride { clip: handle.clone(), skeleton: rider_clips.skeleton.clone(), time: t });
        }
        if !sitting {
            commands.entity(e).insert(Sitting);
        }
    }
}

/// How near (m) a place's seat must be to a story seat's bone to stand for
/// it.
const SAME_SEAT: f32 = 0.75;

/// The story's characters in a vehicle's seats sit there on the seat's own
/// clip: the first-person seated one where the seat has it (the wheel, the
/// seat beside it, a 4x4's back seats), else the place whose seat is that
/// one (`places`, nearest the seat's bone). Not while a scene plays them.
#[allow(clippy::type_complexity)]
fn seat_story_riders(
    time: Res<Time>,
    server: Res<AssetServer>,
    rider_clips: Option<Res<RiderClips>>,
    clips: Res<Assets<Fc3Clip>>,
    vehicles: Query<(&Transform, &Vehicle)>,
    mut actors: Query<(Entity, &mut Transform, &crate::story_actors::StoryActor, Has<Sitting>), Without<Vehicle>>,
    mut seated: Local<std::collections::HashMap<&'static str, Handle<Fc3Clip>>>,
    mut commands: Commands,
) {
    let Some(rider_clips) = rider_clips else { return };
    for (e, mut at, actor, sitting) in &mut actors {
        let seat = (!actor.dead && !actor.in_scene)
            .then(|| vehicles.iter().find_map(|(vt, v)| v.seat_of(actor.id).map(|(i, kind, pose)| (*vt, v, i, kind, pose))))
            .flatten()
            // (A gun's seat is its gunner's business: `turrets`.)
            .filter(|(.., kind, _)| *kind != 2);
        let Some((vt, v, i, _, pose)) = seat else {
            // Out of the seat (or in a scene's hands, or dead): its own clips.
            if sitting {
                if let (Some(rig), false) = (actor.rig, actor.in_scene) {
                    commands.entity(rig).remove::<ClipOverride>();
                }
                commands.entity(e).remove::<Sitting>();
            }
            continue;
        };
        let handle = match v.first_person_clip(i) {
            Some(path) => Some(
                seated
                    .entry(path)
                    .or_insert_with(|| server.load(cache_path(&fc3_formats::cache::anim_rel(&format!(r"graphics\characters\_common\animations\{path}")))))
                    .clone(),
            ),
            None => places(v.name())
                .iter()
                .filter_map(|p| rider_clips.clips.get(p.sit))
                .filter_map(|h| clips.get(h).map(|c| (h, crate::cinema::clip_root(&c.0, 0.0).translation.distance(pose.translation))))
                .filter(|(_, d)| *d < SAME_SEAT)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(h, _)| h.clone()),
        };
        let Some((handle, clip)) = handle.and_then(|h| clips.get(&h).map(|c| (h.clone(), c))) else { continue };
        let length = clip.0.duration.max(0.1);
        let t = (time.elapsed_secs() + (e.to_bits() % 16) as f32 * 0.37).rem_euclid(length);
        *at = capsule(vt * crate::cinema::clip_root(&clip.0, t));
        if let Some(rig) = actor.rig {
            commands.entity(rig).insert(ClipOverride { clip: handle, skeleton: rider_clips.skeleton.clone(), time: t });
        }
        if !sitting {
            commands.entity(e).insert(Sitting);
        }
    }
}

/// Starts a rider getting out by its place's move, if it has one and it has
/// loaded: false leaves it to be put out as before.
pub fn begin_exit(
    commands: &mut Commands,
    rider: Entity,
    vehicle: (Entity, &Vehicle),
    ride: &Riding,
    rider_clips: Option<&RiderClips>,
    clips: &Assets<Fc3Clip>,
) -> bool {
    let (Some(place), Some(rider_clips)) = (place_of(vehicle.1, ride), rider_clips) else { return false };
    let (Some(sit), Some(exit)) = (rider_clips.clips.get(place.sit), place.exit.and_then(|p| rider_clips.clips.get(p))) else { return false };
    let (Some(seated), true) = (clips.get(sit), clips.contains(exit)) else { return false };
    let seat = crate::cinema::clip_root(&seated.0, 0.0);
    commands.entity(rider).insert(GettingOut { vehicle: vehicle.0, seat, clip: exit.clone(), t: 0.0 }).remove::<Sitting>();
    true
}

/// Riders getting out follow their move from the seat; at its end they
/// stand on the ground beside the vehicle and fight.
#[allow(clippy::type_complexity)]
fn get_out(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    rider_clips: Option<Res<RiderClips>>,
    clips: Res<Assets<Fc3Clip>>,
    vehicles: Query<(&Transform, &Vehicle), Without<GettingOut>>,
    mut out: Query<(Entity, &mut GettingOut, &mut Transform, &mut Npc, &Health), Without<Vehicle>>,
    mut commands: Commands,
) {
    let Some(rider_clips) = rider_clips else { return };
    let dt = time.delta_secs();
    for (e, mut g, mut at, mut npc, health) in &mut out {
        g.t += dt;
        let clip = clips.get(&g.clip);
        let length = clip.map_or(0.0, |c| c.0.duration);
        let vehicle = vehicles.get(g.vehicle).ok();
        let vt = vehicle.map(|(vt, _)| *vt);
        if let (Some(clip), Some(vt)) = (clip, vt) {
            let root = vt * g.seat * crate::cinema::clip_root(&clip.0, g.t.min(length));
            *at = capsule(root);
            npc.yaw = root.rotation.to_euler(EulerRot::YXZ).0;
        }
        let done = health.dead || vt.is_none() || g.t >= length;
        if !done {
            if let Some(rig) = npc.rig {
                commands.entity(rig).insert(ClipOverride { clip: g.clip.clone(), skeleton: rider_clips.skeleton.clone(), time: g.t });
            }
            continue;
        }
        // Out: on the ground, upright, and into the fight.
        if let Some(rig) = npc.rig {
            commands.entity(rig).remove::<ClipOverride>();
        }
        let mut feet = at.translation - Vec3::Y * HALF_HEIGHT;
        // (Clear of the vehicle before its body is solid again.)
        if let Some((vt, v)) = vehicle {
            let local = vt.rotation.inverse() * (feet - vt.translation);
            feet = vt.translation + vt.rotation * clear_of(local, v.bounds());
        }
        feet.y = heights.height_at(feet).unwrap_or(feet.y);
        at.translation = feet + Vec3::Y * HALF_HEIGHT;
        at.rotation = Quat::from_rotation_y(npc.yaw);
        npc.post = feet;
        if !health.dead {
            npc.state = NpcState::Combat;
            npc.state_time = 0.0;
        }
        commands.entity(e).remove::<(GettingOut, Riding, avian3d::prelude::ColliderDisabled)>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_have_their_moves_and_boats_keep_their_crews() {
        assert!(places("Technical").iter().all(|p| p.exit.is_some()));
        assert!(places("Boat").iter().all(|p| p.exit.is_none()));
        assert!(places("Hang glider").is_empty());
    }

    #[test]
    fn one_out_of_a_vehicle_stands_clear_of_its_nearest_side() {
        let bounds = (Vec3::new(-1.0, 0.0, -2.5), Vec3::new(1.0, 2.0, 2.5));
        // Just beside the left door: out to the left.
        assert!(clear_of(Vec3::new(-1.2, 0.0, 0.0), bounds).abs_diff_eq(Vec3::new(-1.5, 0.0, 0.0), 1e-5));
        // Behind the bed: out behind it.
        assert!(clear_of(Vec3::new(0.2, 0.0, 2.8), bounds).abs_diff_eq(Vec3::new(0.2, 0.0, 3.0), 1e-5));
        // Already clear: where it is.
        assert_eq!(clear_of(Vec3::new(2.0, 0.0, 0.0), bounds), Vec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn a_root_at_the_feet_puts_the_capsule_half_a_body_up_along_its_own_up() {
        let tilted = Transform::from_xyz(1.0, 2.0, 3.0).with_rotation(Quat::from_rotation_z(0.3));
        let c = capsule(tilted);
        assert!((c.translation - tilted.translation).length() - HALF_HEIGHT < 1e-4);
        assert!(c.translation.abs_diff_eq(tilted.translation + tilted.rotation * Vec3::Y * HALF_HEIGHT, 1e-5));
    }
}
