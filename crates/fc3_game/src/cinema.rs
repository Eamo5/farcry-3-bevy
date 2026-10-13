//! In-engine cinematics: the game's sequences (`.cseq`, converted into the
//! cache) played in the world when the story asks.
//!
//! A sequence plays from a scene entity: its track groups bind to the
//! story's characters and props (the scene entity names which), the player
//! group is the player, overlay tracks are unbound. Characters are posed by
//! their clips at the scene's place (clip root motion is in the scene
//! entity's frame); the view is the bound camera's path while a camera cut
//! holds it, else the player clip's `Camera` bone (most scenes are first
//! person); lines are spoken from their speakers with their subtitles and
//! the soundtrack plays; faces play the scene's facial tracks (`faces`), and
//! props the groups hold move on their clips. The story hears `Started`,
//! `Finished`, `FinishedLoop`, `Stopped`, `Skipped` and sync points as the
//! engine sends them. Lights and screen effects aren't played yet.

use std::collections::HashMap;

use bevy::prelude::*;
use fc3_formats::cinema::{self, ClipKind, GroupKind, Sequence};

use crate::cache_assets::{cache_path, Fc3Clip, Fc3Skeleton};
use crate::characters::ClipOverride;
use crate::data::{to_bevy, CacheInfo};
use crate::player::{Player, PlayerCamera, HALF_HEIGHT};
use crate::story::StoryHost;
use crate::story_actors::StoryActor;

pub struct CinemaPlugin;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CinemaSet;

impl Plugin for CinemaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cinema>()
            .add_systems(Update, (start_scenes, play_scenes).chain().in_set(CinemaSet).after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PostUpdate, scene_view.before(TransformSystems::Propagate));
    }
}

/// The human skeletons clips target.
const MASTER_SKELETON: &str = r"graphics\actors_\_skeleton_master\hips_ref.skeleton";
const NARRATIVE_SKELETON: &str = r"graphics\actors_\_skeleton_master\narrative_hips_ref.skeleton";

/// A sequence request from the story: `play`, `play_from_start`,
/// `play_at_sync_point`, `stop`, `skip` or `pause`.
pub struct Request {
    pub mode: String,
    pub scene: u64,
    /// The sequence as Domino names it.
    pub file: String,
    /// A sync point to start from, by index or name.
    pub sync: Option<String>,
    /// The original manager's meta-sequence id, which owns looping scenes.
    pub meta: Option<u64>,
}

#[derive(Resource, Default)]
pub struct Cinema {
    pub requests: Vec<Request>,
    playing: Vec<Playing>,
    /// Scenes that ended this frame (their characters go back).
    released: Vec<Playing>,
    /// The view while a scene holds it: world transform and vertical field
    /// of view (degrees).
    pub view: Option<(Transform, f32)>,
    /// A player track owns the body; an establishing shot only owns the view.
    pub view_moves_player: bool,
    /// How far the player may turn his head within a first-person view (a
    /// player-camera clip's limits), if at all.
    pub head_look: Option<fc3_formats::cinema::HeadLook>,
    /// Sequences the game makes itself (a QTE's phases), to start.
    built: Vec<Built>,
    /// Opt-in completion receipts for gameplay-owned scenes. Disappearance
    /// from `progress` alone can also mean cancellation, not success.
    built_results: HashMap<String, bool>,
}

/// A sequence made by the game (not a file): played like the story's, from
/// `origin`, its track groups bound by uid to the story's characters.
pub struct Built {
    pub name: String,
    pub seq: Sequence,
    pub origin: Transform,
    pub bindings: HashMap<u64, u64>,
}

impl Cinema {
    /// Drops every scene (a reload).
    pub fn reset(&mut self) {
        self.requests.clear();
        self.playing.clear();
        self.released.clear();
        self.built.clear();
        self.built_results.clear();
        self.view = None;
    }

    /// Plays a sequence the game made.
    pub fn play_built(&mut self, b: Built) {
        self.built.push(b);
    }

    pub fn play_built_tracked(&mut self, b: Built) {
        self.built_results.insert(b.name.clone(), false);
        self.play_built(b);
    }

    /// A normal end is acknowledged once. Stop/reset never produces success.
    pub fn take_built_finished(&mut self, name: &str) -> bool {
        if self.built_results.get(name) != Some(&true) { return false; }
        self.built_results.remove(name);
        true
    }

    /// A built sequence's time played and length, while it plays (or waits
    /// to start).
    pub fn progress(&self, name: &str) -> Option<(f32, f32)> {
        self.playing
            .iter()
            .find(|p| p.scene == 0 && p.file == name)
            .map(|p| (p.t, p.seq.duration))
            .or_else(|| self.built.iter().find(|b| b.name == name).map(|b| (0.0, b.seq.duration)))
    }

    /// Ends a built sequence now.
    pub fn stop_built(&mut self, name: &str) {
        self.built_results.remove(name);
        self.built.retain(|b| b.name != name);
        if let Some(i) = self.playing.iter().position(|p| p.scene == 0 && p.file == name) {
            let p = self.playing.remove(i);
            self.released.push(p);
        }
    }

    /// Stops every sequence owned by a meta-sequence, releasing its actors
    /// through the same path as a normal stop. Built QTEs have no owner.
    fn stop_meta(&mut self, meta: u64) -> Vec<(u64, String)> {
        let mut stopped = Vec::new();
        let mut i = 0;
        while i < self.playing.len() {
            if self.playing[i].meta == Some(meta) {
                let p = self.playing.remove(i);
                stopped.push((p.scene, p.file.clone()));
                self.released.push(p);
            } else {
                i += 1;
            }
        }
        stopped
    }

    /// A cutscene is playing (input, HUD and weapon are the scene's).
    pub fn cutscene(&self) -> bool {
        self.playing.iter().any(|p| p.seq.cutscene)
    }

    /// Scene-owned props must not simultaneously fall or be driven by physics.
    pub fn owns_entity(&self, id: u64) -> bool {
        self.playing.iter().any(|p| p.bindings.values().any(|&entity| entity == id))
    }

    /// A scene plays the player's own body (a player group with tracks)
    /// and doesn't have this vehicle in it: he's out of it for the scene.
    pub fn plays_player_without(&self, vehicle: u64) -> bool {
        self.playing
            .iter()
            .any(|p| p.seq.groups.iter().any(|g| g.kind == GroupKind::Player && !g.clips.is_empty()) && !p.bindings.values().any(|&e| e == vehicle))
    }
}

struct Playing {
    scene: u64,
    meta: Option<u64>,
    file: String,
    seq: Sequence,
    t: f32,
    paused: bool,
    /// A dispatched pausing cue; excluded when ordinary playback resumes.
    paused_cue: bool,
    /// The scene entity's place (Bevy space).
    origin: Transform,
    /// Movement tracks retain the bound prop's initial placement. Capture
    /// their authored frame once, so pauses/restarts do not accumulate motion.
    movement_frames: HashMap<u64, Transform>,
    /// Track group uid -> world entity id.
    bindings: HashMap<u64, u64>,
    /// The scene's exposed entity anchor (characters can animate in its frame).
    anchor: Option<u64>,
    clips: HashMap<String, Handle<Fc3Clip>>,
    /// Seconds spent waiting for clips to load.
    waited: f32,
}

/// Stop both pose sampling and event dispatch at the first pausing cue.
/// The half-open event interval includes that cue but excludes everything
/// after it. Ordinary resume advances its lower bound by one float ULP.
fn advance_to_pause(seq: &Sequence, from: f32, dt: f32) -> (f32, f32, bool) {
    if !dt.is_finite() || dt <= 0.0 { return (from, from, false); }
    let end = from + dt;
    let pause = seq.sync_points.iter().filter(|s| s.kind != 1 && s.pause && s.time >= from && s.time <= end)
        .map(|s| s.time).min_by(f32::total_cmp);
    match pause {
        Some(t) => (t, t.next_up(), true),
        None => (end, end, false),
    }
}

