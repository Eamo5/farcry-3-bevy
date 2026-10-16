//! The poker tables (`CPokerMiniGame...`, SQC_100's `..._Pokerminigame`):
//! Texas hold'em (`holdem`) with the table's own movie (`original_ui`).
//!
//! A table's gameplay names its computer players (`gsf::PokerSettings`);
//! they find the table (the centre anchor nearest them, its four chair
//! anchors) and take the seats they ask for, the player the one left
//! (the first free). A bar's script starts its table's gameplay when the
//! player comes near; the player sits by using the free chair, chooses a
//! wager (the difficulty box: the buy-in paid, played for as chips), and
//! plays hands until they leave (their chips paid out), go bust (the
//! restart box) or clean the table (everyone's buy-in won). The story's
//! tables (JAS_170's with Sam, HOY_040's with Hoyt) start on their script's
//! `StartPoker...` event and play for nothing; between hands the scenes
//! their scripts wait for (`JAS220_B10C_01`...) are broadcast in turn, the
//! next once the script says one has played (`CinemaSequenceFinished`),
//! and last the game's own end of the player's game (`JasonWinsGame`,
//! `JasonLeavesPoker`). SQC_100's table is a bar's game whose script hears
//! the player clean it (`JasonWinsGame`) and leave.
//!
//! The data has no blinds or stacks: a game starts each player with the
//! buy-in (1000 in practice and at the story's tables), the blinds a
//! fortieth and a twentieth of it, doubling every ten hands.

use std::collections::HashMap;

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::gsf::PokerSettings;

use crate::cache_assets::Fc3Model;
use crate::controls::{Action, ActionState};
use crate::holdem::{self, Action as Bet, Card, Category, Dealer, Street, Style, Table};
use crate::player::{Player, ViewArc, ViewLimits, HALF_HEIGHT};
use crate::story::StoryHost;

pub struct PokerPlugin;

impl Plugin for PokerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PokerTable>()
            .add_systems(PreUpdate, hold_controls.after(crate::controls::apply_action_maps))
            .add_systems(Update, play_poker.after(crate::story::StorySet).before(crate::cinema::CinemaSet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, hold_seat.after(crate::player::player_move))
            .add_systems(Update, set_table.after(play_poker))
            .add_systems(Update, pose_players.after(play_poker).before(crate::cinema::CinemaSet));
    }
}

/// The messages the game itself broadcasts when the player is done at the
/// table, in this order (not scripted scenes of its story).
const DONE: [&str; 2] = ["JasonWinsGame", "JasonLeavesPoker"];
/// How long a scene's end is waited for before the game goes on without
/// it (s): a script that never says it has played.
const SCENE_WAIT: f32 = 180.0;
/// Hands played with no scene before a story table's game is over with
/// none (a script may arm its first scene a while after it starts).
const HANDS_BEFORE_DONE: u32 = 3;
/// Chips each player starts with in practice and at the story's tables.
const PRACTICE_CHIPS: u32 = 1000;
/// Hands between the blinds doubling.
const BLINDS_EVERY: u32 = 10;
/// How near the free chair the player sits down (m).
const SIT_REACH: f32 = 1.8;
/// How long a computer player thinks (s, least and most), and how long a
/// hand's results show unless the player goes on.
const THINK: [f32; 2] = [0.9, 2.2];
const RESULTS: f32 = 6.0;
/// How long the player thinks before a computer player gets impatient (s).
const IMPATIENT: f32 = 14.0;
/// The player's seat: back from the chair's anchor, facing the table.
const SEAT_BACK: f32 = 0.25;
const SEAT_PITCH: f32 = -28.0;

/// A name on the table's movie: a string of the game's (Jason, Sam, Hoyt)
/// or a seat's ("Player 2").
#[derive(Clone, Debug, PartialEq)]
pub enum Name {
    Text(u32),
    Seat(usize),
}

/// The player, Sam and Hoyt by name.
const JASON: u32 = 90985;
const SAM: u32 = 91000;
const HOYT: u32 = 90987;

/// A line for the info box: one of the game's strings (`0` the player
/// named, `1` the chips).
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub text: u32,
    pub name: Option<Name>,
    pub chips: Option<u32>,
}

/// The info box's strings.
mod line {
    pub const BIG_BLIND: u32 = 155031;
    pub const ALL_IN: u32 = 155032;
    pub const CALLED: u32 = 155033;
    pub const RAISED: u32 = 155034;
    pub const CHECKED: u32 = 155035;
    pub const FOLDED: u32 = 155036;
    pub const NEW_HAND: u32 = 155038;
    pub const REVEALING: u32 = 155039;
}

/// The action panel's label: CHECK, CALL or RAISE.
pub const CHECK: u32 = 153745;
pub const CALL: u32 = 153758;
pub const RAISE: u32 = 153330;

/// What the table's movie shows (`original_ui::drive_poker`).
#[derive(Clone, Debug, PartialEq)]
pub struct PokerHud {
    pub panel: Panel,
    /// P1 (the player), P2 (on their left), P3 (across), P4 (on their
    /// right): name, chips, and how it shows in the table's diagram (1 in the
    /// hand, 10 out of it, 20 no one there).
    pub seats: [(Name, u32, u32); 4],
    /// Who has the button (P1..P4 as 0..4).
    pub dealer: usize,
    pub pot: u32,
    pub last_bet: u32,
    pub chips: u32,
    pub info: Option<Info>,
    /// The player's turn: the action's label, the bet set, and whether it
    /// can go lower and higher.
    pub actions: Option<(u32, u32, bool, bool)>,
    /// The player's cards and the board (`2-9TJQKA` + `hdcs`), whether
    /// they've folded, and the hand they make (a string id).
    pub cards: Option<(String, bool, u32, bool)>,
    /// The table's diagram (while the totals are asked for).
    pub totals: bool,
}

/// The movie's boxes.
#[derive(Clone, Debug, PartialEq)]
pub enum Panel {
    /// The difficulty box: the row chosen, each row's wager and whether
    /// it's offered, whether the row chosen costs more than the player has,
    /// and the subtitle (a string id).
    Difficulty {
        row: usize,
        wagers: [u32; 4],
        enabled: [bool; 4],
        short: bool,
        subtitle: u32,
    },
    Table,
    Results(Results),
    /// Leaving: the buy-in, and the chips the player would leave with.
    Quit {
        buy_in: u32,
        chips: u32,
    },
    Busted,
    Cleaned {
        won: u32,
    },
    /// SQC_100's: the gambler's money won back.
    CleanedStory,
}

/// A hand's end, a line a player: their name, the five cards that made
/// their hand and its name (a string id; none when everyone else folded),
/// and how it went. Winners come first, then those it put out of the game,
/// then the rest (the movie's order).
#[derive(Clone, Debug, PartialEq)]
pub struct Results {
    pub lines: Vec<(Name, Option<(String, u32)>, Line)>,
}

/// How a hand went for a player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Line {
    Won(u32),
    BustedOut,
    Lost,
}

/// The poker game being played, if any, and its movie.
#[derive(Resource, Default)]
pub struct PokerTable {
    game: Option<Game>,
    pub hud: Option<PokerHud>,
    /// What the mouse is over in the movie's boxes (confirm, cancel, a
    /// difficulty row).
    pub hover: (bool, bool, Option<usize>),
}

impl PokerTable {
    /// The player is at the table: the story's autopilot leaves him to it.
    pub fn holds_player(&self) -> bool {
        self.game.as_ref().is_some_and(|g| g.seated)
    }

    /// Escape backs out of the table's boxes rather than pausing.
    pub fn claims_escape(&self) -> bool {
        self.holds_player()
    }
}

