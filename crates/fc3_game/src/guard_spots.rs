//! Guard spots: the world's places for idle guards to stand at and do
//! something (its `SimpleAnimation` points): lean on a wall, smoke, look
//! over a fence or round a corner, check their gear, sit on the ground, take
//! a leak, or just stand easy or watchful. An idle guard posted near a free
//! spot takes it, faces its way and plays its clip; anything that puts him on
//! guard sends him back to the usual.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use fc3_formats::cache;
use fc3_formats::hash::name_hash;

use crate::cache_assets::{cache_path, Fc3Clip};
use crate::characters::{AnimKind, CharacterRig, ClipSet, HeldWeapon, CLIP_ROOT};
use crate::combat::Health;
use crate::npc::{Faction, Npc, NpcState};
use crate::CacheInfo;

pub struct GuardSpotsPlugin;

impl Plugin for GuardSpotsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<AddGuardSpot>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_spots)
            .add_systems(Update, add_spots.before(take_spots).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, (take_spots, leave_spots, enter_parts, spot_test, part_motion).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A guard takes a free spot this near his post (m, across; and up or down).
const REACH: f32 = 18.0;
const REACH_UP: f32 = 3.0;

struct Spot {
    kind: &'static str,
    at: Vec3,
    yaw: f32,
    /// A `move` spot's loop clip (its archive path), from the move its own
    /// smart terrain point names.
    clip: Option<String>,
    /// A scene's part: only he plays it (else anyone), and its clip's root
    /// motion moves him about it.
    owner: Option<Entity>,
    part: bool,
    /// A scene part's start: its clip, and where it begins.
    start: Option<(String, Vec3, f32)>,
    /// Its move's abort clips (getting up startled, or curious).
    abort: Vec<String>,
}

/// A guard's own part in his encounter's scene (`fc3_formats::encounters::Part`):
/// its clip (archive path) and where the scene puts him, and its start
/// sequence's clip and where that begins.
#[derive(Component)]
pub struct OwnSpot {
    pub clip: String,
    pub at: Vec3,
    pub yaw: f32,
    pub start: Option<(String, Vec3, f32)>,
}

/// A rig playing a scene's part: while its clip plays, the clip's root
/// motion moves it about where it stands (its own place, `base`, else),
/// from where the clip begins about that (`from`).
#[derive(Component)]
pub struct PartMotion {
    clip: Handle<Fc3Clip>,
    from: Transform,
    base: Option<Transform>,
}

impl PartMotion {
    pub fn play(&mut self, clip: Handle<Fc3Clip>, from: Transform) {
        (self.clip, self.from) = (clip, from);
    }
}

/// Plays a part's clip on a rig, from where it begins about it.
fn set_motion(rig_e: Entity, motion: Option<Mut<PartMotion>>, clip: Handle<Fc3Clip>, from: Transform, commands: &mut Commands) {
    match motion {
        Some(mut m) => m.play(clip, from),
        None => {
            commands.entity(rig_e).insert(PartMotion { clip, from, base: None });
        }
    }
}

fn part_motion(clips: Res<Assets<Fc3Clip>>, mut rigs: Query<(&CharacterRig, &mut Transform, &mut PartMotion)>) {
    for (rig, mut tf, mut m) in &mut rigs {
        let base = *m.base.get_or_insert(*tf);
        let (kind, t) = rig.playing();
        let playing = kind == AnimKind::Idle && rig.clips().clips.get(&AnimKind::Idle) == Some(&m.clip);
        // (The rig's time runs on past each loop.)
        let moved = clips.get(&m.clip).filter(|_| playing).map(|c| {
            let d = c.0.duration;
            c.0.root_motion.as_ref().map(|r| r.sample(if d > 0.0 { t % d } else { 0.0 }))
        });
        let next = match moved {
            Some(Some((tr, q))) => base * m.from * Transform { translation: Vec3::from_array(tr), rotation: Quat::from_array(q), ..default() },
            Some(None) => base * m.from,
            None => base,
        };
        if *tf != next {
            *tf = next;
        }
    }
}

/// A guard getting into his scene part: playing its start sequence's clip
/// (`start`) where that begins, then its loop (`into`) where that is.
/// `playing`: he is there, at it.
#[derive(Component)]
pub struct Entering {
    start: Handle<Fc3Clip>,
    into: Handle<Fc3Clip>,
    at: Vec3,
    yaw: f32,
    playing: bool,
}

/// The loop takes over this long (s) before the start's end (the rig poses
/// a frame after its time moves on).
const ENTER_EARLY: f32 = 0.05;

/// What a scene partner's guard (a looter, an assailant, a skinner) is
/// doing: on his way, playing the start, or the loop.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Phase {
    Coming,
    Start,
    Loop,
}

