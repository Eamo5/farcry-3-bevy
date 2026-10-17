//! Knife throwing (`CKnivesMiniGame`, `gsf::KnivesSettings`): each of the
//! five sites' scripts starts the base game when its layer comes in (its
//! country's outpost taken), naming its challenger, its wheel and its
//! anchor (the gameplay's parameters: `CounterGuy/EntityID`,
//! `TurningWheel/EntityID`, `TransitionAnchor`). The game is the engine's:
//! near the challenger the player takes them on, chooses a wager (the row's
//! difficulty is how well the challenger throws), and each throws five
//! knives at the spinning wheel (a turn every 24 s, the bullseye's five rings
//! on it, worth 100 at the middle down to 5 at the edge), the challenger
//! first. The player aims from the throwing spot with the view swaying in a
//! figure of eight, holds the throw to charge it (the power climbs the
//! game's curve: next to nothing below half, full past four fifths) and
//! lets go: the knife flies at the power's share of the top speed, falling
//! as it goes, and sticks where it meets the wheel, turning with it. The
//! better score takes the wager back doubled; a draw gives it back. The
//! challenger calls the player over, cheers and curses the throws and wins
//! or loses in their own lines. Places are the anchor's (the moves are made
//! about it): the throwing spot, where the player watches, where the
//! challenger talks.

use std::collections::HashMap;

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::gsf::KnivesSettings;

use crate::cache_assets::{Fc3Clip, Fc3Model};
use crate::controls::{Action, ActionState};
use crate::holdem::Dealer;
use crate::player::{Player, ViewArc, ViewLimits, EYE_HEIGHT, HALF_HEIGHT};
use crate::story::StoryHost;

pub struct KnivesPlugin;

impl Plugin for KnivesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<KnifeGame>()
            .add_systems(PreUpdate, hold_controls.after(crate::controls::apply_action_maps))
            .add_systems(Update, play_knives.after(crate::story::StorySet).before(crate::cinema::CinemaSet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, (dress_wheel, fly_knives).chain().after(play_knives))
            .add_systems(Update, hold_place.after(crate::player::player_move));
    }
}

/// Where the moves are (anchor space, Dunia: x right, y toward the wheel,
/// z up): the throwing spot, where the player watches the challenger, where
/// the challenger talks.
const THROW_SPOT: [f32; 3] = [-3.891, -4.636, -0.128];
const WATCH_SPOT: [f32; 3] = [-4.510, -7.224, -0.128];
const TALK_SPOT: [f32; 3] = [-3.840, -4.587, -0.121];
/// The wheel's turn (radians a second: 360 degrees in 24 s), its radius,
/// and its painted face (in front of its centre, m).
const SPIN: f32 = std::f32::consts::TAU / 24.0;
const WHEEL_RADIUS: f32 = 1.04;
const FACE: f32 = 0.08;
/// The bullseye's rings' outer radii (m), the largest first (the targets'
/// order, `KnivesSettings::rewards`).
const RINGS: [f32; 5] = [0.333, 0.267, 0.200, 0.133, 0.067];
/// The power bar: a fill up and down this many times a second.
const FILL_RATE: f32 = 0.8;
/// The view's sway at its multipliers' full (degrees).
const SWAY_DEGREES: f32 = 10.0;
/// How near the challenger the player takes them on, and calls them over.
const TALK_REACH: f32 = 2.5;
const CALL_REACH: f32 = 12.0;
const CALL_EVERY: f32 = 25.0;
/// How long a challenger's throw takes (its move), and when its knife
/// leaves the hand; a result's showing.
const NPC_THROW: f32 = 4.267;
const NPC_RELEASE: f32 = 1.533;
const RESULT: f32 = 6.0;
/// Gravity (m/s²).
const GRAVITY: f32 = 9.81;
/// The knife thrown and stuck, and the bullseye's rings.
const KNIFE_MODEL: &str = r"graphics\__fc3_graphics\_common\weapons\npcknife\npc_knife.xbg";
const RING_MODELS: [&str; 5] = [
    r"graphics\__fc3_graphics\singleplayer\prototypes\knifewheel_target_xlarge_01.xbg",
    r"graphics\__fc3_graphics\singleplayer\prototypes\knifewheel_target_large_01.xbg",
    r"graphics\__fc3_graphics\singleplayer\prototypes\knifewheel_target_medium_01.xbg",
    r"graphics\__fc3_graphics\singleplayer\prototypes\knifewheel_target_small_01.xbg",
    r"graphics\__fc3_graphics\singleplayer\prototypes\knifewheel_target_xsmall_01.xbg",
];
/// The challenger's moves.
const MOVES: &str = r"graphics\characters\_common\animations\exoticgameplay\minigames\knifethrowing\";
const TALK_MOVE: &str = "ex_knifethrow_gm_talkilde.mab";
const THROW_MOVE: &str = "ex_knife_throw_npc_throw_knife.mab";
const WATCH_MOVE: &str = "ex_knife_throw_npc_spectate_idle.mab";
/// The challenger's throw's whoosh (the player's comes with their arms'
/// throw).
const NPC_THROW_SOUND: u32 = 0x0015_911F;
/// The player's arms' throw (`fp`'s knife context): how long it takes, and
/// when the knife leaves the hand; the wait after each throw.
const THROW_ARMS: f32 = 0.4;
const PLAYER_RELEASE: f32 = 0.367;
const BETWEEN_THROWS: f32 = 1.5;

