//! The races (`CRacingMiniGame_*`, every one the engine's `CDrivingMiniGame`,
//! `gsf::RaceSettings`). A race's keeper (`racechallenge.main.lua`), used,
//! starts its gameplay and hears how it went from the message it broadcasts
//! (`driving_mini_game_finished`, `_failed`, `_quit`, `_cancelled`), then
//! stops it; meanwhile the keeper's entice system calls the player over.
//!
//! The race plays as the engine played it: a wager chosen (the row's wager
//! and target time; practice is free), the track's base and row layers
//! brought in (its gates, which register themselves with the player,
//! `GateRegisterEvent`, and its vehicles' spawners, whose vehicles are made
//! here), the player put at the start beside the first vehicle, a 4.5 s
//! countdown, then the clock: the target time running down, red and
//! beeping in its last ten seconds. Only the next gate counts, passed when
//! the player (or the vehicle he drives) crosses the line between its two
//! flares; the finish passed in time wins the wager back multiplied, the
//! clock out loses it. Escape asks first (the quit box: the wager lost).
//! What the HUD shows (`acti_driving.feu`, `original_ui`) is kept in
//! `Races::hud`, the next gate in `Races::marker`.

use std::collections::HashMap;

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::gsf::{RaceResources, RaceSettings};

use crate::cache_assets::{cache_path, Fc3Model};
use crate::data::{to_bevy, to_dunia};
use crate::player::{Player, HALF_HEIGHT};

pub struct RacesPlugin;

impl Plugin for RacesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Races>()
            .add_systems(Update, (run, entice, dress_course).chain().after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A race the story starts.
pub struct Start {
    pub id: u64,
    pub name: String,
    pub params: HashMap<String, String>,
}

/// A gate registering itself with the player (`GateRegisterEvent`): its
/// entity, its place in the course and the tolerance (m).
#[derive(Clone, Debug)]
pub struct GateRegistration {
    pub id: u64,
    pub index: u32,
    pub tolerance: f32,
}

/// A keeper's entice system: the gameplay, the keeper and his dialog set.
#[derive(Clone, Copy, Debug)]
pub struct Entice {
    pub id: u64,
    pub keeper: u64,
    pub set: usize,
}

/// The races the story asks for, the gates registered, the keepers
/// enticing, and what the HUD shows.
#[derive(Resource, Default)]
pub struct Races {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    pub gates: Vec<GateRegistration>,
    pub entices: Vec<Entice>,
    pub hud: Option<RaceHud>,
    /// The mouse over a wager row, the confirm button and the cancel button.
    pub hover: (Option<usize>, bool, bool),
    /// The next gate (Bevy space), shown as an objective.
    pub marker: Option<Vec3>,
    run: Option<Run>,
}

impl Races {
    /// A checkpoint reload: what runs ends.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.entices.clear();
        if let Some(r) = &self.run {
            self.cancelled.push(r.id);
        }
    }

    /// A race runs: the story's autopilot leaves the player to it.
    pub fn holds_player(&self) -> bool {
        self.run.is_some()
    }

    /// Escape is the race's (the wager box, the quit box, the result).
    pub fn claims_escape(&self) -> bool {
        self.run.is_some()
    }
}

/// The HUD's state (`acti_driving.feu`'s boxes).
#[derive(Clone, Debug, PartialEq)]
pub enum RaceHud {
    /// The wager box: the row (0 practice, then the race's three), each
    /// row's wager and target time, whether the row costs more than the
    /// player has, and the subtitle (the track's name, a string id).
    Choosing {
        row: usize,
        wagers: [u32; 4],
        targets: [f32; 4],
        short: bool,
        subtitle: u32,
    },
    /// Getting ready, then the count (3, 2, 1), with the target time and
    /// the gates to come.
    Ready {
        target: f32,
        gates: u32,
    },
    Count {
        n: u32,
        target: f32,
        gates: u32,
    },
    /// The clock (seconds left, in its last ten seconds), the gates passed
    /// and expected, the start's "go" still up, and the quit box open.
    Racing {
        left: f32,
        hurry: bool,
        passed: u32,
        gates: u32,
        go: bool,
        quitting: bool,
    },
    /// How it went: won, the target time, the time left and the money won.
    Result {
        won: bool,
        target: f32,
        left: f32,
        money: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Choosing,
    Countdown,
    Racing,
    Result,
}

/// A gate of the course: its entity, its place, its line (the middle
/// between its flares in its own space, Dunia axes, and half its width),
/// the tolerance, and whether it's the finish.
#[derive(Clone, Debug)]
struct Gate {
    id: u64,
    index: u32,
    at: Transform,
    middle: Vec3,
    half: f32,
    tolerance: f32,
    model: Option<String>,
}

impl Gate {
    /// Where a point is from the gate's line, in its own space (Dunia: x
    /// along the line, y the way the course goes, z up).
    fn local(&self, p: Vec3) -> Vec3 {
        let l = self.at.rotation.inverse() * (p - self.at.translation);
        Vec3::new(l.x, -l.z, l.y) - self.middle
    }