impl Phase {
    pub fn of(entering: Option<&Entering>) -> Phase {
        match entering {
            None => Phase::Loop,
            Some(e) if e.playing => Phase::Start,
            Some(_) => Phase::Coming,
        }
    }
}

/// A scene partner's own clips (a body's, a carcass's, a victim's): its
/// loop, and its start with where that begins about it; what it plays, and
/// its own clips meanwhile.
pub struct PartClips {
    looped: Handle<Fc3Clip>,
    start: Option<(Handle<Fc3Clip>, Transform)>,
    current: Option<Handle<Fc3Clip>>,
    base: Option<Arc<ClipSet>>,
}

impl PartClips {
    pub fn new(looped: Handle<Fc3Clip>, start: Option<(Handle<Fc3Clip>, Transform)>) -> PartClips {
        PartClips { looped, start, current: None, base: None }
    }

    pub fn ready(&self, clips: &Assets<Fc3Clip>) -> bool {
        clips.contains(&self.looped)
    }

    /// Plays the clip for its guard's phase: the start while he plays his,
    /// else the loop; with no one at it, what it was playing (a body not
    /// yet touched, `still`, lies as the start has it).
    pub fn follow(
        &mut self,
        phase: Option<Phase>,
        still: bool,
        (rig_e, rig, motion): (Entity, &mut CharacterRig, Option<Mut<PartMotion>>),
        clips: &Assets<Fc3Clip>,
        commands: &mut Commands,
    ) {
        let start = self.start.as_ref().map(|(c, _)| c.clone());
        let want = match phase {
            Some(Phase::Loop) => self.looped.clone(),
            Some(Phase::Start) => start.unwrap_or_else(|| self.looped.clone()),
            _ => self.current.clone().or_else(|| start.filter(|_| still)).unwrap_or_else(|| self.looped.clone()),
        };
        self.play(want, (rig_e, rig, motion), clips, commands);
    }

    /// Plays its start once, then its loop (a civilian's own point, or a
    /// chat by the fire: no one else's part to keep in step with).
    pub fn own(&mut self, (rig_e, rig, motion): (Entity, &mut CharacterRig, Option<Mut<PartMotion>>), clips: &Assets<Fc3Clip>, commands: &mut Commands) {
        let start = self.start.as_ref().map(|(c, _)| c.clone());
        let want = match (&self.current, start) {
            (None, Some(s)) => s,
            (Some(c), Some(s)) if *c == s => {
                let (kind, t) = rig.playing();
                let through = kind == AnimKind::Idle && clips.get(&s).is_some_and(|c| t >= c.0.duration - ENTER_EARLY);
                if through {
                    self.looped.clone()
                } else {
                    s
                }
            }
            _ => self.looped.clone(),
        };
        self.play(want, (rig_e, rig, motion), clips, commands);
    }