/// A scene entity's bindings for one of its sequences.
fn bindings(data: &fc3_story::data::StoryData, scene: u64, archive: &str) -> HashMap<u64, u64> {
    let mut out = HashMap::new();
    let Some(e) = data.entities.get(&scene) else { return out };
    let Some(c) = e.component("CCinematicSceneComponent") else { return out };
    let mut entries: Vec<&fc3_formats::fcb::Object> = Vec::new();
    c.walk(&mut |o, _| {
        if o.str_h(0xB1C2_6EBB).is_some() {
            entries.push(o);
        }
    });
    for entry in entries {
        let path = entry.str_h(0xB1C2_6EBB).unwrap_or_default().to_ascii_lowercase();
        if path != archive {
            continue;
        }
        entry.walk(&mut |b, _| {
            if let (Some(entity), Some(track)) = (b.u64("EntityID"), b.u64("TrackID")) {
                out.insert(track, entity);
            }
        });
    }
    out
}

fn anchor_of(data: &fc3_story::data::StoryData, scene: u64, archive: &str) -> Option<u64> {
    let c = data.entities.get(&scene)?.component("CCinematicSceneComponent")?;
    let mut anchor = None;
    c.walk(&mut |entry, _| {
        if entry.str_h(0xB1C2_6EBB).is_some_and(|p| p.eq_ignore_ascii_case(archive)) {
            anchor = entry.child("EntityAnchors").and_then(|a| a.children.iter().find_map(|a| a.u64("ID")));
        }
    });
    anchor
}

fn origin_of(data: &fc3_story::data::StoryData, scene: u64) -> Transform {
    data.entities
        .get(&scene)
        .map(|e| Transform { translation: to_bevy(e.pos), rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)), scale: Vec3::ONE })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn start_scenes(host: Option<NonSendMut<StoryHost>>, mut cinema: ResMut<Cinema>, info: Res<CacheInfo>, server: Res<AssetServer>) {
    let Some(mut host) = host else {
        cinema.requests.clear();
        return;
    };
    let requests = std::mem::take(&mut cinema.requests);
    let data = host.story.data();
    for b in std::mem::take(&mut cinema.built) {
        let clips = clip_handles(&b.seq, &server);
        debug!("cinema: built {} ({:.1} s)", b.name, b.seq.duration);
        cinema.playing.push(Playing { scene: 0, meta: None, file: b.name, bindings: b.bindings, anchor: None, origin: b.origin, movement_frames: HashMap::new(), seq: b.seq, t: 0.0, paused: false, paused_cue: false, clips, waited: 0.0 });
    }
    for r in requests {
        debug!("cinema request: {} {} {} {:?}", r.mode, r.scene, r.file, r.sync);
        if r.mode == "stop_meta" {
            if let Some(meta) = r.meta {
                for (scene, file) in cinema.stop_meta(meta) {
                    host.story.sequence_event(&scene.to_string(), &file, "Stopped", None);
                }
            }
            continue;
        }
        let archive = cinema::archive_path(&r.file);
        let found = cinema.playing.iter().position(|p| p.scene == r.scene && p.file.eq_ignore_ascii_case(&r.file));
        match r.mode.as_str() {
            "pause" => {
                if let Some(i) = found {
                    cinema.playing[i].paused = true;
                    host.story.sequence_event(&r.scene.to_string(), &r.file, "Paused", None);
                }
            }
            "stop" | "skip" => {
                let event = if r.mode == "stop" { "Stopped" } else { "Skipped" };
                match found {
                    Some(i) => {
                        let p = cinema.playing.remove(i);
                        host.story.sequence_event(&p.scene.to_string(), &p.file, event, None);
                        cinema.released.push(p);
                    }
                    // Stopped from one of its last sync points, as it ended:
                    // the scripts still hear it.
                    None => host.story.sequence_event(&r.scene.to_string(), &r.file, event, None),
                }
            }
            _ => {
                if let Some(i) = found {
                    // Resume (or restart, or jump to a sync point).
                    let p = &mut cinema.playing[i];
                    if r.meta.is_some() {
                        p.meta = r.meta;
                    }
                    match (r.mode.as_str(), r.sync.as_deref()) {
                        ("play_from_start", _) => p.t = 0.0,
                        ("play_at_sync_point", Some(sp)) => p.t = sync_time(&p.seq, sp),
                        _ if p.paused_cue => p.t = p.t.next_up(),
                        _ => {}
                    }
                    p.paused = false;
                    p.paused_cue = false;
                    continue;
                }
                let text = std::fs::read_to_string(info.root.join(cinema::sequence_rel(&archive))).ok();
                let Some(seq) = text.as_deref().and_then(Sequence::from_text) else {
                    // Not converted: it starts and ends at once.
                    warn!("cinema: no sequence {archive}");
                    host.story.sequence_event(&r.scene.to_string(), &r.file, "Started", None);
                    host.story.sequence_event(&r.scene.to_string(), &r.file, "Finished", None);
                    continue;
                };
                let clips = clip_handles(&seq, &server);
                let t = match (r.mode.as_str(), r.sync.as_deref()) {
                    ("play_at_sync_point", Some(sp)) => sync_time(&seq, sp),
                    _ => 0.0,
                };
                info!("cinema: {} ({:.0} s, {} groups)", seq.name, seq.duration, seq.groups.len());
                debug!(
                    "cinema: {} cutscene {}, sync points {:?}",
                    seq.name,
                    seq.cutscene,
                    seq.sync_points.iter().map(|s| (s.name.as_str(), s.time, s.kind, s.pause)).collect::<Vec<_>>()
                );
                let playing = Playing {
                    scene: r.scene,
                    meta: r.meta,
                    file: r.file.clone(),
                    bindings: bindings(&data, r.scene, &archive),
                    anchor: anchor_of(&data, r.scene, &archive),
                    origin: origin_of(&data, r.scene),
                    movement_frames: HashMap::new(),
                    seq,
                    t,
                    paused: false,
                    paused_cue: false,
                    clips,
                    waited: 0.0,
                };
                host.story.sequence_event(&r.scene.to_string(), &r.file, "Started", None);
                cinema.playing.push(playing);
            }
        }
    }
}

/// A sequence's animation clips, loading.
fn clip_handles(seq: &Sequence, server: &AssetServer) -> HashMap<String, Handle<Fc3Clip>> {
    let mut clips = HashMap::new();
    for g in &seq.groups {
        for c in &g.clips {
            if let ClipKind::Animation { source, .. } | ClipKind::SimpleAnimation { source, .. } = &c.kind {
                clips.entry(source.clone()).or_insert_with(|| server.load(cache_path(&fc3_formats::cache::anim_rel(source))));
            }
        }
    }
    clips
}

/// Where playing resumes from a sync point (by name, or by index among the
/// entry points): just after it, so it doesn't fire again.
fn sync_time(seq: &Sequence, sp: &str) -> f32 {
    seq.sync_points
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(sp))
        .or_else(|| sp.parse::<usize>().ok().and_then(|i| seq.sync_points.iter().filter(|s| s.kind >= 1).nth(i)))
        .map(|s| s.time + 1e-4)
        .unwrap_or(0.0)
}

/// The clip of a group playing at `t`, and the time into it.
fn clip_at(clips: &[cinema::Clip], t: f32, duration: impl Fn(&str) -> Option<f32>) -> Option<(&str, f32)> {
    clips.iter().rev().find_map(|c| match &c.kind {
        ClipKind::Animation { source, from, .. } | ClipKind::SimpleAnimation { source, from, .. } if t >= c.start && t <= c.start + c.duration.max(0.05) => {
            let (from, local) = (*from as f32 / 30.0, t - c.start);
            // A clip kept only for the range the scene plays (a segmented
            // one) starts where the scene's range does.
            let trimmed = from > 0.0 && duration(source).is_some_and(|d| d + 0.1 < from + c.duration);
            Some((source.as_str(), if trimmed { local } else { from + local }))
        }
        _ => None,
    })
}

