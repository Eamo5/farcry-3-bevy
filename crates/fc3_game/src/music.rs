//! Music: the game's music service, read from its object template in the
//! cache (`story/templates.fcb`).
//!
//! The service has a state per piece of music: the sound event that starts
//! it, a priority (lower wins) and when it may play. The menus have their own
//! tracks. In the open world the service plays combat music while enemies
//! fight the player and, now and then when all is calm, an exploration piece.
//! Missions start and stop their own states by name (the story's
//! `MusicState` commands); while a mission runs, only states at or above the
//! service's mission priority play. The winning state's track fades in as
//! the last one fades out.

use std::collections::HashMap;

use bevy::audio::{AudioSink, AudioSinkPlayback, PlaybackMode, Volume};
use bevy::prelude::*;

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;
use crate::menu::GameState;
use crate::sound::SoundBank;
use crate::sound_source::Sfx;
use crate::weapons::Rng;

pub struct MusicPlugin;

impl Plugin for MusicPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Music>().add_systems(OnExit(GameState::Loading), load_music).add_systems(Update, play_music.after(crate::story::StorySet));
    }
}

/// Enemies fighting the player this near (m) bring on combat music, which
/// stays this long (s) after the last of them stops.
const COMBAT_RANGE: f32 = 90.0;
const COMBAT_LINGER: f32 = 8.0;
/// Calm this long (s) before an exploration piece, and the wait (s) between
/// pieces.
const EXPLORE_CALM: f32 = 90.0;
const EXPLORE_GAP: (f32, f32) = (240.0, 480.0);

/// What brings a state on without a script asking.
#[derive(Clone, Copy)]
enum Trigger {
    /// Scripts start and stop it.
    External,
    /// Enemies in combat with the player (at least this many).
    Combat(f32),
    /// Calm in the open world (the state the others transition from).
    Explore,
}

struct StateDef {
    name: String,
    start: u32,
    priority: u32,
    fade_in: f32,
    fade_out: f32,
    trigger: Trigger,
}

/// A track playing: the menu's, the pause screen's or a state's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Menu,
    Pause,
    State(usize),
}

#[derive(Resource, Default)]
pub struct Music {
    states: Vec<StateDef>,
    /// State index by name and by the alias scripts use (lower case).
    by_name: HashMap<String, usize>,
    menu: Option<u32>,
    pause: Option<u32>,
    /// Fade time (s) for states that don't give one.
    fade: f32,
    /// While a mission runs, states with a larger priority number keep out.
    mission_cap: u32,
    /// States scripts started, in order, with a multitrack level (0 = all
    /// tracks).
    requested: Vec<(usize, u8)>,
    /// A mission is being played.
    pub in_mission: bool,
    combat_left: f32,
    calm_for: f32,
    explore_wait: f32,
    exploring: bool,
}

impl Music {
    fn state(&self, name: &str) -> Option<usize> {
        let n = name.trim().to_ascii_lowercase();
        self.by_name.get(&n).or_else(|| self.by_name.get(n.trim_start_matches("play_"))).copied()
    }

    /// A script starts (or stops) a state by name.
    pub fn request(&mut self, name: &str, on: bool) {
        let Some(i) = self.state(name) else {
            debug!("music: no state {name}");
            return;
        };
        self.requested.retain(|r| r.0 != i);
        if on {
            self.requested.push((i, 0));
        }
    }

    /// A script sets how many of a state's tracks play.
    pub fn set_level(&mut self, name: &str, level: u8) {
        let Some(i) = self.state(name) else { return };
        if let Some(r) = self.requested.iter_mut().find(|r| r.0 == i) {
            r.1 = level;
        }
    }

    /// Drops what scripts asked for (a reload).
    pub fn reset(&mut self) {
        self.requested.clear();
    }
}

/// One file of a playing track; `voice` is its place in the event's
/// tracks.
#[derive(Component)]
struct MusicVoice {
    key: Key,
    voice: usize,
    gain: f32,
    level: f32,
    /// Fading to: 1 (in) or 0 (out, then gone), at this rate per second.
    target: f32,
    rate: f32,
}

fn hex(v: Option<String>) -> Option<u32> {
    v.and_then(|s| u32::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()).filter(|&id| id != 0xFFFF_FFFF)
}