/// The game the player is at, if any, and what its HUD shows.
#[derive(Resource, Default)]
pub struct KnifeGame {
    game: Option<Game>,
    pub hud: Option<KnifeHud>,
    /// The mouse over the boxes' buttons (confirm, cancel) and a wager row.
    pub hover: (bool, bool, Option<usize>),
    /// The throw held (read before the weapons have it suppressed).
    held: bool,
}

impl KnifeGame {
    /// The player is at the game (its view and HUD are the game's).
    pub fn holds_player(&self) -> bool {
        self.game.as_ref().is_some_and(|g| g.stage != Stage::Away)
    }

    /// What the player's arms do, on their turn.
    pub fn arms(&self) -> Option<KnifeArms> {
        let g = self.game.as_ref()?;
        let Stage::Thrower(thrown, charge, wait) = g.stage else { return None };
        Some(match charge {
            Some(_) => KnifeArms::Charging,
            None if thrown > 0 && BETWEEN_THROWS - wait < THROW_ARMS => KnifeArms::Throwing,
            None if thrown >= g.settings.knives => {
                if wait > 0.1 {
                    KnifeArms::Away
                } else {
                    return None;
                }
            }
            None => KnifeArms::Ready,
        })
    }
}

/// The player's arms on their turn (`fp`'s knife context, the game's own
/// first-person moves with the knife in hand): drawn and held ready, the
/// throw held, thrown, put away after the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnifeArms {
    Ready,
    Charging,
    Throwing,
    Away,
}

