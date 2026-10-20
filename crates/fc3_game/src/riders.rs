//! Characters riding in a vehicle sit in its seats on the game's own moves:
//! the seated loops of `com_inside_vehicle` (a technical's bed, a boat's
//! benches), and at the wheel the driving pose the player's own seat plays
//! (its body's whole, hands on the wheel). These clips are made in the
//! vehicle's frame, their root at the seat, so a rider is placed by the
//! clip's root on the vehicle. They get out by `com_exit_vehicle`'s moves,
//! whose root moves from the seat out to the ground beside it, and the
//! story's get in by `com_enter_vehicle`'s, from beside the door to the seat.

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
            .add_systems(Update, entry_test)
            .add_systems(PostUpdate, entry_shots.after(story_get_in))
            .add_systems(PostUpdate, (sit_riders, get_out).chain().after(crate::patrols::carry_crew).before(bevy::transform::TransformSystems::Propagate))
            .add_systems(
                PostUpdate,
                (seat_story_riders, story_get_in, story_get_out)
                    .chain()
                    .after(crate::vehicles::seat_story_actors)
                    .before(bevy::transform::TransformSystems::Propagate),
            );
    }
}

/// A place in a vehicle: its seated loop, and the move out of it (none for
/// a boat's crew, who stay aboard), under `animations\vehicles\`.
pub struct Place {
    pub sit: &'static str,
    pub exit: Option<&'static str>,
    /// The moves into it, from either side or with or without a gun (one
    /// is chosen by how the character comes to it).
    pub entry: &'static [&'static str],
}

/// The technical's: the wheel, the seat beside it, the bed's four (its own
/// cab has no moves out, the 4x4's are taken; their cabs are alike).
const TECHNICAL: [Place; 6] = [
    Place {
        sit: r"4x4_technical\1stge_fulb_driving_nodir_nowep_4x4tech.mab",
        exit: Some(r"truck\4x4\alert\exits\3rdge_fulbst_alert_driverexit4x4_v00_-090lt_ak74.mab"),
        entry: &[
            r"truck\4x4\alert\entries\onspot\3rdge_fulbst_idlealert_driverenter4x4_v01_000lt_ak74.mab",
            r"truck\4x4\alert\entries\onspot\3rdge_fulbst_idlealert_driverenter4x4_v01_090lt_ak74.mab",
            r"truck\4x4\alert\entries\onspot\3rdge_fulbst_idlealert_driverenter4x4_v01_090lt_nowep.mab",
        ],
    },
    Place {
        sit: r"4x4_technical\1stge_fulb_passenger_nodir_nowep_4x4tech.mab",
        exit: Some(r"truck\4x4\alert\exits\3rdge_fulbst_alert_passengerexit4x4_v00_+090rt_ak74.mab"),
        entry: &[
            r"truck\4x4\alert\entries\onspot\3rdge_fulbst_idlealert_passengerenter4x4_v01_000rt_ak74.mab",
            r"truck\4x4\alert\entries\onspot\3rdge_fulbst_idlealert_passengerenter4x4_v01_090rt_ak74.mab",
            r"truck\4x4\alert\entries\onspot\3rdge_fulbst_idlealert_passengerenter4x4_v01_090rt_nowep.mab",
        ],
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionl1_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionl1_4x4tech_exit_+000fw_ak74.mab"),
        entry: &[
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionl1_4x4tech_entry_+000fw_ak74.mab",
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionl1_4x4tech_entry_+000fw_noweap.mab",
        ],
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionr1_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionr1_4x4tech_exit_+000fw_ak74.mab"),
        entry: &[
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionr1_4x4tech_entry_+000fw_ak74.mab",
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionr1_4x4tech_entry_+000fw_noweap.mab",
        ],
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionl2_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionl2_4x4tech_exit_+000fw_ak74.mab"),
        entry: &[
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionl2_4x4tech_entry_+000fw_ak74.mab",
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionl2_4x4tech_entry_+000fw_noweap.mab",
        ],
    },
    Place {
        sit: r"truck\4x4technical\alert\idles\3rdge_fulb_alert_reinfpositionr2_4x4tech_idle_ak74.mab",
        exit: Some(r"truck\4x4technical\alert\exits\reinforcement\3rdge_fulb_alert_reinfpositionr2_4x4tech_exit_+000fw_ak74.mab"),
        entry: &[
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionr2_4x4tech_entry_+000fw_ak74.mab",
            r"truck\4x4technical\alert\entries\reinforcement\3rdge_fulb_alert_reinfpositionr2_4x4tech_entry_+000fw_noweap.mab",
        ],
    },
];

