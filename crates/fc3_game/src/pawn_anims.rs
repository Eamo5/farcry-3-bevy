//! Moves the scripts play on a character by name (`CAPI_AI.StartPlayAnim`,
//! the PlayerPlayAnim box). On the player, the move's first-person clip
//! plays as a scene from where he stands, the view in the clip's camera (a
//! fall from a blast: down on his back and up again). The scripts hear how
//! it goes as `Domino_PlayAnim` events on him: `Started`, `FinishingSoon`
//! and `Finished`, or `Interrupted` when stopped.

use bevy::prelude::*;
use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};
use fc3_formats::moves::MoveStates;
use fc3_story::{Arg, PLAYER};

use crate::cache_assets::{cache_path, Fc3Clip};
use crate::cinema::{Built, Cinema};
use crate::player::{Player, HALF_HEIGHT};
use crate::qte::MoveTable;
use crate::story::StoryHost;

pub struct PawnAnimsPlugin;

impl Plugin for PawnAnimsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PawnAnims>().add_systems(Update, run_pawn_anims.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A move the scripts asked a character to play, or to stop playing.
pub enum Request {
    /// (`quiet`: a gameplay's own move, of which the scripts hear nothing.)
    Play {
        pawn: u64,
        name: String,
        looping: bool,
        quiet: bool,
        /// A message broadcast when it has played out (a gameplay's own
        /// move the scripts wait on: the intoxication's cough).
        done: Option<&'static str>,
    },
    Stop {
        pawn: u64,
    },
}

#[derive(Resource, Default)]
pub struct PawnAnims {
    pub requests: Vec<Request>,
    active: Option<Playing>,
}

impl PawnAnims {
    pub fn reset(&mut self) {
        self.requests.clear();
        self.active = None;
    }
}

/// The player's move under way: its clip, and the scene playing it once
/// the clip has loaded.
struct Playing {
    clip: String,
    handle: Handle<Fc3Clip>,
    looping: bool,
    quiet: bool,
    done: Option<&'static str>,
    waited: f32,
    scene: Option<String>,
    finishing: bool,
}

/// `FinishingSoon` comes this long (s) before the end.
const FINISHING: f32 = 0.5;
/// The longest wait (s) for a clip to load.
const LOAD_WAIT: f32 = 4.0;
/// The scene's name.
const SCENE: &str = "pawn_anim";

/// A move's clip for the player: a first-person one (`1st...`), its
/// `Default` state's when that is one.
pub(crate) fn player_clip(states: &MoveStates) -> Option<&str> {
    // (Or an exotic one's, `ex_1st...`: the mushroom's cough.)
    let first_person = |c: &str| {
        c.rsplit('\\').next().is_some_and(|f| {
            let f = f.to_ascii_lowercase();
            f.starts_with("1st") || f.starts_with("ex_1st")
        })
    };
    states
        .iter()
        .find(|(s, c)| s.eq_ignore_ascii_case("default") && first_person(c))
        .or_else(|| states.iter().find(|(_, c)| first_person(c)))
        .map(|(_, c)| c.as_str())
}

/// A `Domino_PlayAnim` event on a character.
fn tell(host: &mut StoryHost, pawn: u64, what: &str) {
    host.story.entity_event(pawn, "Domino_PlayAnim", vec![Arg::id(pawn), Arg::Str(what.into())]);
}

#[allow(clippy::too_many_arguments)]
fn run_pawn_anims(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    mut anims: ResMut<PawnAnims>,
    table: Res<MoveTable>,
    mut cinema: ResMut<Cinema>,
    server: Res<AssetServer>,
    clips: Res<Assets<Fc3Clip>>,
    players: Query<(&Transform, &Player)>,
) {
    let Some(mut host) = host else { return };
    let anims = &mut *anims;
    for r in std::mem::take(&mut anims.requests) {
        match r {
            Request::Play { pawn, name, looping, quiet, done } => {
                let clip = (pawn == PLAYER).then(|| table.0.get(&name.to_ascii_lowercase())).flatten().and_then(player_clip);
                let Some(clip) = clip else {
                    // (A character's moves, and moves with no first-person
                    // clip, aren't played yet: over at once.)
                    info!("pawn anim: {name} on {pawn} not played");
                    if !quiet {
                        tell(&mut host, pawn, "Started");
                        tell(&mut host, pawn, "Finished");
                    }
                    if let Some(m) = done {
                        host.story.message(m);
                    }
                    continue;
                };
                // A new move cuts short the one playing (over, for one whose
                // end is told: the intoxication's cough, cut by its watching
                // the hands a moment later).
                if let Some(old) = anims.active.take() {
                    if let Some(s) = old.scene {
                        cinema.stop_built(&s);
                    }
                    if !old.quiet {
                        tell(&mut host, PLAYER, "Interrupted");
                    }
                    if let Some(m) = old.done {
                        host.story.message(m);
                    }
                }
                info!("pawn anim: {name}: {clip}");
                let handle = server.load(cache_path(&fc3_formats::cache::anim_rel(clip)));
                anims.active = Some(Playing { clip: clip.to_string(), handle, looping, quiet, done, waited: 0.0, scene: None, finishing: false });
            }
            Request::Stop { pawn } => {
                if pawn != PLAYER {
                    continue;
                }
                if let Some(old) = anims.active.take() {
                    if let Some(s) = old.scene {
                        cinema.stop_built(&s);
                    }
                    if !old.quiet {
                        tell(&mut host, PLAYER, "Interrupted");
                    }
                }
            }
        }
    }
    let quiet = anims.active.as_ref().is_some_and(|p| p.quiet);
    let tell = |host: &mut StoryHost, pawn: u64, what: &str| {
        if !quiet {
            tell(host, pawn, what);
        }
    };
    let Some(p) = anims.active.as_mut() else { return };
    match &p.scene {
        None => {
            p.waited += time.delta_secs();
            if !crate::cache_assets::settled(&server, p.handle.id()) && p.waited < LOAD_WAIT {
                return;
            }
            let length = clips.get(&p.handle).map_or(0.0, |c| c.0.duration);
            let Some((t, player)) = players.single().ok().filter(|_| length > 0.0) else {
                let done = p.done;
                anims.active = None;
                tell(&mut host, PLAYER, "Started");
                tell(&mut host, PLAYER, "Finished");
                if let Some(m) = done {
                    host.story.message(m);
                }
                return;
            };
            // From his feet, facing his way.
            let origin = Transform::from_translation(t.translation - Vec3::Y * HALF_HEIGHT).with_rotation(Quat::from_rotation_y(player.yaw));
            let name = SCENE.to_string();
            let anim = Clip {
                start: 0.0,
                duration: length,
                kind: ClipKind::Animation { source: p.clip.clone(), from: 0, to: (length * 30.0) as u32, blend: 0.15, anchored: false },
            };
            let groups = vec![Group { uid: 1, kind: GroupKind::Player, name: "player".into(), archetype: 0, clips: vec![anim] }];
            cinema.play_built(Built {
                name: name.clone(),
                seq: Sequence { name: name.clone(), duration: length, looping: p.looping, groups, ..Default::default() },
                origin,
                bindings: Default::default(),
            });
            p.scene = Some(name);
            tell(&mut host, PLAYER, "Started");
        }
        Some(name) => match cinema.progress(name) {
            Some((t, length)) => {
                if !p.looping && !p.finishing && length - t <= FINISHING {
                    p.finishing = true;
                    tell(&mut host, PLAYER, "FinishingSoon");
                }
            }
            None => {
                let (finishing, done) = (p.finishing, p.done);
                anims.active = None;
                if let Some(m) = done {
                    host.story.message(m);
                }
                if !finishing {
                    tell(&mut host, PLAYER, "FinishingSoon");
                }
                tell(&mut host, PLAYER, "Finished");
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_player_plays_a_moves_first_person_clip() {
        let states: MoveStates = vec![
            ("MultiPlayer".into(), r"a\mulge_fulb_fall_nowep.mab".into()),
            ("Back".into(), r"a\1stge_uppb_roll_nowep.mab".into()),
            ("Default".into(), r"a\1stge_uppb_fall_nowep.mab".into()),
        ];
        assert_eq!(player_clip(&states), Some(r"a\1stge_uppb_fall_nowep.mab"));
        // Without a first-person Default, the first first-person clip.
        let states: MoveStates = vec![("Default".into(), r"a\3rdge_fulb_fall.mab".into()), ("Front".into(), r"a\1STge_uppb_front.mab".into())];
        assert_eq!(player_clip(&states), Some(r"a\1STge_uppb_front.mab"));
        // None for a third-person move.
        assert_eq!(player_clip(&vec![("Default".into(), r"a\3rdge_fulb_fall.mab".into())]), None);
    }
}
