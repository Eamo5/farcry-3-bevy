//! Quick-time events: the game's `QTEGSF` specific gameplay (the first
//! struggle with a pirate, the knife fights).
//!
//! A QTE names a move for the player and one for the opponent; the move
//! database (`fc3_formats::moves`, in the cache) gives each move's states
//! and clips: `Intro`, a `Struggle` that loops while the player fights it,
//! then `Exit` (won) or `Fail`. The phases play as sequences in the
//! cinematic player, from the scene's anchor, the opponent bound to its
//! character. While it struggles the player presses the interact key to
//! fill a meter before time runs out. The scripts hear `QTE0_InProgress` as
//! the struggle starts, `QTE0_Success` or `QTE0_Fail` as it is decided (and
//! `PlayerDie` once a lost one has played out), then the gameplay ends.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};
use fc3_formats::moves::MoveStates;

use crate::cache_assets::{cache_path, Fc3Clip};
use crate::cinema::{Built, Cinema};
use crate::controls::{Action, ActionState};
use crate::data::{to_bevy, CacheInfo};
use crate::story::StoryHost;

pub struct QtePlugin;

impl Plugin for QtePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Qtes>()
            .init_resource::<MoveTable>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_moves)
            .add_systems(Update, run_qtes.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// Each press fills this share of the meter, which drains at `DRAIN` a
/// second; a full meter wins, `TIME_LIMIT` seconds of struggle lose.
const PRESS: f32 = 0.12;
const DRAIN: f32 = 0.16;
const TIME_LIMIT: f32 = 7.0;

/// The engine's moves by lower-case name.
#[derive(Resource, Default)]
pub struct MoveTable(pub BTreeMap<String, MoveStates>);

fn load_moves(info: Res<CacheInfo>, mut table: ResMut<MoveTable>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::moves::MOVES_REL)).unwrap_or_default();
    table.0 = fc3_formats::moves::moves_from_text(&text);
}

/// A QTE the story created: its gameplay id and parameters.
pub struct QteStart {
    pub id: u64,
    pub params: HashMap<String, String>,
}

#[derive(Resource, Default)]
pub struct Qtes {
    pub pending: Vec<QteStart>,
    /// Gameplays the scripts destroyed.
    pub cancelled: Vec<u64>,
    active: Option<Qte>,
}