/// The zodiac's: its driver, and the benches on its right.
const BOAT: [Place; 3] = [
    Place {
        sit: r"boat\zodiac\alert\idles\3rdge_fulbst_driver_idlealert_zodiac_+000fw_nowep.mab",
        exit: None,
        entry: &[
            r"boat\zodiac\alert\entry\3rdge_fulbst_driver_enteralert_zodiac_+090rt_nowep.mab",
            r"boat\zodiac\alert\entry\3rdge_fulbst_driver_enteralert_zodiac_-090lt_nowep.mab",
        ],
    },
    Place {
        sit: r"boat\zodiac\alert\idles\3rdge_fulbst_passright01_idlealert_zodiac_+000fw_nowep.mab",
        exit: None,
        entry: &[
            r"boat\zodiac\alert\entry\3rdge_fulbst_passright01_enteralert_zodiac_+090rt_nowep.mab",
            r"boat\zodiac\alert\entry\3rdge_fulbst_passright01_enteralert_zodiac_-090lt_nowep.mab",
        ],
    },
    Place {
        sit: r"boat\zodiac\alert\idles\3rdge_fulbst_passright02_idlealert_zodiac_+000fw_nowep.mab",
        exit: None,
        entry: &[
            r"boat\zodiac\alert\entry\3rdge_fulbst_passright02_enteralert_zodiac_+090rt_nowep.mab",
            r"boat\zodiac\alert\entry\3rdge_fulbst_passright02_enteralert_zodiac_-090lt_nowep.mab",
        ],
    },
];

/// The move out of a story vehicle's seat, by its kind and index (the
/// cab's: the wheel and the seat beside it), where the game has one.
fn seat_exit(vehicle: &str, kind: i32, index: i32) -> Option<&'static str> {
    match (vehicle, kind, index) {
        ("4x4" | "Technical", 1, 0) => Some(r"truck\4x4\alert\exits\3rdge_fulbst_alert_driverexit4x4_v00_-090lt_ak74.mab"),
        ("4x4" | "Technical", 3, 0) => Some(r"truck\4x4\alert\exits\3rdge_fulbst_alert_passengerexit4x4_v00_+090rt_ak74.mab"),
        ("Truck", 1, 0) => Some(r"truck\bigtruck\alert\exits\3rdge_fulbst_driveralert_driverexitbigtruck_v01_090lt_ak74.mab"),
        ("Truck", 3, 0) => Some(r"truck\bigtruck\alert\exits\3rdge_fulbst_passengeralert_passengerexitbigtruck_v01_090rt_ak74.mab"),
        ("Sedan" | "Hatchback", 1, 0) => Some(r"car\alert\exits\3rdge_fulbst_alert_driverexitcivcar_v00_000lt_ak74.mab"),
        ("Sedan" | "Hatchback", 3, 0) => Some(r"car\alert\exits\3rdge_fulbst_alert_passengerexitcivcar_v00_000rt_ak74.mab"),
        _ => None,
    }
}

/// The moves into a story vehicle's seat, by its kind and index (the cab's:
/// the wheel and the seat beside it; a zodiac's wheel), where the game has
/// them.
fn seat_entries(vehicle: &str, kind: i32, index: i32) -> &'static [&'static str] {
    match (vehicle, kind, index) {
        ("4x4" | "Technical", 1, 0) => TECHNICAL[0].entry,
        ("4x4" | "Technical", 3, 0) => TECHNICAL[1].entry,
        ("Truck", 1, 0) => &[r"truck\bigtruck\alert\entries\onspot\3rdge_fulbst_idlealert_driverenterbigtruck_v01_090lt_ak74.mab"],
        ("Truck", 3, 0) => &[r"truck\bigtruck\alert\entries\onspot\3rdge_fulbst_idlealert_passengerenterbigtruck_v01_090rt_ak74.mab"],
        ("Sedan" | "Hatchback", 1, 0) => &[
            r"car\alert\entry\weapon\3rdge_fulbst_alert_driverentrycivcar_v00_090lt_ak74.mab",
            r"car\relax\entries\noweapon\3rdcivm_fulbst_relax_driverentrycivcar_v00_000lt_nowep.mab",
        ],
        ("Sedan" | "Hatchback", 3, 0) => &[
            r"car\alert\entry\weapon\3rdge_fulbst_alert_passengerentrycivcar_v00_090rt_ak74.mab",
            r"car\relax\entries\noweapon\3rdcivm_fulbst_relax_passengerentrycivcar_v00_000rt_nowep.mab",
            r"car\relax\entries\noweapon\3rdcivm_fulbst_relax_passengerentrycivcar_v00_090rt_nowep.mab",
        ],
        ("Boat", 1, 0) => BOAT[0].entry,
        _ => &[],
    }
}

/// The moves in made for one's hands (a gun in them or not), else all.
fn for_hands(entries: &'static [&'static str], armed: bool) -> Vec<&'static str> {
    let unarmed = |p: &str| p.contains("nowep") || p.contains("noweap");
    let fit: Vec<_> = entries.iter().copied().filter(|p| unarmed(p) != armed).collect();
    if fit.is_empty() {
        entries.to_vec()
    } else {
        fit
    }
}