/// Where a clip puts its character at `t`, in the scene's frame: its first
/// root key (where it starts in the scene it was authored in) and the root
/// motion since.
/// The facial clip a group plays at `t`: its source, start frame and the
/// time into the clip since it began.
fn facial_at(clips: &[cinema::Clip], t: f32) -> Option<(&str, u32, f32)> {
    clips.iter().find_map(|c| match &c.kind {
        ClipKind::Facial { source, from, .. } if t >= c.start && t <= c.start + c.duration.max(0.05) => Some((source.as_str(), *from, t - c.start)),
        _ => None,
    })
}

pub(crate) fn clip_root(clip: &fc3_formats::anim::Clip, t: f32) -> Transform {
    match &clip.root_motion {
        Some(rm) => {
            let (tr, q) = rm.sample(t.clamp(0.0, clip.duration));
            let start = Transform { translation: Vec3::from_array(rm.start_translation), rotation: Quat::from_array(rm.start_rotation), scale: Vec3::ONE };
            start * Transform { translation: Vec3::from_array(tr), rotation: Quat::from_array(q), scale: Vec3::ONE }
        }
        None => Transform::IDENTITY,
    }
}

/// Recorded poses hold their first/last key. The movement track often ends
/// before its scene's dialogue/exit animation, and must not release the car.
fn vehicle_pose(keys: &[cinema::VehicleKey], t: f32) -> Option<(Transform, f32, f32)> {
    let pose = |k: &cinema::VehicleKey| Transform { translation: to_bevy(k.pos),
        rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(k.angles)), scale: Vec3::ONE };
    let first = keys.first()?;
    if t <= first.time { return Some((pose(first), first.gas, first.steering)); }
    let last = keys.last()?;
    if t >= last.time { return Some((pose(last), last.gas, last.steering)); }
    let i = keys.partition_point(|k| k.time < t);
    let Some(b) = keys.get(i) else {
        let last = keys.last()?;
        return Some((pose(last), last.gas, last.steering));
    };
    let a = &keys[i - 1];
    let w = ((t - a.time) / (b.time - a.time).max(1e-6)).clamp(0.0, 1.0);
    let (a_pose, b_pose) = (pose(a), pose(b));
    Some((Transform { translation: a_pose.translation.lerp(b_pose.translation, w),
        rotation: a_pose.rotation.slerp(b_pose.rotation, w), scale: Vec3::ONE },
        a.gas + (b.gas - a.gas) * w, a.steering + (b.steering - a.steering) * w))
}

fn recorded_pose(g: &cinema::Group, t: f32) -> Option<(Transform, f32, f32)> {
    g.clips.iter().find_map(|c| match &c.kind {
        ClipKind::VehicleMovement { keys } => vehicle_pose(keys, t - c.start),
        ClipKind::Movement { keys } => movement_pose(keys, t - c.start).map(|p| (p, 0.0, 0.0)),
        _ => None,
    })
}

fn movement_pose(keys: &[cinema::MovementKey], t: f32) -> Option<Transform> {
    let pose = |k: &cinema::MovementKey| Transform { translation: to_bevy(k.pos),
        rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(k.angles)), scale: Vec3::ONE };
    let first = keys.first()?;
    if t <= first.time { return Some(pose(first)); }
    let last = keys.last()?;
    if t >= last.time { return Some(pose(last)); }
    let i = keys.partition_point(|k| k.time < t);
    let (a, b) = (&keys[i - 1], &keys[i]);
    let w = ((t - a.time) / (b.time - a.time).max(1e-6)).clamp(0.0, 1.0);
    let (a, b) = (pose(a), pose(b));
    Some(Transform { translation: a.translation.lerp(b.translation, w), rotation: a.rotation.slerp(b.rotation, w), scale: Vec3::ONE })
}

fn anchored_at(clips: &[cinema::Clip], t: f32) -> bool {
    clips.iter().rev().find_map(|c| match c.kind {
        ClipKind::Animation { anchored, .. } if t >= c.start && t <= c.start + c.duration.max(0.05) => Some(anchored),
        _ => None,
    }).unwrap_or(false)
}

/// The world transform of a skeleton bone posed by a clip.
pub(crate) fn bone_world(skel: &fc3_formats::anim::Skeleton, clip: &fc3_formats::anim::Clip, t: f32, bone: &str) -> Option<Mat4> {
    let idx = skel.find(bone)?;
    let nodes: Vec<fc3_formats::anim::Node> = skel
        .bones
        .iter()
        .enumerate()
        .map(|(i, b)| fc3_formats::anim::Node {
            name: b.name.clone(),
            name_hash: b.name_hash,
            parent: b.parent,
            first_child: -1,
            next_sibling: -1,
            translation: b.translation,
            rotation: b.rotation,
            scale: [1.0; 3],
            bind_index: i as i32,
        })
        .collect();
    let locals = fc3_formats::anim::pose_locals(&nodes, skel, clip, t.clamp(0.0, clip.duration));
    let worlds = fc3_formats::anim::compose_worlds(&nodes, &locals, &Mat4::IDENTITY.to_cols_array());
    worlds.get(idx).map(Mat4::from_cols_array)
}

