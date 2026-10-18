//! The Trials of the Rakyat (the engine's "social challenges",
//! `fc3_formats::challenges`). Each trial has its stone in the world (a
//! rock with the challenge board on it, `CFCXChallengeBoardComponent`; the
//! stone has no script: the engine ran it) and its own mission block
//! (`<ID>`: its zone's `_intro` script and the `_main` layer's
//! reinforcement points, paths and props), off until the trial runs.
//!
//! As the engine did: the stone used opens its board (the pause menu's
//! challenge page: the trial's name, description, the player's best, the
//! scores to beat and the rewards); started, the player's gear is put
//! away, the block comes in and the player hears `CChallengeEvent` (0, the
//! trial), on which its scripts give the trial's weapons, put up its
//! dialog and start the clock (`StartChallengeTimer`); from then the kills
//! score (`challengescorerules`: by the enemy's kind, with headshots,
//! stealth and takedowns on top) until the clock runs out, the scripts
//! fail it (`NotifyChallengerFailure`: off the zone too long) or the player
//! dies. Then the block goes, the gear comes back, each reward reached for
//! the first time pays (money, XP, money) and the result page offers the
//! trial again. What the HUD shows (`h_challenges.feu`, `original_ui`) is
//! in `Trials::hud`, the pages' in `Trials::page`.

use bevy::prelude::*;
use fc3_formats::challenges::{bonus, Challenge, Manager, ScoreRule};
use fc3_story::{Arg, LayerState, PLAYER};

use crate::data::to_bevy;
use crate::player::Player;

pub struct TrialsPlugin;

impl Plugin for TrialsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Trials>()
            .add_systems(Update, run.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(Update, pause_page);
    }
}

/// The trials' tables: the trials, the score rules and the manager's
/// rewards and sounds.
struct Table {
    trials: Vec<Challenge>,
    rules: Vec<ScoreRule>,
    manager: Manager,
}

/// A trial's stone: its entity, where it is (Bevy), its layer, and its
/// trial (an index into the table).
#[derive(Clone, Copy)]
struct Stone {
    at: Vec3,
    layer: u64,
    trial: usize,
}

/// What the player chose on a trial's pages (`original_ui`): start it
/// from the board, leave the board, play it again or leave from the
/// result, and abort or go on from the pause page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Start,
    Leave,
    Again,
    Abort,
    Resume,
}

/// Which of the trial's pages is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Board,
    Result { success: bool },
    Pause,
}

/// A trial's page: the trial's name and description (string ids), the
/// player's best and (after a run) this run's score, the scores to beat
/// (the developers', and the player's own best among them), and the
/// rewards (their line, reached or not).
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub kind: PageKind,
    pub name: u32,
    pub description: u32,
    pub best: u32,
    pub score: u32,
    pub board: Vec<(String, u32, bool)>,
    pub rewards: Vec<(String, bool)>,
}

/// The HUD while a trial runs: the score, the next score to reach (the
/// next reward's), the seconds left, and a pulse when the score moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hud {
    pub score: u32,
    pub target: u32,
    pub left: f32,
}

/// A score popup: its kind (0 a kill, 1 a headshot, 2 stealth, 3 a
/// takedown, 4 time), its name (a string id) and what it adds.
#[derive(Clone, Debug, PartialEq)]
pub struct Popup {
    pub kind: usize,
    pub name: u32,
    pub value: String,
}

#[derive(Resource, Default)]
pub struct Trials {
    /// The trial natives the story's scripts called: (name, trial).
    pub natives: Vec<(String, u32)>,
    /// The pages' choices (`original_ui`).
    pub choices: Vec<Choice>,
    /// The page the pause menu should open (and its data), until it has.
    pub page: Option<Page>,
    pub page_wanted: Option<&'static str>,
    pub hud: Option<Hud>,
    /// Score popups not yet shown, and whether the score just moved.
    pub popups: Vec<Popup>,
    pub pulse: bool,
    table: Option<Option<Table>>,
    stones: Option<Vec<Stone>>,
    run: Option<Run>,
    /// The trial the last page was about (to play it again).
    last: Option<usize>,
    /// FC3_TRIAL_TEST: started once.
    tested: bool,
    /// The units' names (seconds, points), as shown.
    units: [String; 2],
    /// Seconds by a stone (tests).
    waited: f32,
    /// After a trial: where it was and how long what comes is still its.
    clearing: Option<(Vec3, f32)>,
}