/// Where a table is (Bevy space): its centre, and its chairs' anchors and
/// their facing (as scenes take an entity's angles).
#[derive(Clone, Copy, Debug)]
struct Place {
    centre: Vec3,
    chairs: [Vec3; 4],
    facing: [Quat; 4],
}

#[derive(Clone, Debug)]
struct Npc {
    name: Name,
    style: Style,
    /// Its entry in the settings, its character, and its moves (`MOVES`).
    index: usize,
    entity: u64,
    moves: usize,
}

/// The computer players' moves at the table, each way of playing's own:
/// their folder, and the suffix their names end with.
const MOVES: [(&str, &str); 4] = [("unskilled", "unsk"), ("skilled", "sk"), ("expert", "ex"), ("loosecanon", "lc")];

/// A computer player's move: seated with cards or without (looping), and
/// those it makes once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Move {
    IdleCards,
    IdleEmpty,
    TakeCards,
    Check,
    Bet,
    BigBet,
    AllIn,
    Fold,
    Win,
    Lose,
    /// Waiting on the player: ahead of it, on its left or right.
    Impatient(&'static str),
}

impl Move {
    fn looping(self) -> bool {
        matches!(self, Move::IdleCards | Move::IdleEmpty)
    }
}

/// A move's clip for a way of playing (`MOVES`).
fn move_clip(moves: usize, m: Move) -> String {
    let (folder, suffix) = MOVES[moves.min(3)];
    let expert = suffix == "ex";
    let name = match m {
        Move::IdleCards if expert => "fulbst_poker_sitidlwcards01".to_string(),
        Move::IdleCards => "fullst_poker_sitidlecards".to_string(),
        Move::IdleEmpty => "fulbst_poker_sitidlenocards01".to_string(),
        Move::TakeCards => "fulbst_poker_takecards".to_string(),
        Move::Check => "fulbst_poker_check".to_string(),
        Move::Bet => "fulbst_poker_betcallraisesmall".to_string(),
        Move::BigBet => "fulbst_poker_betcallraiselarge".to_string(),
        Move::AllIn => "fulbst_poker_allin".to_string(),
        Move::Fold => "fulbst_poker_fold".to_string(),
        Move::Win => "fulbst_poker_winhand".to_string(),
        Move::Lose if expert => "fulbst_poker_losehand01".to_string(),
        Move::Lose => "fulbst_poker_loosehand".to_string(),
        Move::Impatient(side) => format!("fulbst_poker_impatient{side}"),
    };
    let tail = if expert && m == Move::AllIn { "noweapontake02" } else { "noweapon" };
    format!(r"graphics\characters\_common\animations\exoticgameplay\minigames\poker\{folder}\3rdge_{name}_{suffix}_+000fw_{tail}.mab")
}

/// A computer player's moves under way: the one-shot asked for, and the
/// scene playing (its name and move; a count for fresh names).
#[derive(Clone, Debug, Default)]
struct Pose {
    asked: Option<Move>,
    playing: Option<(String, Move)>,
    count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// Not sat at the table (a bar's, until the player takes the chair).
    Away,
    Choosing,
    Playing,
    /// A hand's results showing, for this long so far.
    Results(f32),
    Quit,
    Busted,
    Cleaned,
}

struct Game {
    id: u64,
    name: String,
    settings: PokerSettings,
    place: Option<Place>,
    npcs: [Option<Npc>; 4],
    player_seat: usize,
    seated: bool,
    stage: Stage,
    table: Option<Table>,
    dealer: Dealer,
    row: usize,
    buy_in: u32,
    /// The chips each player started with.
    stake: u32,
    hands: u32,
    /// The bet the player has set (to this much in front of them).
    bet: u32,
    /// Time to the next computer player's move.
    think: f32,
    info: Option<Info>,
    last_bet: u32,
    results: Option<Results>,
    /// The story's side: started by its script, the scene broadcast whose
    /// end is waited for and how long since, those sent, hands with none.
    started: bool,
    scene: Option<(String, f32)>,
    sent: Vec<String>,
    idle_hands: u32,
    done: bool,
    /// `FC3_POKER_SHOTS`: the stage being shown and for how long, and the
    /// stages captured.
    shot: (String, f32, Vec<String>),
    /// The player has got up from the table (a test sits down once).
    left: bool,
    /// The computer players' moves, by seat; how long the player has been
    /// thinking.
    poses: [Pose; 4],
    waiting: f32,
}

impl Game {
    /// A story's table (scenes between hands, played for nothing).
    fn story(&self) -> bool {
        matches!(self.settings.kind, 1..=3)
    }
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

/// A poker table's gameplay (not the resources the scripts preload).
pub fn poker_gameplay(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("pokerminigame") && !lower.ends_with("resourcecontainer")
}

/// A computer player's way of playing (`PlayerType`; as the difficulty
/// has it when it's the default), and its moves (`MOVES`).
fn style(player_type: u32, row: usize, dealer: &mut Dealer) -> (Style, usize) {
    let unskilled = Style { caution: 0.0, raise_at: 0.72, bluff: 0.02 };
    let expert = Style { caution: 0.08, raise_at: 0.58, bluff: 0.1 };
    let ways = [unskilled, Style::STEADY, expert, Style::LOOSE];
    let way = match player_type {
        0..=3 => player_type as usize,
        4 => (dealer.unit() * 4.0) as usize % 4,
        _ => [0, 0, 1, 2][row.min(3)],
    };
    (ways[way], way)
}

/// A card as the movie writes it.
fn card_text(c: Card) -> String {
    let rank = b"23456789TJQKA"[(c.rank() - 2) as usize] as char;
    let suit = b"cdhs"[c.suit() as usize] as char;
    format!("{rank}{suit}")
}

/// A kind of hand's name (a string id).
fn hand_name(c: Category) -> u32 {
    match c {
        Category::HighCard => 154993,
        Category::Pair => 153354,
        Category::TwoPair => 153353,
        Category::ThreeOfAKind => 153352,
        Category::Straight => 153357,
        Category::Flush => 154992,
        Category::FullHouse => 153351,
        Category::FourOfAKind => 153356,
        Category::StraightFlush => 153355,
        Category::RoyalFlush => 153350,
    }
}

/// The table near the computer players (else the player: a story's table,
/// its players placed for its scenes): the centre anchor nearest them and
/// the chair anchors about it (by their numbers).
fn find_place(data: &fc3_story::data::StoryData, npcs: &[u64], player: Vec3) -> Option<Place> {
    let at: Vec<Vec3> = npcs.iter().filter_map(|id| data.position(*id)).map(crate::data::to_bevy).collect();
    let mid = at.iter().sum::<Vec3>() / at.len().max(1) as f32;
    let centres: Vec<Vec3> =
        data.entities.values().filter(|e| e.name.eq_ignore_ascii_case("Exotic/PokerTableCenterAnchor")).map(|e| crate::data::to_bevy(e.pos)).collect();
    let nearest = |to: Vec3, reach: f32| centres.iter().copied().filter(|c| c.distance(to) < reach).min_by(|a, b| a.distance(to).total_cmp(&b.distance(to)));
    let centre = nearest(mid, 12.0).filter(|_| !at.is_empty()).or_else(|| nearest(player, 60.0))?;
    let mut chairs = [None; 4];
    let mut facing = [Quat::IDENTITY; 4];
    for e in data.entities.values() {
        let Some(n) = e.name.strip_prefix("Exotic/PokerChairAnchor").and_then(|n| n.parse::<usize>().ok()) else { continue };
        let p = crate::data::to_bevy(e.pos);
        if (1..=4).contains(&n) && p.distance(centre) < 2.0 {
            chairs[n - 1] = Some(p);
            facing[n - 1] = Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles));
        }
    }
    Some(Place { centre, chairs: chairs.map(|c| c.unwrap_or(centre)), facing })
}

