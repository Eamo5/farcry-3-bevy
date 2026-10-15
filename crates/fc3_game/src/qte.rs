//! Quick-time events: the game's `QTEGSF` specific gameplay (the first
//! struggle with a pirate, the escape crawl, the knife fights).
//!
//! A QTE names a move for the player and one for the opponent, and the
//! move's own data runs it (`fc3_formats::qte`, in the cache): its states
//! play under the QTEs' results (none, in progress, failed, succeeded), and
//! the QTEs are events in their clips. A press, a hold or a mash asks the
//! player for an input (the prompt, and a meter for a mash or a hold); a
//! "zombie" QTE waits for the scripts (`QteSetResult`). The state's clip
//! plays to its end before the next is chosen, unless a QTE is decided and
//! the state no longer holds (a press answered while idle crawls at once).
//! A state played from the scene's anchor starts there; others go on from
//! where the last one left the player. The phases play as sequences in the
//! cinematic player, the opponent bound to its character.
//!
//! The scripts hear `QTE<n>_InProgress` as a QTE starts, `QTE<n>_Success`
//! or `QTE<n>_Fail` as it is decided, the clips' own messages (`PlayerDie`
//! once a lost fight has played out), and `ExoticQteGSFEnded` as the
//! sequence ends, then the gameplay ends.

use std::collections::{BTreeMap, HashMap};

use bevy::prelude::*;
use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};
use fc3_formats::moves::MoveStates;
use fc3_formats::qte::{self as q, Kind, QteTable};

use crate::cache_assets::{cache_path, Fc3Clip};
use crate::cinema::{clip_root, Built, Cinema};
use crate::controls::{Action, ActionState};
use crate::data::{to_bevy, CacheInfo};
use crate::story::StoryHost;

pub struct QtePlugin;

impl Plugin for QtePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Qtes>()
            .init_resource::<MoveTable>()
            .init_resource::<QteMoves>()
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_moves, load_qte_moves))
            .add_systems(Update, run_qtes.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The engine's moves by lower-case name.
#[derive(Resource, Default)]
pub struct MoveTable(pub BTreeMap<String, MoveStates>);

fn load_moves(info: Res<CacheInfo>, mut table: ResMut<MoveTable>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::moves::MOVES_REL)).unwrap_or_default();
    table.0 = fc3_formats::moves::moves_from_text(&text);
}

/// The QTE moves and their clips' QTE events.
#[derive(Resource, Default)]
pub struct QteMoves(pub QteTable);

fn load_qte_moves(info: Res<CacheInfo>, mut moves: ResMut<QteMoves>) {
    let text = std::fs::read_to_string(info.root.join(q::QTE_REL)).unwrap_or_default();
    if !QteTable::is_current(&text) {
        warn!("qte: the cache has no current {} (run the converter, or `fc3tool qte-moves write`); QTEs end at once", q::QTE_REL);
    }
    moves.0 = QteTable::from_text(&text);
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
    /// QTEs the scripts decided (`QteSetResult`): (QTE id, succeeded).
    pub decided: Vec<(u8, bool)>,
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
        self.decided.clear();
        self.active = None;
    }
}

/// How many QTEs a move runs at once (their ids).
const SLOTS: usize = 8;

/// How long the story autopilot takes to answer a QTE.
const AUTOPILOT_ANSWER: f32 = 0.3;

/// The longest a QTE plays before it is ended (a guard against one nothing
/// decides).
const MAX_AGE: f32 = 300.0;

/// A QTE in progress that asks the player for an input.
#[derive(Clone)]
struct Asked {
    kind: Kind,
    input: u8,
    prompt: Option<u32>,
    values: [f32; 4],
    /// Time since it started; a mash's intensity or a hold's time held; the
    /// last counted press.
    t: f32,
    progress: f32,
    last: f32,
}