impl Trials {
    /// A checkpoint reload: what runs ends (the reload restores the gear).
    pub fn reset(&mut self) {
        self.run = None;
        self.hud = None;
        self.popups.clear();
        self.natives.clear();
    }

    /// A trial runs: the story's autopilot leaves the player to it.
    pub fn holds_player(&self) -> bool {
        self.run.is_some()
    }

    /// The trial's pause page's legend: what kills score (the least and
    /// the most) and what a time bonus adds, with their units.
    pub fn pause_legend(&self) -> [String; 4] {
        let Some(Some(t)) = &self.table else { return Default::default() };
        let kills: Vec<i32> = t.rules.iter().filter(|r| !r.time && r.score > 0).map(|r| r.score).collect();
        let (lo, hi) = (kills.iter().min().copied().unwrap_or(0), kills.iter().max().copied().unwrap_or(0));
        let time = t.rules.iter().find(|r| r.time).map_or(0, |r| r.score);
        [format!("{lo}-{hi}"), self.units[1].clone(), format!("+{time}"), self.units[0].clone()]
    }
}

/// The page the pause menu opens while a trial runs: its own (abort or go
/// on).
fn pause_page(mut trials: ResMut<Trials>, state: Res<State<crate::menu::GameState>>, mut was: Local<bool>) {
    let paused = *state.get() == crate::menu::GameState::Paused;
    if paused && !*was && trials.page_wanted.is_none() && trials.run.as_ref().is_some_and(|r| r.phase == Phase::Running) {
        trials.page_wanted = Some(PAUSE_PAGE);
    }
    *was = paused;
}

/// The pause menu's pages: the board, the result and the trial's pause.
pub const BOARD_PAGE: &str = "6";
pub const RESULT_PAGE: &str = "7";
pub const PAUSE_PAGE: &str = "11";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    /// The block in, waiting for the scripts to start the clock.
    Starting,
    Running,
    /// Over; the result shows after a moment.
    Over {
        success: bool,
    },
}

/// The gear put away while a trial runs.
struct Gear {
    slots: Vec<crate::weapons::WeaponSlot>,
    current: usize,
    grenades: u32,
    molotovs: u32,
    c4: u32,
    mines: u32,
    arrows: [u32; 2],
    medkits: u32,
    armour: f32,
}

struct Run {
    trial: usize,
    block: u64,
    zone: Option<u64>,
    phase: Phase,
    /// Seconds in the phase, and since the trial's event was last sent.
    t: f32,
    told: Option<f32>,
    left: f32,
    score: u32,
    gear: Option<Gear>,
    /// The men brought in while it ran (gone with it).
    spawned: Vec<Entity>,
    /// The popups' turn within each kind's pair.
    turns: [usize; 5],
    /// The last whole seconds the clock's end beeped at.
    beeped: i32,
}

/// The stone's reach (m), and how near men brought in while a trial runs
/// are its own.
const REACH: f32 = 4.0;
const SPAWNED_NEAR: f32 = 800.0;
/// Seconds after a trial that what it called in is still its own (a
/// reinforcement on its way spawns late).
const CLEAR_AFTER: f32 = 6.0;

/// A string of the game's, as shown.
fn text(host: &crate::story::StoryHost, id: u32) -> String {
    host.story.data().strings.get(id).map(|s| crate::story::plain(s)).unwrap_or_default()
}

/// Seconds after the block comes in that the trial's event goes (its zone's
/// script listening by then), and the longest its scripts may take to start
/// the clock.
const TELL_AT: f32 = 1.0;
const START_WAIT: f32 = 30.0;
/// Seconds after the end before the result shows.
const RESULT_AFTER: f32 = 2.0;

fn load_table(root: &std::path::Path) -> anyhow::Result<Table> {
    use fc3_formats::challenges as c;
    Ok(Table {
        trials: c::parse_challenges(&std::fs::read(root.join(c::DATA_REL))?)?,
        rules: c::parse_rules(&std::fs::read(root.join(c::RULES_REL))?)?,
        manager: c::Manager::parse(&std::fs::read(root.join("story/templates.fcb"))?)?,
    })
}