/// The HUD's state.
#[derive(Clone, Debug, PartialEq)]
pub struct KnifeHud {
    pub panel: KnifePanel,
    /// The player's score and the challenger's.
    pub scores: [u32; 2],
    /// Knives a side, and those thrown by whoever is throwing (the player's
    /// turn or not).
    pub knives: u32,
    pub thrown: u32,
    pub player_turn: bool,
    /// The power bar's fill while the throw is held.
    pub power: Option<f32>,
    /// Throws landed this game (each pops its points up) and the last's
    /// points.
    pub landed: u32,
    pub points: u32,
    /// The throw's key, and whether the challenger's throws can be skipped
    /// (with Space).
    pub throw_key: String,
    pub skip: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KnifePanel {
    /// The wager box: the row chosen, each row's wager, whether the row
    /// costs more than the player has, and the subtitle (a string id).
    Betting {
        row: usize,
        wagers: [u32; 4],
        short: bool,
        subtitle: u32,
    },
    Playing,
    /// Escape mid-game: leave (and lose the wager)?
    Leaving,
    Won(u32),
    Lost(u32),
    Draw,
}

/// A site's places (Bevy space).
#[derive(Clone, Copy, Debug)]
struct Site {
    gameplay: u64,
    challenger: u64,
    wheel: u64,
    anchor: Transform,
}

impl Site {
    /// A place in the anchor's space (Dunia) in the world.
    fn at(&self, local: [f32; 3]) -> Vec3 {
        self.anchor.transform_point(Vec3::new(local[0], local[2], -local[1]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Away,
    Betting,
    /// The challenger's turn: knives thrown, and how far into this throw.
    Challenger(u32, f32),
    /// The player's turn: knives thrown, and how long the throw has been
    /// held (none between throws, for this long).
    Thrower(u32, Option<f32>, f32),
    /// The result, showing this long.
    Result(f32),
}

struct Game {
    site: Site,
    settings: KnivesSettings,
    stage: Stage,
    row: usize,
    wager: u32,
    scores: [u32; 2],
    dealer: Dealer,
    /// The wheel: its turn so far, its place unturned (found once loaded),
    /// whether its rings are on.
    spin: f32,
    wheel: Option<(Entity, Transform)>,
    rings: bool,
    /// Throws in the air: whose, from, the aim's point on the wheel, when
    /// it lands (game seconds) and the points it makes there.
    throws: Vec<Throw>,
    /// Throws landed this game and the last's points.
    landed: u32,
    points: u32,
    /// The leave box is up (the game waits), since when.
    leaving: Option<f32>,
    /// The challenger's move playing (its scene's name and the move).
    pose: Option<(String, &'static str)>,
    poses: u32,
    /// Lines: until when the challenger talks, and when they last called.
    quiet_until: f32,
    called: f32,
    clock: f32,
    /// The view the player was put at (the stage's place and what it faces).
    view: u8,
    /// A test's captures.
    shot: (String, f32, Vec<String>),
    left: bool,
}

#[derive(Clone, Debug)]
struct Throw {
    player: bool,
    from: Vec3,
    to: Vec3,
    /// When it leaves the hand and lands (game seconds).
    leaves: f32,
    lands: f32,
    points: u32,
    on_wheel: bool,
    spawned: bool,
}

/// The knife game's gameplay.
pub fn knives_gameplay(name: &str) -> bool {
    name == fc3_formats::gsf::KNIVES
}

/// The power a held throw's fill gives (`Curves:Exotic/KnifeShotPower`, a
/// Bézier from (0, 0) through (0.689, 0.496) to (1, 1): next to nothing
/// below half, full past four fifths), sampled.
pub fn shot_power(fill: f32) -> f32 {
    const CURVE: [(f32, f32); 11] =
        [(0.0, 0.0), (0.3, 0.016), (0.5, 0.094), (0.6, 0.22), (0.65, 0.34), (0.7, 0.55), (0.75, 0.88), (0.8, 0.97), (0.85, 0.99), (0.9, 1.0), (1.0, 1.0)];
    let x = fill.clamp(0.0, 1.0);
    let i = CURVE.iter().position(|(cx, _)| *cx >= x).unwrap_or(CURVE.len() - 1).max(1);
    let ((x0, y0), (x1, y1)) = (CURVE[i - 1], CURVE[i]);
    y0 + (y1 - y0) * (x - x0) / (x1 - x0).max(1e-6)
}

/// The power bar's fill a throw held this long has: up and back down
/// `FILL_RATE` times a second.
fn fill_of(held: f32) -> f32 {
    let u = (held * FILL_RATE * 2.0) % 2.0;
    if u <= 1.0 {
        u
    } else {
        2.0 - u
    }
}

/// A point on the wheel's face's points (by its distance from the centre):
/// the smallest ring it's in, 0 on the wheel outside them.
fn points(rewards: &[u32], r: f32) -> u32 {
    RINGS.iter().zip(rewards).filter(|(ring, _)| r <= **ring).map(|(_, p)| *p).last().unwrap_or(0)
}

/// Where a knife thrown from `eye` along `dir` at `speed` meets the wheel's
/// face (its centre and normal toward the thrower), falling as it flies:
/// the point, its distance from the centre, and the flight's time. None
/// when it never gets there.
fn flight(eye: Vec3, dir: Vec3, speed: f32, gravity: f32, centre: Vec3, normal: Vec3) -> Option<(Vec3, f32, f32)> {
    let denom = dir.dot(-normal);
    if speed < 1.0 || denom <= 0.05 {
        return None;
    }
    let along = (eye - centre).dot(normal) / denom;
    let time = along / speed;
    if !(0.0..3.0).contains(&time) {
        return None;
    }
    let hit = eye + dir * along - Vec3::Y * 0.5 * GRAVITY * gravity * time * time;
    let off = hit - centre;
    let r = (off - normal * off.dot(normal)).length();
    Some((hit, r, time))
}

#[allow(clippy::too_many_arguments)]
fn play_knives(
    host: Option<NonSendMut<StoryHost>>,
    mut knives: ResMut<KnifeGame>,
    time: Res<Time>,
    info: Option<Res<crate::CacheInfo>>,
    (keys, mouse, actions, settings): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<ActionState>, Res<crate::settings::Settings>),
    (mut progress, mut hud, mut limits, mut hold): (
        ResMut<crate::progression::Progress>,
        ResMut<crate::hud::HudState>,
        ResMut<ViewLimits>,
        ResMut<crate::hud::Minigame>,
    ),
    (mut cinema, server, clips): (ResMut<crate::cinema::Cinema>, Res<AssetServer>, Res<Assets<Fc3Clip>>),
    mut clip_handles: Local<HashMap<String, Handle<Fc3Clip>>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut players: Query<(&mut Transform, &mut Player)>,
    mut commands: Commands,
) {
    let Some(host) = host else { return };
    let dt = time.delta_secs();
    let knives = &mut *knives;
    let test = std::env::var("FC3_KNIFE_TEST").ok().and_then(|s| s.parse::<usize>().ok());
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    let player_at = t.translation;
    // The nearest game the scripts run (each site's own), its places from
    // its parameters.
    let data = host.story.data();
    let anchor_of = |id: u64| {
        data.entities.get(&id).map(|e| Transform {
            translation: crate::data::to_bevy(e.pos),
            rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)),
            scale: Vec3::ONE,
        })
    };
    let sites: Vec<Site> = host
        .story
        .gameplays()
        .into_iter()
        .filter(|(_, name)| knives_gameplay(name))
        .filter_map(|(id, _)| {
            let params = host.gameplay_params.get(&id)?;
            let get = |k: &str| params.iter().find(|(key, _)| key == k).and_then(|(_, v)| v.parse::<u64>().ok());
            Some(Site {
                gameplay: id,
                challenger: get("CounterGuy/EntityID")?,
                wheel: get("TurningWheel/EntityID")?,
                anchor: anchor_of(get("TransitionAnchor")?)?,
            })
        })
        .collect();
    let nearest = sites.iter().min_by(|a, b| a.anchor.translation.distance(player_at).total_cmp(&b.anchor.translation.distance(player_at))).copied();
    if knives.game.as_ref().is_some_and(|g| nearest.is_none_or(|n| n.gameplay != g.site.gameplay) && g.stage == Stage::Away) {
        knives.game = None;
    }
    if knives.game.is_none() {
        let Some(site) = nearest.filter(|s| s.anchor.translation.distance(player_at) < 120.0) else {
            knives.hud = None;
            hold.knives = false;
            return;
        };
        let bytes = info.as_ref().and_then(|i| std::fs::read(i.root.join(fc3_formats::gsf::CACHE_REL)).ok()).unwrap_or_default();
        let settings = KnivesSettings::parse(&bytes).map_err(|e| warn!("knives: {e:#}")).unwrap_or_default();
        info!("knives: the game at {:.0?} (challenger {}, wheel {})", site.anchor.translation, site.challenger, site.wheel);
        knives.game = Some(Game {
            site,
            settings,
            stage: Stage::Away,
            row: 1,
            wager: 0,
            scores: [0; 2],
            dealer: Dealer::new(time.elapsed().as_nanos() as u64 ^ site.gameplay),
            spin: 0.0,
            wheel: None,
            rings: false,
            throws: Vec::new(),
            landed: 0,
            points: 0,
            leaving: None,
            pose: None,
            poses: 0,
            quiet_until: 0.0,
            called: 0.0,
            clock: 0.0,
            view: 0,
            shot: Default::default(),
            left: false,
        });
    }
    let Some(game) = knives.game.as_mut() else { return };
    game.clock += dt;
    game.spin = (game.spin + SPIN * dt) % std::f32::consts::TAU;
    let now = game.clock;
    let site = game.site;
    let talk_at = site.at(TALK_SPOT) + Vec3::Y * 1.6;
    let say = |game: &mut Game, kind: &str, sfx: &mut MessageWriter<crate::sound::PlaySfx>| {
        if game.clock < game.quiet_until {
            return;
        }
        let pick = game.dealer.below(8);
        if let Some(line) = game.settings.line(kind, pick) {
            sfx.write(crate::sound::PlaySfx { event: line, at: Some(talk_at), volume: 1.0 });
            game.quiet_until = game.clock + 3.0;
        }
    };
    let (confirm_hover, cancel_hover, row_hover) = knives.hover;
    let click = mouse.just_pressed(MouseButton::Left);
    let confirm = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || (click && confirm_hover);
    let cancel = keys.just_pressed(KeyCode::Escape) || (click && cancel_hover);
    let near = site.at(TALK_SPOT).with_y(0.0).distance(player_at.with_y(0.0));
    let throw_at = site.at(THROW_SPOT);
    let wheel_centre = game.wheel.map_or(site.at([-3.875, 3.125, 1.5]), |(_, w)| w.translation);
    // (Tests: FC3_KNIFE_SKIP skips the challenger's throws, FC3_KNIFE_LEAVE
    // asks to leave before the player's third throw and says yes.)
    let test_skip = test.is_some() && std::env::var_os("FC3_KNIFE_SKIP").is_some();
    let test_leave = test.is_some() && std::env::var_os("FC3_KNIFE_LEAVE").is_some();
    // Escape mid-game asks first (the leave box; leaving loses the wager),
    // the game waiting meanwhile.
    let mut paused = game.leaving.is_some();
    if let Some(since) = game.leaving {
        let yes = confirm || (test_leave && now - since > 1.5);
        if yes {
            info!("knives: the player leaves");
            game.stage = Stage::Away;
            game.left = true;
            game.view = 0;
            limits.0 = None;
        }
        if yes || cancel {
            game.leaving = None;
        }
    } else if (matches!(game.stage, Stage::Challenger(..) | Stage::Thrower(..)) && cancel && test.is_none())
        || (test_leave && matches!(game.stage, Stage::Thrower(2, None, _)))
    {
        game.leaving = Some(now);
        paused = true;
    }
    match game.stage {
        _ if paused => {}
        Stage::Away => {
            // Calling the player over; taking them on.
            if near < CALL_REACH && now - game.called > CALL_EVERY {
                game.called = now;
                say(game, "ComePlayWithMe", &mut sfx);
            }
            let take = if test.is_some() { !game.left } else { near < TALK_REACH };
            if take && test.is_none() && hud.prompt.is_none() {
                hud.prompt = Some(format!("{}  Knife throwing", actions.label(Action::Interact, &settings)));
            }
            if take && (actions.just_pressed(Action::Interact) || test.is_some()) {
                info!("knives: the player takes the challenger on");
                game.stage = Stage::Betting;
                game.row = 1;
                game.clock = 0.0;
                game.quiet_until = 0.0;
                say(game, "GameOpening", &mut sfx);
            }
        }
        Stage::Betting => {
            if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
                game.row = (game.row + 3) % 4;
            }
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
                game.row = (game.row + 1) % 4;
            }
            if let Some(r) = row_hover {
                game.row = r.min(3);
            }
            if let Some(r) = test {
                game.row = r.min(3);
            }
            let wager = game.settings.wagers[game.row];
            let short = progress.money < wager;
            if (confirm || (click && row_hover.is_some()) || (test.is_some() && now > 1.5)) && !short {
                if wager > 0 {
                    progress.spend(wager);
                }
                game.wager = wager;
                game.scores = [0; 2];
                game.landed = 0;
                game.stage = Stage::Challenger(0, 0.0);
                info!("knives: row {} (wager {wager}, difficulty {})", game.row, game.settings.difficulty[game.row]);
            } else if cancel {
                game.stage = Stage::Away;
                game.left = true;
                game.view = 0;
                limits.0 = None;
            }
        }
        // Space skips the challenger's throws: the rest thrown at once.
        Stage::Challenger(thrown, into) if keys.just_pressed(KeyCode::Space) || test_skip => {
            let first = if into >= NPC_RELEASE { thrown + 1 } else { thrown };
            for _ in first..game.settings.knives {
                let th = challenger_throw(game, now);
                game.throws.push(th);
            }
            game.stage = Stage::Thrower(0, None, 1.0);
        }
        Stage::Challenger(thrown, into) => {
            let into = into + dt;
            // (The knife leaves the hand partway through the move.)
            if into - dt < NPC_RELEASE && into >= NPC_RELEASE {
                let th = challenger_throw(game, now);
                sfx.write(crate::sound::PlaySfx { event: NPC_THROW_SOUND, at: Some(th.from), volume: 1.0 });
                game.throws.push(th);
            }
            game.stage = Stage::Challenger(thrown, into);
            if into >= NPC_THROW {
                let thrown = thrown + 1;
                game.stage = if thrown >= game.settings.knives { Stage::Thrower(0, None, 1.0) } else { Stage::Challenger(thrown, 0.0) };
            }
        }
        Stage::Thrower(thrown, charge, wait) => {
            let wait = (wait - dt).max(0.0);
            // (A test lets go four fifths of the way up.)
            let held = knives.held || (test.is_some() && wait == 0.0 && charge.is_none_or(|c| fill_of(c) < 0.83));
            let mut next = Stage::Thrower(thrown, charge, wait);
            match charge {
                // Charging while held.
                Some(c) if held => next = Stage::Thrower(thrown, Some(c + dt), wait),
                // Let go: the knife flies.
                Some(c) => {
                    let power = shot_power(fill_of(c));
                    let eye = t.translation + Vec3::Y * (EYE_HEIGHT - HALF_HEIGHT);
                    let dir = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0) * Vec3::NEG_Z;
                    let (centre, normal) = wheel_face(game, site);
                    let speed = game.settings.speed.max(1.0) * power;
                    // (It leaves the hand partway through the arms' throw.)
                    let leaves = now + PLAYER_RELEASE;
                    let (to, pts, on_wheel, lands) = match flight(eye, dir, speed, game.settings.gravity, centre, normal) {
                        Some((hit, r, time)) if r <= WHEEL_RADIUS => (hit, points(&game.settings.rewards, r), true, leaves + time),
                        Some((hit, _, time)) => (hit, 0, false, leaves + time),
                        None => (eye + dir * 2.0 - Vec3::Y * 1.5, 0, false, leaves + 0.5),
                    };
                    info!("knives: the player throws at {:.2} power ({pts} points)", power);
                    game.throws.push(Throw { player: true, from: eye - Vec3::Y * 0.15, to, leaves, lands, points: pts, on_wheel, spawned: false });
                    let thrown = thrown + 1;
                    next = Stage::Thrower(thrown, None, BETWEEN_THROWS);
                }
                None if held && wait == 0.0 && thrown < game.settings.knives => next = Stage::Thrower(thrown, Some(0.0), 0.0),
                None if thrown >= game.settings.knives && wait == 0.0 && game.throws.is_empty() => {
                    let [mine, theirs] = game.scores;
                    let (paid, panel) = match mine.cmp(&theirs) {
                        std::cmp::Ordering::Greater => (game.wager * 2, "won"),
                        std::cmp::Ordering::Less => (0, "lost"),
                        std::cmp::Ordering::Equal => (game.wager, "draw"),
                    };
                    if paid > 0 {
                        progress.add_money(paid);
                    }
                    let (line, sound) = match panel {
                        "won" => ("NPC_Lose", game.settings.win),
                        "lost" => ("NPC_Win", game.settings.fail),
                        _ => ("Rematch", 0),
                    };
                    if sound != 0 {
                        sfx.write(crate::sound::PlaySfx { event: sound, at: None, volume: 1.0 });
                    }
                    game.quiet_until = 0.0;
                    say(game, line, &mut sfx);
                    info!("knives: {panel}, {mine} to {theirs} (paid {paid})");
                    next = Stage::Result(0.0);
                }
                None => {}
            }
            game.stage = next;
        }
        Stage::Result(shown) => {
            let shown = shown + dt;
            game.stage = Stage::Result(shown);
            if confirm || cancel || shown > RESULT {
                game.stage = Stage::Away;
                game.left = true;
                game.view = 0;
                limits.0 = None;
            }
        }
    }
    // Throws landing: points, the lines they draw.
    let landed: Vec<Throw> = game.throws.iter().filter(|th| th.lands <= now).cloned().collect();
    game.throws.retain(|th| th.lands > now || !th.spawned);
    for th in landed {
        game.scores[if th.player { 0 } else { 1 }] += th.points;
        game.landed += 1;
        game.points = th.points;
        let line = match (th.player, th.points) {
            (false, p) if p >= 25 => "NPC_GoodThrowCheer",
            (false, _) => "NPC_BadThrowCursing",
            (true, p) if p >= 50 => "Player_GoodThrowCursing",
            (true, 0) => "Player_VeryBadThrowCheer",
            (true, p) if p <= 10 => "Player_BadThrowCheer",
            _ => "",
        };
        if !line.is_empty() {
            say(game, line, &mut sfx);
        }
    }
    // The player's place: facing the challenger for the wager and the
    // result, watching their throws, then throwing.
    let place = match game.stage {
        Stage::Betting | Stage::Result(_) => Some((site.at(WATCH_SPOT), talk_at, 1)),
        Stage::Challenger(..) => Some((site.at(WATCH_SPOT), wheel_centre, 2)),
        Stage::Thrower(..) => Some((throw_at, wheel_centre, 3)),
        Stage::Away => None,
    };
    if let Some((at, look, view)) = place {
        let fresh = Vec2::new(t.translation.x - at.x, t.translation.z - at.z).length() > 0.3 || game.view != view;
        if fresh {
            game.view = view;
            let eye = at + Vec3::Y * EYE_HEIGHT;
            let to = look - eye;
            let yaw = (-to.x).atan2(-to.z);
            let pitch = (to.y / to.with_y(0.0).length().max(1.0)).atan();
            t.translation = at + Vec3::Y * HALF_HEIGHT;
            p.yaw = yaw;
            p.pitch = pitch;
            p.velocity = Vec3::ZERO;
            let [h, v] = game.settings.view.map(f32::to_radians);
            limits.0 = Some(ViewArc { yaw, pitch, yaw_range: [-h, h], pitch_range: [-v, v] });
        }
        // The aim sways in a figure of eight while throwing (the view moved
        // by the sway's change; the player's own look on top).
        if matches!(game.stage, Stage::Thrower(..)) {
            let [hs, vs, hf, vf] = game.settings.sway;
            let sway = |c: f32| Vec2::new(hs * (c * hf * std::f32::consts::TAU).sin(), vs * (c * vf * std::f32::consts::TAU).sin()) * SWAY_DEGREES.to_radians();
            let delta = sway(now) - sway(now - dt);
            p.yaw += delta.x;
            p.pitch += delta.y;
            // (A test aims at the middle, allowing for the drop at the power
            // it lets go at, as a steady hand would.)
            if test.is_some() {
                let eye = t.translation + Vec3::Y * (EYE_HEIGHT - HALF_HEIGHT);
                let flies = (wheel_centre - eye).length() / (game.settings.speed.max(1.0) * shot_power(0.83));
                let to = wheel_centre + Vec3::Y * 0.5 * GRAVITY * game.settings.gravity * flies * flies - eye;
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.with_y(0.0).length().max(1.0)).atan();
            }
        }
    }
    // The challenger's moves.
    pose_challenger(game, &mut cinema, &server, &clips, &mut clip_handles);
    // The HUD.
    let thrower = matches!(game.stage, Stage::Thrower(..));
    let panel = match game.stage {
        Stage::Betting => KnifePanel::Betting {
            row: game.row,
            wagers: game.settings.wagers,
            short: progress.money < game.settings.wagers[game.row],
            subtitle: game.settings.subtitle,
        },
        Stage::Result(_) => match game.scores[0].cmp(&game.scores[1]) {
            std::cmp::Ordering::Greater => KnifePanel::Won(game.wager),
            std::cmp::Ordering::Less => KnifePanel::Lost(game.wager),
            std::cmp::Ordering::Equal => KnifePanel::Draw,
        },
        _ => KnifePanel::Playing,
    };
    let panel = if game.leaving.is_some() { KnifePanel::Leaving } else { panel };
    knives.hud = (game.stage != Stage::Away).then(|| KnifeHud {
        panel,
        scores: game.scores,
        knives: game.settings.knives,
        thrown: match game.stage {
            Stage::Challenger(n, _) | Stage::Thrower(n, ..) => n,
            _ => 0,
        },
        player_turn: thrower,
        power: match game.stage {
            Stage::Thrower(_, charge, _) => charge.map(fill_of),
            _ => None,
        },
        landed: game.landed,
        points: game.points,
        throw_key: actions.label(Action::Fire, &settings),
        skip: matches!(game.stage, Stage::Challenger(..)) && game.leaving.is_none(),
    });
    hold.knives = game.stage != Stage::Away;
    // FC3_KNIFE_SHOTS=<dir> (tests): each stage captured a moment in (its
    // box played in; the knife drawn; a throw while it's held; the third's
    // points as they pop up).
    if let Ok(dir) = std::env::var("FC3_KNIFE_SHOTS") {
        let label = match game.stage {
            _ if game.leaving.is_some() => "leaving".into(),
            Stage::Away => String::new(),
            Stage::Betting => "betting".into(),
            Stage::Challenger(n, _) => {
                if n >= 2 {
                    "challenger".into()
                } else {
                    String::new()
                }
            }
            Stage::Thrower(n, Some(_), _) if n >= 2 => "thrower".into(),
            Stage::Thrower(3, None, _) => "landed".into(),
            Stage::Thrower(0, None, _) => "drawn".into(),
            Stage::Thrower(..) => String::new(),
            Stage::Result(_) => "result".into(),
        };
        if label != game.shot.0 {
            game.shot = (label, 0.0, std::mem::take(&mut game.shot.2));
        }
        game.shot.1 += dt;
        let moment = match game.shot.0.as_str() {
            "thrower" => 0.4,
            "landed" => 0.35,
            "drawn" => 0.95,
            _ => 1.0,
        };
        if !game.shot.0.is_empty() && game.shot.1 > moment && !game.shot.2.contains(&game.shot.0) {
            game.shot.2.push(game.shot.0.clone());
            crate::camera::capture(&mut commands, format!("{dir}/{}.png", game.shot.0));
        }
    }
}