struct Qte {
    id: u64,
    /// The player's move, and the opponent's (both keys of the table).
    player: String,
    npc: Option<String>,
    opponent: Option<u64>,
    anchor: Transform,
    results: [u8; SLOTS],
    asked: [Option<Asked>; SLOTS],
    handles: HashMap<String, Handle<Fc3Clip>>,
    loading: f32,
    /// The state playing (in the player's move), its clip's time (and the
    /// time its events have fired to) and length, the sequence playing it
    /// and where it plays from.
    state: Option<usize>,
    t: f32,
    fired: f32,
    length: f32,
    playing: Option<String>,
    origin: Transform,
    plays: u32,
    age: f32,
    ended: bool,
}

impl Qte {
    /// A QTE decided: its result, and the scripts told. Whether it changed.
    fn decide(&mut self, host: &mut StoryHost, id: usize, won: bool) -> bool {
        let r = if won { q::SUCCEEDED } else { q::FAILED };
        if id >= SLOTS || self.results[id] == r {
            return false;
        }
        self.results[id] = r;
        host.story.message(&format!("QTE{id}_{}", if won { "Success" } else { "Fail" }));
        info!("qte: QTE{id} {}", if won { "succeeded" } else { "failed" });
        true
    }
}

/// The actions that answer a QTE's input (`fc3_formats::qte::INPUTS`): the
/// gamepad's buttons and sticks as the controls map them.
fn answers(input: u8) -> &'static [Action] {
    match q::input_name(input) {
        "Button_1" => &[Action::Interact, Action::Jump],
        "Button_2" => &[Action::Crouch],
        "Button_3" => &[Action::Reload, Action::Interact],
        "Button_4" => &[Action::NextWeapon],
        "RT" => &[Action::Fire],
        "LT" => &[Action::Aim],
        "LB" => &[Action::Grenade],
        "RB" => &[Action::Melee],
        "Up" | "LeftStickUp" | "RightStickUp" => &[Action::Forward],
        "Down" | "LeftStickDown" | "RightStickDown" => &[Action::Back],
        "Left" | "LeftStickLeft" | "RightStickLeft" => &[Action::Left],
        "Right" | "LeftStickRight" | "RightStickRight" => &[Action::Right],
        _ => &[Action::Interact, Action::Jump, Action::Fire],
    }
}

/// A clip loaded for the QTE, by archive path.
fn clip_of<'a>(handles: &HashMap<String, Handle<Fc3Clip>>, clips: &'a Assets<Fc3Clip>, path: &Option<String>) -> Option<&'a Fc3Clip> {
    path.as_ref().and_then(|p| handles.get(p)).and_then(|h| clips.get(h))
}

#[derive(Component)]
struct QtePrompt;

#[derive(Component)]
struct QteMeter;