/// The seats: each computer player the one it asks for when free, else the
/// first free; the player the first left.
fn seat_players(wanted: &[u32]) -> ([Option<usize>; 4], usize) {
    let mut taken = [None; 4];
    for (i, &w) in wanted.iter().enumerate() {
        if (w as usize) < 4 && taken[w as usize].is_none() {
            taken[w as usize] = Some(i);
        }
    }
    for (i, &w) in wanted.iter().enumerate() {
        if (w as usize) >= 4 || taken[w as usize] != Some(i) {
            if let Some(s) = (0..4).find(|&s| taken[s].is_none()) {
                taken[s] = Some(i);
            }
        }
    }
    let player = (0..4).find(|&s| taken[s].is_none()).unwrap_or(0);
    (taken, player)
}

/// Fills a line's `0` with a name and `1` with chips.
pub fn fill(text: &str, name: &str, chips: Option<u32>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let alone = |j: Option<usize>| j.and_then(|j| chars.get(j)).is_none_or(|n| !n.is_alphanumeric());
        if (c == '0' || c == '1') && alone(i.checked_sub(1)) && alone(Some(i + 1)) {
            match (c, chips) {
                ('0', _) => out.push_str(name),
                ('1', Some(n)) => out.push_str(&n.to_string()),
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn play_poker(
    host: Option<NonSendMut<StoryHost>>,
    mut poker: ResMut<PokerTable>,
    time: Res<Time>,
    cinema: Res<crate::cinema::Cinema>,
    info: Option<Res<crate::CacheInfo>>,
    (keys, mouse, actions, settings): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<ActionState>, Res<crate::settings::Settings>),
    (mut progress, mut hud, mut limits): (ResMut<crate::progression::Progress>, ResMut<crate::hud::HudState>, ResMut<ViewLimits>),
    mut players: Query<(&mut Transform, &mut Player)>,
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    let events = std::mem::take(&mut host.gsf_events);
    let test = std::env::var("FC3_POKER_TEST").ok().and_then(|s| s.parse::<usize>().ok());
    // (The player's hands played for them: in a test, and under the story's
    // autopilot, which a story's table holds while its scenes come.)
    let auto = test.is_some() || std::env::var_os("FC3_STORY_AUTOPILOT").is_some();
    let dt = time.delta_secs();
    let poker = &mut *poker;
    // (The latest poker game: a bar's table, near, may have one of its own
    // waiting for the player.)
    let Some((id, name)) = host.story.gameplays().into_iter().rev().find(|g| poker_gameplay(&g.1)) else {
        if poker.game.take().is_some_and(|g| g.seated) {
            limits.0 = None;
        }
        poker.hud = None;
        return;
    };
    if poker.game.as_ref().is_none_or(|g| g.id != id) {
        let settings = info
            .as_ref()
            .and_then(|i| std::fs::read(i.root.join(fc3_formats::gsf::CACHE_REL)).ok())
            .and_then(|b| PokerSettings::parse(&b, &name).map_err(|e| warn!("poker: {name}: {e:#}")).ok())
            .unwrap_or_default();
        let data = host.story.data();
        let player = players.single().map(|(t, _)| t.translation).unwrap_or_default();
        let place = find_place(&data, &settings.npcs.iter().map(|n| n.entity).collect::<Vec<_>>(), player);
        let seed = (time.elapsed().as_nanos() as u64) ^ id;
        let mut dealer = Dealer::new(seed);
        let (taken, player_seat) = seat_players(&settings.npcs.iter().map(|n| n.seat).collect::<Vec<_>>());
        let npcs = taken.map(|t| {
            t.map(|i| {
                let n = settings.npcs[i];
                let name = match n.dialog {
                    4 => Name::Text(SAM),
                    5 => Name::Text(HOYT),
                    _ => Name::Seat(0),
                };
                let (style, moves) = style(n.player_type, 0, &mut dealer);
                Npc { name, style, index: i, entity: n.entity, moves }
            })
        });
        info!(
            "poker: {name} ({} players, kind {}), the player's seat {}, table {:?}",
            settings.npcs.len(),
            settings.kind,
            player_seat + 1,
            place.map(|p| p.centre)
        );
        if let Some(p) = place {
            debug!("poker: chairs {:?} facing {:?}", p.chairs.map(|c| c - p.centre), p.facing.map(|q| (q * Vec3::Z).xz()));
        }
        poker.game = Some(Game {
            id,
            name,
            settings,
            place,
            npcs,
            player_seat,
            seated: false,
            stage: Stage::Away,
            table: None,
            dealer,
            row: 1,
            buy_in: 0,
            stake: 0,
            hands: 0,
            bet: 0,
            think: 0.0,
            info: None,
            last_bet: 0,
            results: None,
            started: false,
            scene: None,
            sent: Vec::new(),
            idle_hands: 0,
            done: false,
            shot: Default::default(),
            left: false,
            poses: Default::default(),
            waiting: 0.0,
        });
    }
    let Some(game) = poker.game.as_mut() else { return };
    for e in events {
        if e.starts_with("StartPoker") {
            info!("poker: {} starts ({e})", game.name);
            game.started = true;
        } else if e == "CinemaSequenceFinished" {
            if let Some((scene, _)) = game.scene.take() {
                info!("poker: scene {scene} played");
            }
        }
    }
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    let player_at = t.translation;
    let seat_at = game.place.map(|pl| {
        let chair = pl.chairs[game.player_seat];
        let away = (chair - pl.centre).with_y(0.0).normalize_or_zero();
        (chair + away * SEAT_BACK, pl.centre)
    });
    // Sitting down: a story's table once its script starts the game, a
    // bar's when the player takes the free chair.
    if !game.seated && !game.done {
        let sit = if game.story() {
            game.started
        } else {
            let near = seat_at.is_some_and(|(at, _)| at.with_y(0.0).distance(player_at.with_y(0.0)) < SIT_REACH.max(if test.is_some() { 8.0 } else { 0.0 }));
            if near && hud.prompt.is_none() {
                hud.prompt = Some(format!("{}  Play poker", actions.label(Action::Interact, &settings)));
            }
            near && (actions.just_pressed(Action::Interact) || (test.is_some() && !game.left))
        };
        if sit {
            game.seated = true;
            if let Some((at, centre)) = seat_at {
                let to = (centre - at).with_y(0.0);
                let yaw = (-to.x).atan2(-to.z);
                let ground = at.y.min(player_at.y - HALF_HEIGHT);
                t.translation = Vec3::new(at.x, ground + HALF_HEIGHT, at.z);
                p.yaw = yaw;
                p.pitch = SEAT_PITCH.to_radians();
                p.velocity = Vec3::ZERO;
                limits.0 = Some(ViewArc { yaw, pitch: p.pitch, yaw_range: [-1.1, 1.1], pitch_range: [-0.9, 0.35] });
                // FC3_POKER_AIM=<yaw>,<pitch> (tests, degrees): the view
                // turned from the table's middle and pitched.
                let aim = std::env::var("FC3_POKER_AIM").ok();
                if let Some((dy, dp)) = aim.as_deref().and_then(|s| s.split_once(',')).and_then(|(a, b)| Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)))
                {
                    p.yaw += dy.to_radians();
                    p.pitch = dp.to_radians();
                }
            }
            if game.story() {
                start_game(game, PRACTICE_CHIPS, 0);
            } else {
                game.stage = Stage::Choosing;
            }
            info!("poker: the player sits at {} (seat {})", game.name, game.player_seat + 1);
        }
    }
    if !game.seated {
        poker.hud = None;
        return;
    }
    let (confirm_hover, cancel_hover, row_hover) = poker.hover;
    let click = mouse.just_pressed(MouseButton::Left);
    let confirm = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || (click && confirm_hover);
    let cancel = keys.just_pressed(KeyCode::Escape) || (click && cancel_hover);
    // (Only those the scripts listen for.)
    let mut stand = false;
    let mut tell: Vec<&'static str> = Vec::new();
    match game.stage {
        Stage::Away => {}
        Stage::Choosing => {
            let wagers: [u32; 4] = std::array::from_fn(|i| game.settings.difficulties.get(i).map_or(0, |d| d.buy_in));
            let enabled: [bool; 4] = std::array::from_fn(|i| game.settings.difficulties.get(i).is_some_and(|d| d.enabled));
            if !enabled[game.row] {
                game.row = (0..4).find(|&r| enabled[r]).unwrap_or(0);
            }
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
                game.row = if enabled[r.min(3)] { r.min(3) } else { game.row };
            }
            let short = progress.money < wagers[game.row];
            // (A test takes its row after a moment.)
            game.think += dt;
            if (confirm || (click && row_hover.is_some()) || (test.is_some() && game.think > 1.5)) && enabled[game.row] && !short {
                let buy_in = wagers[game.row];
                if buy_in > 0 {
                    progress.spend(buy_in);
                }
                for n in game.npcs.iter_mut().flatten() {
                    if let Some(npc) = game.settings.npcs.get(n.index) {
                        (n.style, n.moves) = style(npc.player_type, game.row, &mut game.dealer);
                    }
                }
                start_game(game, buy_in, buy_in);
                info!("poker: row {} (buy-in {buy_in})", game.row);
            } else if cancel {
                stand = true;
            }
            if let Ok(dir) = std::env::var("FC3_POKER_SHOTS") {
                game.shot.1 += dt;
                if game.shot.1 > 0.6 && !game.shot.2.iter().any(|s| s == "difficulty") {
                    game.shot.2.push("difficulty".into());
                    crate::camera::capture(&mut commands, format!("{dir}/difficulty.png"));
                }
            }
            poker.hud = Some(hud_of(game, Panel::Difficulty { row: game.row, wagers, enabled, short, subtitle: game.settings.subtitle }, &keys));
            finish(poker, stand, &tell, &mut host, &mut limits);
            return;
        }
        Stage::Playing => play(game, dt, test, auto, &keys, confirm, cancel, &mut host, &cinema),
        Stage::Results(shown) => {
            let shown = shown + dt;
            game.stage = Stage::Results(shown);
            if confirm || shown > if auto { 2.0 } else { RESULTS } {
                game.results = None;
                game.stage = Stage::Playing;
                after_hand(game, &mut host);
            }
        }
        Stage::Quit => {
            if confirm || test.is_some() {
                let chips = game.table.as_ref().map_or(0, |t| t.seats[game.player_seat].chips);
                if game.buy_in > 0 && chips > 0 {
                    progress.add_money(chips);
                }
                info!("poker: the player leaves with {chips}");
                tell.push("JasonLeavesPoker");
                stand = true;
            } else if cancel {
                game.stage = Stage::Playing;
            }
        }
        Stage::Busted => {
            if confirm && !game.story() {
                game.stage = Stage::Choosing;
                game.table = None;
            } else if cancel || test.is_some() || game.story() {
                tell.push("JasonLeavesPoker");
                stand = true;
            }
        }
        Stage::Cleaned => {
            if confirm && game.settings.kind == 0 && test.is_none() {
                game.stage = Stage::Choosing;
                game.table = None;
            } else if confirm || cancel || test.is_some() {
                tell.push("JasonLeavesPoker");
                stand = true;
            }
        }
    }
    if game.done {
        stand = true;
    }
    if !stand {
        let panel = match game.stage {
            Stage::Results(_) => game.results.clone().map_or(Panel::Table, Panel::Results),
            Stage::Quit => Panel::Quit { buy_in: game.buy_in, chips: game.table.as_ref().map_or(0, |t| t.seats[game.player_seat].chips) },
            Stage::Busted => Panel::Busted,
            Stage::Cleaned if game.settings.kind == 4 => Panel::CleanedStory,
            Stage::Cleaned => Panel::Cleaned { won: game.table.as_ref().map_or(0, |t| t.seats[game.player_seat].chips) },
            _ => Panel::Table,
        };
        // FC3_POKER_SHOTS=<dir> (tests): the screen captured a moment into
        // each kind of box (the table once the player has a turn).
        if let Ok(dir) = std::env::var("FC3_POKER_SHOTS") {
            let label = match (&panel, game.table.as_ref().and_then(|t| t.to_act)) {
                (Panel::Table, Some(s)) if s == game.player_seat => "turn".to_string(),
                // (And each street's board.)
                (Panel::Table, _) => match game.table.as_ref().map_or(0, |t| t.board.len()) {
                    0 => "table".to_string(),
                    n => format!("board{n}"),
                },
                (p, _) => format!("{p:?}").split([' ', '(', '{']).next().unwrap_or("box").to_ascii_lowercase(),
            };
            if game.shot.0 != label {
                game.shot = (label, 0.0, std::mem::take(&mut game.shot.2));
            }
            game.shot.1 += dt;
            if game.shot.1 > 0.6 && !game.shot.2.contains(&game.shot.0) {
                game.shot.2.push(game.shot.0.clone());
                crate::camera::capture(&mut commands, format!("{dir}/{}.png", game.shot.0));
            }
        }
        poker.hud = (cinema.view.is_none()).then(|| hud_of(game, panel, &keys));
    }
    finish(poker, stand, &tell, &mut host, &mut limits);
}