    /// The gate's middle in the world.
    fn centre(&self) -> Vec3 {
        let m = self.middle;
        self.at.transform_point(Vec3::new(m.x, m.z, -m.y))
    }

    /// Crossed going from `from` to `to`: through the line between the
    /// flares (within the tolerance), from one side to the other, or onto
    /// it.
    fn crossed(&self, from: Vec3, to: Vec3) -> bool {
        let (a, b) = (self.local(from), self.local(to));
        let within = |l: Vec3| l.x.abs() <= self.half + self.tolerance + 0.5 && (-3.0..8.0).contains(&l.z);
        if b.y.abs() < 1.0 {
            return within(b);
        }
        if (a.y < 0.0) == (b.y < 0.0) {
            return false;
        }
        // Where the way from one to the other meets the line.
        within(a.lerp(b, a.y / (a.y - b.y)))
    }
}

/// A gate model's line: the middle between its flares (its own space,
/// Dunia axes) and half its width (from the models' `..._Bone01/02`: the
/// small land gate's, the large and water gates', the finish lines').
fn gate_line(model: &str) -> (Vec3, f32) {
    let m = model.to_ascii_lowercase();
    if m.contains("racing_sf_checkpoint") {
        (Vec3::new(0.005, -0.019, 1.273), 3.13)
    } else if m.contains("racing_mf_checkpoint") {
        (Vec3::new(0.0, -0.025, 1.95), 5.0)
    } else if m.contains("waterracing_finishline") {
        (Vec3::new(0.0, 0.0, 1.5), 5.0)
    } else if m.contains("racing_finishline") {
        (Vec3::new(0.016, 0.002, 1.338), 3.21)
    } else {
        (Vec3::new(0.0, 0.0, 1.3), 4.0)
    }
}

struct Run {
    id: u64,
    name: String,
    settings: RaceSettings,
    sounds: RaceResources,
    /// The messages for finished, failed, quit and cancelled.
    messages: [String; 4],
    spawn: Option<u64>,
    keeper: Option<u64>,
    /// The graph that started the race (its prefab), kept running while the
    /// race drives far from it.
    controller: Option<u64>,
    set: usize,
    phase: Phase,
    t: f32,
    row: usize,
    wager: u32,
    /// The layers brought in (ids), and the track's base layer's.
    layers: Vec<u64>,
    base: Option<u64>,
    /// The vehicles' spawners not yet made (spawner, archetype), and those
    /// made.
    to_make: Vec<(u64, u64)>,
    made: Vec<u64>,
    gates: Vec<Gate>,
    passed: u32,
    last: Option<Vec3>,
    /// The race's clock, stopped while the quit box is open.
    clock: f32,
    quitting: bool,
    won: bool,
    money: u32,
    told: bool,
    /// The phase last captured (`FC3_RACE_SHOTS`), the test's last drive.
    shot: Option<(Phase, u32)>,
    test_drive: f32,
}

/// The countdown's length, its count's (the last three seconds), how long
/// "go" shows, and the last stretch the clock goes red.
const COUNTDOWN: f32 = 4.5;
const COUNT: u32 = 3;
const GO: f32 = 2.0;
const HURRY: f32 = 10.5;
const RESULT_WAIT: f32 = 6.0;
const ROWS: usize = 4;
/// How near (m) a keeper calls the player over, and how often (s).
const ENTICE_REACH: f32 = 15.0;
const ENTICE_EVERY: [f32; 2] = [10.0, 20.0];

/// A race's gameplay (not its entice system or its resources).
pub fn race_gameplay(name: &str) -> bool {
    name.starts_with("CRacingMiniGame_")
}

/// A keeper's entice system.
pub fn entice_gameplay(name: &str) -> bool {
    name == "CRacingMiniGameEnticeSystem"
}

/// A marker on the vehicles a race made.
#[derive(Component)]
struct RaceVehicle(u64);

/// A gate's model, drawn while its race runs.
#[derive(Component)]
struct GateModel(u64);

/// The light of a gate's flares (the next gates lit).
#[derive(Component)]
struct FlareLight(u64);

/// A land gate's flares (its `FX` and `FX2` bones, its own space, Dunia
/// axes) and their colour (`waypointflare`'s violet, `finishflare`'s red);
/// the water gates have none.
fn flares(model: &str) -> (&'static [[f32; 3]], Vec3) {
    const VIOLET: Vec3 = Vec3::new(0.37, 0.24, 0.98);
    const RED: Vec3 = Vec3::new(1.0, 0.3, 0.12);
    let m = model.to_ascii_lowercase();
    if m.contains("waterracing") {
        (&[], VIOLET)
    } else if m.contains("racing_sf_checkpoint") {
        (&[[-2.385, -0.024, 0.097], [2.484, -0.024, 0.097]], VIOLET)
    } else if m.contains("racing_mf_checkpoint") {
        (&[[-4.14, -0.1, 0.087], [3.726, -0.052, 0.052]], VIOLET)
    } else if m.contains("racing_finishline") {
        (&[[-3.197, 0.002, 0.1], [3.229, 0.002, 0.1]], RED)
    } else {
        (&[], VIOLET)
    }
}

/// How many gates ahead burn their flares, and their sparks a second.
const LIT_AHEAD: usize = 2;
const SPARKS_PER_SECOND: f32 = 40.0;

/// An entity's place (Bevy space) and its yaw.
fn place_of(data: &fc3_story::data::StoryData, id: u64) -> Option<(Vec3, Quat)> {
    if let Some(e) = data.entities.get(&id) {
        return Some((to_bevy(e.pos), Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles))));
    }
    let sp = data.spawn_points.get(&id)?;
    Some((to_bevy(sp.pos), Quat::from_array(fc3_formats::world::angles_to_bevy_quat(sp.angles))))
}