/// The frame a move in plays in: the seat's for one ending at nought (the
/// 4x4's, the technical's bed's), else the vehicle's own (the big truck's,
/// the car's and the zodiac's end at the seat already).
fn entry_frame(seat: Transform, entry: &fc3_formats::anim::Clip) -> Transform {
    if crate::cinema::clip_root(entry, entry.duration).translation.length() < 0.05 {
        seat
    } else {
        Transform::IDENTITY
    }
}

/// Whether a segment on the floor passes through the inside of a box.
fn crosses(p: Vec2, q: Vec2, lo: Vec2, hi: Vec2) -> bool {
    let d = q - p;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (pa, da, l, h) in [(p.x, d.x, lo.x, hi.x), (p.y, d.y, lo.y, hi.y)] {
        if da.abs() < 1e-6 {
            if pa <= l || pa >= h {
                return false;
            }
        } else {
            let (a, b) = ((l - pa) / da, (h - pa) / da);
            t0 = t0.max(a.min(b));
            t1 = t1.min(a.max(b));
            if t0 >= t1 {
                return false;
            }
        }
    }
    true
}

/// The way (in a vehicle's frame, waypoints on its floor ending at `to`)
/// from one point to another round its footprint: straight when nothing is
/// in the way, else by the footprint's corners the shorter way round.
fn route_round(from: Vec3, to: Vec3, (min, max): (Vec3, Vec3)) -> Vec<Vec3> {
    // (Kept off by a little less than one stands clear, the corners a
    // little more, so the way between them is clear.)
    let (lo, hi) = (Vec2::new(min.x, min.z) - Vec2::splat(CLEAR * 0.8), Vec2::new(max.x, max.z) + Vec2::splat(CLEAR * 0.8));
    let (clo, chi) = (Vec2::new(min.x, min.z) - Vec2::splat(CLEAR + 0.1), Vec2::new(max.x, max.z) + Vec2::splat(CLEAR + 0.1));
    // (A start in the footprint is reached from just outside it.)
    let door = clear_of(to, (min, max));
    let nodes = vec![from.xz(), clo, Vec2::new(chi.x, clo.y), chi, Vec2::new(clo.x, chi.y), door.xz()];
    let end = nodes.len() - 1;
    let open = |a: Vec2, b: Vec2| !crosses(a, b, lo, hi);
    let mut way = Vec::new();
    if open(nodes[0], nodes[end]) {
        way.push(door);
    } else {
        let (mut dist, mut prev, mut done) = (vec![f32::INFINITY; nodes.len()], vec![usize::MAX; nodes.len()], vec![false; nodes.len()]);
        dist[0] = 0.0;
        while let Some(u) = (0..nodes.len()).filter(|&i| !done[i] && dist[i].is_finite()).min_by(|&i, &j| dist[i].total_cmp(&dist[j])) {
            done[u] = true;
            for v in 0..nodes.len() {
                let d = dist[u] + nodes[u].distance(nodes[v]);
                if !done[v] && d < dist[v] && open(nodes[u], nodes[v]) {
                    dist[v] = d;
                    prev[v] = u;
                }
            }
        }
        let mut at = end;
        while at != 0 && at != usize::MAX {
            way.push(Vec3::new(nodes[at].x, to.y, nodes[at].y));
            at = prev[at];
        }
        way.reverse();
        if way.is_empty() {
            way.push(door);
        }
    }
    if way.last().is_none_or(|l| l.xz().distance(to.xz()) > 0.01) {
        way.push(to);
    }
    way
}

/// The frame a move out plays in: from the seat for one whose root starts
/// at nought (the 4x4's, the technical's bed's), else the vehicle's own (the
/// big truck's and the car's start at the seat already).
fn exit_frame(seat: Transform, exit: &fc3_formats::anim::Clip) -> Transform {
    let start = exit.root_motion.as_ref().map_or(Vec3::ZERO, |r| Vec3::from_array(r.start_translation));
    if start.length() < 0.05 {
        seat
    } else {
        Transform::IDENTITY
    }
}

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

/// A story character in a seat: its vehicle, where it sits (the seated
/// clip's root, in the vehicle's frame) and the seat's move out, for when it
/// leaves.
#[derive(Component)]
pub struct StorySeat {
    vehicle: Entity,
    seat: Transform,
    exit: Option<Handle<Fc3Clip>>,
}

/// A story character getting out of its seat on its move: as `GettingOut`.
#[derive(Component)]
pub struct StoryGettingOut {
    vehicle: Entity,
    seat: Transform,
    clip: Handle<Fc3Clip>,
    t: f32,
}