/// The player up from the table, the scripts told what they listen for.
fn finish(poker: &mut PokerTable, stand: bool, tell: &[&str], host: &mut StoryHost, limits: &mut ViewLimits) {
    if !stand {
        return;
    }
    let Some(game) = poker.game.as_mut() else { return };
    let awaited = host.story.mission_messages();
    for m in tell.iter().filter(|m| awaited.iter().any(|a| a == *m)) {
        info!("poker: {m}");
        host.story.message(m);
    }
    info!("poker: the player gets up from {}", game.name);
    game.seated = false;
    game.left = true;
    game.stage = Stage::Away;
    game.table = None;
    game.results = None;
    limits.0 = None;
    poker.hud = None;
}

/// A game begun: everyone at the table with the stake, the blinds by it.
fn start_game(game: &mut Game, stake: u32, buy_in: u32) {
    let chips = if stake == 0 { PRACTICE_CHIPS } else { stake };
    let stacks: Vec<u32> = (0..4).map(|s| if s == game.player_seat || game.npcs[s].is_some() { chips } else { 0 }).collect();
    let small = (chips / 40).max(1);
    let mut table = Table::new(&stacks, small, small * 2);
    table.button = game.player_seat;
    game.table = Some(table);
    game.buy_in = buy_in;
    game.stake = chips;
    game.hands = 0;
    game.stage = Stage::Playing;
    game.think = 0.0;
}