/// A time as the race's boxes show it (`m:ss.s`).
pub fn race_time(seconds: f32) -> String {
    let s = seconds.max(0.0);
    let m = (s / 60.0) as u32;
    format!("{m}:{:04.1}", s - m as f32 * 60.0)
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut races: ResMut<Races>,
    mut host: Option<NonSendMut<crate::story::StoryHost>>,
    info: Option<Res<crate::CacheInfo>>,
    (server, models, assets, mut music, mut progress): (
        Res<AssetServer>,
        Res<Assets<Fc3Model>>,
        Option<Res<crate::vehicles::VehicleAssets>>,
        ResMut<crate::music::Music>,
        ResMut<crate::progression::Progress>,
    ),
    mut players: Query<(&mut Transform, &mut Player, &crate::combat::Health)>,
    (driving, mut seats, mut drive): (Res<crate::vehicles::Driving>, ResMut<crate::vehicles::StorySeatRequests>, ResMut<crate::vehicles::AutopilotDrive>),
    vehicles: Query<(Entity, &GlobalTransform, &crate::vehicles::Vehicle)>,
    made: Query<(Entity, &RaceVehicle)>,
    (keys, mouse, time, mut sfx, mut commands): (
        Res<ButtonInput<KeyCode>>,
        Res<ButtonInput<MouseButton>>,
        Res<Time>,
        MessageWriter<crate::sound::PlaySfx>,
        Commands,
    ),
    mut test_used: Local<bool>,
) {
    let Ok((mut t, mut p, health)) = players.single_mut() else { return };
    let races = &mut *races;
    let mut cancelled = std::mem::take(&mut races.cancelled);
    if health.dead {
        cancelled.extend(races.run.as_ref().map(|r| r.id));
    }
    races.entices.retain(|e| !cancelled.contains(&e.id));
    for start in std::mem::take(&mut races.pending) {
        if cancelled.contains(&start.id) {
            continue;
        }
        let read = || -> anyhow::Result<(RaceSettings, RaceResources)> {
            use anyhow::Context;
            let bytes = std::fs::read(info.as_ref().context("cache unavailable")?.root.join(fc3_formats::gsf::CACHE_REL))?;
            Ok((RaceSettings::parse(&bytes, &start.name)?, RaceResources::parse(&bytes)?))
        };
        let (settings, sounds) = match read() {
            Ok(s) => s,
            Err(e) => {
                warn!("race: {} not played: {e:#}", start.name);
                continue;
            }
        };
        let param = |k: &str| start.params.get(k).cloned().unwrap_or_default();
        let id = |k: &str| param(k).parse::<u64>().ok().filter(|&i| i != 0 && i != u64::MAX);
        info!("race ({}): {} ({} rows, track {})", start.id, start.name, settings.rows.len(), settings.base_layer);
        let controller = id("hidHiddenSettings/ControllerEntity");
        if let (Some(host), Some(c)) = (host.as_mut(), controller) {
            host.story.hold_script(c, true);
        }
        races.run = Some(Run {
            id: start.id,
            name: start.name.clone(),
            messages: ["DominoFinishMessage", "DominoFailMessage", "DominoQuitMessage", "DominoCancelMessage"]
                .map(|k| param(&format!("hidHiddenSettings/{k}"))),
            spawn: id("hidHiddenSettings/PlayerSpawnPoint"),
            keeper: id("hidHiddenSettings/GameMasterEntity"),
            controller,
            set: param("hidHiddenSettings/AIDialogIndex").parse().unwrap_or(0),
            settings,
            sounds,
            phase: Phase::Choosing,
            t: 0.0,
            row: 1,
            wager: 0,
            layers: Vec::new(),
            base: None,
            to_make: Vec::new(),
            made: Vec::new(),
            gates: Vec::new(),
            passed: 0,
            last: None,
            clock: 0.0,
            quitting: false,
            won: false,
            money: 0,
            told: false,
            shot: None,
            test_drive: 0.0,
        });
        races.hover = (None, false, false);
    }
    // Over (the keeper stopped it, a reload, death): the track out.
    if races.run.as_ref().is_some_and(|r| cancelled.contains(&r.id)) {
        let r = races.run.take().unwrap();
        info!("race ({}): {} over", r.id, r.name);
        if !r.sounds.music.is_empty() {
            music.request(&r.sounds.music, false);
        }
        if let Some(host) = host.as_mut() {
            for &l in &r.layers {
                host.story.set_layer_state(l, fc3_story::LayerState::Disabled);
            }
            if let Some(c) = r.controller {
                host.story.hold_script(c, false);
            }
        }
        for (e, v) in &made {
            if v.0 == r.id {
                commands.entity(e).despawn();
            }
        }
        races.gates.clear();
        races.hud = None;
        races.marker = None;
    }
    let Some(r) = races.run.as_mut() else {
        races.hud = None;
        races.marker = None;
        // FC3_RACE_TEST (tests): the nearest keeper used, once, as soon as
        // he can be.
        if let (Some(host), true) = (host.as_mut(), std::env::var_os("FC3_RACE_TEST").is_some() && !*test_used) {
            let here = to_dunia(t.translation);
            let usable = host.story.usable_pawns();
            let near = |p: &[f32; 3]| (p[0] - here[0]).hypot(p[1] - here[1]);
            if let Some((keeper, _, _)) = usable
                .iter()
                .filter(|(id, at, _)| races.entices.iter().any(|e| e.keeper == *id) && at.is_some_and(|a| near(&a) < 100.0))
                .min_by(|a, b| near(&a.1.unwrap_or_default()).total_cmp(&near(&b.1.unwrap_or_default())))
            {
                info!("race test: keeper {keeper} used");
                host.story.entity_event(*keeper, "Use", vec![fc3_story::Arg::id(fc3_story::PLAYER)]);
                *test_used = true;
            }
        }
        return;
    };
    let dt = time.delta_secs();
    r.t += dt;
    // FC3_RACE_TEST=<row> (tests): that row, the player put in the first
    // vehicle and carried from gate to gate.
    let test = std::env::var("FC3_RACE_TEST").ok().and_then(|s| s.parse::<usize>().ok());
    let say = |sfx: &mut MessageWriter<crate::sound::PlaySfx>, event: u32, at: Option<Vec3>| {
        if event != 0 {
            sfx.write(crate::sound::PlaySfx { event, at, volume: 1.0 });
        }
    };
    let keeper_at = r.keeper.and_then(|k| host.as_ref().and_then(|h| h.story.data().position(k))).map(to_bevy);
    let keeper_line = |r: &Run, kind: usize| -> u32 {
        let lines = r.sounds.keepers.get(r.set).or(r.sounds.keepers.first()).map(|k| &k[kind]);
        lines.filter(|l| !l.is_empty()).map_or(0, |l| l[(r.t * 997.0) as usize % l.len()])
    };
    let tell = |host: &mut Option<NonSendMut<crate::story::StoryHost>>, message: &str| {
        if let Some(host) = host.as_mut().filter(|_| !message.is_empty()) {
            info!("race: {message}");
            host.story.message(message);
        }
    };
    let rows: Vec<fc3_formats::gsf::RaceRow> = r.settings.rows.clone();
    // (Practice runs the first row's track and time, for nothing.)
    let row_of = |row: usize| rows.get(row.saturating_sub(1).min(rows.len().saturating_sub(1))).cloned().unwrap_or_default();
    let confirm = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::KeyE) || (mouse.just_pressed(MouseButton::Left) && races.hover.1);
    let cancel = keys.just_pressed(KeyCode::Escape) || (mouse.just_pressed(MouseButton::Left) && races.hover.2);
    let mut start_over = false;
    match r.phase {
        Phase::Choosing => {
            if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
                r.row = (r.row + ROWS - 1) % ROWS;
            }
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
                r.row = (r.row + 1) % ROWS;
            }
            if let Some(row) = races.hover.0 {
                r.row = row.min(ROWS - 1);
            }
            if let Some(row) = test {
                r.row = row.min(ROWS - 1);
            }
            let wagers = [0, row_of(1).wager, row_of(2).wager, row_of(3).wager];
            let targets = [row_of(1).target, row_of(1).target, row_of(2).target, row_of(3).target];
            let short = progress.money < wagers[r.row];
            races.hud = Some(RaceHud::Choosing { row: r.row, wagers, targets, short, subtitle: r.settings.subtitle });
            let accept = confirm || (mouse.just_pressed(MouseButton::Left) && races.hover.0.is_some()) || (test.is_some() && r.t > 1.0);
            if cancel && !r.told {
                r.told = true;
                tell(&mut host, &r.messages[3].clone());
            } else if accept && !short {
                r.wager = wagers[r.row];
                start_over = true;
                say(&mut sfx, r.sounds.player[0], None);
                if !r.sounds.music.is_empty() {
                    music.request(&r.sounds.music, true);
                }
                // The track in: its base layer and the row's.
                if let Some(host) = host.as_mut() {
                    let data = host.story.data();
                    let block = data.block_named(&r.settings.block).map(|b| b.id);
                    let row = row_of(r.row);
                    for name in [&r.settings.base_layer, &row.layer] {
                        if let Some(l) = data.layer_named(block, name).filter(|_| !name.is_empty()) {
                            if !r.layers.contains(&l.id) {
                                host.story.set_layer_state(l.id, fc3_story::LayerState::Active);
                                r.layers.push(l.id);
                            }
                        }
                    }
                    r.base = data.layer_named(block, &r.settings.base_layer).map(|l| l.id);
                }
                info!("race ({}): row {} (wager {}, target {} s), layers {:?}", r.id, r.row, r.wager, row_of(r.row).target, r.layers);
            }
        }
        Phase::Countdown => {
            // (The test is put in the first vehicle.)
            if test.is_some() && driving.vehicle.is_none() && r.t > 0.5 {
                let spawn = r.spawn.and_then(|s| host.as_ref().and_then(|h| place_of(&h.story.data(), s))).map(|s| s.0).unwrap_or(t.translation);
                if let Some((_, _, v)) = vehicles
                    .iter()
                    .filter(|(_, _, v)| r.made.contains(&v.story_id()))
                    .min_by(|a, b| a.1.translation().distance(spawn).total_cmp(&b.1.translation().distance(spawn)))
                {
                    seats.force_player(&[fc3_story::PLAYER.to_string(), "1".into(), "0".into(), v.story_id().to_string()]);
                }
            }
            let row = row_of(r.row);
            let (target, gates) = (row.target, row.gates.max(r.gates.len() as u32));
            if r.t < COUNTDOWN - COUNT as f32 {
                races.hud = Some(RaceHud::Ready { target, gates });
            } else {
                let n = (COUNTDOWN - r.t).ceil().max(1.0) as u32;
                if !matches!(races.hud, Some(RaceHud::Count { n: was, .. }) if was == n) {
                    say(&mut sfx, r.sounds.countdown, None);
                }
                races.hud = Some(RaceHud::Count { n, target, gates });
            }
            if r.t >= COUNTDOWN {
                r.phase = Phase::Racing;
                r.t = 0.0;
                r.clock = 0.0;
                say(&mut sfx, r.sounds.start, None);
                info!("race ({}): go ({} gates registered)", r.id, r.gates.len());
            }
        }
        Phase::Racing => {
            let row = row_of(r.row);
            // Escape asks first (the quit box; the clock stops meanwhile).
            if r.quitting {
                if confirm && !r.told {
                    r.told = true;
                    say(&mut sfx, keeper_line(r, 1), keeper_at);
                    info!("race ({}): quit", r.id);
                    tell(&mut host, &r.messages[2].clone());
                } else if cancel {
                    r.quitting = false;
                }
            } else if cancel && test.is_none() {
                r.quitting = true;
            } else {
                let before = row.target - r.clock;
                r.clock += dt;
                let left = row.target - r.clock;
                // The clock's last seconds: a beep each.
                if left < HURRY && left > 0.0 && before.ceil() != left.ceil() {
                    say(&mut sfx, r.sounds.out_of_time, None);
                }
            }
            // Where the player is (in the vehicle he drives, the vehicle).
            let here = driving.vehicle.and_then(|e| vehicles.get(e).ok()).map_or(t.translation, |(_, g, _)| g.translation());
            let from = r.last.replace(here).unwrap_or(here);
            let last_index = r.gates.iter().map(|g| g.index).max();
            let next = r.gates.iter().filter(|g| g.index >= r.passed).min_by_key(|g| g.index).cloned();
            if let Some(g) = next.as_ref().filter(|g| !r.quitting && g.crossed(from, here)) {
                r.passed = g.index + 1;
                let finish = Some(g.index) == last_index;
                say(&mut sfx, if finish { r.sounds.finish } else { r.sounds.gate }, Some(g.centre()));
                info!("race ({}): gate {} of {} at {:.1}s", r.id, g.index + 1, r.gates.len(), r.clock);
                if finish {
                    r.won = true;
                }
            }
            let left = (row.target - r.clock).max(0.0);
            if r.won || left <= 0.0 {
                r.money = if r.won { (r.wager as f32 * r.settings.winnings).round() as u32 } else { 0 };
                if r.money > 0 {
                    progress.add_money(r.money);
                }
                say(&mut sfx, if r.won { r.sounds.win } else { r.sounds.fail }, None);
                say(&mut sfx, r.sounds.player[if r.won { 2 } else { 1 }], None);
                say(&mut sfx, keeper_line(r, if r.won { 3 } else { 2 }), keeper_at);
                info!("race ({}): {} with {:.1}s left, {} won", r.id, if r.won { "won" } else { "lost" }, left, r.money);
                r.phase = Phase::Result;
                r.t = 0.0;
                r.quitting = false;
            }
            races.marker = next.as_ref().map(|g| g.centre());
            let gates = row.gates.max(r.gates.len() as u32);
            races.hud = Some(RaceHud::Racing { left, hurry: left < HURRY, passed: r.passed.min(gates), gates, go: r.t < GO, quitting: r.quitting });
            // The test: carried to the next gate every so often.
            if test.is_some() && driving.vehicle.is_some() && r.t - r.test_drive > 0.8 {
                if let Some(g) = next {
                    r.test_drive = r.t;
                    let ahead = g.at.rotation * Vec3::NEG_Z;
                    drive.0 = Some((g.centre() - Vec3::Y * g.middle.z, Some((-ahead.x).atan2(-ahead.z))));
                }
            }
        }
        Phase::Result => {
            races.marker = None;
            races.hud = Some(RaceHud::Result { won: r.won, target: row_of(r.row).target, left: (row_of(r.row).target - r.clock).max(0.0), money: r.money });
            // Again (the wager taken again), or leave: the keeper hears how
            // it went. (A test leaves after a while.)
            let short = progress.money < r.wager;
            if confirm && test.is_none() && !r.told && !short {
                start_over = true;
                info!("race ({}): again", r.id);
            } else if (cancel || (test.is_some() && r.t > RESULT_WAIT)) && !r.told {
                r.told = true;
                tell(&mut host, &r.messages[if r.won { 0 } else { 1 }].clone());
            }
        }
    }
    // (Re)started: the wager taken, the gates and clock back to the start,
    // the vehicles made afresh, the player put at the start.
    if start_over {
        if r.wager > 0 {
            progress.spend(r.wager);
        }
        for (e, v) in &made {
            if v.0 == r.id {
                commands.entity(e).despawn();
            }
        }
        r.made.clear();
        r.passed = 0;
        r.last = None;
        r.clock = 0.0;
        r.won = false;
        r.money = 0;
        r.phase = Phase::Countdown;
        r.t = 0.0;
        say(&mut sfx, r.sounds.get_ready, None);
        if let (Some(host), Some(base)) = (host.as_ref(), r.base) {
            let data = host.story.data();
            r.to_make = data
                .entities
                .values()
                .filter(|e| e.layer == base)
                .filter_map(|e| Some((e.id, e.component("CEntitySpawner")?.u64("arkArchetypeToSpawn")?)))
                .collect();
            if let Some((at, rotation)) = r.spawn.and_then(|s| place_of(&data, s)) {
                t.translation = at + Vec3::Y * HALF_HEIGHT;
                p.yaw = rotation.to_euler(EulerRot::YXZ).0;
                p.pitch = 0.0;
                p.velocity = Vec3::ZERO;
            }
        }
        info!("race ({}): {} vehicles to make", r.id, r.to_make.len());
    }
    // The track's vehicles, made once their models are in.
    if let (Some(host), Some(assets)) = (host.as_ref(), assets.as_ref()) {
        let data = host.story.data();
        let id = r.id;
        let made_now = &mut r.made;
        r.to_make.retain(|&(spawner, archetype)| {
            let Some(entry) = data.archetypes.get(&archetype) else { return false };
            let Some(def) = crate::story_rides::vehicle_def(entry) else { return false };
            let Some(model) = Some(&assets.models[def]).filter(|h| crate::cache_assets::usable(&server, h.id())).and_then(|h| models.get(h)) else {
                return true;
            };
            let Some((at, rotation)) = place_of(&data, spawner) else { return false };
            let layout = crate::story_rides::layout_of(&data, entry);
            let root = crate::vehicles::build_story_vehicle(&mut commands, def, model, at + Vec3::Y * 0.4, rotation, spawner, &layout);
            commands.entity(root).insert(RaceVehicle(id));
            made_now.push(spawner);
            info!("race ({id}): vehicle {spawner} made at {:.1?}", to_dunia(at));
            false
        });
    }
    // The gates registered (as the track came in), in order.
    if let Some(host) = host.as_ref() {
        let data = host.story.data();
        for g in &races.gates {
            if r.gates.iter().any(|x| x.id == g.id) {
                continue;
            }
            let Some((at, rotation)) = place_of(&data, g.id) else { continue };
            let model = data.entities.get(&g.id).and_then(crate::story_props::archetype_model);
            let (middle, half) = gate_line(model.as_deref().unwrap_or(""));
            r.gates.push(Gate {
                id: g.id,
                index: g.index,
                at: Transform { translation: at, rotation, scale: Vec3::ONE },
                middle,
                half,
                tolerance: g.tolerance,
                model,
            });
        }
        r.gates.sort_by_key(|g| g.index);
    }
    // FC3_RACE_SHOTS=<dir> (tests): each phase captured a moment in, the
    // race a few times.
    if let Ok(dir) = std::env::var("FC3_RACE_SHOTS") {
        let taken = r.shot.filter(|s| s.0 == r.phase).map_or(0, |s| s.1);
        let (due, most) = if r.phase == Phase::Racing {
            (1.0 + 6.0 * taken as f32, 3)
        } else if r.phase == Phase::Countdown {
            (2.5, 1)
        } else {
            (1.0, 1)
        };
        if r.t > due && taken < most {
            r.shot = Some((r.phase, taken + 1));
            crate::camera::capture(&mut commands, format!("{dir}/{}_{taken}.png", format!("{:?}", r.phase).to_ascii_lowercase()));
        }
    }
}