#[allow(clippy::too_many_arguments)]
fn play_scenes(
    host: Option<NonSendMut<StoryHost>>,
    mut cinema: ResMut<Cinema>,
    time: Res<Time>,
    server: Res<AssetServer>,
    clips: Res<Assets<Fc3Clip>>,
    skeletons: Res<Assets<Fc3Skeleton>>,
    mut skel_handles: Local<Option<(Handle<Fc3Skeleton>, Handle<Fc3Skeleton>)>>,
    mut actors: Query<(&mut StoryActor, &mut Transform)>,
    mut commands: Commands,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut ui: ResMut<crate::story::StoryUi>,
    keys: Res<ButtonInput<KeyCode>>,
    mut hold: ResMut<crate::fp::Cinematic>,
    (mut faces, mut library): (Query<&mut crate::faces::Face>, Option<ResMut<crate::faces::FaceLibrary>>),
    mut props: Query<(&crate::objects::PlacedId, &mut Transform, Option<&mut crate::vehicles::Vehicle>), Without<StoryActor>>,
) {
    let Some(mut host) = host else { return };
    // A cutscene holds the player still.
    if cinema.cutscene() {
        hold.0 = hold.0.max(0.2);
    }
    let (master, narrative) = skel_handles
        .get_or_insert_with(|| {
            (
                server.load(cache_path(&fc3_formats::cache::skeleton_rel(MASTER_SKELETON))),
                server.load(cache_path(&fc3_formats::cache::skeleton_rel(NARRATIVE_SKELETON))),
            )
        })
        .clone();
    let data = host.story.data();
    let dt = time.delta_secs().min(0.1);
    // Scenes that ended: their characters go back to the world.
    let released = std::mem::take(&mut cinema.released);
    let mut view = None;
    let mut view_moves_player = false;
    let mut head_look = None;
    let cinema = &mut *cinema;
    let mut finished = Vec::new();
    for (i, p) in cinema.playing.iter_mut().enumerate() {
        // Hold the start until the clips are in (briefly).
        let loading = p.clips.values().any(|h| !crate::cache_assets::settled(&server, h.id()));
        if loading && p.waited < 3.0 {
            p.waited += dt;
            continue;
        }
        // Skipping a cutscene (the original's skip key; FC3_STORY_SKIP_SCENES
        // for tests).
        // The sync points skipped over still reach the scripts (in order, none
        // pausing), as their graphs go on from them.
        // Gameplay transitions must play through before their owner receives
        // a completion receipt; the story cutscene shortcut cannot finish them.
        let tracked = p.scene == 0 && cinema.built_results.contains_key(&p.file);
        if p.seq.cutscene && !tracked && (keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) || std::env::var("FC3_STORY_SKIP_SCENES").is_ok()) {
            let from = p.t;
            p.t = p.seq.duration;
            p.paused = false;
            for s in p.seq.sync_points.iter().filter(|s| s.kind != 1 && s.time >= from && s.time < p.seq.duration) {
                host.story.sequence_event(&p.scene.to_string(), &p.file, "SyncPointReached", Some(&s.name));
            }
        }
        let prev = p.t;
        let (sample, b, pause) = advance_to_pause(&p.seq, prev, if p.paused { 0.0 } else { dt });
        p.t = sample;
        if pause { p.paused = true; p.paused_cue = true; }
        let a = prev;
        let crossed = |s: f32| s >= a && s < b || (a == 0.0 && s == 0.0 && b > 0.0);
        // Keep the elapsed interval for event crossings, but sample the
        // endpoint on the last frame instead of falling outside every clip.
        // This is also the body pose handed back by player-track scenes.
        if !p.seq.looping { p.t = p.t.min(p.seq.duration); }
        for g in &p.seq.groups {
            let speaker = p.bindings.get(&g.uid).copied();
            for c in &g.clips {
                if !crossed(c.start) {
                    continue;
                }
                match &c.kind {
                    ClipKind::Dialog { sound } => {
                        let at = match g.kind {
                            GroupKind::Player => None,
                            _ => speaker.and_then(|s| actors.iter().find(|(x, _)| x.id == s).map(|(_, t)| t.translation)).or(Some(p.origin.translation)),
                        };
                        sfx.write(crate::sound::PlaySfx { event: *sound, at, volume: 1.0 });
                        if let Some(text) = data.strings.subtitle(*sound) {
                            let secs = data.speech.get(sound).copied().unwrap_or(3.0);
                            ui.subtitle = Some((crate::story::plain(text), secs.max(1.0)));
                        }
                    }
                    ClipKind::Sound { sound, .. } => {
                        sfx.write(crate::sound::PlaySfx { event: *sound, at: None, volume: 1.0 });
                    }
                    _ => {}
                }
            }
        }
        for s in &p.seq.sync_points {
            if s.kind != 1 && crossed(s.time) {
                debug!("cinema: {} reached {} at {:.1} s{}", p.seq.name, s.name, s.time, if s.pause { ", pausing" } else { "" });
                host.story.sequence_event(&p.scene.to_string(), &p.file, "SyncPointReached", Some(&s.name));
                if s.pause {
                    p.paused = true;
                }
            }
        }
        // Whole-track vehicle displacement is independent of its looping
        // simple animation. Applying that loop's reference rotation to the
        // whole truck used to flip it over and bury it below the terrain.
        for g in &p.seq.groups {
            let Some((local, gas, steering)) = recorded_pose(g, p.t) else { continue };
            let Some(&id) = p.bindings.get(&g.uid) else { continue };
            let Some((_, mut at, vehicle)) = props.iter_mut().find(|(pid, _, _)| pid.0 == id) else { continue };
            let frame = if p.scene != 0 && g.kind == GroupKind::Generic {
                g.clips.iter().find_map(|c| match &c.kind {
                    ClipKind::Movement { keys } => movement_pose(keys, 0.0),
                    _ => None,
                }).map(|first| *p.movement_frames.entry(g.uid).or_insert_with(|| {
                    Transform::from_matrix(at.to_matrix() * first.to_matrix().inverse())
                })).unwrap_or(p.origin)
            } else { p.origin };
            *at = frame * local;
            if matches!(g.clips.first().map(|c| &c.kind), Some(ClipKind::Movement { .. }))
                && (p.t * 2.0) as u32 != (prev * 2.0) as u32 {
                debug!("cinema {}: prop {id} at t={:.2} world {:.2?}", p.seq.name, p.t, at.translation);
            }
            if std::env::var("FC3_VEHICLE_DEBUG").is_ok() && (p.t / 5.0) as u32 != (prev / 5.0) as u32 {
                info!("cinema {}: recorded vehicle {id} at t={:.1} world {:.2?}", p.seq.name, p.t, at.translation);
            }
            if (p.t / 15.0) as u32 != (prev / 15.0) as u32 {
                if let Ok(stem) = std::env::var("FC3_CINEMA_SHOTS") {
                    crate::camera::capture(&mut commands, format!("{stem}_{}_{}.png", p.scene, (p.t / 15.0) as u32));
                }
            }
            if let Some(mut vehicle) = vehicle {
                let speed = recorded_pose(g, (p.t - 0.05).max(0.0)).map_or(0.0, |(prev, _, _)| {
                    (local.translation - prev.translation).dot(local.rotation * Vec3::NEG_Z) / 0.05
                });
                vehicle.recorded_controls(gas, steering, if p.paused { 0.0 } else { speed });
            }
        }
        // Anchored clips are authored in the exposed entity's model frame,
        // not at the static scene placement (Dennis's root is the driver seat).
        let anchor = p.anchor.and_then(|id| props.iter().find(|(pid, _, _)| pid.0 == id).map(|(_, at, _)| *at));
        // Characters on their clips, at the scene or moving anchor's place.
        if (p.t / 2.0) as u32 != (prev / 2.0) as u32 {
            if let Some(id) = std::env::var("FC3_STORY_ACTOR_DEBUG").ok().and_then(|s| s.parse::<u64>().ok()) {
                if p.bindings.values().any(|&bound| bound == id) {
                    let groups: Vec<_> = p.seq.groups.iter().filter(|g| p.bindings.get(&g.uid) == Some(&id))
                        .map(|g| (g.name.as_str(), &g.kind, g.clips.len())).collect();
                    info!("cinema actor diagnostic {id}: {} t {:.2}, groups {:?}, looping {}, end hold {}", p.seq.name, p.t, groups, p.seq.looping, p.seq.pause_at_end);
                }
            }
        }
        for g in &p.seq.groups {
            if !matches!(g.kind, GroupKind::Character | GroupKind::Generic) {
                continue;
            }
            let Some(&id) = p.bindings.get(&g.uid) else { continue };
            let Some((mut actor, mut at)) = actors.iter_mut().find(|(x, _)| x.id == id) else { continue };
            actor.in_scene = true;
            let Some((source, ct)) = clip_at(&g.clips, p.t, |s| p.clips.get(s).and_then(|h| clips.get(h)).map(|c| c.0.duration)) else { continue };
            let Some(handle) = p.clips.get(source) else { continue };
            let root = clips.get(handle).map(|c| clip_root(&c.0, ct)).unwrap_or_default();
            let origin = if anchored_at(&g.clips, p.t) { anchor.unwrap_or(p.origin) } else { p.origin };
            let world = origin * root;
            *at = Transform { translation: world.translation + world.rotation * Vec3::Y * HALF_HEIGHT, rotation: world.rotation, scale: Vec3::ONE };
            if let Some(rig) = actor.rig {
                let skeleton = if source.contains("narrative") { narrative.clone() } else { master.clone() };
                commands.entity(rig).insert(ClipOverride { clip: handle.clone(), skeleton, time: ct });
                // Its face on the scene's facial track.
                if let (Ok(mut face), Some(library)) = (faces.get_mut(rig), library.as_mut()) {
                    face.scene = facial_at(&g.clips, p.t).and_then(|(source, from, ft)| {
                        let clip = library.clip(source)?;
                        let t = from as f32 / clip.frame_rate.max(1.0) + ft;
                        Some((clip, t))
                    });
                }
            }
        }
        // Props on their clips: the clip's root at the scene's place, and
        // the prop's own node on it.
        for g in &p.seq.groups {
            if g.kind != GroupKind::Generic {
                continue;
            }
            if recorded_pose(g, p.t).is_some() { continue; }
            let Some(&id) = p.bindings.get(&g.uid) else { continue };
            let Some((source, ct)) = clip_at(&g.clips, p.t, |s| p.clips.get(s).and_then(|h| clips.get(h)).map(|c| c.0.duration)) else { continue };
            let Some(clip) = p.clips.get(source).and_then(|h| clips.get(h)) else { continue };
            let Some((_, mut at, _)) = props.iter_mut().find(|(pid, _, _)| pid.0 == id) else { continue };
            let node = clip.0.tracks.iter().min_by_key(|t| t.bone).map(|t| Transform {
                translation: t.sample_translation(ct).map(Vec3::from_array).unwrap_or_default(),
                rotation: t.sample_rotation(ct).map(Quat::from_array).unwrap_or_default(),
                scale: Vec3::ONE,
            });
            let to = p.origin * clip_root(&clip.0, ct) * node.unwrap_or_default();
            if p.t < 0.5 {
                debug!(
                    "cinema {}: prop {id} placed {:.2?} scene {:.2?} ({:.2} m)",
                    p.seq.name,
                    at.translation,
                    to.translation,
                    at.translation.distance(to.translation)
                );
            }
            *at = to;
        }
        // The view: a camera cut's path, else the player clip's camera bone.
        let cut = p.seq.groups.iter().filter(|g| g.kind == GroupKind::Camera).find_map(|g| {
            let holds = g.clips.iter().filter(|c| matches!(c.kind, ClipKind::CameraCut { .. }) && c.start <= p.t).max_by(|x, y| x.start.total_cmp(&y.start));
            if !matches!(holds.map(|c| &c.kind), Some(ClipKind::CameraCut { start: true, .. })) {
                return None;
            }
            g.clips.iter().find_map(|c| match &c.kind {
                ClipKind::CameraAnimation { keys } if p.t >= c.start && p.t <= c.start + c.duration && !keys.is_empty() => {
                    let lt = p.t - c.start;
                    let i = keys.iter().position(|k| k.time >= lt).unwrap_or(keys.len() - 1);
                    let k = keys[i];
                    let local = Transform { translation: to_bevy(k.pos), rotation: Quat::from_xyzw(k.rot[0], k.rot[2], -k.rot[1], k.rot[3]), scale: Vec3::ONE };
                    Some((p.origin * local, k.fov))
                }
                _ => None,
            })
        });
        let fov = p
            .seq
            .groups
            .iter()
            .flat_map(|g| &g.clips)
            .find_map(|c| match &c.kind {
                ClipKind::Fov { samples } if p.t >= c.start && p.t <= c.start + c.duration => {
                    samples.get(((p.t - c.start) / cinema::FOV_STEP) as usize).copied()
                }
                _ => None,
            })
            .unwrap_or(60.0);
        let first_person = p
            .seq
            .groups
            .iter()
            .find(|g| g.kind == GroupKind::Player)
            .and_then(|g| clip_at(&g.clips, p.t, |s| p.clips.get(s).and_then(|h| clips.get(h)).map(|c| c.0.duration)))
            .and_then(|(source, ct)| {
                let skel_handle = if source.contains("narrative") { &narrative } else { &master };
                let (Some(skel), Some(clip)) = (skeletons.get(skel_handle), p.clips.get(source).and_then(|h| clips.get(h))) else { return None };
                let m = bone_world(&skel.0, &clip.0, ct, "Camera")?;
                let (_, r, tr) = m.to_scale_rotation_translation();
                Some((p.origin * clip_root(&clip.0, ct) * Transform { translation: tr, rotation: r, scale: Vec3::ONE }, fov))
            });
        if first_person.is_none() && (p.t * 0.5) as u32 != (prev * 0.5) as u32 {
            let pg = p.seq.groups.iter().find(|g| g.kind == GroupKind::Player);
            let at = pg.and_then(|g| clip_at(&g.clips, p.t, |s| p.clips.get(s).and_then(|h| clips.get(h)).map(|c| c.0.duration)));
            let state = at.map(|(source, _)| {
                let skel_handle = if source.contains("narrative") { &narrative } else { &master };
                let skel = skeletons.get(skel_handle);
                let clip = p.clips.get(source).and_then(|h| clips.get(h));
                format!("skeleton {} (camera bone {:?}), clip {}", skel.is_some(), skel.map(|s| s.0.find("Camera")), clip.is_some())
            });
            debug!("cinema {}: t {:.1}, no first-person view: player group {}, clip {:?}: {state:?}", p.seq.name, p.t, pg.is_some(), at.map(|a| a.0));
        }
        if let Some(v) = cut.or(first_person) {
            if (p.t * 2.0) as u32 != (prev * 2.0) as u32 {
                debug!(
                    "cinema {}: t {:.1} origin {:.1?} view {:.1?} fwd {:.2?} (cut {})",
                    p.seq.name,
                    p.t,
                    p.origin.translation,
                    v.0.translation,
                    v.0.forward(),
                    cut.is_some()
                );
            }
            view = Some(v);
            view_moves_player = p.seq.groups.iter().any(|g| g.kind == GroupKind::Player);
            // In his own eyes (no camera shot), a player-camera clip lets
            // him turn his head within its limits.
            head_look = if cut.is_some() {
                None
            } else {
                p.seq.groups.iter().flat_map(|g| &g.clips).find_map(|c| match c.kind {
                    fc3_formats::cinema::ClipKind::PlayerCamera { head } if head.turns() && (c.start..c.start + c.duration).contains(&p.t) => Some(head),
                    _ => None,
                })
            };
        }
        if p.t >= p.seq.duration {
            if p.seq.looping {
                p.t -= p.seq.duration.max(0.01);
                host.story.sequence_event(&p.scene.to_string(), &p.file, "FinishedLoop", None);
            } else if !p.seq.pause_at_end {
                finished.push(i);
            }
        }
    }
    for i in finished.into_iter().rev() {
        let p = cinema.playing.remove(i);
        if p.scene == 0 {
            if let Some(done) = cinema.built_results.get_mut(&p.file) { *done = true; }
        }
        host.story.sequence_event(&p.scene.to_string(), &p.file, "Finished", None);
        cinema.released.push(p);
    }
    for p in released.iter().chain(cinema.released.iter()) {
        for g in &p.seq.groups {
            let Some(&id) = p.bindings.get(&g.uid) else { continue };
            if cinema.playing.iter().any(|q| q.bindings.values().any(|&x| x == id)) {
                continue;
            }
            if let Some((mut actor, _)) = actors.iter_mut().find(|(x, _)| x.id == id) {
                actor.in_scene = false;
                if let Some(rig) = actor.rig {
                    commands.entity(rig).remove::<ClipOverride>();
                }
            }
        }
    }
    cinema.released.clear();
    cinema.view = view;
    cinema.view_moves_player = view_moves_player;
    cinema.head_look = head_look;
}