/// A man's kind, as the score rules count kills (the game's classes by
/// our roles).
fn kind_of(role: crate::npc::Role) -> u32 {
    use crate::npc::Role::*;
    match role {
        Assault => bonus::ASSAULTER,
        Shotgunner => bonus::BERSERKER,
        Sniper => bonus::DEFENDER,
        Heavy | Flamer => bonus::HEAVY_GUNNER,
        Rocket => bonus::ROCKET,
        Molotov => bonus::MOLOTOV,
        Charger => bonus::BEHEADER,
    }
}

/// The rewards' lines on the pages: the score each needs and what it pays.
fn reward_lines(trial: &Challenge, manager: &Manager, unit: &str, got: u8) -> Vec<(String, bool)> {
    (0..3)
        .map(|i| {
            let pays = if i == 1 { format!("{} XP", manager.rewards[i]) } else { format!("${}", manager.rewards[i]) };
            (format!("{} {unit}: {pays}", trial.thresholds[i]), got & (1 << i) != 0)
        })
        .collect()
}

/// The scores to beat (the developers') with the player's best among
/// them, highest first.
fn board_rows(trial: &Challenge, player: &str, best: u32) -> Vec<(String, u32, bool)> {
    let mut rows: Vec<(String, u32, bool)> = trial.challengers.iter().map(|(n, s)| (n.clone(), *s as u32, false)).collect();
    rows.push((player.to_string(), best, true));
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    rows
}