/// The keepers calling the player over (their entice lines, now and then,
/// while he's near and no race runs).
fn entice(
    races: Res<Races>,
    host: Option<NonSend<crate::story::StoryHost>>,
    info: Option<Res<crate::CacheInfo>>,
    players: Query<&Transform, With<Player>>,
    time: Res<Time>,
    mut sounds: Local<Option<RaceResources>>,
    mut next: Local<HashMap<u64, f32>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let (Some(host), Ok(pt)) = (host, players.single()) else { return };
    if races.entices.is_empty() || races.run.is_some() {
        return;
    }
    if sounds.is_none() {
        let bytes = info.as_ref().and_then(|i| std::fs::read(i.root.join(fc3_formats::gsf::CACHE_REL)).ok()).unwrap_or_default();
        *sounds = Some(RaceResources::parse(&bytes).unwrap_or_default());
    }
    let Some(sounds) = sounds.as_ref() else { return };
    let now = time.elapsed_secs();
    let data = host.story.data();
    for e in &races.entices {
        let Some(at) = data.position(e.keeper).map(to_bevy) else { continue };
        if at.distance(pt.translation) > ENTICE_REACH || !host.story.is_loaded(e.keeper) {
            continue;
        }
        let due = next.entry(e.keeper).or_insert(now + 2.0);
        if now < *due {
            continue;
        }
        let lines = sounds.keepers.get(e.set).or(sounds.keepers.first()).map(|k| k[0].as_slice()).unwrap_or_default();
        if let Some(&line) = lines.get((now * 13.0) as usize % lines.len().max(1)) {
            sfx.write(crate::sound::PlaySfx { event: line, at: Some(at + Vec3::Y * 1.6), volume: 1.0 });
        }
        *due = now + ENTICE_EVERY[0] + (now * 7.0).fract() * (ENTICE_EVERY[1] - ENTICE_EVERY[0]);
    }
}