fn load_music(info: Res<CacheInfo>, mut music: ResMut<Music>) {
    let Ok(bytes) = std::fs::read(info.root.join("story/templates.fcb")) else {
        warn!("music: no object templates in the cache (convert the game again)");
        return;
    };
    let Some(root) = bytes.windows(4).position(|w| w == b"nbCF").and_then(|at| fc3_formats::fcb::parse(&bytes[at..]).ok()) else {
        warn!("music: the object templates don't read");
        return;
    };
    let mut service = None;
    root.walk(&mut |o, _| {
        if service.is_none() && o.str("Name").as_deref() == Some("MusicService") {
            service = Some(o);
        }
    });
    let Some(service) = service else {
        warn!("music: no music service template");
        return;
    };
    let mut m = Music { fade: 0.5, mission_cap: u32::MAX, ..Music::default() };
    service.walk(&mut |o, _| {
        if o.is("settings") {
            m.menu = hex(o.str("sndStartMenuStartId"));
            m.pause = hex(o.str("sndPauseMenuStartId"));
            m.fade = o.f32("VolumeFadeTime").filter(|f| *f > 0.0).unwrap_or(0.5);
            m.mission_cap = o.u32("iMaxPriorityDuringMission").unwrap_or(u32::MAX);
        }
        if !o.is("State") {
            return;
        }
        let (Some(name), Some(start)) = (o.str("sName").filter(|n| !n.is_empty()), hex(o.str("sndStartSoundId"))) else { return };
        let condition = o.child("Condition");
        let combat = condition.and_then(|c| c.f32("fMinCombatAi")).unwrap_or(0.0);
        let trigger = if o.bool("bExternal").unwrap_or(true) {
            Trigger::External
        } else if combat > 0.0 {
            Trigger::Combat(combat)
        } else {
            Trigger::Explore
        };
        // Multiplayer modes' states keep to their game states.
        if !matches!(trigger, Trigger::External) && condition.and_then(|c| c.u32("selGameState")).unwrap_or(0) != 0 {
            return;
        }
        let fade = |f: Option<f32>| f.filter(|f| *f >= 0.0);
        let i = m.states.len();
        m.states.push(StateDef {
            name: name.clone(),
            start,
            priority: o.u32("iPriority").unwrap_or(u32::MAX),
            fade_in: fade(o.f32("fFadeInTime")).unwrap_or(-1.0),
            fade_out: fade(o.f32("fFadeOutTime")).unwrap_or(-1.0),
            trigger,
        });
        m.by_name.entry(name.to_ascii_lowercase()).or_insert(i);
        if let Some(alias) = o.str_h(0xD128_7212).filter(|a| !a.is_empty()) {
            let alias = alias.to_ascii_lowercase();
            m.by_name.entry(alias.trim_start_matches("play_").to_string()).or_insert(i);
            m.by_name.entry(alias).or_insert(i);
        }
    });
    for s in &mut m.states {
        if s.fade_in < 0.0 {
            s.fade_in = m.fade;
        }
        if s.fade_out < 0.0 {
            s.fade_out = m.fade;
        }
    }
    let systemic = m.states.iter().filter(|s| !matches!(s.trigger, Trigger::External)).count();
    info!("music: {} states ({systemic} the game's own)", m.states.len());
    m.explore_wait = EXPLORE_GAP.0 * 0.5;
    *music = m;
}