    fn play(
        &mut self,
        want: Handle<Fc3Clip>,
        (rig_e, rig, motion): (Entity, &mut CharacterRig, Option<Mut<PartMotion>>),
        clips: &Assets<Fc3Clip>,
        commands: &mut Commands,
    ) {
        if self.current.as_ref() == Some(&want) || !clips.contains(&want) {
            return;
        }
        let base = self.base.get_or_insert_with(|| rig.clips().clone()).clone();
        let mut set = ClipSet { skeleton: base.skeleton.clone(), clips: base.clips.clone() };
        set.clips.insert(AnimKind::Idle, want.clone());
        // (From what they were doing; a body laid out at once.)
        if self.current.is_some() {
            rig.set_clips_blended(Arc::new(set));
        } else {
            rig.set_clips(Arc::new(set));
        }
        let from = self.start.as_ref().filter(|(c, _)| *c == want).map_or(Transform::IDENTITY, |(_, f)| *f);
        set_motion(rig_e, motion, want.clone(), from, commands);
        self.current = Some(want);
    }

    /// Their own clips back (with `then` in the hit's slot: getting up from
    /// it); whether it was playing.
    pub fn stop(&mut self, rig: &mut CharacterRig, then: Option<Handle<Fc3Clip>>) -> bool {
        let playing = self.current.take().is_some();
        if let Some(base) = self.base.take() {
            let mut set = ClipSet::clone(&base);
            if let Some(clip) = then {
                set.clips.insert(AnimKind::Hit, clip);
            }
            rig.set_clips_blended(Arc::new(set));
        }
        playing
    }
}

/// Where a part's start begins about where its loop is (`at`, `yaw`): the
/// rig's offset as the start plays.
pub fn start_from(at: Vec3, yaw: f32, start_at: Vec3, start_yaw: f32) -> Transform {
    let d = Quat::from_rotation_y(-yaw) * (start_at - at);
    Transform { translation: Vec3::new(d.x, 0.0, d.z), rotation: Quat::from_rotation_y(start_yaw - yaw), ..default() }
}

/// A guard at his part's start, there and through its clip: into the loop,
/// where the start has brought him.
fn enter_parts(
    clips: Res<Assets<Fc3Clip>>,
    mut npcs: Query<(Entity, &mut Npc, &mut Transform, &mut Entering), With<AtSpot>>,
    mut rigs: Query<(&mut CharacterRig, Option<&mut PartMotion>)>,
    mut commands: Commands,
) {
    for (e, mut npc, mut t, mut en) in &mut npcs {
        let Some(rig_e) = npc.rig else { continue };
        let Ok((mut rig, motion)) = rigs.get_mut(rig_e) else { continue };
        let (kind, time) = rig.playing();
        let there = Vec2::new(t.translation.x - npc.post.x, t.translation.z - npc.post.z).length() < crate::npc::SPOT_SETTLE + 0.1;
        en.playing = kind == AnimKind::Idle && there;
        let Some(d) = clips.get(&en.start).map(|c| c.0.duration) else { continue };
        if !en.playing || time < d - ENTER_EARLY {
            continue;
        }
        let mut set = ClipSet { skeleton: rig.clips().skeleton.clone(), clips: rig.clips().clips.clone() };
        set.clips.insert(AnimKind::Idle, en.into.clone());
        rig.set_clips_blended(Arc::new(set));
        set_motion(rig_e, motion, en.into.clone(), Transform::IDENTITY, &mut commands);
        (npc.post, npc.post_yaw, npc.yaw) = (en.at, en.yaw, en.yaw);
        t.translation.x = en.at.x;
        t.translation.z = en.at.z;
        t.rotation = Quat::from_rotation_y(en.yaw);
        commands.entity(e).remove::<Entering>();
        if std::env::var("FC3_SPOT_DEBUG").is_ok() {
            info!("guard spot: through its start ({d:.1} s), into its loop at {:.1?}", en.at);
        }
    }
}

#[derive(Resource, Default)]
struct GuardSpots {
    list: Vec<Spot>,
    /// Who holds each taken spot.
    taken: HashMap<usize, Entity>,
}