/// A challenger's throw: nearer the middle the harder the row (its
/// difficulty), landing a moment after it leaves the hand.
fn challenger_throw(game: &mut Game, now: f32) -> Throw {
    let difficulty = game.settings.difficulty[game.row].max(0.5);
    let spread = 0.32 / difficulty.powf(0.6);
    let (u, v) = (game.dealer.unit(), game.dealer.unit());
    let r = spread * (-2.0 * (u.max(1e-6)).ln()).sqrt() * 0.7;
    let angle = v * std::f32::consts::TAU;
    let (centre, normal) = wheel_face(game, game.site);
    let side = Vec3::Y.cross(normal).normalize_or_zero();
    let to = centre + (side * angle.cos() + Vec3::Y * angle.sin()) * r;
    let from = game.site.at(THROW_SPOT) + Vec3::Y * 1.6;
    let on_wheel = r <= WHEEL_RADIUS;
    let points = if on_wheel { points(&game.settings.rewards, r) } else { 0 };
    Throw { player: false, from, to, leaves: now, lands: now + 0.25, points, on_wheel, spawned: false }
}

/// The wheel's face: its centre (the painted face's) and its normal toward
/// the throwing spot.
fn wheel_face(game: &Game, site: Site) -> (Vec3, Vec3) {
    let (centre, rotation) = game.wheel.map_or((site.at([-3.875, 3.125, 1.5]), site.anchor.rotation), |(_, w)| (w.translation, w.rotation));
    let mut normal = rotation * Vec3::Z;
    if normal.dot(site.at(THROW_SPOT) - centre) < 0.0 {
        normal = -normal;
    }
    (centre + normal * FACE, normal)
}