#[allow(clippy::too_many_arguments)]
fn play_music(
    time: Res<Time<Real>>,
    state: Res<State<GameState>>,
    settings: Res<crate::settings::Settings>,
    bank: Option<Res<SoundBank>>,
    server: Res<AssetServer>,
    mut rng: ResMut<Rng>,
    mut music: ResMut<Music>,
    npcs: Query<(&GlobalTransform, &crate::npc::Npc, &crate::combat::Health)>,
    players: Query<&Transform, With<crate::player::Player>>,
    mut voices: Query<(Entity, &mut MusicVoice, Option<&mut AudioSink>)>,
    mut commands: Commands,
    videos: Res<crate::video::Videos>,
) {
    let Some(bank) = bank else { return };
    let dt = time.delta_secs().min(0.1);
    let music = &mut *music;
    if music.states.is_empty() && music.menu.is_none() {
        return;
    }
    // What should play.
    let want: Option<Key> = match state.get() {
        // A full-screen video has its own soundtrack.
        _ if videos.fullscreen() => None,
        GameState::Loading => None,
        GameState::MainMenu => music.menu.map(|_| Key::Menu),
        GameState::Paused => music.pause.map(|_| Key::Pause),
        _ => {
            let player = players.single().map(|t| t.translation).ok();
            let fighting = player.map_or(0, |p| {
                npcs.iter()
                    .filter(|(t, n, h)| {
                        !h.dead && n.faction.hostile_to_player() && n.state == crate::npc::NpcState::Combat && t.translation().distance(p) < COMBAT_RANGE
                    })
                    .count()
            }) as f32;
            if fighting > 0.0 {
                music.combat_left = COMBAT_LINGER;
                music.calm_for = 0.0;
            } else {
                music.combat_left = (music.combat_left - dt).max(0.0);
                music.calm_for += dt;
            }
            let explore_now =
                music.exploring && voices.iter().any(|(_, v, _)| matches!(v.key, Key::State(i) if matches!(music.states[i].trigger, Trigger::Explore)));
            if music.exploring && !explore_now {
                // The piece came to its end.
                music.exploring = false;
                music.explore_wait = EXPLORE_GAP.0 + rng.f32() * (EXPLORE_GAP.1 - EXPLORE_GAP.0);
            }
            if !music.exploring && music.calm_for > EXPLORE_CALM && music.requested.is_empty() && !music.in_mission {
                music.explore_wait -= dt;
                if music.explore_wait <= 0.0 {
                    music.exploring = true;
                }
            }
            let cap = if music.in_mission { music.mission_cap } else { u32::MAX };
            let mut best: Option<(u32, usize)> = None;
            let consider = |i: usize, best: &mut Option<(u32, usize)>| {
                let p = music.states[i].priority;
                if p <= cap && best.is_none_or(|b| p <= b.0) {
                    *best = Some((p, i));
                }
            };
            for (i, s) in music.states.iter().enumerate() {
                match s.trigger {
                    Trigger::Combat(min) if music.combat_left > 0.0 && fighting.max(1.0) >= min => consider(i, &mut best),
                    Trigger::Explore if music.exploring => consider(i, &mut best),
                    _ => {}
                }
            }
            // Scripts' states, the latest winning a tie.
            for r in music.requested.clone() {
                consider(r.0, &mut best);
            }
            if music.exploring && best.is_some_and(|b| !matches!(music.states[b.1].trigger, Trigger::Explore)) {
                music.exploring = false;
                music.explore_wait = EXPLORE_GAP.0;
            }
            best.map(|b| Key::State(b.1))
        }
    };
    let level_of = |key: Key| match key {
        Key::State(i) => music.requested.iter().find(|r| r.0 == i).map_or(0, |r| r.1),
        _ => 0,
    };
    let fades = |key: Key| match key {
        Key::State(i) => (music.states[i].fade_in, music.states[i].fade_out),
        _ => (music.fade, music.fade),
    };
    // Fade the others out, the wanted one in.
    let mut have = false;
    for (e, mut v, sink) in &mut voices {
        if Some(v.key) == want {
            have = true;
            let level = level_of(v.key);
            v.target = if level == 0 || v.voice <= level as usize { 1.0 } else { 0.0 };
            v.rate = 1.0 / fades(v.key).0.max(0.05);
        } else {
            v.target = 0.0;
            v.rate = 1.0 / fades(v.key).1.max(0.05);
        }
        let step = v.rate * dt;
        v.level = if v.target > v.level { (v.level + step).min(v.target) } else { (v.level - step).max(v.target) };
        if v.level <= 0.0 && Some(v.key) != want {
            commands.entity(e).despawn();
            continue;
        }
        if let Some(mut s) = sink {
            s.set_volume(Volume::Linear(v.level * v.gain * settings.music_volume));
        }
    }
    let Some(key) = want else { return };
    if have {
        return;
    }
    let event = match key {
        Key::Menu => music.menu,
        Key::Pause => music.pause,
        Key::State(i) => Some(music.states[i].start),
    };
    let Some(event) = event else { return };
    let (files, gain) = bank.resolve(event, &mut rng);
    if files.is_empty() {
        debug!("music: nothing to play for {key:?} ({event:08x})");
        return;
    }
    let looping = bank.loops(event);
    let name = match key {
        Key::State(i) => music.states[i].name.as_str(),
        Key::Menu => "menu",
        Key::Pause => "pause",
    };
    debug!("music: {name} ({event:08x}), {} track(s){}", files.len(), if looping { ", looping" } else { "" });
    let mode = if looping { PlaybackMode::Loop } else { PlaybackMode::Despawn };
    for (voice, (f, own)) in files.into_iter().enumerate() {
        commands.spawn((
            AudioPlayer::<Sfx>(server.load(cache_path(&f))),
            PlaybackSettings { mode, volume: Volume::Linear(0.0), ..default() },
            MusicVoice { key, voice, gain: gain * own, level: 0.0, target: 1.0, rate: 1.0 },
        ));
    }
}