/// A story character the story put in a seat to get in by its move (a
/// script's `TakeSeat` not skipping it, a ride's crew): where it stood then,
/// and when (s), until its move in is chosen.
#[derive(Component)]
pub struct WillGetIn {
    pub from: Transform,
    pub since: f32,
}

/// A story character getting into its seat: walking round to where its
/// move in starts (by waypoints in the vehicle's frame), turning to face as
/// it does, then following it (in `frame`, the vehicle's or the seat's).
#[derive(Component)]
pub struct StoryGettingIn {
    pub vehicle: Entity,
    frame: Transform,
    clip: Handle<Fc3Clip>,
    way: Vec<Vec3>,
    feet: Vec3,
    yaw: f32,
    run: bool,
    walked: f32,
    t: Option<f32>,
    age: f32,
}

/// How far (m, round the vehicle) one walks to get in by its move; farther,
/// it is put in its seat. Beyond `RUN_IN` it runs there.
const WALK_IN: f32 = 30.0;
const RUN_IN: f32 = 8.0;
/// How long (s) a move in may take to load, and the way to it to walk.
const ENTRY_LOAD: f32 = 2.0;
const ENTRY_GIVE_UP: f32 = 30.0;

/// The move in for one standing at `feet` (in the world), with its way
/// there: of the loaded moves, the one facing most along the last stretch
/// of the walk (least turning at the door), the nearer if alike. None when
/// it stands in the footprint (put there to board) or too far off.
fn choose_entry(
    feet: Vec3,
    vt: &Transform,
    bounds: (Vec3, Vec3),
    seat: Transform,
    moves: &[(Handle<Fc3Clip>, &fc3_formats::anim::Clip)],
) -> Option<(Handle<Fc3Clip>, Transform, Vec<Vec3>, f32)> {
    let local = vt.rotation.inverse() * (feet - vt.translation);
    if clear_of(local, bounds).xz().distance(local.xz()) > 1e-3 {
        return None;
    }
    let best = moves
        .iter()
        .map(|(h, clip)| {
            let frame = entry_frame(seat, clip);
            let start = frame * crate::cinema::clip_root(clip, 0.0);
            let way = route_round(local, start.translation, bounds);
            let mut length = 0.0;
            let mut last = local;
            let mut along = Vec2::ZERO;
            for w in &way {
                length += last.xz().distance(w.xz());
                if last.xz().distance(w.xz()) > 0.05 {
                    along = (w.xz() - last.xz()).normalize();
                }
                last = *w;
            }
            let facing = (start.rotation * Vec3::NEG_Z).xz().normalize_or_zero();
            let score = (1.0 - facing.dot(along)) + 0.05 * length;
            (h.clone(), frame, way, length, score)
        })
        .min_by(|a, b| a.4.total_cmp(&b.4))?;
    (best.3 <= WALK_IN).then_some((best.0, best.1, best.2, best.3))
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
    vehicles: Query<(Entity, &Transform, &Vehicle)>,
    mut actors: Query<
        (Entity, &mut Transform, &crate::story_actors::StoryActor, Has<Sitting>, Option<&StorySeat>, Option<&WillGetIn>),
        (Without<Vehicle>, Without<StoryGettingIn>),
    >,
    mut seated: Local<std::collections::HashMap<String, Handle<Fc3Clip>>>,
    mut commands: Commands,
) {
    let Some(rider_clips) = rider_clips else { return };
    let mut load = |path: String| seated.entry(path.clone()).or_insert_with(|| server.load(cache_path(&fc3_formats::cache::anim_rel(&path)))).clone();
    for (e, mut at, actor, sitting, story_seat, will) in &mut actors {
        let seat = (!actor.dead && !actor.in_scene)
            .then(|| vehicles.iter().find_map(|(ve, vt, v)| v.seat_of(actor.id).map(|(i, kind, pose)| (ve, *vt, v, i, kind, pose))))
            .flatten()
            // (A gun's seat is its gunner's business: `turrets`.)
            .filter(|(.., kind, _)| *kind != 2);
        let Some((ve, vt, v, i, _, pose)) = seat else {
            if will.is_some() {
                commands.entity(e).remove::<WillGetIn>();
            }
            // Out of the seat: by its move, where it has one (not dead, nor
            // taken by a scene); else its own clips again.
            let out = story_seat.filter(|_| !actor.dead && !actor.in_scene).and_then(|s| s.exit.clone().map(|clip| (s, clip)));
            if let Some((s, clip)) = out.filter(|(_, clip)| clips.contains(clip)) {
                commands.entity(e).insert((StoryGettingOut { vehicle: s.vehicle, seat: s.seat, clip, t: 0.0 }, avian3d::prelude::ColliderDisabled)).remove::<(
                    StorySeat,
                    Sitting,
                )>(
                );
                continue;
            }
            if sitting {
                if let (Some(rig), false) = (actor.rig, actor.in_scene) {
                    commands.entity(rig).remove::<ClipOverride>();
                }
                commands.entity(e).remove::<(Sitting, StorySeat)>();
            }
            continue;
        };
        let occupant = v.occupant(i);
        let place = places(v.name())
            .iter()
            .filter_map(|p| rider_clips.clips.get(p.sit).map(|h| (p, h)))
            .filter_map(|(p, h)| clips.get(h).map(|c| (p, h, crate::cinema::clip_root(&c.0, 0.0).translation.distance(pose.translation))))
            .filter(|(.., d)| *d < SAME_SEAT)
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let (handle, exit) = match v.first_person_clip(i) {
            Some(path) => (Some(load(format!(r"graphics\characters\_common\animations\{path}"))), occupant.and_then(|o| seat_exit(v.name(), o.kind, o.index))),
            None => (place.map(|(_, h, _)| h.clone()), place.and_then(|(p, ..)| p.exit)),
        };
        let exit = exit.map(|path| load(format!("{PREFIX}{path}")));
        let Some((handle, clip)) = handle.and_then(|h| clips.get(&h).map(|c| (h.clone(), c))) else { continue };
        // Asked in by its move: round to the door and in, once the moves
        // have loaded (standing where it was meanwhile).
        if let Some(w) = will {
            let entries = match v.first_person_clip(i) {
                Some(_) => occupant.map_or(&[][..], |o| seat_entries(v.name(), o.kind, o.index)),
                None => place.map_or(&[][..], |(p, ..)| p.entry),
            };
            let handles: Vec<_> = for_hands(entries, actor.armed).into_iter().map(|p| load(format!("{PREFIX}{p}"))).collect();
            let moves: Vec<_> = handles.iter().filter_map(|h| clips.get(h).map(|c| (h.clone(), &*c.0))).collect();
            if moves.len() < handles.len() && time.elapsed_secs() - w.since < ENTRY_LOAD {
                *at = w.from;
                continue;
            }
            commands.entity(e).remove::<WillGetIn>();
            let feet = w.from.translation - Vec3::Y * HALF_HEIGHT;
            let seat = crate::cinema::clip_root(&clip.0, 0.0);
            if let Some((clip, frame, way, length)) = choose_entry(feet, &vt, v.bounds(), seat, &moves) {
                debug!("riders: story pawn {} walks {length:.1} m to get in by its move", actor.id);
                let (yaw, ..) = w.from.rotation.to_euler(EulerRot::YXZ);
                commands.entity(e).insert(StoryGettingIn { vehicle: ve, frame, clip, way, feet, yaw, run: length > RUN_IN, walked: 0.0, t: None, age: 0.0 });
                *at = w.from;
                continue;
            }
        }
        if story_seat.is_none_or(|s| s.vehicle != ve) {
            commands.entity(e).insert(StorySeat { vehicle: ve, seat: crate::cinema::clip_root(&clip.0, 0.0), exit });
        }
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
    let (Some(seated), Some(out)) = (clips.get(sit), clips.get(exit)) else { return false };
    let seat = exit_frame(crate::cinema::clip_root(&seated.0, 0.0), &out.0);
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

/// The story's characters getting in walk (or run) round to their move's
/// start on their own walk, turn as it starts, and follow it into the seat,
/// where its loop takes over. Broken off if they die, a scene takes them or
/// the seat is theirs no longer; put in the seat if the way takes too long.
#[allow(clippy::type_complexity)]
fn story_get_in(
    time: Res<Time>,
    nav: Res<crate::nav::NavMesh>,
    heights: Res<crate::terrain::Heightfield>,
    rider_clips: Option<Res<RiderClips>>,
    clips: Res<Assets<Fc3Clip>>,
    vehicles: Query<(&Transform, &Vehicle), Without<StoryGettingIn>>,
    mut entering: Query<(Entity, &mut StoryGettingIn, &mut Transform, &crate::story_actors::StoryActor), Without<Vehicle>>,
    rigs: Query<&crate::characters::CharacterRig>,
    mut commands: Commands,
) {
    let Some(rider_clips) = rider_clips else { return };
    let dt = time.delta_secs();
    for (e, mut g, mut at, actor) in &mut entering {
        g.age += dt;
        let vt = vehicles.get(g.vehicle).ok().filter(|(_, v)| v.seat_of(actor.id).is_some()).map(|(vt, _)| *vt);
        let clip = clips.get(&g.clip);
        let (Some(vt), Some(clip), false, false) = (vt, clip, actor.dead, actor.in_scene) else {
            if let (Some(rig), false) = (actor.rig, actor.in_scene) {
                commands.entity(rig).remove::<ClipOverride>();
            }
            commands.entity(e).remove::<StoryGettingIn>();
            continue;
        };
        let length = clip.0.duration;
        if let Some(t) = g.t {
            let t = t + dt;
            g.t = Some(t);
            *at = capsule(vt * g.frame * crate::cinema::clip_root(&clip.0, t.min(length)));
            if let Some(rig) = actor.rig {
                commands.entity(rig).insert(ClipOverride { clip: g.clip.clone(), skeleton: rider_clips.skeleton.clone(), time: t.min(length) });
            }
            if t >= length {
                commands.entity(e).remove::<StoryGettingIn>();
            }
            continue;
        }
        if g.age > ENTRY_GIVE_UP {
            debug!("riders: story pawn {} took too long getting to its door", actor.id);
            commands.entity(e).remove::<StoryGettingIn>();
            continue;
        }
        let start = vt * g.frame * crate::cinema::clip_root(&clip.0, 0.0);
        let walk = if let Some(next) = g.way.first().copied() {
            let target = vt.transform_point(next);
            let to = (target - g.feet).with_y(0.0);
            let step = if g.run { 4.0 } else { 1.6 } * dt;
            if to.length() <= step.max(0.05) {
                g.feet = target.with_y(g.feet.y);
                g.way.remove(0);
            } else {
                let dir = to.normalize();
                g.feet += dir * step;
                g.yaw = (-dir.x).atan2(-dir.z);
            }
            g.walked += dt;
            true
        } else {
            // At the start: turning to face as the move does, then into it.
            let (want, ..) = start.rotation.to_euler(EulerRot::YXZ);
            let diff = (want - g.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            let turn = 6.0 * dt;
            if diff.abs() <= turn {
                g.yaw = want;
                g.t = Some(0.0);
            } else {
                g.yaw += turn * diff.signum();
            }
            false
        };
        if let Some(y) = nav.height_at(g.feet).or_else(|| heights.height_at(g.feet)) {
            g.feet.y = y;
        }
        *at = Transform::from_translation(g.feet + Vec3::Y * HALF_HEIGHT).with_rotation(Quat::from_rotation_y(g.yaw));
        // On its own walk (or stood): a clip set aside for the move's, so
        // what else asks of the rig meanwhile can't fight it.
        let Some(rig) = actor.rig else { continue };
        let Ok(r) = rigs.get(rig) else { continue };
        let set = r.clips();
        let kind = match (walk, g.run) {
            (false, _) => crate::characters::AnimKind::Idle,
            (true, false) => crate::characters::AnimKind::Walk,
            (true, true) => crate::characters::AnimKind::Run,
        };
        let own = set.clips.get(&kind).and_then(|h| clips.get(h).map(|c| (h.clone(), c.0.duration.max(0.1))));
        if let Some((h, d)) = own {
            commands.entity(rig).insert(ClipOverride { clip: h, skeleton: set.skeleton.clone(), time: if walk { g.walked } else { g.age }.rem_euclid(d) });
        }
    }
}

/// `FC3_ENTRY_TEST=<s>`: that long in, the story character nearest the
/// player (standing, not in a scene) is asked to the wheel of the nearest of
/// the story's vehicles with a move in there, as a script's `TakeSeat`
/// would; the player turns to watch, captured with `--screenshot`.
#[allow(clippy::type_complexity)]
fn entry_test(
    time: Res<Time>,
    args: Res<crate::Args>,
    host: Option<NonSend<crate::story::StoryHost>>,
    mut seats: ResMut<crate::vehicles::StorySeatRequests>,
    mut players: Query<(&Transform, &mut crate::player::Player)>,
    actors: Query<(&Transform, &crate::story_actors::StoryActor), Without<crate::player::Player>>,
    vehicles: Query<(&Transform, &Vehicle), Without<crate::player::Player>>,
    mut state: Local<(Option<(u64, f32)>, u32)>,
    mut commands: Commands,
) {
    let Some(after) = std::env::var("FC3_ENTRY_TEST").ok().and_then(|s| s.parse::<f32>().ok()) else { return };
    let (Some(host), Ok((pt, mut p))) = (host, players.single_mut()) else { return };
    let now = time.elapsed_secs();
    let (asked, shots) = &mut *state;
    let Some((pawn, at)) = *asked else {
        if now < after || p.spawning {
            return;
        }
        let standing = |a: &crate::story_actors::StoryActor| !a.dead && !a.seated && !a.in_scene && a.rig.is_some();
        let pick = actors
            .iter()
            .filter(|(_, a)| standing(a))
            .flat_map(|(at, a)| {
                vehicles
                    .iter()
                    .filter(|(_, v)| host.story.is_loaded(v.story_id()))
                    .filter(|(_, v)| v.driver().is_none() && !seat_entries(v.name(), 1, 0).is_empty())
                    .map(move |(vt, v)| (a.id, v.story_id(), v.name(), at.translation, vt.translation))
            })
            .filter(|(.., a, v)| a.distance(*v) < 25.0)
            .min_by(|x, y| x.3.distance(pt.translation).total_cmp(&y.3.distance(pt.translation)));
        let Some((pawn, vehicle, name, a, v)) = pick else {
            if now as u32 != (now - time.delta_secs()) as u32 && (now as u32).is_multiple_of(5) {
                let standing: Vec<_> = actors.iter().filter(|(_, a)| standing(a)).map(|(t, _)| t.translation).collect();
                let free: Vec<_> = vehicles
                    .iter()
                    .filter(|(_, v)| v.driver().is_none() && !seat_entries(v.name(), 1, 0).is_empty())
                    .map(|(t, v)| (t.translation, host.story.is_loaded(v.story_id())))
                    .collect();
                let nearest = standing.iter().flat_map(|a| free.iter().map(move |(v, _)| a.distance(*v))).min_by(f32::total_cmp);
                info!(
                    "entry test: none yet: {} standing, {} free wheels ({} the story's), nearest {nearest:?} m",
                    standing.len(),
                    free.len(),
                    free.iter().filter(|(_, l)| *l).count()
                );
            }
            return;
        };
        info!("entry test: pawn {pawn} at {a:.1?} asked to the wheel of {name} {vehicle} at {v:.1?} ({:.1} m)", a.distance(v));
        seats.board(vehicle, pawn, true);
        let look = (a + v) * 0.5 - pt.translation;
        p.yaw = (-look.x).atan2(-look.z);
        *asked = Some((pawn, now));
        return;
    };
    // Watching: the character's place every half second, and captures.
    let t = now - at;
    if let Some((ct, _)) = actors.iter().find(|(_, a)| a.id == pawn) {
        let look = ct.translation - pt.translation;
        p.yaw = (-look.x).atan2(-look.z);
        if (t * 2.0) as u32 != ((t - time.delta_secs()) * 2.0) as u32 {
            info!("entry test: pawn {pawn} at {:.2?} t {t:.1}", ct.translation);
        }
    }
    let stem = args.screenshot.as_ref().map(|p| p.strip_suffix(".png").unwrap_or(p).to_string());
    if let Some(stem) = stem.filter(|_| *shots < 10 && t >= 1.0 + *shots as f32 * 1.0) {
        crate::camera::capture(&mut commands, format!("{stem}_in{shots}.png"));
        *shots += 1;
    }
}

/// `FC3_ENTRY_SHOTS=<dir>`: whoever gets into a seat by its move is watched
/// (the player turned to it) and captured every half second.
fn entry_shots(
    time: Res<Time>,
    entering: Query<(&Transform, &crate::story_actors::StoryActor), (With<StoryGettingIn>, Without<crate::player::Player>)>,
    mut players: Query<(&Transform, &mut crate::player::Player)>,
    (mut next, mut taken): (Local<f32>, Local<u32>),
    mut commands: Commands,
) {
    let Some(dir) = std::env::var_os("FC3_ENTRY_SHOTS") else { return };
    let (Some((at, actor)), Ok((pt, mut p))) = (entering.iter().next(), players.single_mut()) else { return };
    let look = at.translation - (pt.translation + Vec3::Y * 0.6);
    p.yaw = (-look.x).atan2(-look.z);
    p.pitch = look.y.atan2(Vec2::new(look.x, look.z).length().max(0.3));
    let now = time.elapsed_secs();
    if now < *next || *taken >= 40 {
        return;
    }
    *next = now + 0.5;
    crate::camera::capture(&mut commands, format!("{}/entry_{}_{:02}.png", dir.to_string_lossy(), actor.id, *taken));
    *taken += 1;
}

/// The story's characters getting out follow their seat's move; at its end
/// they stand on the ground clear of the vehicle, theirs to move again.
#[allow(clippy::type_complexity)]
fn story_get_out(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    rider_clips: Option<Res<RiderClips>>,
    clips: Res<Assets<Fc3Clip>>,
    vehicles: Query<(&Transform, &Vehicle), Without<StoryGettingOut>>,
    mut out: Query<(Entity, &mut StoryGettingOut, &mut Transform, &crate::story_actors::StoryActor), Without<Vehicle>>,
    mut commands: Commands,
) {
    let Some(rider_clips) = rider_clips else { return };
    let dt = time.delta_secs();
    for (e, mut g, mut at, actor) in &mut out {
        g.t += dt;
        let clip = clips.get(&g.clip);
        let length = clip.map_or(0.0, |c| c.0.duration);
        let vehicle = vehicles.get(g.vehicle).ok();
        if let (Some(clip), Some((vt, _))) = (clip, vehicle) {
            *at = capsule(*vt * exit_frame(g.seat, &clip.0) * crate::cinema::clip_root(&clip.0, g.t.min(length)));
        }
        let done = actor.dead || actor.in_scene || vehicle.is_none() || g.t >= length;
        if !done {
            if let Some(rig) = actor.rig {
                commands.entity(rig).insert(ClipOverride { clip: g.clip.clone(), skeleton: rider_clips.skeleton.clone(), time: g.t });
            }
            continue;
        }
        if let (Some(rig), false) = (actor.rig, actor.in_scene) {
            commands.entity(rig).remove::<ClipOverride>();
        }
        let mut feet = at.translation - Vec3::Y * HALF_HEIGHT;
        if let Some((vt, v)) = vehicle {
            let local = vt.rotation.inverse() * (feet - vt.translation);
            feet = vt.translation + vt.rotation * clear_of(local, v.bounds());
        }
        feet.y = heights.height_at(feet).unwrap_or(feet.y);
        at.translation = feet + Vec3::Y * HALF_HEIGHT;
        let (yaw, ..) = at.rotation.to_euler(EulerRot::YXZ);
        at.rotation = Quat::from_rotation_y(yaw);
        commands.entity(e).remove::<(StoryGettingOut, avian3d::prelude::ColliderDisabled)>();
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
    fn the_cab_seats_have_their_moves_out_and_a_gun_has_none() {
        assert!(seat_exit("4x4", 1, 0).is_some_and(|p| p.contains("driverexit4x4")));
        assert!(seat_exit("Truck", 3, 0).is_some_and(|p| p.contains("passengerexitbigtruck")));
        assert!(seat_exit("Sedan", 1, 0).is_some_and(|p| p.contains("driverexitcivcar")));
        assert_eq!(seat_exit("4x4", 2, 0), None);
        assert_eq!(seat_exit("Jet ski", 1, 0), None);
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
    fn the_cab_seats_and_the_beds_have_their_moves_in_for_either_hands() {
        assert!(seat_entries("Technical", 1, 0).iter().any(|p| p.contains("driverenter4x4")));
        assert!(seat_entries("Truck", 3, 0).iter().any(|p| p.contains("passengerenterbigtruck")));
        assert!(seat_entries("Boat", 1, 0).iter().any(|p| p.contains("driver_enteralert_zodiac")));
        assert!(seat_entries("Jet ski", 1, 0).is_empty());
        assert!(places("Technical").iter().chain(places("Boat")).all(|p| !p.entry.is_empty()));
        // A gun in hand: the armed moves; none: the unarmed, else any.
        assert!(for_hands(seat_entries("Sedan", 1, 0), true).iter().all(|p| p.contains("ak74")));
        assert!(for_hands(seat_entries("Sedan", 1, 0), false).iter().all(|p| p.contains("nowep")));
        assert_eq!(for_hands(seat_entries("Truck", 1, 0), false).len(), 1);
    }

    #[test]
    fn the_way_to_a_door_goes_round_the_vehicle_not_through_it() {
        let bounds = (Vec3::new(-1.0, 0.0, -2.5), Vec3::new(1.0, 2.0, 2.5));
        let (lo, hi) = (Vec2::new(-1.0 - CLEAR * 0.8, -2.5 - CLEAR * 0.8), Vec2::new(1.0 + CLEAR * 0.8, 2.5 + CLEAR * 0.8));
        let door = Vec3::new(-1.7, -0.9, -0.3);
        // From the door's side: straight there.
        assert_eq!(route_round(Vec3::new(-6.0, 0.0, 1.0), door, bounds), vec![door]);
        // From the far side, a little ahead: round the front, two corners.
        let from = Vec3::new(4.0, 0.0, -1.5);
        let way = route_round(from, door, bounds);
        assert_eq!(way.len(), 3, "{way:?}");
        assert!(way.iter().take(2).all(|w| w.z < -2.5), "{way:?}");
        let mut last = from;
        for w in &way {
            assert!(!crosses(last.xz(), w.xz(), lo, hi), "{last:?} -> {w:?}");
            last = *w;
        }
        assert_eq!(*way.last().unwrap(), door);
        // A start inside the footprint (a bed's, by the tailgate): reached
        // from just outside it.
        let tail = Vec3::new(0.2, 0.0, 2.3);
        let way = route_round(Vec3::new(0.0, 0.0, 8.0), tail, bounds);
        assert_eq!(way.len(), 2, "{way:?}");
        assert!(way[0].z >= 2.5 + CLEAR - 1e-4);
        assert_eq!(way[1], tail);
    }

    #[test]
    fn a_root_at_the_feet_puts_the_capsule_half_a_body_up_along_its_own_up() {
        let tilted = Transform::from_xyz(1.0, 2.0, 3.0).with_rotation(Quat::from_rotation_z(0.3));
        let c = capsule(tilted);
        assert!((c.translation - tilted.translation).length() - HALF_HEIGHT < 1e-4);
        assert!(c.translation.abs_diff_eq(tilted.translation + tilted.rotation * Vec3::Y * HALF_HEIGHT, 1e-5));
    }
}