#[allow(clippy::too_many_arguments)]
fn run(
    host: Option<NonSendMut<crate::story::StoryHost>>,
    mut trials: ResMut<Trials>,
    time: Res<Time>,
    info: Option<Res<crate::CacheInfo>>,
    (mut actions, settings, original): (ResMut<crate::controls::ActionState>, Res<crate::settings::Settings>, Res<crate::hud::OriginalHud>),
    (mut progress, mut hud, mut next): (ResMut<crate::progression::Progress>, ResMut<crate::hud::HudState>, ResMut<NextState<crate::menu::GameState>>),
    mut players: Query<(Entity, &Transform, &mut crate::weapons::Inventory, &mut crate::combat::PlayerStats, &crate::combat::Health), With<Player>>,
    (npcs, added): (Query<&crate::npc::Npc>, Query<(Entity, &GlobalTransform), Or<(Added<crate::npc::Npc>, Added<crate::vehicles::Vehicle>)>>),
    (mut kills, mut died): (MessageReader<crate::progression::PlayerKilled>, MessageReader<crate::combat::Died>),
    (mut xp, mut sfx): (MessageWriter<crate::progression::XpGain>, MessageWriter<crate::sound::PlaySfx>),
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    let trials = &mut *trials;
    if trials.table.is_none() {
        let table = info.as_ref().map(|i| load_table(&i.root));
        trials.table = Some(match table {
            Some(Ok(t)) => Some(t),
            Some(Err(e)) => {
                warn!("trials: not played: {e:#}");
                None
            }
            None => return,
        });
    }
    let Some(Some(table)) = trials.table.as_ref() else { return };
    // The stones (once): each board's trial by the block its name begins
    // with (`D4_SC_06_Interaction/ChallengeBoard`: the trial `D4_SC_06`).
    if trials.stones.is_none() {
        let data = host.story.data();
        let stones: Vec<Stone> = data
            .entities
            .values()
            .filter(|e| e.component("CFCXChallengeBoardComponent").is_some())
            .filter_map(|e| {
                let block: String = e.name.split('_').take(3).collect::<Vec<_>>().join("_");
                let trial = table.trials.iter().position(|t| t.block.eq_ignore_ascii_case(&block))?;
                Some(Stone { at: to_bevy(e.pos), layer: e.layer, trial })
            })
            .collect();
        info!("trials: {} stones", stones.len());
        trials.stones = Some(stones);
    }
    let dt = time.delta_secs();
    let Ok((me, tf, mut inv, mut stats, health)) = players.single_mut() else { return };
    let at = tf.translation;
    let unit = text(&host, table.manager.units[1]);
    if trials.units[1].is_empty() {
        trials.units = [text(&host, table.manager.units[0]), unit.clone()];
    }
    let entry = |p: &crate::progression::Progress, t: &Challenge| p.trials.get(&t.id).copied().unwrap_or((0, 0));
    // The pages' choices.
    for choice in std::mem::take(&mut trials.choices) {
        match choice {
            Choice::Start | Choice::Again if trials.run.is_none() => {
                if let Some(i) = trials.last {
                    trials.run = start(&mut host, table, i, &mut progress, &mut inv, &mut stats);
                }
            }
            Choice::Abort => {
                if let Some(r) = trials.run.as_mut() {
                    info!("trials: {} given up", table.trials[r.trial].block);
                    r.phase = Phase::Over { success: false };
                    r.t = RESULT_AFTER;
                }
            }
            _ => {}
        }
    }
    // FC3_TRIAL_TEST=<block> (tests): that trial started at once, wherever
    // the player is (its boxes accepted: `story_boxes`).
    if !trials.tested && trials.run.is_none() {
        if let Ok(want) = std::env::var("FC3_TRIAL_TEST") {
            trials.tested = true;
            if let Some(i) = table.trials.iter().position(|t| t.block.eq_ignore_ascii_case(&want)) {
                trials.last = Some(i);
                trials.run = start(&mut host, table, i, &mut progress, &mut inv, &mut stats);
            }
        }
    }
    let Some(r) = trials.run.as_mut() else {
        trials.hud = None;
        // (What a trial called in that comes after it: gone too.)
        if let Some((near, until)) = trials.clearing {
            for (e, t) in &added {
                if t.translation().distance(near) < SPAWNED_NEAR {
                    commands.entity(e).despawn();
                }
            }
            trials.clearing = (until > dt).then_some((near, until - dt));
        }
        // A stone within reach: its trial's board.
        let stones = trials.stones.as_deref().unwrap_or(&[]);
        let near = stones
            .iter()
            .filter(|s| s.layer == fc3_formats::world::DEFAULT_MISSION_LAYER || host.story.layer_state(s.layer).present())
            .filter(|s| s.at.distance(at) < REACH)
            .min_by(|a, b| a.at.distance(at).total_cmp(&b.at.distance(at)))
            .copied();
        if let Some(stone) = near.filter(|_| hud.prompt.is_none()) {
            let trial = &table.trials[stone.trial];
            hud.prompt = Some(format!("{}  {}", actions.label(crate::controls::Action::Interact, &settings), text(&host, trial.name)));
            // FC3_TRIAL_USE (tests): the stone used once the player is by it
            // (and the interface is up).
            trials.waited += dt;
            let test_use = !trials.tested && std::env::var("FC3_TRIAL_USE").is_ok() && trials.waited > 3.0 && (original.0 || trials.waited > 10.0);
            if actions.just_pressed(crate::controls::Action::Interact) || test_use {
                trials.tested = true;
                actions.consume(crate::controls::Action::Interact);
                trials.last = Some(stone.trial);
                if original.0 {
                    let (best, got) = entry(&progress, trial);
                    trials.page = Some(Page {
                        kind: PageKind::Board,
                        name: trial.name,
                        description: trial.description,
                        best,
                        score: 0,
                        board: board_rows(trial, "Jason", best),
                        rewards: reward_lines(trial, &table.manager, &unit, got),
                    });
                    trials.page_wanted = Some(BOARD_PAGE);
                    next.set(crate::menu::GameState::Paused);
                } else {
                    trials.run = start(&mut host, table, stone.trial, &mut progress, &mut inv, &mut stats);
                }
            }
        }
        return;
    };
    let trial = &table.trials[r.trial];
    r.t += dt;
    // (The men brought in about it: the trial's.)
    for (e, t) in &added {
        if t.translation().distance(at) < SPAWNED_NEAR {
            r.spawned.push(e);
        }
    }
    let natives = std::mem::take(&mut trials.natives);
    let called = |name: &str| natives.iter().any(|(n, id)| n == name && *id == trial.id);
    match r.phase {
        Phase::Starting => {
            // The trial's event (once: each hearing sets the trial up).
            if r.t >= TELL_AT && r.told.is_none() {
                debug!("trials: CChallengeEvent 0 {}", trial.id);
                host.story.entity_event(PLAYER, "CChallengeEvent", vec![Arg::Num(0.0), Arg::Num(trial.id as f64)]);
                r.told = Some(r.t);
            }
            if called("StartChallengeTimer") {
                info!("trials: {} starts ({} s)", trial.block, trial.duration);
                r.phase = Phase::Running;
                r.t = 0.0;
                r.left = trial.duration;
            } else if r.t > START_WAIT {
                warn!("trials: {}: its scripts never started the clock", trial.block);
                r.phase = Phase::Over { success: false };
                r.t = 0.0;
            }
        }
        Phase::Running => {
            // FC3_TRIAL_SHOTS=<dir> (tests): the trial captured as it runs.
            if let Ok(dir) = std::env::var("FC3_TRIAL_SHOTS") {
                for at in [20.0, 60.0] {
                    if r.t >= at && r.t - dt < at {
                        crate::camera::capture(&mut commands, format!("{dir}/running_{}.png", at as u32));
                    }
                }
            }
            r.left -= dt;
            // The kills, by kind and how.
            for k in kills.read() {
                let Ok(npc) = npcs.get(k.victim) else { continue };
                let rule = |bit: u32| table.rules.iter().find(|x| x.bonus == bit && !x.time);
                let mut scored = Vec::new();
                if let Some(rule) = rule(kind_of(npc.role)) {
                    scored.push((0, rule));
                }
                for (on, bit, kind) in [(k.headshot, bonus::HEADSHOT, 1), (k.unaware && !k.takedown, bonus::STEALTH, 2), (k.takedown, bonus::TAKEDOWN, 3)] {
                    if let Some(rule) = rule(bit).filter(|_| on) {
                        scored.push((kind, rule));
                    }
                }
                for (kind, rule) in scored {
                    r.score += rule.score.max(0) as u32;
                    trials.popups.push(Popup { kind, name: rule.name, value: format!("+{}", rule.score) });
                    trials.pulse = true;
                }
                sfx.write(crate::sound::PlaySfx { event: table.manager.sounds.kill, at: None, volume: 1.0 });
            }
            // The clock's last ten seconds beep.
            let whole = r.left.ceil() as i32;
            if (1..=10).contains(&whole) && whole != r.beeped {
                r.beeped = whole;
                sfx.write(crate::sound::PlaySfx { event: table.manager.sounds.timer_ending, at: None, volume: 1.0 });
            }
            let dead = health.dead || died.read().any(|d| d.entity == me);
            if called("NotifyChallengerFailure") || dead {
                info!("trials: {} failed (score {})", trial.block, r.score);
                r.phase = Phase::Over { success: false };
                r.t = 0.0;
            } else if r.left <= 0.0 {
                let success = r.score >= trial.thresholds[0];
                info!("trials: {} over: score {} ({})", trial.block, r.score, if success { "done" } else { "not enough" });
                r.phase = Phase::Over { success };
                r.t = 0.0;
            }
        }
        Phase::Over { .. } => {}
    }
    let target = trial.thresholds.iter().copied().find(|&t| t > r.score).unwrap_or(*trial.thresholds.last().unwrap_or(&0));
    trials.hud = (r.phase == Phase::Running).then_some(Hud { score: r.score, target, left: r.left.max(0.0) });
    let Phase::Over { success } = r.phase else { return };
    if r.t < RESULT_AFTER {
        return;
    }
    // Over: the block away, the men it brought gone, the gear back, the
    // rewards reached paid (each once), the best kept, and the result.
    let r = trials.run.take().unwrap();
    host.story.set_block_state(r.block, LayerState::Disabled);
    if let Some(zone) = r.zone {
        host.story.hold_script(zone, false);
    }
    for e in r.spawned {
        if let Ok(mut c) = commands.get_entity(e) {
            c.despawn();
        }
    }
    trials.clearing = Some((at, CLEAR_AFTER));
    if let Some(g) = r.gear {
        inv.slots = g.slots;
        inv.current = g.current.min(inv.slots.len().saturating_sub(1));
        inv.mounted_from = None;
        (inv.grenades, inv.molotovs, inv.c4, inv.mines, inv.special_arrows) = (g.grenades, g.molotovs, g.c4, g.mines, g.arrows);
        crate::weapons::settle_unarmed(&mut inv);
        stats.medkits = g.medkits;
        progress.armour = g.armour;
    }
    let (best, mut got) = entry(&progress, trial);
    for (i, &needed) in trial.thresholds.iter().enumerate() {
        if r.score >= needed && got & (1 << i) == 0 {
            got |= 1 << i;
            let pays = table.manager.rewards[i];
            if i == 1 {
                xp.write(crate::progression::XpGain { amount: pays, reason: "Trial reward" });
            } else {
                progress.add_money(pays);
            }
            info!("trials: {} reward {i} ({pays})", trial.block);
        }
    }
    let best = best.max(r.score);
    progress.trials.insert(trial.id, (best, got));
    let played = progress.trials.len() as u32;
    crate::statistics::at_least(&mut progress, crate::statistics::stat::TRIALS_PLAYED, played);
    sfx.write(crate::sound::PlaySfx { event: if success { table.manager.sounds.success } else { table.manager.sounds.failed }, at: None, volume: 1.0 });
    trials.hud = None;
    trials.popups.clear();
    trials.last = Some(r.trial);
    if original.0 && std::env::var("FC3_TRIAL_TEST").is_err() {
        trials.page = Some(Page {
            kind: PageKind::Result { success },
            name: trial.name,
            description: trial.description,
            best,
            score: r.score,
            board: board_rows(trial, "Jason", best),
            rewards: reward_lines(trial, &table.manager, &unit, got),
        });
        trials.page_wanted = Some(RESULT_PAGE);
        next.set(crate::menu::GameState::Paused);
    } else {
        hud.notify(format!("{}: {} {unit}", text(&host, trial.name), r.score));
    }
}