/// A hand being played: the deal, the computer players' turns, the
/// player's keys, and its end.
#[allow(clippy::too_many_arguments)]
fn play(
    game: &mut Game,
    dt: f32,
    test: Option<usize>,
    auto: bool,
    keys: &ButtonInput<KeyCode>,
    confirm: bool,
    cancel: bool,
    host: &mut StoryHost,
    cinema: &crate::cinema::Cinema,
) {
    if cinema.view.is_some() || game.scene.is_some() {
        if let Some((scene, waited)) = game.scene.as_mut() {
            *waited += dt;
            if *waited > SCENE_WAIT {
                warn!("poker: scene {scene} never said it had played; going on");
                game.scene = None;
            }
        }
        return;
    }
    let seat = game.player_seat;
    let story = game.story();
    let Some(table) = game.table.as_mut() else { return };
    // Leaving (where the table lets the player).
    if cancel && (game.settings.leave || test.is_some()) {
        game.stage = Stage::Quit;
        return;
    }
    if test.is_some() && game.hands >= std::env::var("FC3_POKER_HANDS").ok().and_then(|s| s.parse().ok()).unwrap_or(6) && table.street == Street::Over {
        game.stage = Stage::Quit;
        return;
    }
    if table.street == Street::Over {
        // (A story's table plays on for its scenes: anyone out has their
        // stake again.)
        if story && (table.seats[seat].chips == 0 || table.seats.iter().enumerate().all(|(i, s)| i == seat || s.chips == 0)) {
            for (i, s) in table.seats.iter_mut().enumerate() {
                if (i == seat || game.npcs[i].is_some()) && s.chips == 0 {
                    s.chips = PRACTICE_CHIPS;
                }
            }
        }
        // The game's end, or the next hand.
        if table.seats[seat].chips == 0 {
            game.stage = Stage::Busted;
            return;
        }
        if table.seats.iter().enumerate().all(|(i, s)| i == seat || s.chips == 0) {
            info!("poker: the player has cleaned the table");
            game.stage = Stage::Cleaned;
            if game.settings.kind == 4 || story {
                let awaited = host.story.mission_messages();
                if awaited.iter().any(|m| m == "JasonWinsGame") {
                    host.story.message("JasonWinsGame");
                }
            }
            return;
        }
        game.hands += 1;
        if game.hands > 1 && (game.hands - 1) % BLINDS_EVERY == 0 {
            table.small_blind *= 2;
            table.big_blind *= 2;
        }
        table.deal(&mut game.dealer);
        game.last_bet = table.big_blind;
        // (The big blind posted, the last thing before the first to act.)
        let big = (0..4).filter(|&i| table.seats[i].put_in > 0).max_by_key(|&i| (table.seats[i].put_in, i));
        game.info = Some(match big {
            Some(b) => Info { text: line::BIG_BLIND, name: Some(name_of_seat(game.player_seat, &game.npcs, b)), chips: Some(table.seats[b].put_in) },
            None => Info { text: line::NEW_HAND, name: None, chips: None },
        });
        game.think = THINK[1];
        game.bet = 0;
        for (i, pose) in game.poses.iter_mut().enumerate() {
            if game.npcs[i].is_some() && table.seats[i].hole.is_some() {
                pose.asked = Some(Move::TakeCards);
            }
        }
        info!("poker: hand {} (blinds {}/{})", game.hands, table.small_blind, table.big_blind);
        debug!("poker: cards {:?}", table.seats.iter().map(|s| s.hole.map(|h| h.map(card_text))).collect::<Vec<_>>());
        return;
    }
    let Some(to_act) = table.to_act else { return };
    let before = (table.seats[to_act].bet, table.seats[to_act].put_in, table.current_bet);
    // (A computer player gets impatient with a player who takes a while.)
    if to_act == seat && !auto {
        game.waiting += dt;
        let npcs: Vec<usize> = (0..4).filter(|&i| game.npcs[i].is_some() && table.seats[i].hole.is_some() && !table.seats[i].folded).collect();
        if game.waiting > IMPATIENT && !npcs.is_empty() {
            game.waiting = 0.0;
            let i = npcs[game.dealer.below(npcs.len())];
            let side = match (seat + 4 - i) % 4 {
                1 => "lt",
                3 => "rt",
                _ => "fw",
            };
            game.poses[i].asked = Some(Move::Impatient(side));
        }
    } else {
        game.waiting = 0.0;
    }
    let action = if to_act == seat && !auto {
        // The player: the bet up and down by the big blind, then check,
        // call or raise to it, fold, or all in.
        let (call, least, most) = table.options(seat);
        let floor = (table.seats[seat].bet + call).min(most);
        if game.bet < floor || game.bet > most {
            game.bet = floor;
        }
        let step = table.big_blind.max(1);
        if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) || keys.just_pressed(KeyCode::ArrowRight) {
            game.bet = if game.bet < least { least } else { (game.bet + step).min(most) };
        }
        if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) || keys.just_pressed(KeyCode::ArrowLeft) {
            game.bet = if game.bet <= least { floor } else { game.bet.saturating_sub(step).max(least) };
        }
        if keys.just_pressed(KeyCode::KeyE) {
            Some(Bet::Fold)
        } else if keys.just_pressed(KeyCode::KeyQ) {
            Some(Bet::AllIn)
        } else if confirm {
            Some(if game.bet > table.current_bet {
                Bet::Raise(game.bet)
            } else if call > 0 {
                Bet::Call
            } else {
                Bet::Check
            })
        } else {
            None
        }
    } else {
        game.think -= dt;
        if game.think > 0.0 {
            None
        } else {
            let style = if to_act == seat { Style::STEADY } else { game.npcs[to_act].as_ref().map_or(Style::STEADY, |n| n.style) };
            let speed = if auto { 0.3 } else { 1.0 };
            game.think = (THINK[0] + game.dealer.unit() * (THINK[1] - THINK[0])) * speed;
            Some(holdem::decide(table, to_act, style, &mut game.dealer))
        }
    };
    let Some(action) = action else { return };
    table.act(to_act, action);
    // What it came to, for the info box (by what went in: the street may
    // have ended with it).
    let s = &table.seats[to_act];
    let name = Some(name_of_seat(game.player_seat, &game.npcs, to_act));
    let added = s.put_in - before.1;
    let now = before.0 + added;
    let line = if s.folded {
        Info { text: line::FOLDED, name, chips: None }
    } else if s.chips == 0 && added > 0 {
        Info { text: line::ALL_IN, name, chips: Some(s.put_in) }
    } else if added == 0 {
        Info { text: line::CHECKED, name, chips: None }
    } else if now > before.2 {
        Info { text: line::RAISED, name, chips: Some(now) }
    } else {
        Info { text: line::CALLED, name, chips: Some(added) }
    };
    if added > 0 {
        game.last_bet = added;
    }
    if game.npcs[to_act].is_some() {
        game.poses[to_act].asked = Some(if s.folded {
            Move::Fold
        } else if s.chips == 0 && added > 0 {
            Move::AllIn
        } else if added == 0 {
            Move::Check
        } else if added >= table.big_blind * 4 {
            Move::BigBet
        } else {
            Move::Bet
        });
    }
    debug!("poker: seat {} {action:?}", to_act + 1);
    game.info = Some(line);
    if table.street == Street::Over {
        let outcome = table.outcome.clone().unwrap_or_default();
        let board: Vec<Card> = table.board.clone();
        // Those shown down, or the one left when the rest folded.
        let mut in_it: Vec<usize> = (0..4).filter(|&i| outcome.shown.get(i).copied().flatten().is_some()).collect();
        if in_it.is_empty() {
            in_it = (0..4).filter(|&i| outcome.won.get(i).is_some_and(|w| *w > 0)).collect();
        }
        let how = |i: usize| match outcome.won.get(i).copied().unwrap_or(0) {
            0 if table.seats[i].chips == 0 => Line::BustedOut,
            0 => Line::Lost,
            w => Line::Won(w),
        };
        in_it.sort_by_key(|&i| match how(i) {
            Line::Won(_) => 0,
            Line::BustedOut => 1,
            Line::Lost => 2,
        });
        let lines = in_it
            .iter()
            .map(|&i| {
                let hand = outcome.shown.get(i).copied().flatten().zip(table.seats[i].hole).map(|(v, hole)| {
                    let five = best_five(&[&hole[..], &board].concat());
                    (five.iter().map(|c| card_text(*c)).collect::<Vec<_>>().join(" "), hand_name(v.category))
                });
                (name_of_seat(game.player_seat, &game.npcs, i), hand, how(i))
            })
            .collect();
        if in_it.len() > 1 {
            game.info = Some(Info { text: line::REVEALING, name: None, chips: None });
        }
        info!("poker: hand {} over: won {:?}", game.hands, outcome.won);
        for &i in &in_it {
            if game.npcs[i].is_some() {
                game.poses[i].asked = Some(if matches!(how(i), Line::Won(_)) { Move::Win } else { Move::Lose });
            }
        }
        game.results = Some(Results { lines });
        game.stage = Stage::Results(0.0);
    }
}