/// The running race's gates drawn (their models where they stand), the
/// next ones' flares burning (sparks, a glow and their light), all gone with
/// the race.
#[allow(clippy::too_many_arguments)]
fn dress_course(
    races: Res<Races>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    drawn: Query<(Entity, &GateModel)>,
    lights: Query<(Entity, &FlareLight)>,
    (time, mut particles, mut rng): (Res<Time>, ResMut<crate::particles::Particles>, ResMut<crate::weapons::Rng>),
    mut handles: Local<HashMap<String, Handle<Fc3Model>>>,
    mut commands: Commands,
) {
    use crate::particles::{Particle, Sheet};
    let gates: Vec<&Gate> = races.run.as_ref().map(|r| r.gates.iter().collect()).unwrap_or_default();
    // The next gates' flares (once the race is on).
    let racing = races.run.as_ref().is_some_and(|r| matches!(r.phase, Phase::Countdown | Phase::Racing));
    let passed = races.run.as_ref().map_or(0, |r| r.passed);
    let lit: Vec<&Gate> = if racing { gates.iter().copied().filter(|g| g.index >= passed).take(LIT_AHEAD).collect() } else { Vec::new() };
    for (e, l) in &lights {
        if !lit.iter().any(|g| g.id == l.0) {
            commands.entity(e).despawn();
        }
    }
    let dt = time.delta_secs();
    for g in &lit {
        let (spots, colour) = flares(g.model.as_deref().unwrap_or(""));
        if spots.is_empty() {
            continue;
        }
        let at = |s: &[f32; 3]| g.at.transform_point(Vec3::new(s[0], s[2], -s[1]));
        for spot in spots {
            let base = at(spot) + Vec3::Y * 0.15;
            // The flame's halo, flickering, and its whiter heart.
            let flicker = rng.range(0.85, 1.15);
            let mut halo = Particle::new(Sheet::Glow, base + Vec3::Y * 0.25, 1.0, 1.8 * flicker);
            halo.colour = (colour * 1.4).extend(0.8);
            particles.sprite(halo);
            let mut heart = Particle::new(Sheet::Glow, base + Vec3::Y * 0.15, 1.0, 0.5 * flicker);
            heart.colour = colour.lerp(Vec3::ONE, 0.6).extend(1.0);
            particles.sprite(heart);
            // Its sparks, rising and falling (glow dots: the spark sheet is
            // orange whatever it's tinted).
            let mut n = SPARKS_PER_SECOND * dt + rng.f32();
            while n >= 1.0 {
                n -= 1.0;
                let mut spark = Particle::new(Sheet::Glow, base, rng.range(0.4, 0.8), 0.12);
                let side = Vec3::new(rng.range(-1.0, 1.0), 0.0, rng.range(-1.0, 1.0)) * 0.6;
                spark.vel = (Vec3::Y * 2.5 + side) * rng.range(0.6, 1.4);
                spark.gravity = 3.0;
                spark.colour = (colour * 2.0).extend(1.0);
                particles.spawn(spark);
            }
        }
        if !lights.iter().any(|(_, l)| l.0 == g.id) {
            let middle = spots.iter().map(at).sum::<Vec3>() / spots.len() as f32;
            commands.spawn((
                PointLight { color: Color::srgb(colour.x, colour.y, colour.z), intensity: 2.0e5, range: 9.0, shadow_maps_enabled: false, ..default() },
                Transform::from_translation(middle + Vec3::Y * 0.6),
                FlareLight(g.id),
            ));
        }
    }
    for (e, g) in &drawn {
        if !gates.iter().any(|x| x.id == g.0) {
            commands.entity(e).despawn();
        }
    }
    for g in gates {
        let Some(path) = g.model.as_ref() else { continue };
        if drawn.iter().any(|(_, d)| d.0 == g.id) {
            continue;
        }
        let h = handles.entry(path.clone()).or_insert_with(|| server.load(cache_path(&fc3_formats::cache::model_rel(path)))).clone();
        if !crate::cache_assets::done(&server, h.id()) {
            continue;
        }
        let Some(lod) = models.get(&h).and_then(|m| m.lods.first()) else { continue };
        let root = commands.spawn((g.at, Visibility::Visible, GateModel(g.id))).id();
        for part in &lod.parts {
            commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(part.material.clone()), NotShadowCaster, ChildOf(root)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> Gate {
        let (middle, half) = gate_line("racing_sf_checkpoint_r_breakable01.xbg");
        Gate { id: 1, index: 0, at: Transform::from_xyz(10.0, 0.0, 0.0), middle, half, tolerance: 1.0, model: None }
    }

    /// A gate is crossed through its line, not beside it.
    #[test]
    fn gates_are_crossed_through_their_line() {
        let g = gate();
        // (Its course runs along its own +y: Bevy's -z.)
        assert!(g.crossed(Vec3::new(10.0, 1.0, 2.0), Vec3::new(10.0, 1.0, -2.0)));
        assert!(g.crossed(Vec3::new(12.0, 1.0, 1.0), Vec3::new(12.0, 1.0, -1.0)));
        assert!(!g.crossed(Vec3::new(20.0, 1.0, 2.0), Vec3::new(20.0, 1.0, -2.0)));
        assert!(!g.crossed(Vec3::new(10.0, 1.0, 9.0), Vec3::new(10.0, 1.0, 6.0)));
        // Onto the line itself (a vehicle set down there).
        assert!(g.crossed(Vec3::new(30.0, 1.0, 30.0), Vec3::new(10.0, 1.0, 0.0)));
    }

    #[test]
    fn race_times() {
        assert_eq!(race_time(140.0), "2:20.0");
        assert_eq!(race_time(9.25), "0:09.2");
    }
}