/// The challenger's move for the stage: talking (waiting, the wager, the
/// result), a throw each knife, watching the player's. Each in a scene of
/// its own about the anchor.
fn pose_challenger(
    game: &mut Game,
    cinema: &mut crate::cinema::Cinema,
    server: &AssetServer,
    clips: &Assets<Fc3Clip>,
    handles: &mut HashMap<String, Handle<Fc3Clip>>,
) {
    use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};
    let (want, once) = match game.stage {
        Stage::Challenger(n, into) => (THROW_MOVE, Some((n, into))),
        Stage::Thrower(..) => (WATCH_MOVE, None),
        _ => (TALK_MOVE, None),
    };
    // (A throw's move starts with each knife; the rest loop.)
    let fresh_throw = once.is_some_and(|(_, into)| into == 0.0 || game.pose.as_ref().is_none_or(|(_, m)| *m != THROW_MOVE));
    if game.pose.as_ref().is_some_and(|(_, m)| *m == want) && !fresh_throw {
        return;
    }
    let path = format!("{MOVES}{want}");
    let handle = handles.entry(path.clone()).or_insert_with(|| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(&path))));
    if !crate::cache_assets::done(server, handle.id()) {
        return;
    }
    let Some(length) = clips.get(&*handle).map(|c| c.0.duration).filter(|l| *l > 0.0) else { return };
    let name = format!("knives_challenger_{}", game.poses);
    game.poses += 1;
    let anim =
        Clip { start: 0.0, duration: length, kind: ClipKind::Animation { source: path, from: 0, to: (length * 30.0) as u32, blend: 0.2, anchored: false } };
    let groups = vec![Group { uid: 1, kind: GroupKind::Character, name: "challenger".into(), archetype: 0, clips: vec![anim] }];
    cinema.play_built(crate::cinema::Built {
        name: name.clone(),
        seq: Sequence { name: name.clone(), duration: length, looping: once.is_none(), groups, ..Default::default() },
        origin: game.site.anchor,
        bindings: HashMap::from([(1, game.site.challenger)]),
    });
    if let Some((old, _)) = game.pose.replace((name, want)) {
        cinema.stop_built(&old);
    }
}

