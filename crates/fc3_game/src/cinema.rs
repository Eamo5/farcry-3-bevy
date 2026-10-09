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

impl Plugin for CinemaPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cinema>()
            .add_systems(Update, (start_scenes, play_scenes).chain().after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)))
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
    /// Sequences the game makes itself (a QTE's phases), to start.
    built: Vec<Built>,
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
        self.view = None;
    }

    /// Plays a sequence the game made.
    pub fn play_built(&mut self, b: Built) {
        self.built.push(b);
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
}

struct Playing {
    scene: u64,
    meta: Option<u64>,
    file: String,
    seq: Sequence,
    t: f32,
    paused: bool,
    /// The scene entity's place (Bevy space).
    origin: Transform,
    /// Track group uid -> world entity id.
    bindings: HashMap<u64, u64>,
    clips: HashMap<String, Handle<Fc3Clip>>,
    /// Seconds spent waiting for clips to load.
    waited: f32,
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
        cinema.playing.push(Playing { scene: 0, meta: None, file: b.name, bindings: b.bindings, origin: b.origin, seq: b.seq, t: 0.0, paused: false, clips, waited: 0.0 });
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
                    p.paused = false;
                    match (r.mode.as_str(), r.sync.as_deref()) {
                        ("play_from_start", _) => p.t = 0.0,
                        ("play_at_sync_point", Some(sp)) => p.t = sync_time(&p.seq, sp),
                        _ => {}
                    }
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
                    origin: origin_of(&data, r.scene),
                    seq,
                    t,
                    paused: false,
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

fn clip_root(clip: &fc3_formats::anim::Clip, t: f32) -> Transform {
    match &clip.root_motion {
        Some(rm) => {
            let (tr, q) = rm.sample(t.clamp(0.0, clip.duration));
            let start = Transform { translation: Vec3::from_array(rm.start_translation), rotation: Quat::from_array(rm.start_rotation), scale: Vec3::ONE };
            start * Transform { translation: Vec3::from_array(tr), rotation: Quat::from_array(q), scale: Vec3::ONE }
        }
        None => Transform::IDENTITY,
    }
}

/// The world transform of a skeleton bone posed by a clip.
fn bone_world(skel: &fc3_formats::anim::Skeleton, clip: &fc3_formats::anim::Clip, t: f32, bone: &str) -> Option<Mat4> {
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
    mut props: Query<(&crate::objects::PlacedId, &mut Transform), Without<StoryActor>>,
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
        if p.seq.cutscene && (keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Enter) || std::env::var("FC3_STORY_SKIP_SCENES").is_ok()) {
            let from = p.t;
            p.t = p.seq.duration;
            p.paused = false;
            for s in p.seq.sync_points.iter().filter(|s| s.kind != 1 && s.time >= from && s.time < p.seq.duration) {
                host.story.sequence_event(&p.scene.to_string(), &p.file, "SyncPointReached", Some(&s.name));
            }
        }
        let prev = p.t;
        if !p.paused {
            p.t += dt;
        }
        let (a, b) = (prev, p.t);
        let crossed = |s: f32| s >= a && s < b || (a == 0.0 && s == 0.0 && b > 0.0);
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
        // Characters on their clips, at the scene's place.
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
            let world = p.origin * root;
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
            let Some(&id) = p.bindings.get(&g.uid) else { continue };
            let Some((source, ct)) = clip_at(&g.clips, p.t, |s| p.clips.get(s).and_then(|h| clips.get(h)).map(|c| c.0.duration)) else { continue };
            let Some(clip) = p.clips.get(source).and_then(|h| clips.get(h)) else { continue };
            let Some((_, mut at)) = props.iter_mut().find(|(pid, _)| pid.0 == id) else { continue };
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
}

/// While a scene holds the view, the player is where the scene's camera is
/// (the world streams around them; no physics, the first-person weapon
/// away), and is set down on the ground where the scene leaves them.
#[allow(clippy::type_complexity)]
fn scene_view(
    cinema: Res<Cinema>,
    settings: Res<crate::settings::Settings>,
    mut players: Query<(&mut Transform, &mut Player), Without<PlayerCamera>>,
    mut cams: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
    mut view_models: Query<&mut Visibility, With<crate::weapons::ViewModel>>,
    mut held: Local<Option<bool>>,
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
            p.yaw = yaw;
            pt.translation = world.translation - eye;
            pt.rotation = Quat::from_rotation_y(yaw);
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
    fn meta_stop_releases_loops_without_stopping_other_scenes_or_qtes() {
        let playing = |scene, meta, name: &str| Playing {
            scene, meta, file: name.into(), seq: Sequence { looping: true, ..Default::default() },
            t: 0.0, paused: false, origin: Transform::default(),
            bindings: HashMap::new(), clips: HashMap::new(), waited: 0.0,
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