/// A guard posted at a spot: which, his clips from before, and whether the
/// spot's clip has his weapon put away.
#[derive(Component)]
pub struct AtSpot {
    index: usize,
    base: Arc<ClipSet>,
    unarmed: bool,
    part: bool,
    /// Its move's abort clips, by name.
    abort: Vec<(String, Handle<Fc3Clip>)>,
}

impl AtSpot {
    /// Whether it is a scene's part (a body, a carcass or a victim plays
    /// theirs by him).
    pub fn part(&self) -> bool {
        self.part
    }
}

/// Looked for a spot once (taken or not; or never to take one).
#[derive(Component)]
pub(crate) struct SpotChecked;

/// The clips for a spot (under `animations\`), by what's done there; the
/// stances come in pirate and privateer builds.
fn clips(kind: &str, privateer: bool) -> &'static [&'static str] {
    match (kind, privateer) {
        ("relax", false) => &[
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v02_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v03_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\pirate\3rdge_fulbst_relaxguard_pirate_v04_loop_ak74.mab",
        ],
        ("relax", true) => &[
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\privateer\3rdpriv_fulbst_relaxguard_privateer_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\relax\relaxguard\privateer\3rdpriv_fulbst_relaxguard_privateer_v02_loop_ak74.mab",
        ],
        ("cautious", false) => &[
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\pirate\3rdge_fulbst_cautiousguard_pirate_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\pirate\3rdge_fulbst_cautiousguard_pirate_v01_loop_ak74.mab",
        ],
        ("cautious", true) => &[
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\privateer\3rdpriv_fulbst_cautiousguard_privateer_v00_loop_ak74.mab",
            r"smart_terrain_points\fighters\behavior_specific\cautious\cautiousguard\privateer\3rdpriv_fulbst_cautiousguard_privateer_v01_loop_ak74.mab",
        ],
        ("smoke", _) => &[
            r"smart_terrain_points\fighters\faction_specific\generic\simple_animation\stand\smoke\3rdge_fulbst_idlerelax_smoke_loop_v00_+000fw_nowep.mab",
            r"smart_terrain_points\fighters\faction_specific\generic\simple_animation\stand\smoke\3rdge_fulbst_idlerelax_smoke_loop_v01_+000fw_nowep.mab",
        ],
        ("piss", _) => &[
            r"contextual_actions\patrol\pissing\3rdge_fulbst_pissingloop_v00_+000fw_ak74.mab",
            r"contextual_actions\patrol\pissing\3rdge_fulbst_pissingloop_v01_+000fw_ak74.mab",
        ],
        ("lean", _) => &[r"contextual_actions\patrol\leanonwall\ak74\3rdge_fulbst_idlerelaxleanonwall_v00_ak74.mab"],
        ("lookover", _) => {
            &[r"smart_terrain_points\fighters\behavior_specific\cautious\lookoverobject\3rdge_fulbst_cautious_lookoverobject_v00_+000fw_ak74.mab"]
        }
        ("corner_left", _) => {
            &[r"smart_terrain_points\fighters\behavior_specific\cautious\lookaroundcorner\3rdge_fulbst_lookaroundcorner_left_v00_+000fw_ak74.mab"]
        }
        ("corner_right", _) => {
            &[r"smart_terrain_points\fighters\behavior_specific\cautious\lookaroundcorner\3rdge_fulbst_lookaroundcorner_right_v00_+000fw_ak74.mab"]
        }
        ("gear", _) => &[r"smart_terrain_points\fighters\theme_specific\weapons_handling\verifygear\3rdge_fulbst_verifygear_merc_000fw_ak74.mab"],
        ("sit", _) => {
            &[r"smart_terrain_points\fighters\faction_specific\generic\simple_animation\ground\3rdge_fulbst_idlerelax_sitground_loop_+000fw_ak74.mab"]
        }
        ("flies", _) => &[r"smart_terrain_points\fighters\theme_specific\dead_bodies\swatflies\3rdge_fulbst_cautiousak_swatflies_v00_000fw_ak74.mab"],
        _ => &[],
    }
}