/// A knife in the air or stuck in the wheel.
#[derive(Component)]
struct Knife {
    from: Vec3,
    to: Vec3,
    start: f32,
    lands: f32,
    on_wheel: bool,
}

/// A bullseye ring on a wheel.
#[derive(Component)]
struct Ring;

/// The wheel turning, its bullseye's rings on it, and the knives thrown
/// set flying (models loaded once).
#[allow(clippy::too_many_arguments)]
fn dress_wheel(
    mut knives: ResMut<KnifeGame>,
    time: Res<Time>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    mut handles: Local<HashMap<&'static str, Handle<Fc3Model>>>,
    mut wheels: Query<(Entity, &crate::objects::PlacedId, &mut Transform), Without<Knife>>,
    rings: Query<Entity, With<Ring>>,
    mut commands: Commands,
) {
    let Some(game) = knives.game.as_mut() else {
        for e in &rings {
            commands.entity(e).despawn();
        }
        return;
    };
    // The wheel, once it's in: its place unturned kept.
    if game.wheel.is_none() {
        if let Some((e, _, t)) = wheels.iter().find(|(_, id, _)| id.0 == game.site.wheel) {
            game.wheel = Some((e, *t));
        }
    }
    let Some((wheel, base)) = game.wheel else { return };
    let Ok((_, _, mut t)) = wheels.get_mut(wheel) else {
        game.wheel = None;
        game.rings = false;
        return;
    };
    t.rotation = base.rotation * Quat::from_rotation_z(-game.spin);
    let mut model = |path: &'static str| {
        let h = handles.entry(path).or_insert_with(|| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::model_rel(path)))).clone();
        crate::cache_assets::done(&server, h.id()).then(|| models.get(&h).and_then(|m| m.lods.first().cloned())).flatten()
    };
    // The bullseye's rings, just before the painted face (toward the
    // throwing spot).
    if !game.rings {
        let face = if (base.rotation * Vec3::Z).dot(game.site.at(THROW_SPOT) - base.translation) < 0.0 { -FACE } else { FACE };
        let lods: Vec<_> = RING_MODELS.iter().map(|m| model(m)).collect();
        if lods.iter().all(Option::is_some) {
            for lod in lods.into_iter().flatten() {
                let ring = commands.spawn((Transform::from_translation(Vec3::Z * face), Visibility::Visible, Ring, ChildOf(wheel))).id();
                for part in &lod.parts {
                    commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(ring)));
                }
            }
            game.rings = true;
        }
    }
    // Throws set flying.
    let Some(knife) = model(KNIFE_MODEL) else { return };
    let now = time.elapsed_secs();
    let clock = game.clock;
    for th in game.throws.iter_mut().filter(|th| !th.spawned) {
        th.spawned = true;
        let e = commands
            .spawn((
                Transform::from_translation(th.from).looking_to(th.to - th.from, Vec3::Y),
                Visibility::Visible,
                Knife {
                    from: th.from,
                    to: th.to,
                    start: now + (th.leaves - clock).max(0.0),
                    lands: now + (th.lands - clock).max((th.leaves - clock).max(0.0) + 0.05),
                    on_wheel: th.on_wheel,
                },
            ))
            .id();
        for part in &knife.parts {
            commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(e)));
        }
    }
}