impl Qtes {
    /// A QTE is playing (the player's input is its).
    pub fn active(&self) -> bool {
        self.active.is_some()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.cancelled.clear();
        self.active = None;
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Loading,
    Intro,
    Struggle,
    Outro { won: bool },
}

struct Qte {
    id: u64,
    phase: Phase,
    /// Per state (`intro`, `struggle`, `exit`, `fail`): the player's and the
    /// opponent's clips.
    clips: HashMap<&'static str, (Option<String>, Option<String>)>,
    /// Their clips loading, by archive path.
    handles: HashMap<String, Handle<Fc3Clip>>,
    opponent: Option<u64>,
    origin: Transform,
    meter: f32,
    t: f32,
    /// The phase's sequence playing.
    playing: Option<String>,
}

#[derive(Component)]
struct QtePrompt;

#[derive(Component)]
struct QteMeter;

/// Plays a QTE state's clips (the player's, and the opponent's on its
/// character) as a sequence from the anchor.
fn play(cinema: &mut Cinema, q: &mut Qte, clips: &Assets<Fc3Clip>, state: &'static str, looping: bool) {
    let (mine, theirs) = q.clips.get(state).cloned().unwrap_or_default();
    let Some(mine) = mine else {
        q.playing = None;
        return;
    };
    let d = q.handles.get(&mine).and_then(|h| clips.get(h)).map_or(2.0, |c| c.0.duration).max(0.1);
    let anim = |source: String| Clip { start: 0.0, duration: d, kind: ClipKind::Animation { source, from: 0, to: (d * 30.0) as u32, blend: 0.15 } };
    let mut groups = vec![Group { uid: 1, kind: GroupKind::Player, name: "player".into(), archetype: 0, clips: vec![anim(mine)] }];
    let mut bindings = HashMap::new();
    if let (Some(theirs), Some(npc)) = (theirs, q.opponent) {
        groups.push(Group { uid: 2, kind: GroupKind::Character, name: "opponent".into(), archetype: 0, clips: vec![anim(theirs)] });
        bindings.insert(2, npc);
    }
    let name = format!("qte_{}_{state}", q.id);
    cinema.play_built(Built {
        name: name.clone(),
        seq: Sequence { name: name.clone(), duration: d, looping, groups, ..Default::default() },
        origin: q.origin,
        bindings,
    });
    q.playing = Some(name);
}

/// A move's clip for a state (by the state's name, any case).
fn state_clip(states: Option<&MoveStates>, state: &str) -> Option<String> {
    states?.iter().find(|(s, _)| s.eq_ignore_ascii_case(state)).map(|(_, c)| c.clone())
}

#[allow(clippy::too_many_arguments)]
fn run_qtes(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    mut qtes: ResMut<Qtes>,
    table: Res<MoveTable>,
    mut cinema: ResMut<Cinema>,
    server: Res<AssetServer>,
    clips: Res<Assets<Fc3Clip>>,
    (mut actions, settings): (ResMut<ActionState>, Res<crate::settings::Settings>),
    prompts: Query<Entity, With<QtePrompt>>,
    mut meters: Query<&mut Node, With<QteMeter>>,
    mut commands: Commands,
) {
    // Tutorial modals pause virtual time while remaining in Playing. Their
    // inputs must not also fill a struggle meter in the paused world.
    if time.delta_secs() <= 0.0 {
        return;
    }
    let Some(mut host) = host else { return };
    let qtes = &mut *qtes;
    for start in std::mem::take(&mut qtes.pending) {
        let player_move = start.params.get("QTE_move_file").map(|m| m.to_ascii_lowercase()).unwrap_or_default();
        let npc_move = start.params.get("Optional_QTE_move_file").map(|m| m.to_ascii_lowercase());
        let (mine, theirs) = (table.0.get(&player_move), npc_move.as_ref().and_then(|m| table.0.get(m)));
        if mine.is_none() {
            warn!("qte: no move {player_move}; it ends at once");
            host.story.gameplay_ended(start.id);
            continue;
        }
        let mut by_state = HashMap::new();
        for (key, names) in
            [("intro", &["Intro"][..]), ("struggle", &["Struggle", "Idle", "Progress"][..]), ("exit", &["Exit", "Success"][..]), ("fail", &["Fail"][..])]
        {
            let pick = |states| names.iter().find_map(|n| state_clip(states, n));
            by_state.insert(key, (pick(mine), pick(theirs)));
        }
        let handles =
            by_state.values().flat_map(|(a, b)| [a, b]).flatten().map(|c| (c.clone(), server.load(cache_path(&fc3_formats::cache::anim_rel(c))))).collect();
        let data = host.story.data();
        let anchor = start.params.get("AnchorScene").and_then(|a| a.parse::<u64>().ok()).and_then(|a| data.entities.get(&a));
        let origin = anchor
            .map(|e| Transform { translation: to_bevy(e.pos), rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)), scale: Vec3::ONE })
            .unwrap_or_default();
        let opponent = start.params.get("Optional_pawn/EntityID").and_then(|e| e.parse::<u64>().ok());
        info!("qte: {player_move} (opponent {opponent:?})");
        qtes.active = Some(Qte { id: start.id, phase: Phase::Loading, clips: by_state, handles, opponent, origin, meter: 0.3, t: 0.0, playing: None });
    }
    for id in std::mem::take(&mut qtes.cancelled) {
        if qtes.active.as_ref().is_some_and(|q| q.id == id) {
            if let Some(name) = qtes.active.as_ref().and_then(|q| q.playing.clone()) {
                cinema.stop_built(&name);
            }
            qtes.active = None;
        }
    }
    let Some(q) = qtes.active.as_mut() else {
        for e in &prompts {
            commands.entity(e).despawn();
        }
        return;
    };
    let dt = time.delta_secs();
    let done = q.playing.as_ref().is_none_or(|n| cinema.progress(n).is_none());
    let mut ended = None;
    match q.phase {
        Phase::Loading => {
            q.t += dt;
            let settled = q.handles.values().all(|h| crate::cache_assets::settled(&server, h.id()));
            if settled || q.t > 4.0 {
                q.phase = Phase::Intro;
                play(&mut cinema, q, &clips, "intro", false);
            }
        }
        Phase::Intro if done => {
            q.phase = Phase::Struggle;
            q.t = 0.0;
            play(&mut cinema, q, &clips, "struggle", true);
            host.story.message("QTE0_InProgress");
        }
        Phase::Struggle => {
            q.t += dt;
            if actions.just_pressed(Action::Interact) {
                q.meter += PRESS;
            }
            q.meter = (q.meter - DRAIN * dt).max(0.0);
            let autopilot = std::env::var_os("FC3_STORY_AUTOPILOT").is_some() && q.t > 1.5;
            let outcome = if q.meter >= 1.0 || autopilot {
                Some(true)
            } else if q.t > TIME_LIMIT {
                Some(false)
            } else {
                None
            };
            if let Some(won) = outcome {
                if let Some(n) = q.playing.take() {
                    cinema.stop_built(&n);
                }
                q.phase = Phase::Outro { won };
                play(&mut cinema, q, &clips, if won { "exit" } else { "fail" }, false);
                host.story.message(if won { "QTE0_Success" } else { "QTE0_Fail" });
                info!("qte: {}", if won { "won" } else { "lost" });
            }
        }
        Phase::Outro { won } if done => {
            if !won {
                host.story.message("PlayerDie");
            }
            ended = Some(q.id);
        }
        _ => {}
    }
    if let Some(id) = ended {
        qtes.active = None;
        host.story.gameplay_ended(id);
    }
    // The prompt and its meter while struggling; the struggle has the
    // player's presses to itself.
    let struggling = qtes.active.as_ref().is_some_and(|q| q.phase == Phase::Struggle);
    if !struggling {
        for e in &prompts {
            commands.entity(e).despawn();
        }
        return;
    }
    let meter = qtes.active.as_ref().map_or(0.0, |q| q.meter.min(1.0));
    actions.consume(Action::Interact);
    if prompts.is_empty() {
        let key = actions.label(Action::Interact, &settings);
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Percent(22.0),
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(10.0),
                ..default()
            },
            GlobalZIndex(9),
            QtePrompt,
            children![
                (
                    Text::new(format!("[{key}]  STRUGGLE")),
                    TextFont { font_size: bevy::text::FontSize::Px(30.0), ..default() },
                    TextColor(Color::srgb(1.0, 0.85, 0.35))
                ),
                (
                    Node { width: Val::Px(320.0), height: Val::Px(10.0), ..default() },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
                    children![(
                        Node { width: Val::Percent(30.0), height: Val::Percent(100.0), ..default() },
                        BackgroundColor(Color::srgb(1.0, 0.85, 0.35)),
                        QteMeter
                    )],
                ),
            ],
        ));
    }
    for mut n in &mut meters {
        n.width = Val::Percent(meter * 100.0);
    }
}