/// A trial begun: the player's gear put away, its block brought in (its
/// zone's script kept running however far it goes).
fn start(
    host: &mut crate::story::StoryHost,
    table: &Table,
    i: usize,
    progress: &mut crate::progression::Progress,
    inv: &mut crate::weapons::Inventory,
    stats: &mut crate::combat::PlayerStats,
) -> Option<Run> {
    let trial = &table.trials[i];
    let Some((block, zone)) = host.story.block_named(&trial.block) else {
        warn!("trials: no block {}", trial.block);
        return None;
    };
    info!("trials: {} ({}) begins", trial.block, trial.id);
    let gear = Gear {
        slots: std::mem::take(&mut inv.slots),
        current: inv.current,
        grenades: std::mem::take(&mut inv.grenades),
        molotovs: std::mem::take(&mut inv.molotovs),
        c4: std::mem::take(&mut inv.c4),
        mines: std::mem::take(&mut inv.mines),
        arrows: std::mem::take(&mut inv.special_arrows),
        medkits: std::mem::take(&mut stats.medkits),
        armour: std::mem::take(&mut progress.armour),
    };
    inv.current = 0;
    inv.mounted_from = None;
    crate::weapons::settle_unarmed(inv);
    progress.trials.entry(trial.id).or_insert((0, 0));
    if let Some(zone) = zone {
        host.story.hold_script(zone, true);
    }
    host.story.set_block_state(block, LayerState::Active);
    Some(Run {
        trial: i,
        block,
        zone,
        phase: Phase::Starting,
        t: 0.0,
        told: None,
        left: trial.duration,
        score: 0,
        gear: Some(gear),
        spawned: Vec::new(),
        turns: [0; 5],
        beeped: 0,
    })
}

/// The popup slot a score shows in (`ChallengeUpdateScore_<n>`): each kind
/// has two, taken in turn.
pub fn popup_slot(trials: &mut Trials, kind: usize) -> usize {
    let turn = trials.run.as_mut().map_or(0, |r| {
        let t = r.turns[kind.min(4)];
        r.turns[kind.min(4)] = t + 1;
        t
    });
    kind.min(4) * 2 + turn % 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_ranks_the_player_among_the_challengers() {
        let trial = Challenge { challengers: vec![("A".into(), 110.0), ("B".into(), 215.0)], thresholds: [150, 225, 300], ..Default::default() };
        let rows = board_rows(&trial, "Jason", 160);
        assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), vec![215, 160, 110]);
        assert!(rows[1].2);
        let manager = Manager { rewards: [250, 400, 500], ..Default::default() };
        let lines = reward_lines(&trial, &manager, "Pts", 0b001);
        assert_eq!(lines[0], ("150 Pts: $250".to_string(), true));
        assert_eq!(lines[1], ("225 Pts: 400 XP".to_string(), false));
    }
}