fn name_of_seat(player: usize, npcs: &[Option<Npc>; 4], seat: usize) -> Name {
    if seat == player {
        return Name::Text(JASON);
    }
    match npcs[seat].as_ref().map(|n| n.name.clone()) {
        Some(Name::Seat(_)) | None => Name::Seat((seat + 4 - player) % 4 + 1),
        Some(n) => n,
    }
}

/// The five of a hand's cards that make it (the results box shows five).
fn best_five(cards: &[Card]) -> Vec<Card> {
    let n = cards.len();
    if n <= 5 {
        return cards.to_vec();
    }
    let mut best: Option<(holdem::HandValue, Vec<Card>)> = None;
    for skip_a in 0..n {
        for skip_b in skip_a + 1..n {
            let five: Vec<Card> = (0..n).filter(|&i| i != skip_a && i != skip_b).map(|i| cards[i]).take(5).collect();
            let v = holdem::evaluate(&five);
            if best.as_ref().is_none_or(|b| v > b.0) {
                best = Some((v, five));
            }
        }
    }
    best.map(|b| b.1).unwrap_or_default()
}

/// Between hands at a story's table: its next scene, or its end.
fn after_hand(game: &mut Game, host: &mut StoryHost) {
    if !game.story() {
        return;
    }
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

/// The movie's state from the game's.
fn hud_of(game: &Game, panel: Panel, keys: &ButtonInput<KeyCode>) -> PokerHud {
    let seat = game.player_seat;
    let order = |hud: usize| (seat + hud) % 4;
    let table = game.table.as_ref();
    let seats = std::array::from_fn(|h| {
        let s = order(h);
        let present = s == seat || game.npcs[s].is_some();
        let name = if present { name_of_seat(seat, &game.npcs, s) } else { Name::Seat(h + 1) };
        let (chips, frame) = match table.map(|t| &t.seats[s]) {
            Some(st) if present && !st.out => (st.chips, if st.folded || st.hole.is_none() { 10 } else { 1 }),
            Some(_) if present => (0, 10),
            _ if present => (0, 1),
            _ => (0, 20),
        };
        (name, chips, frame)
    });
    let dealer = table.map_or(0, |t| (t.button + 4 - seat) % 4);
    let actions = table.filter(|t| t.to_act == Some(seat) && matches!(game.stage, Stage::Playing)).map(|t| {
        let (call, least, most) = t.options(seat);
        let bet = game.bet.clamp(t.seats[seat].bet + call, most);
        let label = if bet > t.current_bet {
            RAISE
        } else if call > 0 {
            CALL
        } else {
            CHECK
        };
        (label, bet, bet > t.seats[seat].bet + call, bet < most && least < most)
    });
    let cards = table.and_then(|t| {
        let s = &t.seats[seat];
        let hole = s.hole?;
        let mut all: Vec<Card> = hole.to_vec();
        all.extend(&t.board);
        let text = all.iter().map(|c| card_text(*c)).collect::<Vec<_>>().join(" ");
        Some((text, s.folded, hand_name(holdem::evaluate(&all).category), !t.board.is_empty()))
    });
    PokerHud {
        panel,
        seats,
        dealer,
        pot: table.map_or(0, |t| t.pot()),
        last_bet: game.last_bet,
        chips: table.map_or(0, |t| t.seats[seat].chips),
        info: game.info.clone(),
        actions,
        cards,
        totals: keys.pressed(KeyCode::Digit1),
    }
}

/// At the table: no walking, jumping, shooting or other weapons; seated
/// (crouched).
fn hold_controls(poker: Res<PokerTable>, mut actions: ResMut<ActionState>) {
    if !poker.holds_player() {
        return;
    }
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
    ] {
        actions.suppress(x);
    }
    actions.hold(Action::Crouch);
}

/// The player kept at their seat.
fn hold_seat(poker: Res<PokerTable>, mut players: Query<(&mut Transform, &mut Player)>) {
    let Some(game) = poker.game.as_ref().filter(|g| g.seated) else { return };
    let Some(place) = game.place else { return };
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    let chair = place.chairs[game.player_seat];
    let away = (chair - place.centre).with_y(0.0).normalize_or_zero();
    let at = chair + away * SEAT_BACK;
    t.translation.x = at.x;
    t.translation.z = at.z;
    p.velocity = Vec3::ZERO;
}

/// Where the table's props come from.
const MISC: &str = r"graphics\__fc3_graphics\singleplayer\props\misc\";
/// The cards' materials, drawn in their own colours (`DiffuseColor1`) on
/// the standard shaders: the shader the converter pairs them all with draws
/// the face (`PokerCardsWhite01`, whose worn white is a second, masked
/// layer the pair leaves out) with the atlas's court figures, colours the
/// pips and letters (`PokerCardsBlack01`, `PokerCards01`) from elsewhere in
/// the atlas, and the back (`PokerCards01_NoBlend`) only from some sides.
/// Each: its colour, whether it takes the atlas, and whether the atlas's
/// alpha cuts it out.
const CARD_MATERIALS: [(&str, [f32; 3], bool, bool); 4] = [
    (r"graphics\_materials\GJOLIAT-M-2011091433050575.material.bin", [0.85, 0.84, 0.82], false, false),
    (r"graphics\_materials\GJOLIAT-M-2011070439831268.material.bin", [0.204, 0.204, 0.204], true, true),
    (r"graphics\_materials\GJOLIAT-M-2011062958935948.material.bin", [0.557, 0.176, 0.176], true, true),
    (r"graphics\_materials\GJOLIAT-M-2011062962324879.material.bin", [0.51, 0.459, 0.388], true, false),
];
/// A chip's thickness, and the most of them in a column (m, count).
const CHIP: f32 = 0.0031;
const COLUMN: usize = 8;

/// A prop on the table: a card, a chip of a seat's stack or of what it has
/// bet this round, a pile of the pot, or the deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PropKey {
    Card(u8),
    Stack(usize, usize),
    Bet(usize, usize),
    Pot(usize),
    Deck,
}

#[derive(Component)]
struct TableProp(PropKey);

/// A card's model (`pokercard` + suit + rank).
fn card_model(c: Card) -> String {
    let text = card_text(c).to_ascii_lowercase();
    let (rank, suit) = text.split_at(1);
    format!("{MISC}pokercard{suit}{rank}.xbg")
}