/// Player-track scenes carry the body with their camera. Camera-only shots
/// leave the body in place, so flag/establishing shots cannot teleport Jason
/// into scenery or outside his mission zone.
#[allow(clippy::type_complexity)]
pub(crate) fn scene_view(
    cinema: Res<Cinema>,
    settings: Res<crate::settings::Settings>,
    mut players: Query<(&mut Transform, &mut Player), Without<PlayerCamera>>,
    mut cams: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
    mut view_models: Query<&mut Visibility, With<crate::weapons::ViewModel>>,
    mut held: Local<Option<bool>>,
    head: Res<crate::player::SceneHeadLook>,
) {
    let Ok((mut pt, mut p)) = players.single_mut() else { return };
    let Ok((mut ct, mut proj)) = cams.single_mut() else { return };
    let eye = Vec3::Y * (crate::player::EYE_HEIGHT - HALF_HEIGHT);
    match cinema.view {
        Some((world, fov)) => {
            if held.is_none() {
                *held = Some(p.noclip);
                p.noclip = true;
            }
            p.velocity = Vec3::ZERO;
            let (yaw, _, _) = world.rotation.to_euler(EulerRot::YXZ);
            if cinema.view_moves_player {
                p.yaw = yaw;
                pt.translation = world.translation - eye;
                pt.rotation = Quat::from_rotation_y(yaw);
            }
            // (Turned by the player's head look, within the clip's limits.)
            let world = Transform { rotation: world.rotation * Quat::from_euler(EulerRot::YXZ, head.yaw, head.pitch, 0.0), ..world };
            *ct = Transform::from_matrix(pt.to_matrix().inverse() * world.to_matrix());
            if let Projection::Perspective(pp) = &mut *proj {
                pp.fov = fov.clamp(10.0, 120.0).to_radians();
            }
            for mut v in &mut view_models {
                *v = Visibility::Hidden;
            }
        }
        None => {
            if let Some(noclip) = held.take() {
                p.noclip = noclip;
                p.pitch = 0.0;
                p.spawning = true;
                // The scene may end in a cave or on an upper floor. Probe
                // locally instead of respawning on the terrain above it.
                p.keep_height = true;
                *ct = Transform::from_translation(eye);
                if let Projection::Perspective(pp) = &mut *proj {
                    pp.fov = settings.fov.to_radians();
                }
                for mut v in &mut view_models {
                    *v = Visibility::Inherited;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_only_scene_preserves_body_while_player_track_transfers_it() {
        let mut app = App::new();
        app.init_resource::<Cinema>().init_resource::<crate::settings::Settings>().init_resource::<crate::player::SceneHeadLook>().add_systems(Update, scene_view);
        let start = Vec3::new(-110.0, 154.0, -2270.0);
        let player = app
            .world_mut()
            .spawn((
                Transform::from_translation(start),
                Player {
                    velocity: Vec3::ZERO,
                    yaw: 0.4,
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
                },
            ))
            .id();
        let camera = app.world_mut().spawn((PlayerCamera, Transform::default(), Projection::Perspective(PerspectiveProjection::default()))).id();
        let shot = Transform::from_xyz(-140.0, 170.0, -2290.0);
        app.world_mut().resource_mut::<Cinema>().view = Some((shot, 55.0));
        app.update();
        let body = *app.world().get::<Transform>(player).unwrap();
        assert_eq!(body.translation, start);
        assert!((body.transform_point(app.world().get::<Transform>(camera).unwrap().translation) - shot.translation).length() < 0.001);
        assert!(app.world().get::<Player>(player).unwrap().noclip);
        app.world_mut().resource_mut::<Cinema>().view = None;
        app.update();
        assert_eq!(app.world().get::<Transform>(player).unwrap().translation, start);
        assert!(!app.world().get::<Player>(player).unwrap().noclip);
        {
            let mut cinema = app.world_mut().resource_mut::<Cinema>();
            cinema.view = Some((shot, 55.0));
            cinema.view_moves_player = true;
        }
        app.update();
        let expected = shot.translation - Vec3::Y * (crate::player::EYE_HEIGHT - HALF_HEIGHT);
        assert_eq!(app.world().get::<Transform>(player).unwrap().translation, expected);
        app.world_mut().resource_mut::<Cinema>().view = None;
        app.update();
        assert_eq!(app.world().get::<Transform>(player).unwrap().translation, expected);
        assert!(!app.world().get::<Player>(player).unwrap().noclip);
        assert!(app.world().get::<Player>(player).unwrap().keep_height);
    }

    #[test]
    fn recorded_vehicle_pose_interpolates_rotations_and_holds_its_endpoint() {
        let key = |time, x, yaw| cinema::VehicleKey { time, pos: [x, 2.0, 3.0], angles: [0.0, 0.0, yaw], gas: time, steering: -time };
        let keys = [key(0.0, 1.0, 179.0), key(1.0, 11.0, -179.0)];
        let (mid, gas, steering) = vehicle_pose(&keys, 0.5).unwrap();
        assert!(mid.translation.abs_diff_eq(Vec3::new(6.0, 3.0, -2.0), 1e-5));
        assert!(mid.forward().as_vec3().abs_diff_eq(Vec3::Z, 1e-4), "yaw must cross 180, not spin through zero");
        assert_eq!((gas, steering), (0.5, -0.5));
        assert_eq!(vehicle_pose(&keys, -1.0), vehicle_pose(&keys, 0.0));
        assert_eq!(vehicle_pose(&keys, 10.0), vehicle_pose(&keys, 1.0));
        assert!(vehicle_pose(&[], 0.0).is_none());
    }

    #[test]
    fn tracked_playback_distinguishes_finish_pause_stop_and_reset() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Fc3Clip>().init_asset::<Fc3Skeleton>()
            .init_resource::<Cinema>().init_resource::<CacheInfo>().init_resource::<crate::story::StoryUi>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<crate::fp::Cinematic>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
            .add_message::<crate::sound::PlaySfx>().add_systems(Update, (start_scenes, play_scenes).chain());
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        app.insert_non_send(StoryHost::new(story, None));
        let built = |name: &str| Built { name: name.into(), origin: Transform::IDENTITY, bindings: HashMap::new(),
            seq: Sequence { name: name.into(), duration: 0.3, cutscene: true, ..default() } };
        app.world_mut().resource_mut::<Cinema>().play_built_tracked(built("finish"));
        app.update();
        app.world_mut().resource_mut::<Cinema>().playing[0].paused = true;
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Enter);
        for _ in 0..5 { app.update(); }
        assert!(!app.world_mut().resource_mut::<Cinema>().take_built_finished("finish"));
        assert!(app.world().resource::<Cinema>().progress("finish").is_some());
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        app.world_mut().resource_mut::<Cinema>().playing[0].paused = false;
        for _ in 0..5 { app.update(); }
        let mut cinema = app.world_mut().resource_mut::<Cinema>();
        assert!(cinema.take_built_finished("finish"));
        assert!(!cinema.take_built_finished("finish"));
        cinema.play_built_tracked(built("stop"));
        drop(cinema);
        app.update();
        app.world_mut().resource_mut::<Cinema>().stop_built("stop");
        for _ in 0..5 { app.update(); }
        let mut cinema = app.world_mut().resource_mut::<Cinema>();
        assert!(!cinema.take_built_finished("stop"));
        cinema.play_built_tracked(built("reset"));
        cinema.reset();
        assert!(!cinema.take_built_finished("reset"));
        assert!(cinema.built_results.is_empty());
    }

    #[test]
    fn pausing_cues_clamp_prop_motion_and_defer_later_sound_until_resume() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Fc3Clip>().init_asset::<Fc3Skeleton>()
            .init_resource::<Cinema>().init_resource::<CacheInfo>().init_resource::<crate::story::StoryUi>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<crate::fp::Cinematic>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(250)))
            .add_message::<crate::sound::PlaySfx>().add_systems(Update, (start_scenes, play_scenes).chain());
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        app.insert_non_send(StoryHost::new(fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap(), None));
        let prop = app.world_mut().spawn((crate::objects::PlacedId(100), Transform::IDENTITY)).id();
        let seq = Sequence { duration: 1.0,
            // Intentionally not ordered: the earliest pause wins.
            sync_points: vec![
                cinema::SyncPoint { name: "second".into(), time: 0.2, kind: 0, pause: true },
                cinema::SyncPoint { name: "first".into(), time: 0.1, kind: 0, pause: true },
            ],
            groups: vec![cinema::Group { uid: 1, kind: GroupKind::Generic, name: "prop".into(), archetype: 0,
                clips: vec![
                    cinema::Clip { start: 0.0, duration: 1.0, kind: ClipKind::Movement { keys: vec![
                        cinema::MovementKey { time: 0.0, pos: [0.0; 3], angles: [0.0; 3] },
                        cinema::MovementKey { time: 1.0, pos: [10.0, 0.0, 0.0], angles: [0.0; 3] },
                    ] } },
                    cinema::Clip { start: 0.15, duration: 0.001, kind: ClipKind::Sound { sound: 123, kind: 0 } },
                ] }], ..default() };
        assert_eq!(advance_to_pause(&seq, 0.0, 0.8), (0.1, 0.1f32.next_up(), true));
        assert_eq!(advance_to_pause(&seq, 0.1, 0.0), (0.1, 0.1, false));
        app.world_mut().resource_mut::<Cinema>().playing.push(Playing { scene: 1, meta: None, file: "pause-test".into(),
            seq, t: 0.0, paused: false, paused_cue: false, origin: Transform::IDENTITY, movement_frames: HashMap::new(), anchor: None,
            bindings: HashMap::from([(1, 100)]), clips: HashMap::new(), waited: 3.0 });
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Cinema>().playing[0].t, 0.1);
        assert_eq!(app.world().get::<Transform>(prop).unwrap().translation.x, 1.0);
        assert!(app.world().resource::<Messages<crate::sound::PlaySfx>>().is_empty());
        app.update();
        assert_eq!(app.world().resource::<Cinema>().playing[0].t, 0.1);
        let resume = || Request { mode: "play".into(), scene: 1, file: "pause-test".into(), sync: None, meta: None };
        app.world_mut().resource_mut::<Cinema>().requests.push(resume());
        app.update();
        assert_eq!(app.world().resource::<Cinema>().playing[0].t, 0.2);
        assert_eq!(app.world().get::<Transform>(prop).unwrap().translation.x, 2.0);
        assert_eq!(app.world().resource::<Messages<crate::sound::PlaySfx>>().len(), 1);
        app.world_mut().resource_mut::<Cinema>().requests.push(resume());
        app.update();
        assert!(app.world().resource::<Cinema>().playing[0].t > 0.29);
        assert!(!app.world().resource::<Cinema>().playing[0].paused);
    }

    #[test]
    fn bound_movement_keeps_door_hinge_and_restart_frame() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Fc3Clip>().init_asset::<Fc3Skeleton>()
            .init_resource::<Cinema>().init_resource::<CacheInfo>().init_resource::<crate::story::StoryUi>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<crate::fp::Cinematic>()
            .add_message::<crate::sound::PlaySfx>().add_systems(Update, play_scenes);
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        app.insert_non_send(StoryHost::new(fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap(), None));
        let placed = Transform::from_xyz(-1895.85, 98.46, -2379.84).with_rotation(Quat::from_rotation_y(167.0f32.to_radians()));
        let prop = app.world_mut().spawn((crate::objects::PlacedId(100), placed)).id();
        let keys = vec![
            cinema::MovementKey { time: 0.0, pos: [-1.81799, -3.19824, 0.307701], angles: [0.0, 0.0, 91.0001] },
            cinema::MovementKey { time: 1.33333, pos: [-1.81799, -3.19824, 0.307701], angles: [0.0, 0.0, 179.0] },
        ];
        app.world_mut().resource_mut::<Cinema>().playing.push(Playing { scene: 1, meta: None, file: "door-frame".into(),
            t: 0.0, paused: true, paused_cue: false, origin: Transform::from_xyz(-1898.54, 98.1876, -2388.99)
                .with_rotation(Quat::from_rotation_y(-21.0f32.to_radians())),
            movement_frames: HashMap::new(), anchor: None, bindings: HashMap::from([(1, 100)]), clips: HashMap::new(), waited: 3.0,
            seq: Sequence { duration: 2.0, groups: vec![cinema::Group { uid: 1, kind: GroupKind::Generic,
                name: "door".into(), archetype: 0, clips: vec![cinema::Clip { start: 0.0, duration: 2.0,
                    kind: ClipKind::Movement { keys } }] }], ..default() } });
        for t in [0.0, 1.33333, 1.33333, 0.0, 1.33333] {
            app.world_mut().resource_mut::<Cinema>().playing[0].t = t;
            app.update();
            let at = app.world().get::<Transform>(prop).unwrap();
            assert!(at.translation.distance(placed.translation) < 0.001);
            let expected = placed.rotation * Quat::from_rotation_y(if t == 0.0 { 0.0 } else { 87.9999f32.to_radians() });
            assert!(at.rotation.angle_between(expected).abs() < 0.001);
        }
    }

    #[test]
    fn completing_between_ticks_still_samples_the_authored_camera_endpoint() {
        for millis in [16, 37, 100] {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
                .init_asset::<Fc3Clip>().init_asset::<Fc3Skeleton>()
                .init_resource::<Cinema>().init_resource::<CacheInfo>().init_resource::<crate::story::StoryUi>()
                .init_resource::<ButtonInput<KeyCode>>().init_resource::<crate::fp::Cinematic>()
                .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(millis)))
                .add_message::<crate::sound::PlaySfx>().add_systems(Update, (start_scenes, play_scenes).chain());
            let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
            let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
            app.insert_non_send(StoryHost::new(story, None));
            let duration = 0.233;
            app.world_mut().resource_mut::<Cinema>().play_built_tracked(Built {
                name: "endpoint".into(), origin: Transform::from_xyz(10.0, 20.0, 30.0), bindings: HashMap::new(),
                seq: Sequence { duration, groups: vec![cinema::Group {
                    uid: 1, kind: GroupKind::Camera, name: "camera".into(), archetype: 0,
                    clips: vec![
                        cinema::Clip { start: 0.0, duration, kind: ClipKind::CameraCut { start: true, hide_hud: true } },
                        cinema::Clip { start: 0.0, duration, kind: ClipKind::CameraAnimation { keys: vec![
                            cinema::CameraKey { time: 0.0, pos: [0.0; 3], rot: [0.0, 0.0, 0.0, 1.0], fov: 60.0 },
                            cinema::CameraKey { time: duration, pos: [1.0, 2.0, 3.0], rot: [0.0, 0.0, 0.0, 1.0], fov: 45.0 },
                        ] } },
                    ],
                }], ..default() },
            });
            let mut completed = false;
            for _ in 0..30 {
                app.update();
                let mut cinema = app.world_mut().resource_mut::<Cinema>();
                if cinema.take_built_finished("endpoint") {
                    let (view, fov) = cinema.view.expect("the completion frame must retain its endpoint view");
                    assert!(view.translation.abs_diff_eq(Vec3::new(11.0, 23.0, 28.0), 1e-5));
                    assert_eq!(fov, 45.0);
                    completed = true;
                    break;
                }
            }
            assert!(completed, "scene did not complete at {millis} ms timestep");
            app.update();
            assert!(app.world().resource::<Cinema>().view.is_none());
        }
    }

    #[test]
    fn movement_track_is_not_overwritten_by_the_simple_loop_and_actor_rides_its_anchor() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Fc3Clip>().init_asset::<Fc3Skeleton>()
            .init_resource::<Cinema>().init_resource::<crate::story::StoryUi>()
            .init_resource::<ButtonInput<KeyCode>>().init_resource::<crate::fp::Cinematic>()
            .add_message::<crate::sound::PlaySfx>().add_systems(Update, play_scenes);
        let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
        let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
        app.insert_non_send(StoryHost::new(story, None));
        let mut clip = fc3_formats::anim::Clip { duration: 1.0, ..default() };
        clip.root_motion = Some(fc3_formats::anim::RootMotion { start_translation: [-0.4, 0.8, -0.2],
            start_rotation: Quat::IDENTITY.to_array(), ..default() });
        let handle = app.world_mut().resource_mut::<Assets<Fc3Clip>>().add(Fc3Clip(std::sync::Arc::new(clip)));
        let vehicle = app.world_mut().spawn((crate::objects::PlacedId(100), Transform::IDENTITY)).id();
        let actor = app.world_mut().spawn((StoryActor::new(200, None, false), Transform::IDENTITY)).id();
        let prop = app.world_mut().spawn((crate::objects::PlacedId(300),
            Transform::from_xyz(100.0, 1.0, 0.0).with_rotation(Quat::from_rotation_y(179.0f32.to_radians())))).id();
        let movement = cinema::Clip { start: 0.0, duration: 1.0, kind: ClipKind::VehicleMovement { keys: vec![
            cinema::VehicleKey { time: 0.0, pos: [10.0, 20.0, 30.0], angles: [0.0, 0.0, -90.0], gas: 0.0, steering: 0.0 },
            cinema::VehicleKey { time: 1.0, pos: [20.0, 20.0, 30.0], angles: [0.0, 0.0, -90.0], gas: 1.0, steering: 0.0 }] } };
        let simple = cinema::Clip { start: 0.0, duration: 1.0, kind: ClipKind::SimpleAnimation { source: "loop".into(), from: 0, to: 30 } };
        let animation = cinema::Clip { start: 0.0, duration: 1.0, kind: ClipKind::Animation { source: "loop".into(), from: 0, to: 30,
            blend: 0.0, anchored: true } };
        app.world_mut().resource_mut::<Cinema>().playing.push(Playing { scene: 1, meta: None, file: "ride".into(), t: 0.5, paused: true, paused_cue: false,
            origin: Transform::from_xyz(100.0, 0.0, 0.0), anchor: Some(100), bindings: HashMap::from([(1, 100), (2, 200), (3, 300)]),
            movement_frames: HashMap::new(),
            clips: HashMap::from([("loop".into(), handle)]), waited: 3.0,
            seq: Sequence { duration: 1.0, groups: vec![
                cinema::Group { uid: 1, kind: GroupKind::Generic, name: "truck".into(), archetype: 0, clips: vec![movement, simple] },
                cinema::Group { uid: 2, kind: GroupKind::Character, name: "driver".into(), archetype: 0, clips: vec![animation] },
                cinema::Group { uid: 3, kind: GroupKind::Generic, name: "prop".into(), archetype: 0, clips: vec![
                    cinema::Clip { start: 0.0, duration: 1.0, kind: ClipKind::Movement { keys: vec![
                        cinema::MovementKey { time: 0.0, pos: [0.0, 0.0, 1.0], angles: [0.0, 0.0, 179.0] },
                        cinema::MovementKey { time: 1.0, pos: [4.0, 0.0, 1.0], angles: [0.0, 0.0, -179.0] },
                    ] } },
                ] }], ..default() } });
        app.update();
        let vt = *app.world().get::<Transform>(vehicle).unwrap();
        assert!(vt.translation.abs_diff_eq(Vec3::new(115.0, 30.0, -20.0), 1e-5));
        let expected = vt.transform_point(Vec3::new(-0.4, 0.8 + HALF_HEIGHT, -0.2));
        assert!(app.world().get::<Transform>(actor).unwrap().translation.abs_diff_eq(expected, 1e-4));
        assert!(app.world().get::<StoryActor>(actor).unwrap().in_scene);
        let pt = app.world().get::<Transform>(prop).unwrap();
        assert!(pt.translation.abs_diff_eq(Vec3::new(102.0, 1.0, 0.0), 1e-5));
        assert!(pt.forward().as_vec3().abs_diff_eq(Vec3::Z, 1e-4));
        // The driver retains his passenger-relative pose through the final hold.
        app.world_mut().resource_mut::<Cinema>().playing[0].t = 1.5;
        app.world_mut().resource_mut::<Cinema>().playing[0].seq.pause_at_end = true;
        app.update();
        assert!(app.world().get::<Transform>(vehicle).unwrap().translation.abs_diff_eq(Vec3::new(120.0, 30.0, -20.0), 1e-5));
        assert!(app.world().get::<Transform>(prop).unwrap().translation.abs_diff_eq(Vec3::new(104.0, 1.0, 0.0), 1e-5));
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE with refreshed installed-game sequences"]
    fn installed_greenhouse_doors_share_the_placed_greenhouse_frame() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let data = fc3_story::data::StoryData::load(&root).unwrap();
        let greenhouse = origin_of(&data, 2073485233983864237);
        for (name, door) in [("seq_doc01_b02_doors", 2073691951284110795), ("seq_doc01_b02_doors02", 2073691961113462220)] {
            let archive = format!(r"sequences\fc3_main\doc01_b02\{name}.cseq");
            let seq = Sequence::from_text(&std::fs::read_to_string(root.join(cinema::sequence_rel(&archive))).unwrap()).unwrap();
            let bound = bindings(&data, 2073402072836873209, &archive);
            let group = seq.groups.iter().find(|g| bound.get(&g.uid) == Some(&door)).unwrap();
            let (first, _, _) = recorded_pose(group, 0.0).unwrap();
            let placed = origin_of(&data, door);
            let frame = Transform::from_matrix(placed.to_matrix() * first.to_matrix().inverse());
            assert!(frame.translation.distance(greenhouse.translation) < 0.05, "{name}: {frame:?}");
            assert!(frame.rotation.angle_between(greenhouse.rotation).abs() < 0.002);
        }
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE with refreshed installed-game sequences"]
    fn installed_briefing_has_a_670_metre_recorded_ride_and_vehicle_relative_dennis() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let data = fc3_story::data::StoryData::load(&root).unwrap();
        let archive = cinema::archive_path("Sequences/fc3_main/jas_100_brief/JAS_100_BRIEF_B10_CarRide.seq");
        let seq = Sequence::from_text(&std::fs::read_to_string(root.join(cinema::sequence_rel(&archive))).unwrap()).unwrap();
        let bindings = bindings(&data, 2078101851032527610, &archive);
        assert_eq!(anchor_of(&data, 2078101851032527610, &archive), Some(2076001291963743153));
        let car = seq.groups.iter().find(|g| bindings.get(&g.uid) == Some(&2076001291963743153)).unwrap();
        let (first, _, _) = recorded_pose(car, 0.0).expect("missing converted vehicle track");
        let (last, _, _) = recorded_pose(car, seq.duration).unwrap();
        let origin = origin_of(&data, 2078101851032527610);
        let placed = to_bevy(data.entities[&2076001291963743153].pos);
        assert!((origin * first).translation.distance(placed) < 0.02);
        assert!(first.translation.distance(last.translation) > 660.0);
        let finish = origin * last;
        assert!(finish.translation.abs_diff_eq(to_bevy([-198.094, 2266.8264, 137.5769]), 0.02));
        let dennis = seq.groups.iter().find(|g| bindings.get(&g.uid) == Some(&2074715771025504507)).unwrap();
        assert!(anchored_at(&dennis.clips, 30.0));
        let source = clip_at(&dennis.clips, 30.0, |_| None).unwrap().0;
        let clip = fc3_formats::anim::parse_clip(&std::fs::read(root.join(fc3_formats::cache::anim_rel(source))).unwrap()).unwrap();
        assert!(clip_root(&clip, 0.0).translation.distance(Vec3::new(-0.4388, 0.8163, -0.2025)) < 0.001);
    }

    #[test]
    fn meta_stop_releases_loops_without_stopping_other_scenes_or_qtes() {
        let playing = |scene, meta, name: &str| Playing {
            scene, meta, file: name.into(), seq: Sequence { looping: true, ..Default::default() },
            t: 0.0, paused: false, paused_cue: false, origin: Transform::default(),
            movement_frames: HashMap::new(),
                bindings: HashMap::new(), anchor: None, clips: HashMap::new(), waited: 0.0,
        };
        let mut cinema = Cinema::default();
        cinema.playing = vec![playing(1, Some(10), "grant_loop"), playing(2, Some(20), "other"),
            playing(3, Some(10), "prop_loop"), playing(0, None, "qte")];
        cinema.playing[2].bindings.insert(1, 100);
        assert!(cinema.owns_entity(100));
        assert!(!cinema.owns_entity(101));
        assert_eq!(cinema.stop_meta(10), vec![(1, "grant_loop".into()), (3, "prop_loop".into())]);
        assert!(!cinema.owns_entity(100));
        assert_eq!(cinema.released.len(), 2);
        assert_eq!(cinema.playing.iter().map(|p| p.file.as_str()).collect::<Vec<_>>(), vec!["other", "qte"]);
        assert!(cinema.stop_meta(10).is_empty());
    }
}