fn kind_of(s: &str) -> Option<&'static str> {
    ["relax", "cautious", "smoke", "piss", "lean", "lookover", "corner_left", "corner_right", "gear", "sit", "flies"].into_iter().find(|k| *k == s)
}

fn load_spots(mut commands: Commands, info: Res<CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(cache::GUARD_SPOTS_REL)).unwrap_or_default();
    // A fighters' point playing its own move: its armed loop, from the move
    // table (as an encounter's).
    let moves = fc3_formats::moves::from_text(&std::fs::read_to_string(info.root.join(fc3_formats::moves::CA_MOVES_REL)).unwrap_or_default());
    let list: Vec<Spot> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let token = it.next()?;
            let (kind, clip, first) = match token.strip_prefix("move:") {
                Some(mv) => ("move", Some(fighter_clip(fc3_formats::moves::pick_clip(moves.get(mv)?, true)?)?), start_clip(mv, |m| moves.get(m))),
                None => (kind_of(token)?, None, None),
            };
            let mut v = [0.0f32; 7];
            for c in &mut v {
                *c = it.next()?.parse().ok()?;
            }
            let (at, yaw) = (Vec3::new(v[0], v[1], v[2]), Quat::from_xyzw(v[3], v[4], v[5], v[6]).to_euler(EulerRot::YXZ).0);
            let abort = token.strip_prefix("move:").map(|mv| abort_clips(mv, |m| moves.get(m))).unwrap_or_default();
            Some(Spot { kind, at, yaw, clip, owner: None, part: false, start: first.map(|c| (c, at, yaw)), abort })
        })
        .collect();
    info!("{} guard spots", list.len());
    commands.insert_resource(GuardSpots { list, taken: HashMap::new() });
}

/// A point's move's start (`..._loop`, `..._wait`: `..._start`), its clip
/// for a guard (armed), if it has one of its own (some starts name only the
/// loop's clip): getting into the loop.
fn start_clip<'a>(mv: &str, moves: impl Fn(&str) -> Option<&'a Vec<String>>) -> Option<String> {
    let base = mv.strip_suffix("_loop").or_else(|| mv.strip_suffix("_wait"))?;
    let looped = fc3_formats::moves::pick_clip(moves(mv)?, true);
    fc3_formats::moves::pick_clip(moves(&format!("{base}_start"))?, true).filter(|s| Some(*s) != looped).cloned()
}

/// A point's move's abort clips (`..._abort`): getting up from it, startled
/// or curious.
fn abort_clips<'a>(mv: &str, moves: impl Fn(&str) -> Option<&'a Vec<String>>) -> Vec<String> {
    let Some(base) = mv.strip_suffix("_loop").or_else(|| mv.strip_suffix("_wait")) else { return Vec::new() };
    moves(&format!("{base}_abort")).cloned().unwrap_or_default()
}

/// Of a move's abort clips, the one for being startled (`threat`) or only
/// curious (`interest`), else the first.
fn abort_for(abort: &[(String, Handle<Fc3Clip>)], threat: bool) -> Option<&Handle<Fc3Clip>> {
    let word = if threat { "threat" } else { "interest" };
    abort.iter().find(|(n, _)| n.rsplit('\\').next().is_some_and(|f| f.contains(word))).or_else(|| abort.first()).map(|(_, h)| h)
}