/// What lies on the table: each prop, its model and where (Bevy space). The
/// board's cards face the player across the middle; each seat has its
/// cards before it (the computer players' face down until shown), its
/// chips beside them in columns of three colours (a chip a twentieth of the
/// stake), its bet this round nearer the middle; the pot lies in piles
/// beyond the board, the deck by the button.
fn table_props(place: &Place, table: &Table, player: usize, stake: u32) -> Vec<(PropKey, String, Transform)> {
    let top = place.centre.y + 0.001;
    let centre = place.centre.with_y(top);
    let toward = |s: usize| {
        let d = (place.chairs[s] - place.centre).with_y(0.0);
        if d.length() > 0.2 {
            d.normalize()
        } else {
            Quat::from_rotation_y(s as f32 * std::f32::consts::FRAC_PI_2) * Vec3::Z
        }
    };
    let reach = |s: usize| (place.chairs[s] - place.centre).with_y(0.0).length().clamp(0.5, 1.2);
    // (Facing a seat: a card's long side, `+Z`, toward it.)
    let facing = |d: Vec3| Quat::from_rotation_y(d.x.atan2(d.z));
    let face_down = Quat::from_rotation_z(std::f32::consts::PI);
    let unit = (stake / 20).max(1);
    let mut out = Vec::new();
    let me = toward(player);
    let my_side = Vec3::Y.cross(me);
    for (i, &c) in table.board.iter().enumerate() {
        let at = centre + my_side * (i as f32 - 2.0) * 0.09;
        out.push((PropKey::Card(c.0), card_model(c), Transform::from_translation(at).with_rotation(facing(me))));
    }
    let shown = |s: usize| table.outcome.as_ref().is_some_and(|o| o.shown.get(s).copied().flatten().is_some());
    for (s, seat) in table.seats.iter().enumerate() {
        let d = toward(s);
        let side = Vec3::Y.cross(d);
        let near = (reach(s) * 0.55).min(0.42);
        if let Some(hole) = seat.hole.filter(|_| !seat.folded) {
            let up = s == player || shown(s);
            for (k, c) in hole.into_iter().enumerate() {
                let sign = if k == 0 { -1.0 } else { 1.0 };
                let at = centre + d * near + side * sign * 0.045 + Vec3::Y * k as f32 * 0.002;
                let turn = facing(d) * Quat::from_rotation_y(-sign * 0.08);
                out.push((PropKey::Card(c.0), card_model(c), Transform::from_translation(at).with_rotation(if up { turn } else { turn * face_down })));
            }
        }
        if seat.out && seat.chips == 0 {
            continue;
        }
        let colours = ["pokerchipred01", "pokerchipgreen01", "pokerchipblue01"];
        let chips = (seat.chips.div_ceil(unit) as usize).min(COLUMN * 6);
        for i in 0..chips {
            let (col, up) = (i / COLUMN, i % COLUMN);
            let at = centre + d * (near + 0.03 + (col / 3) as f32 * 0.045) + side * (0.14 + (col % 3) as f32 * 0.045) + Vec3::Y * CHIP * (up + 1) as f32;
            out.push((
                PropKey::Stack(s, i),
                format!("{MISC}{}.xbg", colours[col % 3]),
                Transform::from_translation(at).with_rotation(Quat::from_rotation_y(i as f32 * 1.3)),
            ));
        }
        let bet = (seat.bet.div_ceil(unit) as usize).min(COLUMN);
        for i in 0..bet {
            let at = centre + d * (near - 0.15) + side * 0.07 + Vec3::Y * CHIP * (i + 1) as f32;
            out.push((
                PropKey::Bet(s, i),
                format!("{MISC}{}.xbg", colours[i % 3]),
                Transform::from_translation(at).with_rotation(Quat::from_rotation_y(i as f32 * 1.3)),
            ));
        }
    }
    let bets: u32 = table.seats.iter().map(|s| s.bet).sum();
    let piles = (table.pot().saturating_sub(bets).div_ceil(unit * 4) as usize).min(7);
    for i in 0..piles {
        let ring = if i == 0 { Vec3::ZERO } else { Quat::from_rotation_y(i as f32 * std::f32::consts::TAU / 6.0) * Vec3::X * 0.045 };
        let at = centre - me * 0.17 + ring;
        out.push((
            PropKey::Pot(i),
            format!("{MISC}pokerchippiles_01.xbg"),
            Transform::from_translation(at).with_rotation(Quat::from_rotation_y(i as f32 * 0.9)),
        ));
    }
    let d = toward(table.button);
    let at = centre + d * (reach(table.button) * 0.55).min(0.42) * 0.55 - Vec3::Y.cross(d) * 0.16;
    out.push((PropKey::Deck, format!("{MISC}pokercarddecksmall01.xbg"), Transform::from_translation(at).with_rotation(facing(d))));
    out
}

/// The cards and chips on the table while the player sits at it, set out
/// as its game goes (each prop once, moved where it goes next).
fn set_table(
    poker: Res<PokerTable>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut handles: Local<HashMap<String, Handle<Fc3Model>>>,
    mut fixed: Local<HashMap<&'static str, Handle<StandardMaterial>>>,
    mut props: Query<(Entity, &TableProp, &mut Transform)>,
    mut commands: Commands,
) {
    let want = poker
        .game
        .as_ref()
        .filter(|g| g.seated)
        .and_then(|g| Some(table_props(g.place.as_ref()?, g.table.as_ref()?, g.player_seat, g.stake)))
        .unwrap_or_default();
    if want.is_empty() && props.is_empty() {
        return;
    }
    let wanted: HashMap<PropKey, &Transform> = want.iter().map(|(k, _, t)| (*k, t)).collect();
    let mut have = std::collections::HashSet::new();
    for (e, prop, mut t) in &mut props {
        match wanted.get(&prop.0) {
            Some(to) => {
                if *t != **to {
                    *t = **to;
                }
                have.insert(prop.0);
            }
            None => commands.entity(e).despawn(),
        }
    }
    for (key, model, at) in &want {
        if have.contains(key) {
            continue;
        }
        let handle = handles.entry(model.clone()).or_insert_with(|| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::model_rel(model))));
        if !crate::cache_assets::done(&server, handle.id()) {
            continue;
        }
        let Some(lod) = models.get(&*handle).and_then(|m| m.lods.first()) else { continue };
        // Each part's material, the cards' own in their colours (once the
        // atlas the textured ones take is known); all on the standard
        // shaders.
        let mut parts = Vec::new();
        for part in &lod.parts {
            let path = server.get_path(&part.material);
            let fix = CARD_MATERIALS.iter().find(|(m, ..)| path.as_ref().is_some_and(|p| p.path().ends_with(fc3_formats::cache::material_rel(m))));
            let Some(&(name, [r, g, b], textured, cut)) = fix else {
                parts.push((part.mesh.clone(), part.material.clone()));
                continue;
            };
            if !fixed.contains_key(name) {
                let texture = match materials.get(&part.material) {
                    Some(m) if textured => m.base_color_texture.clone(),
                    None if textured => break,
                    _ => None,
                };
                let material = StandardMaterial {
                    base_color: Color::srgb(r, g, b),
                    base_color_texture: texture,
                    alpha_mode: if cut { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
                    perceptual_roughness: 0.7,
                    // (The face is two-sided: the border round the back.)
                    double_sided: !textured,
                    cull_mode: if textured { Some(bevy::render::render_resource::Face::Back) } else { None },
                    ..default()
                };
                fixed.insert(name, materials.add(material));
            }
            parts.push((part.mesh.clone(), fixed[name].clone()));
        }
        if parts.len() < lod.parts.len() {
            continue;
        }
        let root = commands.spawn((*at, Visibility::Visible, TableProp(*key))).id();
        for (mesh, material) in parts {
            commands.spawn((Mesh3d(mesh), MeshMaterial3d(material), NotShadowCaster, crate::tattoo::NoOriginal, ChildOf(root)));
        }
    }
}

