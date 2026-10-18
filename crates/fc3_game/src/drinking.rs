//! The drinking game (`CDrinkingMiniGame`; BNG_010's Bangkok bar plays
//! `CDrinkingMiniGame_Bangkok`). The player, having used the bartender,
//! takes his place at the bar and waits; the scripts hear the game wants its
//! prompts (`DisplayDrinkingHUD`: they put up the drink and leave buttons).
//! Interact buys a drink (its price taken, if he has it; the bartender says
//! so if not) and he downs it with the bartender pouring; each drink makes
//! him drunker (`drinkIntoxicationAmount`). Drunk enough the view sways for
//! a while once he's left (`intoxication`, at least
//! `drunkEffectMinimumActivationTime`); drunker still he passes out and
//! wakes up at the game's wake-up spot. Escape (or the drinks there are, all
//! drunk) leaves the bar. Both play the game's own moves, from the scene's
//! anchor at the bar; the bartender has his lines.

use std::collections::HashMap;

use bevy::prelude::*;
use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};
use fc3_formats::gsf::{DrinkingMoves, DrinkingSettings};

use crate::cache_assets::{cache_path, Fc3Clip};
use crate::player::Player;

pub struct DrinkingPlugin;

impl Plugin for DrinkingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Drinking>().add_systems(Update, run.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The game under way, and how long the drink it left him with lasts.
#[derive(Resource, Default)]
pub struct Drinking {
    game: Option<Game>,
    drunk_left: f32,
}

impl Drinking {
    /// Whether the player is at the bar (Escape leaves it).
    pub fn claims_escape(&self) -> bool {
        self.game.as_ref().is_some_and(|g| matches!(g.stage, Stage::StartWait | Stage::Wait))
    }
}

/// A drinking game the scripts start.
pub fn drinking_gameplay(name: &str) -> bool {
    name.starts_with(fc3_formats::gsf::DRINKING)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Taking his place, then waiting before the first drink.
    Start,
    StartWait,
    Drink,
    /// Waiting between drinks.
    Wait,
    Leave,
    PassOut,
    WakeUp,
}

struct Game {
    id: u64,
    settings: DrinkingSettings,
    /// The scene's anchor at the bar, the bartender, and where he wakes up
    /// (Dunia, with its heading).
    anchor: Transform,
    bartender: u64,
    wake: Option<([f32; 3], f32)>,
    cost: u32,
    drinks: u32,
    level: f32,
    stage: Stage,
    /// The scene playing and when it ends (the game's clock; never for a
    /// loop).
    scene: Option<(String, f32)>,
    clock: f32,
    scenes: u32,
    /// The stage's scene has been asked for, and how long its clips have
    /// been loading.
    staged: bool,
    waited: f32,
    /// The time the screen has been going black (passing out).
    black: f32,
}

/// The drink effect's own id (the intoxication's start).
const DRUNK_ID: u64 = u64::MAX - 0xD41C;
/// Seconds of the screen going black and coming back, passing out.
const BLACK: f32 = 1.5;
/// The longest wait for a scene's clips (s): the stage moves on without it.
const LOAD_WAIT: f32 = 4.0;
/// Tests (`FC3_DRINK_TEST=<drinks>`): seconds of waiting before each choice.
const TEST_WAIT: f32 = 1.5;

/// The clip a move plays on a character (its `Default` state's, or its
/// first).
fn character_clip(states: &fc3_formats::moves::MoveStates) -> Option<&str> {
    states.iter().find(|(s, _)| s.eq_ignore_ascii_case("default")).or(states.first()).map(|(_, c)| c.as_str())
}

impl Game {
    /// The player's and the bartender's moves for a stage, and whether it
    /// loops.
    fn moves(&self, stage: Stage) -> (&str, &str, bool) {
        let (p, b): (&DrinkingMoves, &DrinkingMoves) = (&self.settings.player, &self.settings.bartender);
        match stage {
            Stage::Start => (&p.start, &b.start, false),
            Stage::StartWait => (&p.start_wait, &b.start_wait, true),
            Stage::Drink if self.drinks <= 1 => (&p.first_drink, &b.first_drink, false),
            Stage::Drink => (&p.drink, &b.drink, false),
            Stage::Wait => (&p.wait, &b.wait, true),
            Stage::Leave if self.drinks == 0 => (&p.start_exit, &b.start_exit, false),
            Stage::Leave => (&p.exit, &b.exit, false),
            Stage::PassOut => (&p.drunk_exit, &b.drunk_exit, false),
            Stage::WakeUp => (&p.wake_up, "", false),
        }
    }

    fn enter(&mut self, stage: Stage) {
        debug!("drinking ({}): {:?} -> {stage:?} ({} drinks, {:.0} drunk)", self.id, self.stage, self.drinks, self.level);
        self.stage = stage;
        self.staged = false;
    }

    /// Whether the stage's scene is over (or never came).
    fn scene_over(&self) -> bool {
        self.staged && self.scene.as_ref().is_none_or(|(_, until)| self.clock >= *until)
    }
}

/// One of a list of lines, by the clock.
fn pick(lines: &[u32], clock: f32) -> Option<u32> {
    (!lines.is_empty()).then(|| lines[(clock * 13.0) as usize % lines.len()])
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut drinking: ResMut<Drinking>,
    host: Option<NonSendMut<crate::story::StoryHost>>,
    info: Option<Res<crate::data::CacheInfo>>,
    (time, keys, actions): (Res<Time>, Res<ButtonInput<KeyCode>>, Res<crate::controls::ActionState>),
    (table, server, clips, mut cinema): (Res<crate::qte::MoveTable>, Res<AssetServer>, Res<Assets<Fc3Clip>>, ResMut<crate::cinema::Cinema>),
    (mut progress, mut prompts, mut drunk, mut scripted): (
        ResMut<crate::progression::Progress>,
        ResMut<crate::input_prompts::InputPrompts>,
        ResMut<crate::intoxication::Intoxication>,
        ResMut<crate::story::ScriptedPlayer>,
    ),
    mut players: Query<(&mut Transform, &mut Player)>,
    mut handles: Local<HashMap<String, Handle<Fc3Clip>>>,
    mut test: Local<(f32, bool, bool)>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (mut shot, mut commands): (Local<Option<(f32, String)>>, Commands),
) {
    let Some(mut host) = host else { return };
    let dt = time.delta_secs();
    let drinking = &mut *drinking;
    // The drink wears off.
    if drinking.drunk_left > 0.0 {
        drinking.drunk_left -= dt;
        if drinking.drunk_left <= 0.0 {
            info!("drinking: sober again");
            drunk.cancelled.push(DRUNK_ID);
        }
    }
    // FC3_DRINK_BLOCK=<block>[,-<block>...] (tests): that mission block made
    // active (and those with a `-` disabled), once, ten seconds in: BNG_010's
    // bar, B30, which its mission never starts.
    if let Ok(names) = std::env::var("FC3_DRINK_BLOCK") {
        test.0 += dt;
        if !test.2 && test.0 > 10.0 {
            test.2 = true;
            for name in names.split(',') {
                let (off, name) = name.strip_prefix('-').map_or((false, name), |n| (true, n));
                let Some((block, zone)) = host.story.block_named(name) else { continue };
                if let Some(zone) = zone.filter(|_| !off) {
                    host.story.hold_script(zone, true);
                }
                let state = if off { fc3_story::LayerState::Disabled } else { fc3_story::LayerState::Active };
                host.story.set_block_state(block, state);
                info!("drink test: block {name} {state:?}");
            }
        }
    }
    let running: Vec<(u64, String)> = host.story.gameplays().into_iter().filter(|(_, n)| drinking_gameplay(n)).collect();
    let test_drinks = std::env::var("FC3_DRINK_TEST").ok().and_then(|v| v.parse::<u32>().ok());
    // Stopped by the scripts (or a reload): gone.
    if let Some(g) = drinking.game.as_ref().filter(|g| !running.iter().any(|(id, _)| *id == g.id)) {
        info!("drinking ({}): stopped", g.id);
        if let Some((name, _)) = &g.scene {
            cinema.stop_built(name);
        }
        drinking.game = None;
        prompts.reset();
    }
    if drinking.game.is_none() {
        let Some((id, name)) = running.last().cloned() else {
            // FC3_DRINK_TEST (tests): the bartender near used, once (after
            // FC3_DRINK_BLOCK's blocks, with it).
            let blocks_done = test.2 || std::env::var_os("FC3_DRINK_BLOCK").is_none();
            if test_drinks.is_some() && !test.1 && blocks_done {
                let data = host.story.data();
                let here = players.single().map(|(t, _)| crate::data::to_dunia(t.translation)).unwrap_or_default();
                // (A bartender the scripts listen to be used: BNG_010 B30's
                // trigger at the bar.)
                let bartender = host.story.usable_pawns().into_iter().find(|(id, at, listened)| {
                    *listened
                        && data.entities.get(id).is_some_and(|e| e.name.contains("Bartender"))
                        && at.is_some_and(|a| (a[0] - here[0]).hypot(a[1] - here[1]) < 150.0)
                });
                if let Some((id, ..)) = bartender {
                    info!("drink test: bartender {id} used");
                    host.story.entity_event(id, "Use", vec![fc3_story::Arg::id(fc3_story::PLAYER)]);
                    test.1 = true;
                }
            }
            return;
        };
        let params: HashMap<String, String> = host.gameplay_params.get(&id).cloned().unwrap_or_default().into_iter().collect();
        let get = |k: &str| params.get(k).and_then(|v| v.parse::<u64>().ok()).filter(|&v| v != u64::MAX);
        let bytes = info.as_ref().and_then(|i| std::fs::read(i.root.join(fc3_formats::gsf::CACHE_REL)).ok()).unwrap_or_default();
        let data = host.story.data();
        let anchor = get("AnchorScene").and_then(|a| data.entities.get(&a)).map(|e| Transform {
            translation: crate::data::to_bevy(e.pos),
            rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)),
            scale: Vec3::ONE,
        });
        let (settings, anchor, bartender) = match (DrinkingSettings::parse(&bytes, &name), anchor, get("Bartender/EntityID")) {
            (Ok(s), Some(a), Some(b)) => (s, a, b),
            (s, a, b) => {
                warn!("drinking ({id}): {name} not played ({:?}, anchor {}, bartender {b:?})", s.err(), a.is_some());
                host.story.gameplay_ended(id);
                return;
            }
        };
        let wake = get("WakeUpLocation").and_then(|w| data.entities.get(&w)).map(|e| (e.pos, e.angles[2]));
        let cost = params.get("Drink_Cost").and_then(|v| v.parse().ok()).unwrap_or(settings.cost);
        info!("drinking ({id}): {name} at {:.0}, bartender {bartender}, {cost} a drink, {:?} of them", anchor.translation, settings.limit);
        let at = data.position(bartender).map(crate::data::to_bevy);
        if let Some(line) = pick(&settings.lines.salutation, time.elapsed_secs()) {
            sfx.write(crate::sound::PlaySfx { event: line, at: at.map(|a| a + Vec3::Y * 1.6), volume: 1.0 });
        }
        test.0 = 0.0;
        test.1 = true;
        drinking.game = Some(Game {
            id,
            settings,
            anchor,
            bartender,
            wake,
            cost,
            drinks: 0,
            level: 0.0,
            stage: Stage::Start,
            scene: None,
            clock: 0.0,
            scenes: 0,
            staged: false,
            waited: 0.0,
            black: 0.0,
        });
    }
    let Some(g) = drinking.game.as_mut() else { return };
    g.clock += dt;
    let bartender_at = host.story.data().position(g.bartender).map(|p| crate::data::to_bevy(p) + Vec3::Y * 1.6);
    let say = |sfx: &mut MessageWriter<crate::sound::PlaySfx>, lines: &[u32], clock: f32| {
        if let Some(line) = pick(lines, clock) {
            sfx.write(crate::sound::PlaySfx { event: line, at: bartender_at, volume: 1.0 });
        }
    };
    // The stage's scene: the player's and the bartender's moves from the
    // anchor (waking up, from where he wakes).
    if !g.staged {
        let (pm, bm, looping) = g.moves(g.stage);
        let path = |m: &str, player: bool| -> Option<String> {
            let states = table.0.get(&m.to_ascii_lowercase())?;
            if player { crate::pawn_anims::player_clip(states) } else { character_clip(states) }.map(str::to_string)
        };
        let (pp, bp) = (path(pm, true), path(bm, false));
        let mut load = |p: &Option<String>| {
            p.as_ref().map(|p| handles.entry(p.clone()).or_insert_with(|| server.load(cache_path(&fc3_formats::cache::anim_rel(p)))).clone())
        };
        let (ph, bh) = (load(&pp), load(&bp));
        let ready = |h: &Option<Handle<Fc3Clip>>| h.as_ref().is_none_or(|h| crate::cache_assets::settled(&server, h.id()));
        if !(ready(&ph) && ready(&bh)) && g.waited < LOAD_WAIT {
            g.waited += dt;
            return;
        }
        g.waited = 0.0;
        let len = |h: &Option<Handle<Fc3Clip>>| h.as_ref().and_then(|h| clips.get(h)).map_or(0.0, |c| c.0.duration);
        let (pl, bl) = (len(&ph), len(&bh));
        let length = pl.max(bl);
        if let Some((old, _)) = g.scene.take() {
            cinema.stop_built(&old);
        }
        if length > 0.0 {
            let clip = |source: String, length: f32| Clip {
                start: 0.0,
                duration: length,
                kind: ClipKind::Animation { source, from: 0, to: (length * 30.0) as u32, blend: 0.2, anchored: false },
            };
            let mut groups = Vec::new();
            if let Some(p) = pp.filter(|_| pl > 0.0) {
                groups.push(Group { uid: 1, kind: GroupKind::Player, name: "player".into(), archetype: 0, clips: vec![clip(p, pl)] });
            }
            if let Some(b) = bp.filter(|_| bl > 0.0 && g.stage != Stage::WakeUp) {
                groups.push(Group { uid: 2, kind: GroupKind::Character, name: "bartender".into(), archetype: 0, clips: vec![clip(b, bl)] });
            }
            let origin = match (g.stage, g.wake) {
                (Stage::WakeUp, Some((pos, yaw))) => {
                    Transform::from_translation(crate::data::to_bevy(pos)).with_rotation(Quat::from_rotation_y(yaw.to_radians()))
                }
                _ => g.anchor,
            };
            let name = format!("drinking_{}", g.scenes);
            g.scenes += 1;
            cinema.play_built(crate::cinema::Built {
                name: name.clone(),
                seq: Sequence { name: name.clone(), duration: length, looping, groups, ..Default::default() },
                origin,
                bindings: HashMap::from([(2, g.bartender)]),
            });
            g.scene = Some((name.clone(), if looping { f32::INFINITY } else { g.clock + length }));
            // FC3_DRINK_SHOTS=<dir> (tests): each scene a moment in.
            if let Ok(dir) = std::env::var("FC3_DRINK_SHOTS") {
                *shot = Some((g.clock + length.min(1.5) * 0.6, format!("{dir}/{name}_{:?}.png", g.stage)));
            }
        }
        g.staged = true;
        // Waiting: the scripts put up the prompts.
        if matches!(g.stage, Stage::StartWait | Stage::Wait) {
            host.story.message("DisplayDrinkingHUD");
            test.0 = 0.0;
        }
    }
    if shot.as_ref().is_some_and(|(at, _)| g.clock >= *at) {
        let (_, path) = shot.take().unwrap();
        crate::camera::capture(&mut commands, path);
    }
    match g.stage {
        Stage::Start if g.scene_over() => g.enter(Stage::StartWait),
        Stage::StartWait | Stage::Wait => {
            test.0 += dt;
            let (drink, leave) = match test_drinks {
                Some(n) if test.0 > TEST_WAIT => (g.drinks < n, g.drinks >= n),
                _ => (actions.just_pressed(crate::controls::Action::Interact), keys.just_pressed(KeyCode::Escape)),
            };
            if leave {
                prompts.reset();
                say(&mut sfx, &g.settings.lines.leaving, g.clock);
                g.enter(Stage::Leave);
            } else if drink && progress.money < g.cost {
                say(&mut sfx, &g.settings.lines.no_money, g.clock);
            } else if drink {
                progress.money -= g.cost;
                g.drinks += 1;
                g.level += g.settings.per_drink;
                prompts.reset();
                info!("drinking ({}): drink {} (${}), {:.0} drunk", g.id, g.drinks, g.cost, g.level);
                g.enter(Stage::Drink);
            }
        }
        Stage::Drink if g.scene_over() => {
            if g.level >= g.settings.passing_out {
                say(&mut sfx, &g.settings.lines.passing_out, g.clock);
                g.enter(Stage::PassOut);
            } else {
                // Drunk now: the bartender says so.
                if g.level >= g.settings.drunk && g.level - g.settings.per_drink < g.settings.drunk {
                    say(&mut sfx, &g.settings.lines.drunk, g.clock);
                }
                if g.settings.limit.is_some_and(|l| g.drinks >= l) {
                    say(&mut sfx, &g.settings.lines.empty_glass, g.clock);
                    g.enter(Stage::Leave);
                } else {
                    g.enter(Stage::Wait);
                }
            }
        }
        Stage::PassOut if g.scene_over() => {
            // Black, then up again where he wakes.
            g.black += dt;
            scripted.fade = (scripted.fade.0, 1.0, BLACK);
            if g.black >= BLACK {
                if let (Some((pos, yaw)), Ok((mut t, mut p))) = (g.wake, players.single_mut()) {
                    crate::story::teleport(&mut t, &mut p, pos, yaw);
                }
                scripted.fade = (1.0, 0.0, BLACK);
                g.black = 0.0;
                g.enter(Stage::WakeUp);
            }
        }
        Stage::Leave | Stage::WakeUp if g.scene_over() => {
            let g = drinking.game.take().unwrap();
            if let Some((name, _)) = &g.scene {
                cinema.stop_built(name);
            }
            prompts.reset();
            // Drunk, it lasts a while: the longer, the drunker.
            if g.level >= g.settings.drunk {
                let lasts = g.settings.drunk_time * g.level / g.settings.drunk.max(1.0);
                info!("drinking ({}): left drunk ({:.0}), for {lasts:.0} s", g.id, g.level);
                drunk.pending.push(crate::intoxication::Start { id: DRUNK_ID, params: HashMap::new() });
                drinking.drunk_left = lasts;
            } else {
                info!("drinking ({}): left after {} drinks", g.id, g.drinks);
            }
            host.story.gameplay_ended(g.id);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(limit: Option<u32>) -> Game {
        let m = |s: &str| DrinkingMoves {
            start: format!("{s}start"),
            start_wait: format!("{s}startwait"),
            first_drink: format!("{s}first"),
            drink: format!("{s}drink"),
            wait: format!("{s}wait"),
            start_exit: format!("{s}startexit"),
            exit: format!("{s}exit"),
            drunk_exit: format!("{s}drunkexit"),
            wake_up: format!("{s}wake"),
        };
        Game {
            id: 1,
            settings: DrinkingSettings {
                cost: 5,
                limit,
                per_drink: 30.0,
                drunk: 90.0,
                passing_out: 180.0,
                drunk_time: 10.0,
                player: m("p"),
                bartender: m("b"),
                ..Default::default()
            },
            anchor: Transform::IDENTITY,
            bartender: 2,
            wake: None,
            cost: 5,
            drinks: 0,
            level: 0.0,
            stage: Stage::Start,
            scene: None,
            clock: 0.0,
            scenes: 0,
            staged: false,
            waited: 0.0,
            black: 0.0,
        }
    }

    /// Each stage plays its moves: the first drink its own, leaving before a
    /// drink its own, and the waits loop.
    #[test]
    fn each_stage_plays_its_moves() {
        let mut g = game(Some(3));
        assert_eq!(g.moves(Stage::Start), ("pstart", "bstart", false));
        assert_eq!(g.moves(Stage::StartWait), ("pstartwait", "bstartwait", true));
        assert_eq!(g.moves(Stage::Leave), ("pstartexit", "bstartexit", false));
        g.drinks = 1;
        assert_eq!(g.moves(Stage::Drink), ("pfirst", "bfirst", false));
        assert_eq!(g.moves(Stage::Leave), ("pexit", "bexit", false));
        g.drinks = 2;
        assert_eq!(g.moves(Stage::Drink), ("pdrink", "bdrink", false));
        assert_eq!(g.moves(Stage::Wait), ("pwait", "bwait", true));
        assert_eq!(g.moves(Stage::WakeUp), ("pwake", "", false));
    }

    /// A stage's scene is over once asked for and past its end; a loop never
    /// is.
    #[test]
    fn a_scene_ends_at_its_length() {
        let mut g = game(None);
        assert!(!g.scene_over());
        g.staged = true;
        assert!(g.scene_over());
        g.scene = Some(("s".into(), 2.0));
        g.clock = 1.0;
        assert!(!g.scene_over());
        g.clock = 2.5;
        assert!(g.scene_over());
        g.scene = Some(("s".into(), f32::INFINITY));
        assert!(!g.scene_over());
        assert_eq!(pick(&[7, 8, 9], 0.0), Some(7));
        assert_eq!(pick(&[], 1.0), None);
        assert!(drinking_gameplay("CDrinkingMiniGame_Bangkok") && !drinking_gameplay("CKnivesMiniGame"));
    }
}