#[allow(clippy::too_many_arguments)]
fn run_qtes(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    mut qtes: ResMut<Qtes>,
    table: Res<QteMoves>,
    mut cinema: ResMut<Cinema>,
    server: Res<AssetServer>,
    clips: Res<Assets<Fc3Clip>>,
    (mut actions, settings): (ResMut<ActionState>, Res<crate::settings::Settings>),
    prompts: Query<Entity, With<QtePrompt>>,
    mut meters: Query<&mut Node, With<QteMeter>>,
    mut shown: Local<String>,
    mut commands: Commands,
) {
    // Tutorial modals pause virtual time while remaining in Playing. Their
    // inputs must not also answer a QTE in the paused world.
    if time.delta_secs() <= 0.0 {
        return;
    }
    let Some(mut host) = host else { return };
    let qtes = &mut *qtes;
    let moves = &table.0;
    for start in std::mem::take(&mut qtes.pending) {
        let name = |key: &str| start.params.get(key).map(|m| m.to_ascii_lowercase());
        let player = name("QTE_move_file").unwrap_or_default();
        let Some(states) = moves.moves.get(&player) else {
            warn!("qte: no QTE move {player}; it ends at once");
            host.story.gameplay_ended(start.id);
            continue;
        };
        let npc = name("Optional_QTE_move_file").filter(|m| moves.moves.contains_key(m));
        let paths: Vec<&String> = states.iter().chain(npc.iter().flat_map(|m| &moves.moves[m])).filter_map(|s| s.clip.as_ref()).collect();
        let handles = paths.into_iter().map(|c| (c.clone(), server.load(cache_path(&fc3_formats::cache::anim_rel(c))))).collect();
        let data = host.story.data();
        let anchor = start.params.get("AnchorScene").and_then(|a| a.parse::<u64>().ok()).and_then(|a| data.entities.get(&a));
        let anchor = anchor
            .map(|e| Transform { translation: to_bevy(e.pos), rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)), scale: Vec3::ONE })
            .unwrap_or_default();
        let opponent = start.params.get("Optional_pawn/EntityID").and_then(|e| e.parse::<u64>().ok());
        info!("qte: {player} (opponent {opponent:?}, {npc:?})");
        qtes.active = Some(Qte {
            id: start.id,
            player,
            npc,
            opponent,
            anchor,
            results: [q::NONE; SLOTS],
            asked: std::array::from_fn(|_| None),
            handles,
            loading: 0.0,
            state: None,
            t: 0.0,
            fired: -1.0,
            length: 0.0,
            playing: None,
            origin: anchor,
            plays: 0,
            age: 0.0,
            ended: false,
        });
    }
    for id in std::mem::take(&mut qtes.cancelled) {
        if qtes.active.as_ref().is_some_and(|q| q.id == id) {
            if let Some(name) = qtes.active.as_ref().and_then(|q| q.playing.clone()) {
                cinema.stop_built(&name);
            }
            qtes.active = None;
        }
    }
    let decided = std::mem::take(&mut qtes.decided);
    let Some(qt) = qtes.active.as_mut() else {
        for e in &prompts {
            commands.entity(e).despawn();
        }
        shown.clear();
        return;
    };
    let dt = time.delta_secs();
    qt.age += dt;
    let states = &moves.moves[&qt.player];
    if qt.state.is_none() {
        qt.loading += dt;
        let settled = qt.handles.values().all(|h| crate::cache_assets::settled(&server, h.id()));
        if !settled && qt.loading < 4.0 {
            return;
        }
    }

    // The scripts' and the player's answers.
    let mut decisive = false;
    for (id, won) in decided {
        if let Some(slot) = qt.asked.get_mut(id as usize) {
            *slot = None;
        }
        decisive |= qt.decide(&mut host, id as usize, won);
    }
    let autopilot = std::env::var_os("FC3_STORY_AUTOPILOT").is_some();
    for id in 0..SLOTS {
        let Some(a) = qt.asked[id].as_mut() else { continue };
        a.t += dt;
        let acts = answers(a.input);
        let pressed = acts.iter().any(|&x| actions.just_pressed(x));
        let held = acts.iter().any(|&x| actions.pressed(x));
        let auto = autopilot && a.t > AUTOPILOT_ANSWER;
        let outcome = match a.kind {
            Kind::Press if pressed || auto => Some(true),
            Kind::Press => (a.values[1] > 0.0 && a.t > a.values[1]).then_some(false),
            Kind::Hold => {
                if held || auto {
                    a.progress += dt;
                } else {
                    // Let go: in progress again until held long enough.
                    a.progress = 0.0;
                    if qt.results[id] == q::SUCCEEDED {
                        qt.results[id] = q::IN_PROGRESS;
                    }
                }
                if a.progress >= a.values[0].max(0.01) && qt.results[id] != q::SUCCEEDED {
                    Some(true)
                } else {
                    (a.values[1] > 0.0 && a.t > a.values[1] && a.progress <= 0.0 && qt.results[id] == q::IN_PROGRESS).then_some(false)
                }
            }
            Kind::Mash => {
                if (pressed && a.t - a.last >= a.values[2]) || (auto && a.t - a.last >= 0.1) {
                    a.progress += 1.0;
                    a.last = a.t;
                }
                a.progress = (a.progress - a.values[1] * dt).max(0.0);
                if a.progress >= a.values[0].max(1.0) {
                    Some(true)
                } else {
                    (a.values[3] > 0.0 && a.t > a.values[3]).then_some(false)
                }
            }
            _ => None,
        };
        if let Some(won) = outcome {
            // A hold stays asked: let go, it is in progress again.
            if !(a.kind == Kind::Hold && won) {
                qt.asked[id] = None;
            }
            decisive |= qt.decide(&mut host, id, won);
        }
    }

    // The state's clip: its events, then the next state at its end (or at
    // once when a QTE decided leaves it).
    if let Some(s) = qt.state {
        qt.t += dt;
        let events = states[s].clip.as_ref().and_then(|c| moves.clips.get(c)).map(Vec::as_slice).unwrap_or_default();
        for e in events.iter().filter(|e| e.time > qt.fired && e.time <= qt.t) {
            let id = e.id as usize;
            match e.kind {
                Kind::Press | Kind::Hold | Kind::Mash | Kind::Zombie if id < SLOTS => {
                    qt.asked[id] = (e.kind != Kind::Zombie).then(|| Asked {
                        kind: e.kind,
                        input: e.input,
                        prompt: e.prompt,
                        values: e.values,
                        t: 0.0,
                        progress: 0.0,
                        last: -1.0,
                    });
                    qt.results[id] = q::IN_PROGRESS;
                    host.story.message(&format!("QTE{id}_InProgress"));
                }
                Kind::End if id < SLOTS => qt.asked[id] = None,
                Kind::Reset if id < SLOTS => {
                    qt.results[id] = (e.values[1] as u8).min(q::NONE);
                    if let Some(a) = qt.asked[id].as_mut() {
                        a.progress = e.values[0];
                    }
                }
                Kind::EndSequence => qt.ended = true,
                Kind::PlayerDeath => host.story.message("PlayerDie"),
                Kind::Message if !e.text.is_empty() => host.story.message(&e.text),
                _ => {}
            }
        }
        qt.fired = qt.t;
    }
    let leaves = qt.state.is_some_and(|s| decisive && !q::holds_within(states, s, &qt.results));
    if !qt.ended && (qt.state.is_none() || leaves || qt.t >= qt.length) {
        // (Nothing to play: the state plays again.)
        if let Some(n) = q::select(states, &qt.results).or(qt.state) {
            let next = &states[n];
            let origin = match qt.state {
                Some(s) if !next.anchored => {
                    // On from where the last clip has left the player.
                    let now = clip_of(&qt.handles, &clips, &states[s].clip).map_or(qt.origin, |c| qt.origin * clip_root(&c.0, qt.t.min(c.0.duration)));
                    let from = clip_of(&qt.handles, &clips, &next.clip).map_or(Transform::IDENTITY, |c| clip_root(&c.0, 0.0));
                    now * Transform::from_matrix(from.to_matrix().inverse())
                }
                _ => qt.anchor,
            };
            let npc_clip = qt.npc.as_ref().and_then(|m| q::select(&moves.moves[m], &qt.results).and_then(|i| moves.moves[m][i].clip.clone()));
            let length = |path: &Option<String>| clip_of(&qt.handles, &clips, path).map_or(2.0, |c| c.0.duration).max(0.1);
            let d = length(&next.clip);
            let anim = |source: String, d: f32| Clip {
                start: 0.0,
                duration: d,
                kind: ClipKind::Animation { source, from: 0, to: (d * 30.0) as u32, blend: 0.15, anchored: false },
            };
            let mut groups = Vec::new();
            if let Some(mine) = next.clip.clone() {
                groups.push(Group { uid: 1, kind: GroupKind::Player, name: "player".into(), archetype: 0, clips: vec![anim(mine, d)] });
            }
            let mut bindings = HashMap::new();
            if let (Some(theirs), Some(npc)) = (npc_clip.clone(), qt.opponent) {
                groups.push(Group { uid: 2, kind: GroupKind::Character, name: "opponent".into(), archetype: 0, clips: vec![anim(theirs, length(&npc_clip))] });
                bindings.insert(2, npc);
            }
            if let Some(old) = qt.playing.take() {
                cinema.stop_built(&old);
            }
            let name = format!("qte_{}_{}", qt.id, qt.plays);
            qt.plays += 1;
            cinema.play_built(Built {
                name: name.clone(),
                seq: Sequence { name: name.clone(), duration: d, looping: false, groups, ..Default::default() },
                origin,
                bindings,
            });
            if qt.state != Some(n) {
                info!("qte: {} plays {} ({:.1} s)", qt.player, next.name, d);
            }
            (qt.state, qt.t, qt.fired, qt.length, qt.origin, qt.playing) = (Some(n), 0.0, -1.0, d, origin, Some(name));
        }
    }
    if qt.ended || qt.age > MAX_AGE {
        if !qt.ended {
            warn!("qte: {} played {MAX_AGE} s without ending; it ends", qt.player);
        }
        if let Some(old) = qt.playing.take() {
            cinema.stop_built(&old);
        }
        let id = qt.id;
        info!("qte: {} ended", qt.player);
        qtes.active = None;
        host.story.message("ExoticQteGSFEnded");
        host.story.gameplay_ended(id);
        return;
    }

    // The prompt (and a meter for a mash or a hold) while a QTE asks; its
    // inputs are the QTE's.
    let ask = qt.asked.iter().flatten().next().cloned();
    let Some(a) = ask else {
        for e in &prompts {
            commands.entity(e).despawn();
        }
        shown.clear();
        return;
    };
    for &x in answers(a.input) {
        actions.consume(x);
    }
    let word = a.prompt.and_then(|p| host.story.data().strings.get(p).map(str::to_string)).unwrap_or_else(|| format!("{:?}", a.kind).to_uppercase());
    let text = format!("[{}]  {word}", actions.label(answers(a.input)[0], &settings));
    let meter = match a.kind {
        Kind::Mash => Some(a.progress / a.values[0].max(1.0)),
        Kind::Hold => Some(a.progress / a.values[0].max(0.01)),
        _ => None,
    };
    if *shown != text || prompts.is_empty() {
        for e in &prompts {
            commands.entity(e).despawn();
        }
        *shown = text.clone();
        let mut e = commands.spawn((
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
        ));
        e.with_child((Text::new(text), TextFont { font_size: bevy::text::FontSize::Px(30.0), ..default() }, TextColor(Color::srgb(1.0, 0.85, 0.35))));
        if meter.is_some() {
            e.with_child((
                Node { width: Val::Px(320.0), height: Val::Px(10.0), ..default() },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
                children![(
                    Node { width: Val::Percent(0.0), height: Val::Percent(100.0), ..default() },
                    BackgroundColor(Color::srgb(1.0, 0.85, 0.35)),
                    QteMeter
                )],
            ));
        }
    }
    for mut n in &mut meters {
        n.width = Val::Percent(meter.unwrap_or(0.0).clamp(0.0, 1.0) * 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qte_inputs_are_answered_by_the_controls() {
        // The crawl alternates the triggers (fire and aim on the keyboard);
        // the shimmy holds the stick left; the struggles mash a button.
        assert_eq!(answers(5), [Action::Fire]);
        assert_eq!(answers(6), [Action::Aim]);
        assert_eq!(answers(13), [Action::Left]);
        assert_eq!(answers(1)[0], Action::Interact);
    }
}