/// The computer players seated at their chairs: a bar's while its table's
/// game is on (waiting for the player without cards), a story's once the
/// player sits with them. Each is in a scene of its own (`cinema`'s built
/// ones) at its chair's anchor, facing as the anchor does, seated with its
/// cards or without, and making its moves once as the game asks (then
/// seated again). Not while a story's scene has them; their scenes end with
/// the table's game.
fn pose_players(
    mut poker: ResMut<PokerTable>,
    mut cinema: ResMut<crate::cinema::Cinema>,
    server: Res<AssetServer>,
    clips: Res<Assets<crate::cache_assets::Fc3Clip>>,
    mut handles: Local<HashMap<String, Handle<crate::cache_assets::Fc3Clip>>>,
    mut live: Local<Vec<String>>,
) {
    use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};
    let game = poker.game.as_mut();
    let posing = game.as_ref().is_some_and(|g| (g.seated || !g.story()) && !g.done && g.scene.is_none()) && cinema.view.is_none();
    // (Scenes no seat holds any longer end.)
    let held: Vec<String> = game.as_ref().map(|g| g.poses.iter().filter_map(|p| p.playing.as_ref().map(|(n, _)| n.clone())).collect()).unwrap_or_default();
    for name in live.iter().filter(|n| !held.contains(n) || !posing) {
        cinema.stop_built(name);
    }
    live.retain(|n| held.contains(n) && posing);
    let Some(game) = game else { return };
    for s in 0..4 {
        let pose = &mut game.poses[s];
        let (Some(npc), Some(place), true) = (game.npcs[s].as_ref(), game.place, posing) else {
            pose.playing = None;
            pose.asked = None;
            continue;
        };
        let holding = game.table.as_ref().is_some_and(|t| t.seats[s].hole.is_some() && !t.seats[s].folded);
        let idle = if holding { Move::IdleCards } else { Move::IdleEmpty };
        let busy = pose.playing.as_ref().is_some_and(|(name, m)| !m.looping() && cinema.progress(name).is_some());
        let want = match pose.asked {
            Some(m) => m,
            None if busy => continue,
            None if pose.playing.as_ref().is_some_and(|(name, m)| *m == idle && cinema.progress(name).is_some()) => continue,
            None => idle,
        };
        let path = move_clip(npc.moves, want);
        let handle = handles.entry(path.clone()).or_insert_with(|| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::anim_rel(&path))));
        if !crate::cache_assets::done(&server, handle.id()) {
            continue;
        }
        pose.asked = None;
        let Some(length) = clips.get(&*handle).map(|c| c.0.duration).filter(|l| *l > 0.0) else {
            warn!("poker: no clip {path}");
            continue;
        };
        let name = format!("poker_seat{s}_{}", pose.count);
        pose.count += 1;
        let anim =
            Clip { start: 0.0, duration: length, kind: ClipKind::Animation { source: path, from: 0, to: (length * 30.0) as u32, blend: 0.2, anchored: false } };
        let groups = vec![Group { uid: 1, kind: GroupKind::Character, name: "pokerplayer".into(), archetype: 0, clips: vec![anim] }];
        let origin = Transform::from_translation(place.chairs[s]).with_rotation(place.facing[s]);
        cinema.play_built(crate::cinema::Built {
            name: name.clone(),
            seq: Sequence { name: name.clone(), duration: length, looping: want.looping(), groups, ..Default::default() },
            origin,
            bindings: HashMap::from([(1, npc.entity)]),
        });
        debug!("poker: seat {} {want:?}", s + 1);
        live.push(name.clone());
        if let Some((old, _)) = pose.playing.replace((name, want)) {
            cinema.stop_built(&old);
            live.retain(|n| *n != old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The computer players' moves by way of playing, the expert's own
    /// names where they differ.
    #[test]
    fn move_clips_by_way_of_playing() {
        let dir = r"graphics\characters\_common\animations\exoticgameplay\minigames\poker\";
        assert_eq!(move_clip(0, Move::IdleCards), format!(r"{dir}unskilled\3rdge_fullst_poker_sitidlecards_unsk_+000fw_noweapon.mab"));
        assert_eq!(move_clip(2, Move::IdleCards), format!(r"{dir}expert\3rdge_fulbst_poker_sitidlwcards01_ex_+000fw_noweapon.mab"));
        assert_eq!(move_clip(2, Move::AllIn), format!(r"{dir}expert\3rdge_fulbst_poker_allin_ex_+000fw_noweapontake02.mab"));
        assert_eq!(move_clip(3, Move::Lose), format!(r"{dir}loosecanon\3rdge_fulbst_poker_loosehand_lc_+000fw_noweapon.mab"));
        assert_eq!(move_clip(1, Move::Impatient("rt")), format!(r"{dir}skilled\3rdge_fulbst_poker_impatientrt_sk_+000fw_noweapon.mab"));
    }

    /// The cards' models by suit and rank; after the deal the player's cards
    /// lie face up, the others' face down, the blinds' bets out, each prop
    /// once and all on the table.
    #[test]
    fn table_props_lay_out_the_deal() {
        assert_eq!(card_model(Card::new(10, 0)), format!("{MISC}pokercardct.xbg"));
        assert_eq!(card_model(Card::new(14, 3)), format!("{MISC}pokercardsa.xbg"));
        let chair = |x: f32, z: f32| Vec3::new(x, 0.45, z);
        let place = Place {
            centre: Vec3::new(0.0, 0.75, 0.0),
            chairs: [chair(0.0, 0.8), chair(0.8, 0.0), chair(0.0, -0.8), chair(-0.8, 0.0)],
            facing: [Quat::IDENTITY; 4],
        };
        let mut table = Table::new(&[1000; 4], 25, 50);
        table.deal(&mut Dealer::new(7));
        let props = table_props(&place, &table, 0, 1000);
        let keys: std::collections::HashSet<PropKey> = props.iter().map(|p| p.0).collect();
        assert_eq!(keys.len(), props.len());
        let mine = table.seats[0].hole.unwrap().map(|c| PropKey::Card(c.0));
        let cards: Vec<_> = props.iter().filter(|p| matches!(p.0, PropKey::Card(_))).collect();
        assert_eq!(cards.len(), 8);
        for (key, _, t) in cards {
            assert_eq!((t.rotation * Vec3::Y).y > 0.0, mine.contains(key), "{key:?}");
            // (The player's before them.)
            if mine.contains(key) {
                assert!(t.translation.z > 0.2);
            }
        }
        assert_eq!(props.iter().filter(|p| matches!(p.0, PropKey::Bet(..))).map(|p| p.0).collect::<std::collections::HashSet<_>>().len(), 2);
        assert!(props.iter().all(|p| p.2.translation.y >= 0.75 && p.2.translation.y < 0.8));
    }

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

    /// The tables' gameplays, not the resources their scripts preload.
    #[test]
    fn poker_gameplays_are_the_tables() {
        assert!(poker_gameplay("CPokerMiniGame_C3"));
        assert!(poker_gameplay("SQC_100_OBJ20_Pokerminigame"));
        assert!(!poker_gameplay("CPokerMiniGameResourceContainer"));
        assert!(!poker_gameplay("CShootingGallerySniper"));
    }

    /// The computer players take the seats they ask for (any free one when
    /// it's taken or they don't mind), the player the first left.
    #[test]
    fn the_players_take_their_seats() {
        // A bar's: seats 3, 1, 2 asked for; the player has the fourth.
        assert_eq!(seat_players(&[2, 0, 1]), ([Some(1), Some(2), Some(0), None], 3));
        // Hoyt's: the third seat and the fourth; the player the first.
        assert_eq!(seat_players(&[2, 3]), ([None, None, Some(0), Some(1)], 0));
        // Two wanting one seat, one not minding.
        assert_eq!(seat_players(&[1, 1, 4]), ([Some(1), Some(0), Some(2), None], 3));
    }

    /// The info box's lines name the player and the chips.
    #[test]
    fn lines_are_filled_in() {
        assert_eq!(fill("0  RAISED to 1  chips", "Sam", Some(40)), "Sam  RAISED to 40  chips");
        assert_eq!(fill("0 posts the SMALL BLIND (1  chips)", "Hoyt", Some(5)), "Hoyt posts the SMALL BLIND (5  chips)");
        assert_eq!(fill("0 FOLDED", "Jason", None), "Jason FOLDED");
    }

    #[test]
    fn cards_are_written_as_the_movie_reads_them() {
        assert_eq!(card_text(Card::new(14, 3)), "As");
        assert_eq!(card_text(Card::new(10, 2)), "Th");
        assert_eq!(card_text(Card::new(2, 0)), "2c");
    }
}
