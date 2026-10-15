//! The poker minigame (`CPokerMiniGame`). The story's tables (JAS_170's
//! with Sam, HOY_040's with Hoyt, SQC_100's) run as their scripts lay them
//! out: once a script starts the game (a `StartPoker...` GSF event), the
//! scene it waits for next (`JAS220_B10C_01`...) is broadcast after a hand
//! has gone by, the next one once the script says the scene has played
//! (`CinemaSequenceFinished`), and last the game's own end of the player's
//! game (`JasonWinsGame`, `JasonLeavesPoker`). The hands themselves (the
//! cards, the bets, the table's movie) are not played yet, nor the bars'
//! tables, which no script starts.

use bevy::prelude::*;

use crate::story::StoryHost;

pub struct PokerPlugin;

impl Plugin for PokerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PokerTable>()
            .add_systems(Update, play_poker.after(crate::story::StorySet).before(crate::cinema::CinemaSet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The messages the game itself broadcasts when the player is done at the
/// table, in this order (not scripted scenes of its story).
const DONE: [&str; 2] = ["JasonWinsGame", "JasonLeavesPoker"];
/// How long a hand takes between a table's scenes (s).
const HAND_SECS: f32 = 6.0;
/// How long a scene's end is waited for before the game goes on without
/// it (s): a script that never says it has played.
const SCENE_WAIT: f32 = 180.0;
/// Hands played with no scene before the player's game is over with none
/// (a script may arm its first scene a while after it starts the game).
const HANDS_BEFORE_DONE: u32 = 3;

/// The poker game being played, if any.
#[derive(Resource, Default)]
pub struct PokerTable {
    game: Option<Game>,
}

struct Game {
    /// The specific gameplay's id and name (`CPokerMiniGame_JAS_220_...`).
    id: u64,
    name: String,
    started: bool,
    /// The scene broadcast whose end is waited for, and how long since.
    scene: Option<(String, f32)>,
    sent: Vec<String>,
    /// Time into the hand being played, and hands played with no scene.
    hand: f32,
    idle_hands: u32,
    done: bool,
}

/// What the table does next, given the messages the running missions'
/// scripts wait for and those already broadcast.
#[derive(Debug, PartialEq)]
enum Next {
    /// A scene of the table's story.
    Scene(String),
    /// The player's game is over: the game's own messages, those listened to.
    Done(Vec<&'static str>),
}

fn next(awaited: &[String], sent: &[String]) -> Next {
    match awaited.iter().find(|m| !DONE.contains(&m.as_str()) && !sent.contains(m)) {
        Some(scene) => Next::Scene(scene.clone()),
        None => Next::Done(DONE.into_iter().filter(|d| awaited.iter().any(|m| m == d)).collect()),
    }
}

fn play_poker(host: Option<NonSendMut<StoryHost>>, mut table: ResMut<PokerTable>, time: Res<Time>, cinema: Res<crate::cinema::Cinema>) {
    let Some(mut host) = host else { return };
    let events = std::mem::take(&mut host.gsf_events);
    // (The latest poker game: a bar's table, near, may have one of its own
    // waiting for the player.)
    let Some((id, name)) = host.story.gameplays().into_iter().rev().find(|g| g.1.starts_with("CPokerMiniGame")) else {
        table.game = None;
        return;
    };
    if table.game.as_ref().is_none_or(|g| g.id != id) {
        debug!("poker: {name} at the table");
        table.game = Some(Game { id, name, started: false, scene: None, sent: Vec::new(), hand: 0.0, idle_hands: 0, done: false });
    }
    let Some(game) = table.game.as_mut() else { return };
    for e in events {
        if e.starts_with("StartPoker") {
            info!("poker: {} starts ({e})", game.name);
            game.started = true;
            game.hand = 0.0;
        } else if e == "CinemaSequenceFinished" {
            if let Some((scene, _)) = game.scene.take() {
                info!("poker: scene {scene} played");
            }
            game.hand = 0.0;
        }
    }
    if !game.started || game.done || cinema.view.is_some() {
        return;
    }
    let dt = time.delta_secs();
    if let Some((scene, waited)) = game.scene.as_mut() {
        *waited += dt;
        if *waited < SCENE_WAIT {
            return;
        }
        warn!("poker: scene {scene} never said it had played; going on");
        game.scene = None;
    }
    game.hand += dt;
    if game.hand < HAND_SECS {
        return;
    }
    game.hand = 0.0;
    match next(&host.story.mission_messages(), &game.sent) {
        Next::Scene(scene) => {
            info!("poker: a hand played; scene {scene}");
            game.sent.push(scene.clone());
            game.scene = Some((scene.clone(), 0.0));
            host.story.message(&scene);
        }
        // (Not before anything listens for its end, nor with no scene yet
        // unless a few hands have gone by.)
        Next::Done(done) if done.is_empty() || (game.sent.is_empty() && game.idle_hands < HANDS_BEFORE_DONE) => game.idle_hands += 1,
        Next::Done(done) => {
            info!("poker: {}'s game over ({done:?})", game.name);
            game.done = true;
            for message in done {
                host.story.message(message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table's scenes come in turn, each once; then the game's own end,
    /// only the messages a script listens to, winning before leaving.
    #[test]
    fn a_tables_scenes_come_in_turn_then_its_end() {
        let s = |v: &[&str]| v.iter().map(|m| m.to_string()).collect::<Vec<_>>();
        assert_eq!(next(&s(&["JAS220_B10C_01", "JasonLeavesPoker"]), &[]), Next::Scene("JAS220_B10C_01".into()));
        assert_eq!(next(&s(&["JAS220_B10C_01", "JasonLeavesPoker"]), &s(&["JAS220_B10C_01"])), Next::Done(vec!["JasonLeavesPoker"]));
        assert_eq!(next(&s(&["JasonLeavesPoker", "JasonWinsGame"]), &[]), Next::Done(vec!["JasonWinsGame", "JasonLeavesPoker"]));
        assert_eq!(next(&[], &[]), Next::Done(Vec::new()));
    }
}