/// A clip a guard may play at a spot: a fighter's, not a civilian's.
fn fighter_clip(clip: &str) -> Option<String> {
    let lower = clip.to_ascii_lowercase();
    let civilian = lower.contains(r"\civilians\") || lower.rsplit('\\').next().is_some_and(|f| f.starts_with("3rdciv"));
    (!civilian).then(|| clip.to_string())
}

/// A spot an encounter sets out with its people (by what is done there; a
/// scene's part, `clip:<path>`, for any of them).
#[derive(Message, Clone)]
pub struct AddGuardSpot {
    pub kind: String,
    pub at: Vec3,
    pub yaw: f32,
    /// A scene part's start: its clip, and where it begins.
    pub start: Option<(String, Vec3, f32)>,
}

/// Encounters' spots join the world's (once: the same spot set out again is
/// the one already there).
fn add_spots(mut added: MessageReader<AddGuardSpot>, spots: Option<ResMut<GuardSpots>>, moves: Option<Res<crate::story_actors::CaMoves>>) {
    let Some(mut spots) = spots else {
        added.clear();
        return;
    };
    for a in added.read() {
        // A spot playing its own move (its armed loop) or its part of a
        // scene's loop (`clip:`); a fighters' one: a mourner's or a
        // villager's is not for guards.
        let own = match (a.kind.strip_prefix("move:"), a.kind.strip_prefix("clip:")) {
            (Some(mv), _) => moves.as_ref().and_then(|m| fc3_formats::moves::pick_clip(m.0.get(mv)?, true).cloned()).map(Some),
            (_, Some(clip)) => Some(Some(clip.to_string())),
            _ => Some(None),
        };
        let part = a.kind.starts_with("clip:");
        let (kind, clip) = match own {
            Some(Some(clip)) => {
                let Some(clip) = fighter_clip(&clip) else { continue };
                ("move", Some(clip))
            }
            // (A move without clips.)
            None => continue,
            Some(None) => {
                let Some(kind) = kind_of(&a.kind) else { continue };
                (kind, None)
            }
        };
        // (A move's start, as the world's.)
        let start = a.start.clone().or_else(|| {
            let mv = a.kind.strip_prefix("move:")?;
            Some((start_clip(mv, |m| moves.as_ref()?.0.get(m))?, a.at, a.yaw))
        });
        let abort = a.kind.strip_prefix("move:").map(|mv| abort_clips(mv, |m| moves.as_ref()?.0.get(m))).unwrap_or_default();
        if !spots.list.iter().any(|s| s.at.distance(a.at) < 0.5) {
            spots.list.push(Spot { kind, at: a.at, yaw: a.yaw, clip, owner: None, part, start, abort });
        }
    }
}

/// Shows or hides what a rig holds in its right hand.
pub(crate) fn hold_weapon(rig: &CharacterRig, held: &mut Query<(&ChildOf, &mut Visibility), With<HeldWeapon>>, shown: bool) {
    if let Some((_, hand)) = rig.joint(name_hash("RightHand")) {
        for (_, mut v) in held.iter_mut().filter(|(c, _)| c.parent() == hand) {
            *v = if shown { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
}

/// Idle guards (not patrolling, not heavies) take the nearest free spot by
/// their post.
#[allow(clippy::too_many_arguments)]
fn take_spots(
    time: Res<Time>,
    spots: Option<ResMut<GuardSpots>>,
    server: Res<AssetServer>,
    mut npcs: Query<(Entity, &mut Npc, &Health, Option<&OwnSpot>), (Without<AtSpot>, Without<SpotChecked>)>,
    all: Query<(), With<Npc>>,
    mut rigs: Query<(&mut CharacterRig, Option<&mut PartMotion>)>,
    mut held: Query<(&ChildOf, &mut Visibility), With<HeldWeapon>>,
    mut tidy: Local<f32>,
    mut commands: Commands,
) {
    let Some(mut spots) = spots else { return };
    // Free the spots of guards who are gone.
    *tidy -= time.delta_secs();
    if *tidy <= 0.0 {
        *tidy = 2.0;
        spots.taken.retain(|_, e| all.get(*e).is_ok());
    }
    for (e, mut npc, h, own_spot) in &mut npcs {
        let Some(rig_e) = npc.rig else {
            commands.entity(e).insert(SpotChecked);
            continue;
        };
        let Ok((mut rig, motion)) = rigs.get_mut(rig_e) else { continue };
        // (One with his own part keeps looking until he can take it, and
        // goes back to it once things are calm again.)
        match own_spot {
            Some(_) if h.dead => {
                commands.entity(e).insert(SpotChecked);
                continue;
            }
            Some(_) if npc.state != NpcState::Idle || npc.awareness > 0.1 => continue,
            Some(_) => {}
            None => {
                commands.entity(e).insert(SpotChecked);
            }
        }
        // His own part in a scene: a spot only he takes.
        if let Some(o) = own_spot {
            let i = match spots.list.iter().position(|s| s.at.distance(o.at) < 0.5) {
                Some(i) => i,
                None => {
                    spots.list.push(Spot { kind: "move", at: o.at, yaw: o.yaw, clip: None, owner: None, part: true, start: None, abort: Vec::new() });
                    spots.list.len() - 1
                }
            };
            let s = &mut spots.list[i];
            (s.clip, s.owner, s.part, s.start) = (Some(o.clip.clone()), Some(e), true, o.start.clone());
            if spots.taken.get(&i).is_some_and(|t| *t != e) {
                spots.taken.remove(&i);
            }
        }
        // Guards of the world's posts only (not reinforcements or test
        // spawns); a scene's actor whatever he is.
        if h.dead || npc.state != NpcState::Idle || own_spot.is_none() && (npc.role.armoured() || npc.patrols) || npc.marker_id >= crate::npc::FREE_ID_BASE {
            continue;
        }
        // (No rounds for him.)
        if own_spot.is_some() {
            npc.patrols = false;
        }
        let post = npc.post;
        // (His own first; no one else's.)
        let free = spots
            .list
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                !spots.taken.contains_key(i)
                    && s.owner.is_none_or(|o| o == e)
                    && Vec2::new(s.at.x - post.x, s.at.z - post.z).length() < REACH
                    && (s.at.y - post.y).abs() < REACH_UP
            })
            .min_by(|a, b| a.1.owner.is_none().cmp(&b.1.owner.is_none()).then(a.1.at.distance(post).total_cmp(&b.1.at.distance(post))))
            .map(|(i, s)| (i, s.kind, s.at, s.yaw, s.clip.clone(), s.part, s.start.clone(), s.abort.clone()));
        let Some((i, kind, at, yaw, own, part, start, abort)) = free else { continue };
        let path = match own {
            Some(full) => full,
            None => {
                let options = clips(kind, npc.faction == Faction::Privateer);
                let Some(path) = options.get(i % options.len().max(1)) else { continue };
                format!("{CLIP_ROOT}{path}")
            }
        };
        let clip: Handle<Fc3Clip> = server.load(cache_path(&cache::anim_rel(&path)));
        // (A scene's part from its start: there, that clip first.)
        let (first, post, post_yaw) = match &start {
            Some((s, s_at, s_yaw)) => (server.load(cache_path(&cache::anim_rel(s))), *s_at, *s_yaw),
            None => (clip.clone(), at, yaw),
        };
        let base = rig.clips().clone();
        let mut set = ClipSet { skeleton: base.skeleton.clone(), clips: base.clips.clone() };
        set.clips.insert(AnimKind::Idle, first.clone());
        rig.set_clips_blended(Arc::new(set));
        if part {
            set_motion(rig_e, motion, first.clone(), Transform::IDENTITY, &mut commands);
        }
        if start.is_some() {
            commands.entity(e).insert(Entering { start: first, into: clip, at, yaw, playing: false });
        }
        let lower = path.to_ascii_lowercase();
        let unarmed = kind == "smoke" || ["nowep", "noweap", "noweapon"].iter().any(|w| lower.contains(w));
        if unarmed {
            hold_weapon(&rig, &mut held, false);
        }
        npc.post = post;
        npc.post_yaw = post_yaw;
        npc.at_spot = true;
        spots.taken.insert(i, e);
        let abort = abort.iter().map(|p| (p.to_ascii_lowercase(), server.load(cache_path(&cache::anim_rel(p))))).collect();
        commands.entity(e).insert(AtSpot { index: i, base, unarmed, part, abort });
        // FC3_SPOT_DEBUG: who took which.
        if std::env::var("FC3_SPOT_DEBUG").is_ok() {
            info!("guard spot: {kind} at {at:.1?} facing {:.0}", yaw.to_degrees());
        }
    }
}

/// `FC3_SPOT_TEST[=kind]` (with `--noclip` near an outpost): from 25 s in,
/// stands the player a few metres in front of the nearest posted guard (of
/// that kind) and looks at him, logging what he's doing.
fn spot_test(
    time: Res<Time>,
    spots: Option<Res<GuardSpots>>,
    mut done: Local<bool>,
    guards: Query<(&Transform, &AtSpot, &Npc), Without<crate::player::Player>>,
    mut players: Query<(&mut Transform, &mut crate::player::Player)>,
) {
    let Ok(want) = std::env::var("FC3_SPOT_TEST") else { return };
    let (Some(spots), Ok((mut pt, mut p))) = (spots, players.single_mut()) else { return };
    if time.elapsed_secs() < 25.0 {
        return;
    }
    let near = guards
        .iter()
        .filter(|(_, at, _)| want.is_empty() || want == "1" || spots.list[at.index].kind == want)
        .min_by(|a, b| a.0.translation.distance(pt.translation).total_cmp(&b.0.translation.distance(pt.translation)));
    let Some((gt, at, npc)) = near else { return };
    let facing = Vec3::new(-npc.post_yaw.sin(), 0.0, -npc.post_yaw.cos());
    pt.translation = gt.translation + facing * 7.0 + Vec3::Y * 0.6;
    let to = gt.translation - pt.translation;
    p.yaw = (-to.x).atan2(-to.z);
    p.pitch = (to.y / Vec2::new(to.x, to.z).length().max(0.1)).atan();
    if !*done {
        *done = true;
        info!("spot-test: a guard at a {} spot", spots.list[at.index].kind);
    }
}

/// Put on guard (or killed), a guard leaves his spot: his own clips and
/// weapon back.
fn leave_spots(
    spots: Option<ResMut<GuardSpots>>,
    clips: Res<Assets<Fc3Clip>>,
    mut npcs: Query<(Entity, &mut Npc, &AtSpot, &Health)>,
    mut rigs: Query<&mut CharacterRig>,
    mut held: Query<(&ChildOf, &mut Visibility), With<HeldWeapon>>,
    mut commands: Commands,
) {
    let Some(mut spots) = spots else { return };
    for (e, mut npc, at, h) in &mut npcs {
        if !(h.dead || npc.state != NpcState::Idle || npc.awareness > 0.25) {
            continue;
        }
        if !h.dead {
            if let Some(mut rig) = npc.rig.and_then(|r| rigs.get_mut(r).ok()) {
                // Getting up from it, startled (in a fight, or alarmed) or
                // curious: its abort clip first, where he is.
                let threat = npc.state == NpcState::Combat || npc.awareness > 0.75;
                let getting_up = abort_for(&at.abort, threat).and_then(|h| Some((h.clone(), clips.get(h)?.0.duration)));
                match getting_up {
                    Some((clip, d)) => {
                        let mut set = ClipSet::clone(&at.base);
                        set.clips.insert(AnimKind::Hit, clip);
                        rig.set_clips_blended(Arc::new(set));
                        npc.breaking_off = d;
                        if std::env::var("FC3_SPOT_DEBUG").is_ok() {
                            info!("guard spot: getting up {} ({d:.1} s)", if threat { "startled" } else { "curious" });
                        }
                    }
                    None => rig.set_clips_blended(at.base.clone()),
                }
                if at.unarmed {
                    hold_weapon(&rig, &mut held, true);
                }
            }
        }
        npc.at_spot = false;
        spots.taken.remove(&at.index);
        commands.entity(e).remove::<(AtSpot, Entering)>();
    }
}