/// Knives in the air move along their throw and stick in the wheel (turning
/// with it) or fall; all are gone when the game is.
fn fly_knives(
    knives: Res<KnifeGame>,
    time: Res<Time>,
    mut flying: Query<(Entity, &Knife, &mut Transform, Option<&ChildOf>)>,
    globals: Query<&GlobalTransform>,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    let game = knives.game.as_ref().filter(|g| g.stage != Stage::Away);
    for (e, k, mut t, parent) in &mut flying {
        let Some(game) = game else {
            commands.entity(e).despawn();
            continue;
        };
        if parent.is_some() {
            continue;
        }
        // (Unseen until it leaves the hand.)
        t.scale = if now < k.start { Vec3::ZERO } else { Vec3::ONE };
        let f = ((now - k.start) / (k.lands - k.start)).clamp(0.0, 1.0);
        if f < 1.0 {
            t.translation = k.from.lerp(k.to, f);
            continue;
        }
        match (k.on_wheel, game.wheel) {
            // Stuck: into the wheel's own space, so it turns with it.
            (true, Some((wheel, _))) => {
                let Ok(w) = globals.get(wheel) else { continue };
                let world = Transform::from_translation(k.to).looking_to(k.to - k.from, Vec3::Y);
                let local = w.affine().inverse() * world.compute_affine();
                commands.entity(e).insert((Transform::from_matrix(local.into()), ChildOf(wheel)));
            }
            // Missed: down it goes, and away.
            _ => {
                t.translation.y -= 4.0 * time.delta_secs();
                if now - k.lands > 1.5 {
                    commands.entity(e).despawn();
                }
            }
        }
    }
}

