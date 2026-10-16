//! JAS_130 B60's seat switch (`CExoticCarChase`): from the 4x4's bed the
//! player climbs over the cab into the front passenger seat beside Liza as
//! the truck drives on. His view follows the gameplay's clip
//! (`ex_4x4_climb_backseat2front`), which is made in the 4x4's own frame:
//! it starts at the bed seat's eye, looking back, and ends at the front
//! seat's, looking ahead. On the way he fires twice through the windscreen
//! (the clip's `EXO_FirstHitWindshield` and `EXO_SecondHitWindshield`; only
//! heard: the windscreen isn't a part of ours to break). Seated in front,
//! the gameplay ends, and its scripts give him the 1911 for the rest of
//! the chase.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::cache_assets::{cache_path, Fc3Clip, Fc3Skeleton};
use crate::vehicles::{Driving, Vehicle};

pub struct CarChasePlugin;

impl Plugin for CarChasePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CarChase>().add_systems(Update, run.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

pub struct Start {
    pub id: u64,
    pub params: HashMap<String, String>,
}

/// The gameplay the story starts and stops (by id).
#[derive(Resource, Default)]
pub struct CarChase {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    active: Option<Climb>,
}

impl CarChase {
    /// A checkpoint reload: over, where he is.
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some(c) = &self.active {
            self.cancelled.push(c.id);
        }
    }
}

/// The climb under way.
struct Climb {
    id: u64,
    /// The vehicle (the story's id).
    vehicle: u64,
    clip: Handle<Fc3Clip>,
    t: f32,
    /// How long the clip has been loading (s).
    waited: f32,
}

pub(crate) const CLIP: &str = r"graphics\characters\_common\animations\exoticgameplay\ex_jas_4x4_climb_backseat2front\ex_4x4_climb_backseat2front.mab";
const SKELETON: &str = r"graphics\actors_\_skeleton_master\hips_ref.skeleton";
/// The longest wait (s) for the clip; without it he is moved over at once.
const LOAD_WAIT: f32 = 4.0;
/// How long (s) the view takes to go over from the seat's to the clip's.
const BLEND: f32 = 0.3;
/// The seat he ends in: the first passenger seat (the front one).
const FRONT: (i32, i32) = (3, 0);
/// The clip's shots through the windscreen.
const SHOTS: [&str; 2] = ["EXO_FirstHitWindshield", "EXO_SecondHitWindshield"];

#[allow(clippy::too_many_arguments)]
fn run(
    mut chase: ResMut<CarChase>,
    host: Option<NonSendMut<crate::story::StoryHost>>,
    mut driving: ResMut<Driving>,
    mut vehicles: Query<&mut Vehicle>,
    server: Res<AssetServer>,
    (clips, skeletons, mut skeleton): (Res<Assets<Fc3Clip>>, Res<Assets<Fc3Skeleton>>, Local<Option<Handle<Fc3Skeleton>>>),
    (time, mut sfx, mut commands): (Res<Time>, MessageWriter<crate::sound::PlaySfx>, Commands),
) {
    let Some(mut host) = host else { return };
    let cancelled = std::mem::take(&mut chase.cancelled);
    if chase.active.as_ref().is_some_and(|c| cancelled.contains(&c.id)) {
        info!("car chase: stopped");
        chase.active = None;
    }
    for start in std::mem::take(&mut chase.pending) {
        if cancelled.contains(&start.id) {
            continue;
        }
        let vehicle = start.params.get("Vehicle/EntityID").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        // (Not in that vehicle, there's no climb to make.)
        let aboard = driving.vehicle.and_then(|e| vehicles.get(e).ok()).is_some_and(|v| v.story_id() == vehicle);
        if !aboard {
            info!("car chase ({}): the player isn't in {vehicle}; over", start.id);
            host.story.gameplay_ended(start.id);
            continue;
        }
        info!("car chase ({}): climbing to the front of {vehicle}", start.id);
        let clip = server.load(cache_path(&fc3_formats::cache::anim_rel(CLIP)));
        chase.active = Some(Climb { id: start.id, vehicle, clip, t: 0.0, waited: 0.0 });
    }
    let skeleton = skeleton.get_or_insert_with(|| server.load(cache_path(&fc3_formats::cache::skeleton_rel(SKELETON)))).clone();
    let mut carried = None;
    if let Some(c) = chase.active.as_mut() {
        let dt = time.delta_secs();
        let loaded = clips.get(&c.clip).zip(skeletons.get(&skeleton));
        c.waited += dt;
        let over = match loaded {
            Some((clip, skel)) => {
                let before = c.t;
                c.t += dt;
                // FC3_CHASE_SHOTS=<dir> (debug): the view every second of the
                // climb.
                if let Some(dir) = std::env::var_os("FC3_CHASE_SHOTS").filter(|_| (before + 0.5).floor() != (c.t + 0.5).floor()) {
                    crate::camera::capture(
                        &mut commands,
                        std::path::Path::new(&dir).join(format!("climb_{:.0}.png", c.t * 10.0)).to_string_lossy().into_owned(),
                    );
                }
                for (_, name) in clip.0.events.iter().filter(|(at, _)| *at > before && *at <= c.t) {
                    if SHOTS.contains(&name.as_str()) {
                        sfx.write(crate::sound::PlaySfx { event: crate::sound::ev::PISTOL, at: None, volume: 1.0 });
                    }
                }
                for (_, sound) in clip.0.sounds.iter().filter(|(at, _)| *at >= before && *at < c.t) {
                    sfx.write(crate::sound::PlaySfx { event: *sound, at: None, volume: 1.0 });
                }
                let t = c.t.min(clip.0.duration);
                carried = crate::cinema::bone_world(&skel.0, &clip.0, t, "Camera").map(|camera| {
                    let (_, look, eye) = (crate::cinema::clip_root(&clip.0, t).to_matrix() * camera).to_scale_rotation_translation();
                    (eye, look * Vec3::NEG_Z, (c.t / BLEND).min(1.0))
                });
                c.t >= clip.0.duration
            }
            None => c.waited > LOAD_WAIT,
        };
        if over {
            let c = chase.active.take().unwrap();
            // In the front seat now.
            let seat =
                driving.vehicle.and_then(|e| vehicles.get_mut(e).ok()).filter(|v| v.story_id() == c.vehicle).and_then(|mut v| v.move_player(FRONT.0, FRONT.1));
            if let Some(to) = seat {
                driving.seat = to;
            }
            info!("car chase ({}): over, {}", c.id, if seat.is_some() { "in the front seat" } else { "the front seat taken" });
            host.story.gameplay_ended(c.id);
            carried = None;
        } else if !driving.first_person {
            driving.first_person = true;
        }
    }
    if driving.carried != carried {
        driving.carried = carried;
    }
}