/// At the game: no walking, jumping or other weapons; the throw read before
/// the weapons have the fire button suppressed.
fn hold_controls(mut knives: ResMut<KnifeGame>, mut actions: ResMut<ActionState>) {
    if !knives.holds_player() {
        knives.held = false;
        return;
    }
    knives.held = actions.pressed(Action::Fire);
    actions.movement = Vec2::ZERO;
    actions.cycle = 0;
    for x in [
        Action::Jump,
        Action::NextWeapon,
        Action::PrevWeapon,
        Action::Grenade,
        Action::Molotov,
        Action::Melee,
        Action::ThrowRock,
        Action::C4,
        Action::Mine,
        Action::Fire,
        Action::Aim,
        Action::Reload,
        Action::Interact,
        Action::Crouch,
    ] {
        actions.suppress(x);
    }
}

/// The player kept where the game puts them.
fn hold_place(knives: Res<KnifeGame>, mut players: Query<(&mut Transform, &mut Player)>) {
    let Some(game) = knives.game.as_ref() else { return };
    let at = match game.stage {
        Stage::Betting | Stage::Challenger(..) | Stage::Result(_) => game.site.at(WATCH_SPOT),
        Stage::Thrower(..) => game.site.at(THROW_SPOT),
        Stage::Away => return,
    };
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    t.translation.x = at.x;
    t.translation.z = at.z;
    p.velocity = Vec3::ZERO;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The power curve: next to nothing at half, full past four fifths.
    #[test]
    fn the_power_curve() {
        assert!(shot_power(0.5) < 0.1 && shot_power(0.3) < 0.02);
        assert!((shot_power(0.75) - 0.88).abs() < 1e-3 && shot_power(0.83) > 0.97);
        assert_eq!(shot_power(1.2), 1.0);
        assert_eq!(shot_power(-1.0), 0.0);
    }

    /// The rings: the middle's 100, the edge's 5, the bare wheel nothing.
    #[test]
    fn rings_score_by_distance() {
        let rewards = [5, 10, 25, 50, 100];
        assert_eq!([0.03, 0.1, 0.15, 0.25, 0.3, 0.5].map(|r| points(&rewards, r)), [100, 50, 25, 10, 5, 0]);
    }

    /// A full-power throw from the spot falls a little and lands near the
    /// middle; a weak one falls short of the wheel's face (well below).
    #[test]
    fn throws_fall_as_they_fly() {
        let (eye, centre, normal) = (Vec3::new(0.0, 1.6, 7.76), Vec3::new(0.0, 1.6, 0.0), Vec3::Z);
        let (hit, r, time) = flight(eye, Vec3::NEG_Z, 45.0, 1.0, centre, normal).unwrap();
        assert!((time - 7.76 / 45.0).abs() < 1e-3 && r < 0.2 && hit.y < 1.6);
        let (_, r, _) = flight(eye, Vec3::NEG_Z, 45.0 * shot_power(0.6), 1.0, centre, normal).unwrap();
        assert!(r > WHEEL_RADIUS);
        assert!(flight(eye, Vec3::Z, 45.0, 1.0, centre, normal).is_none());
    }
}
