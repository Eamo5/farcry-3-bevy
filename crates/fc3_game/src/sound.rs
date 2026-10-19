//! Sound effects from the converted audio cache.
//!
//! The converter writes every decoded sound as `audio/<stem>.{ogg,wav}` plus a
//! text manifest mapping event ids to weighted variant groups. Gameplay sends
//! `PlaySfx` (one-shots) or `LoopSfx` (refreshable loops such as automatic
//! fire); this module resolves events to files and spawns audio players.
//! Only effects and ambience are used here.
//!
//! Each sound fades with its distance from the listener by its own curve
//! (the game's positioning, see `fc3_formats::audio::Fade`), and blends
//! cross-fade on the distance (a gunshot's crack near, its echo far). Bevy
//! pans the placed ones; its own fall-off is scaled out of the way.

use std::collections::HashMap;

use std::sync::{Arc, LazyLock};

use bevy::audio::{AudioSink, AudioSinkPlayback, GlobalVolume, PlaybackMode, SpatialAudioSink, SpatialListener, SpatialScale, Volume};
use bevy::prelude::*;
use fc3_formats::audio::Curve;

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;
use crate::mixer::{Mixer, Mixings};
use crate::sound_source::{Sfx, SoundSourcePlugin};
use crate::weapons::Rng;

pub struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SoundSourcePlugin)
            .add_message::<PlaySfx>()
            .add_message::<LoopSfx>()
            .init_resource::<Mixer>()
            .init_resource::<Mixings>()
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_bank, load_sound_points))
            .add_systems(PostStartup, attach_listener)
            .add_systems(Update, (crate::mixer::game_mixings, mix, sound_points, play_one_shots, update_loops, ambience, hear).chain());
    }
}

/// The game's distance parameter (m, 0-300): blends cross-fade on it.
pub const DISTANCE: u32 = 0x0000_5EAB;

/// The authored levels sit low (dialogue near -7 dB, gunfire -12 to -17,
/// music -12, ambience -14 to -20, footsteps -21): the whole mix is raised
/// this much (dB) so dialogue plays near full scale.
const MIX_LIFT: f32 = 6.0;

/// Bevy's spatial sinks fade as 1/d² past a metre; placed this much nearer
/// (a kilometre to a metre) only their panning is left, and the sound's own
/// curve does the fading.
const SPATIAL_SCALE: f32 = 1.0 / 1024.0;

/// How a sound fades with distance from the listener.
#[derive(Clone, Default)]
enum Falloff {
    /// Not known (an older manifest): [`STAND_IN`].
    #[default]
    Unknown,
    /// Not placed in the world (menus, music, beds): no fade.
    Flat,
    /// The game's own volume (dB) by distance (m).
    Curve(Arc<Curve>),
}

/// Fades a sound whose own curve isn't known: full to 5 m, -10 dB at 30,
/// -20 at 60, gone at 100.
static STAND_IN: LazyLock<Curve> =
    LazyLock::new(|| Curve { kind: Curve::VOLUME, param: DISTANCE, points: vec![(0.0, 0.0), (5.0, 0.0), (30.0, -10.0), (60.0, -20.0), (100.0, -96.0)] });

fn gain_of(db: f32) -> f32 {
    if db <= -95.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

impl Falloff {
    /// Linear gain `d` metres from the listener.
    fn gain(&self, d: f32) -> f32 {
        match self {
            Falloff::Flat => 1.0,
            Falloff::Curve(c) => gain_of(c.at(d)),
            Falloff::Unknown => gain_of(STAND_IN.at(d)),
        }
    }

    /// How far it carries (m), where its curve is known.
    fn reach(&self) -> Option<f32> {
        match self {
            Falloff::Curve(c) => c.points.last().map(|p| p.0),
            _ => None,
        }
    }
}

/// Effect event ids (from the converted manifest).
pub mod ev {
    pub const AK_LOOP: u32 = 0x00046CB3;
    pub const RIFLE_3P: u32 = 0x00046CBF;
    pub const LMG_3P: u32 = 0x0002E67A;
    pub const SHOTGUN_3P: u32 = 0x00007966;
    pub const SNIPER_3P: u32 = 0x000208E4;
    pub const PISTOL_3P: u32 = 0x00017069;
    pub const PISTOL: u32 = 0x0001706E;
    pub const SHOTGUN: u32 = 0x0009FA86;
    pub const SNIPER: u32 = 0x00017025;
    pub const SMG_LOOP: u32 = 0x0009FA70;
    pub const LMG_LOOP: u32 = 0x0002E680;
    pub const RPG: u32 = 0x00189801;
    pub const DRY_FIRE: u32 = 0x0002EC34;
    pub const RELOAD: u32 = 0x00189820;
    pub const FOOTSTEP: u32 = 0x004E4DC6;
    pub const NPC_STEP: u32 = 0x004550BA;
    pub const NPC_RUN: u32 = 0x0002F20A;
    pub const LANDING: u32 = 0x0002F219;
    /// Bullets passing close by (`sndPassByNearSound`), and under water.
    pub const PASS_BY: u32 = 0x00448BC8;
    pub const PASS_BY_UNDERWATER: u32 = 0x0045546A;
    pub const SLIDE: u32 = 0x004B68AE;
    pub const IMPACT: u32 = 0x0002572E;
    pub const MELEE_IMPACT: u32 = 0x000526BF;
    /// A thrown rock landing (switched on the surface).
    pub const ROCK_IMPACT: u32 = 0x0002EB9C;
    /// A rifle butt striking.
    pub const BUTT_HIT: u32 = 0x001A6E08;
    pub const GRENADE_BLAST: u32 = 0x0002EC6E;
    pub const ROCKET_BLAST: u32 = 0x00177B45;
    pub const JUNGLE_LOOP: u32 = 0x001133BD;
    pub const BIRDS: u32 = 0x00159609;
    pub const WAVES: u32 = 0x000A6821;
    pub const UI_CLICK: u32 = 0x001A9CC7;
    /// Rain ambience by sound region: forest, coast and open grassland.
    pub const RAIN_FOREST: u32 = 0x004B1969;
    pub const RAIN_COAST: u32 = 0x004B196A;
    pub const RAIN_OPEN: u32 = 0x004B196B;
}

struct SoundFile {
    path: String,
    looping: bool,
    /// How long it plays (s): its samples at its rate.
    seconds: f32,
    falloff: Falloff,
    /// Its mixer buses' level (linear).
    bus: f32,
    /// Its mixer bus (for the mixings).
    bus_id: Option<u32>,
}

#[derive(Clone)]
struct EventDef {
    /// Voices that play together, each of alternatives: (header, weight,
    /// the gain on the way to it, dB).
    voices: Vec<Vec<(u32, f32, f32)>>,
    volume: f32,
}

/// How one voice of a blended event (engines, tyres) responds to the game's
/// parameters: its gain and its volume and pitch curves.
struct VoiceMod {
    gain: f32,
    curves: Vec<fc3_formats::audio::Curve>,
}

impl VoiceMod {
    /// (linear volume, playback speed) at these parameter values and
    /// `distance` (m) from the listener; parameters not given count as 0
    /// when `every`, else their curves are left out (one-shots, which the
    /// game sends no values with).
    fn eval(&self, params: &[(u32, f32)], distance: f32, every: bool) -> (f32, f32) {
        let value = |p: u32| match p {
            DISTANCE => Some(distance),
            _ => params.iter().find(|q| q.0 == p && p != 0).map(|q| q.1).or(every.then_some(0.0)),
        };
        let (mut db, mut semitones) = (self.gain, 0.0);
        for c in &self.curves {
            let Some(x) = value(c.param) else { continue };
            match c.kind {
                Curve::VOLUME => db += c.at(x),
                Curve::PITCH => semitones += c.at(x),
                _ => {}
            }
        }
        (gain_of(db.max(-96.0)), 2f32.powf(semitones / 12.0))
    }
}

#[derive(Resource, Default)]
pub struct SoundBank {
    /// The mixer's buses, states and mixings (see `mixer`).
    mix: crate::mixer::MixData,
    files: HashMap<String, SoundFile>,
    events: HashMap<u32, EventDef>,
    /// Blended events under a switch state, by `id@group=value,...` token.
    cases: HashMap<String, EventDef>,
    /// Per event token, each header's response to the game's parameters.
    mods: HashMap<String, HashMap<u32, Arc<VoiceMod>>>,
}

impl SoundBank {
    fn parse(text: &str) -> SoundBank {
        let mut bank = SoundBank::default();
        let mut same: Vec<(u32, u32)> = Vec::new();
        let mut curves: HashMap<String, Arc<Curve>> = HashMap::new();
        let mut fades: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("same") => {
                    let mut id = || it.next().and_then(|s| u32::from_str_radix(s, 16).ok());
                    if let (Some(a), Some(b)) = (id(), id()) {
                        same.push((a, b));
                    }
                }
                Some("sound") => {
                    let (Some(stem), Some(ext)) = (it.next(), it.next()) else { continue };
                    // codec, channels, rate, samples, then the loop flag.
                    let rest: Vec<&str> = it.collect();
                    let looping = rest.get(4) == Some(&"1");
                    let number = |i: usize| rest.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                    let seconds = if number(2) > 0.0 { number(3) / number(2) } else { 0.0 };
                    let falloff = match rest.get(6) {
                        Some(&"2d") => Falloff::Flat,
                        Some(c) if *c != "-" => {
                            fades.push((stem.to_string(), c.to_string()));
                            Falloff::Unknown
                        }
                        _ => Falloff::Unknown,
                    };
                    let bus = gain_of(number(7));
                    let bus_id = rest.get(8).and_then(|b| u32::from_str_radix(b, 16).ok());
                    bank.files.insert(stem.to_string(), SoundFile { path: format!("audio/{stem}.{ext}"), looping, seconds, falloff, bus, bus_id });
                }
                Some(kind @ ("bus" | "mixstate" | "mixevent" | "mixing")) => bank.mix.read(kind, line.split_once(' ').map_or("", |x| x.1)),
                Some("falloff") => {
                    let (Some(id), Some(points)) = (it.next(), it.next()) else { continue };
                    if let Some(c) = Curve::from_token(&format!("0:{DISTANCE:08x}:{points}")) {
                        curves.insert(id.to_string(), Arc::new(c));
                    }
                }
                Some("mod") => {
                    let (Some(token), Some(header), Some(gain)) =
                        (it.next(), it.next().and_then(|h| u32::from_str_radix(h, 16).ok()), it.next().and_then(|g| g.parse().ok()))
                    else {
                        continue;
                    };
                    let curves = it.filter_map(fc3_formats::audio::Curve::from_token).collect();
                    bank.mods.entry(token.to_string()).or_default().insert(header, Arc::new(VoiceMod { gain, curves }));
                }
                Some("event") => {
                    let Some(token) = it.next() else { continue };
                    let id = if token.contains('@') { None } else { u32::from_str_radix(token, 16).ok() };
                    let _kind = it.next();
                    let volume = it.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
                    let rest: Vec<&str> = it.collect();
                    let voices = rest
                        .join(" ")
                        .split('|')
                        .map(|v| {
                            v.split_whitespace()
                                .filter_map(|alt| {
                                    let mut part = alt.split(':');
                                    let h = u32::from_str_radix(part.next()?, 16).ok()?;
                                    let w = part.next().and_then(|w| w.parse().ok()).unwrap_or(1.0);
                                    Some((h, w, part.next().and_then(|g| g.parse().ok()).unwrap_or(0.0)))
                                })
                                .collect::<Vec<_>>()
                        })
                        .filter(|v| !v.is_empty())
                        .collect();
                    match id {
                        Some(id) => bank.events.insert(id, EventDef { voices, volume }),
                        None => bank.cases.insert(token.to_string(), EventDef { voices, volume }),
                    };
                }
                _ => {}
            }
        }
        for (id, like) in same {
            if let Some(e) = bank.events.get(&like).cloned() {
                bank.events.insert(id, e);
            }
        }
        for (stem, curve) in fades {
            if let (Some(f), Some(c)) = (bank.files.get_mut(&stem), curves.get(&curve)) {
                f.falloff = Falloff::Curve(c.clone());
            }
        }
        bank
    }

    /// How far `event` carries (m) where all its sounds' curves are known:
    /// the farthest of them.
    fn reach(&self, event: u32) -> Option<f32> {
        let e = self.event(event)?;
        let mut far = 0.0f32;
        for (h, ..) in e.voices.iter().flatten() {
            let stem = format!("{h:08x}");
            let f = self.files.get(&stem).or_else(|| self.files.get(&format!("{stem}_l0")))?;
            far = far.max(f.falloff.reach()?);
        }
        (far > 0.0).then_some(far)
    }

    /// The event's record, falling back from a switched id (see [`switched`])
    /// to the same event without the gait, then without any switch.
    fn event(&self, event: u32) -> Option<&EventDef> {
        self.events.get(&event).or_else(|| self.events.get(&(event & 0x3FFF_FFFF))).or_else(|| self.events.get(&(event & 0x00FF_FFFF)))
    }

    /// How long `event` plays (s): its first voice's first sound (a line of
    /// dialogue's length).
    pub fn length(&self, event: u32) -> Option<f32> {
        let (h, ..) = self.event(event)?.voices.first()?.first()?;
        let stem = format!("{h:08x}");
        self.files.get(&stem).or_else(|| self.files.get(&format!("{stem}_l0"))).map(|f| f.seconds).filter(|s| *s > 0.0)
    }

    /// Whether everything `event` plays loops (an ambience bed rather than a
    /// one-off).
    pub fn loops(&self, event: u32) -> bool {
        let Some(e) = self.event(event) else { return false };
        let looping = |h: u32| {
            let stem = format!("{h:08x}");
            self.files.get(&stem).or_else(|| self.files.get(&format!("{stem}_l0"))).is_some_and(|f| f.looping)
        };
        !e.voices.is_empty() && e.voices.iter().flatten().all(|(h, ..)| looping(*h))
    }

    /// Files to play together for one triggering of `event` (one pick per voice;
    /// layered sounds expand to all their tracks).
    /// Each file comes with its gain (linear): the authored gains on the way
    /// to it and its mixer buses'.
    pub fn resolve(&self, event: u32, rng: &mut Rng) -> (Vec<(String, f32)>, f32) {
        let Some(e) = self.event(event) else { return (vec![], 0.0) };
        (
            Self::pick(&self.files, e, rng)
                .into_iter()
                .map(|(f, _, g)| {
                    let gain = self.file_gain(&f, g, false);
                    (f, gain)
                })
                .collect(),
            e.volume,
        )
    }

    /// The gain (linear) a file of header `h` plays at: the gains on the way
    /// to it (else, in a blend, its voice's), its mixer buses' and the mix's
    /// own lift ([`MIX_LIFT`]).
    fn file_gain(&self, path: &str, gain_db: f32, blended: bool) -> f32 {
        let stem = path.rsplit('/').next().unwrap_or(path).split('.').next().unwrap_or_default();
        let bus = self.files.get(stem).map_or(1.0, |f| f.bus);
        gain_of(MIX_LIFT) * bus * if blended { 1.0 } else { gain_of(gain_db) }
    }

    /// Like [`SoundBank::resolve`] for `event` under a switch state (see
    /// [`loop_token`]), with each file's response to the game's parameters
    /// for blended events, whether it loops, how it fades with distance and
    /// its gain (the authored gains on the way to it and its buses').
    #[allow(clippy::type_complexity)]
    fn resolve_case(&self, token: &str, event: u32, rng: &mut Rng) -> (Vec<(String, Option<Arc<VoiceMod>>, bool, Falloff, f32, Option<u32>)>, f32) {
        let (e, mods) = match self.cases.get(token) {
            Some(e) => (e, self.mods.get(token)),
            None => match self.event(event) {
                Some(e) => (e, self.mods.get(&format!("{event:08x}"))),
                None => return (vec![], 0.0),
            },
        };
        let files = Self::pick(&self.files, e, rng)
            .into_iter()
            .map(|(path, h, gain)| {
                let stem = format!("{h:08x}");
                let file = self.files.get(&stem).or_else(|| self.files.get(&format!("{stem}_l0")));
                let (looping, falloff, bus) = file.map_or((false, Falloff::Unknown, None), |f| (f.looping, f.falloff.clone(), f.bus_id));
                let blend = mods.and_then(|m| m.get(&h)).cloned();
                let gain = self.file_gain(&path, gain, blend.is_some());
                (path, blend, looping, falloff, gain, bus)
            })
            .collect();
        (files, e.volume)
    }

    fn pick(files: &HashMap<String, SoundFile>, e: &EventDef, rng: &mut Rng) -> Vec<(String, u32, f32)> {
        let mut out = Vec::new();
        for voice in &e.voices {
            let total: f32 = voice.iter().map(|(_, w, _)| w.max(0.0)).sum();
            let mut pick = rng.f32() * total.max(1e-6);
            let (mut chosen, mut gain) = (voice[0].0, voice[0].2);
            for (h, w, g) in voice {
                if pick <= *w {
                    (chosen, gain) = (*h, *g);
                    break;
                }
                pick -= *w;
            }
            let stem = format!("{chosen:08x}");
            if let Some(f) = files.get(&stem) {
                out.push((f.path.clone(), chosen, gain));
            } else {
                for l in 0..8 {
                    match files.get(&format!("{stem}_l{l}")) {
                        Some(f) => out.push((f.path.clone(), chosen, gain)),
                        None => break,
                    }
                }
            }
        }
        out
    }
}

/// The manifest's name for `event` under a switch state: `id@group=value,...`
/// with the groups in order (unused pairs have group 0), or just the id.
pub fn loop_token(event: u32, switches: &[(u32, u32)]) -> String {
    let mut s: Vec<(u32, u32)> = switches.iter().copied().filter(|p| p.0 != 0).collect();
    if s.is_empty() {
        return format!("{event:08x}");
    }
    s.sort_unstable();
    let state: Vec<String> = s.iter().map(|(g, v)| format!("{g:08x}={v:08x}")).collect();
    format!("{event:08x}@{}", state.join(","))
}

/// `event` as played on a surface (a switch value from
/// `surfaces::SurfaceProbe`, 0 = unknown) at a footstep pace (0 = none,
/// 1 walk, 2 crouched, 3 run).
pub fn switched(event: u32, surface: u32, gait: u8) -> u32 {
    fc3_formats::audio::switched_event(event, surface, gait)
}

/// A one-shot effect. `at = None` plays it non-spatially (e.g. the player's own gun).
#[derive(Message, Clone, Copy)]
pub struct PlaySfx {
    pub event: u32,
    pub at: Option<Vec3>,
    pub volume: f32,
}

/// Keeps a looping effect alive while it's re-sent; it stops shortly after
/// the last refresh. `key` identifies the emitter (e.g. an entity's bits).
/// Blended events (engines, tyres) follow `params` and play the case for
/// `switches`; a change of switch state restarts the loop.
#[derive(Message, Clone, Copy, Default)]
pub struct LoopSfx {
    pub key: u64,
    pub event: u32,
    pub at: Option<Vec3>,
    pub volume: f32,
    /// `(switch group, value)`; group 0 = unused.
    pub switches: [(u32, u32); 2],
    /// `(parameter id, value)`, e.g. engine RPM; id 0 = unused.
    pub params: [(u32, f32); 4],
}

/// A loop stops this long (s) after its last refresh. A slow frame
/// (streaming) counts for at most `LOOP_TTL_STEP`, so it doesn't restart.
const LOOP_TTL: f32 = 0.15;
const LOOP_TTL_STEP: f32 = 0.05;

#[derive(Component)]
struct LoopEmitter {
    key: u64,
    ttl: f32,
    volume: f32,
    token: String,
    params: [(u32, f32); 4],
}

/// A playing sound as heard: its mix level within the event and the
/// sender's volume, how it fades with distance, and in blended events how it
/// follows distance and the game's parameters.
#[derive(Component)]
struct Heard {
    gain: f32,
    volume: f32,
    falloff: Falloff,
    blend: Option<Arc<VoiceMod>>,
    params: [(u32, f32); 4],
    /// A loop (its parameters not sent count as 0).
    looping: bool,
    /// Placed in the world (else heard where the listener is).
    placed: bool,
    /// Its mixer bus, which the mixings change.
    bus: Option<u32>,
}

impl Heard {
    /// (linear volume, playback speed) `d` metres from the listener.
    fn at(&self, d: f32) -> (f32, f32) {
        let d = if self.placed { d } else { 0.0 };
        let (v, speed) = self.blend.as_ref().map_or((1.0, 1.0), |m| m.eval(&self.params, d, self.looping));
        (self.gain * self.volume * v * self.falloff.gain(d), speed)
    }
}

/// Every playing sound's volume (and blends' pitch) at its distance from
/// the listener, under the master volume and the mixer (its bus's level
/// now, paused, stopped).
#[allow(clippy::type_complexity)]
fn hear(
    listener: Query<&GlobalTransform, With<SpatialListener>>,
    master: Res<GlobalVolume>,
    (bank, mut mixer): (Option<Res<SoundBank>>, ResMut<Mixer>),
    mut commands: Commands,
    mut voices: Query<(Entity, &Heard, &GlobalTransform, Option<&mut AudioSink>, Option<&mut SpatialAudioSink>)>,
    (time, mut shown): (Res<Time<Real>>, Local<f32>),
) {
    let ear = listener.single().ok().map(|t| t.translation());
    let master = master.volume.to_linear();
    // (RUST_LOG=farcry3::sound=debug: what the mixer does to the playing
    // sounds, every few seconds.)
    *shown += time.delta_secs();
    let tally = *shown > 3.0;
    let (mut held, mut lowered, mut all) = (0, 0, 0);
    for (e, heard, t, sink, spatial) in &mut voices {
        let (db, paused, stopped) = match (heard.bus, bank.as_deref()) {
            (Some(bus), Some(bank)) => mixer.on_bus(&bank.mix, bus),
            _ => (0.0, false, false),
        };
        all += 1;
        held += paused as u32;
        lowered += (db < -0.5) as u32;
        if stopped {
            commands.entity(e).despawn();
            continue;
        }
        let (v, speed) = heard.at(ear.map_or(0.0, |e| e.distance(t.translation())));
        let v = Volume::Linear(v * master * gain_of(db));
        if let Some(mut s) = sink {
            s.set_volume(v);
            s.set_speed(speed);
            if paused != s.is_paused() {
                if paused {
                    s.pause()
                } else {
                    s.play()
                }
            }
        }
        if let Some(mut s) = spatial {
            s.set_volume(v);
            s.set_speed(speed);
            if paused != s.is_paused() {
                if paused {
                    s.pause()
                } else {
                    s.play()
                }
            }
        }
    }
    mixer.clear_stops();
    if tally {
        *shown = 0.0;
        debug!("mixer: {all} sounds, {held} paused, {lowered} lowered");
    }
}

/// Plays the events of the mixings asked for, and moves the mixer's levels.
fn mix(time: Res<Time<Real>>, bank: Option<Res<SoundBank>>, mut asked: ResMut<Mixings>, mut mixer: ResMut<Mixer>, mut sfx: MessageWriter<PlaySfx>) {
    let asked = asked.take();
    let Some(bank) = bank else { return };
    for (name, on) in asked {
        match bank.mix.mixing(&name) {
            Some((set, remove)) => {
                let event = if on { set } else { remove };
                debug!("mixing {name}: {} ({event:08x})", if on { "start" } else { "stop" });
                if event != 0 {
                    sfx.write(PlaySfx { event, at: None, volume: 1.0 });
                }
            }
            None => debug!("mixing {name}: not in the manifest"),
        }
    }
    mixer.advance(&bank.mix, time.delta_secs());
}

fn load_bank(mut commands: Commands, info: Res<CacheInfo>) {
    let path = info.root.join("audio").join("manifest.txt");
    let bank = match std::fs::read_to_string(&path) {
        Ok(text) => SoundBank::parse(&text),
        Err(e) => {
            warn!("no audio manifest at {} ({e}); sounds disabled", path.display());
            SoundBank::default()
        }
    };
    info!("sound bank: {} files, {} events", bank.files.len(), bank.events.len());
    commands.insert_resource(bank);
}

/// The world's sound points: waterfalls rumbling, wind in the catwalks,
/// branches creaking, flies, fires.
#[derive(Resource, Default)]
struct SoundPoints {
    points: Vec<(u32, Vec3)>,
    /// Seconds to each one-off point's next sounding.
    next: Vec<f32>,
}

/// Sound points are heard within this range (m), louder nearer.
const POINT_RANGE: f32 = 45.0;

fn load_sound_points(mut commands: Commands, info: Res<CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::SOUND_POINTS_REL)).unwrap_or_default();
    let points: Vec<(u32, Vec3)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let event = u32::from_str_radix(it.next()?, 16).ok()?;
            let mut v = [0.0f32; 3];
            for c in &mut v {
                *c = it.next()?.parse().ok()?;
            }
            Some((event, Vec3::from_array(v)))
        })
        .collect();
    info!("{} sound points", points.len());
    let next = vec![0.0; points.len()];
    commands.insert_resource(SoundPoints { points, next });
}

/// The sound points in earshot (as far as their sounds carry, else
/// [`POINT_RANGE`] and fading by ours): loops kept running, one-offs
/// sounded every few seconds.
fn sound_points(
    time: Res<Time>,
    bank: Option<Res<SoundBank>>,
    points: Option<ResMut<SoundPoints>>,
    listener: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    mut rng: ResMut<Rng>,
    mut loops: MessageWriter<LoopSfx>,
    mut shots: MessageWriter<PlaySfx>,
) {
    let (Some(bank), Some(mut points), Ok(ear)) = (bank, points, listener.single()) else { return };
    let ear = ear.translation();
    let dt = time.delta_secs();
    let SoundPoints { points, next } = &mut *points;
    for (i, (event, at)) in points.iter().enumerate() {
        let d = at.distance(ear);
        let reach = bank.reach(*event);
        if d > reach.unwrap_or(POINT_RANGE) {
            continue;
        }
        let near = if reach.is_some() { 1.0 } else { 1.0 - d / POINT_RANGE };
        if bank.loops(*event) {
            loops.write(LoopSfx { key: 0x5350_0000_0000_0000 | i as u64, event: *event, at: Some(*at), volume: near * near, ..default() });
        } else {
            next[i] -= dt;
            if next[i] <= 0.0 {
                next[i] = rng.range(5.0, 14.0);
                shots.write(PlaySfx { event: *event, at: Some(*at), volume: near });
            }
        }
    }
}

fn attach_listener(mut commands: Commands, cams: Query<Entity, With<crate::player::PlayerCamera>>) {
    for c in &cams {
        commands.entity(c).insert(SpatialListener::new(0.25));
    }
}

fn settings(mode: PlaybackMode, volume: f32, spatial: bool) -> PlaybackSettings {
    PlaybackSettings { mode, volume: Volume::Linear(volume), spatial, spatial_scale: Some(SpatialScale::new(SPATIAL_SCALE)), ..default() }
}

fn play_one_shots(
    mut events: MessageReader<PlaySfx>,
    (bank, mut mixer): (Option<Res<SoundBank>>, ResMut<Mixer>),
    server: Res<AssetServer>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    listener: Query<&GlobalTransform, With<SpatialListener>>,
) {
    let Some(bank) = bank else { return };
    let ear = listener.single().ok().map(|t| t.translation());
    for ev in events.read() {
        // (Its pushes and pops of mixer states.)
        mixer.apply(&bank.mix, ev.event);
        let (files, vol) = bank.resolve_case(&loop_token(ev.event, &[]), ev.event, &mut rng);
        let d = match (ev.at, ear) {
            (Some(at), Some(ear)) => at.distance(ear),
            _ => 0.0,
        };
        // FC3_SFX_DEBUG: what each one-shot resolves to.
        if std::env::var("FC3_SFX_DEBUG").is_ok() {
            info!("sfx {:08x}: {} file(s) at {:?}, {d:.0} m", ev.event, files.len(), ev.at.map(|a| a.round()));
        }
        for (f, blend, _, falloff, gain, bus) in files {
            let heard = Heard { gain: vol * gain, volume: ev.volume, falloff, blend, params: [(0, 0.0); 4], looping: false, placed: ev.at.is_some(), bus };
            let (v, speed) = heard.at(d);
            if std::env::var("FC3_SFX_DEBUG").is_ok() {
                info!("  {f} at {v:.3}");
            }
            commands.spawn((
                AudioPlayer::<Sfx>(server.load(cache_path(&f))),
                PlaybackSettings { speed, ..settings(PlaybackMode::Despawn, v, ev.at.is_some()) },
                heard,
                Transform::from_translation(ev.at.unwrap_or(Vec3::ZERO)),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn update_loops(
    time: Res<Time>,
    mut events: MessageReader<LoopSfx>,
    bank: Option<Res<SoundBank>>,
    server: Res<AssetServer>,
    mut rng: ResMut<Rng>,
    mut commands: Commands,
    mut emitters: Query<(Entity, &mut LoopEmitter, &mut Transform, Option<&Children>)>,
    mut voices: Query<&mut Heard>,
    listener: Query<&GlobalTransform, With<SpatialListener>>,
    mut mixer: ResMut<Mixer>,
) {
    let Some(bank) = bank else { return };
    let ear = listener.single().ok().map(|t| t.translation());
    for ev in events.read() {
        let token = loop_token(ev.event, &ev.switches);
        if let Some((e, mut em, mut t, children)) = emitters.iter_mut().find(|(_, em, _, _)| em.key == ev.key) {
            if em.token == token {
                em.ttl = LOOP_TTL;
                if let Some(at) = ev.at {
                    t.translation = at;
                }
                // Follow volume (fades) and parameter changes.
                if (em.volume - ev.volume).abs() > 0.01 || em.params != ev.params {
                    em.volume = ev.volume;
                    em.params = ev.params;
                    for c in children.into_iter().flatten() {
                        if let Ok(mut heard) = voices.get_mut(*c) {
                            heard.volume = ev.volume;
                            heard.params = ev.params;
                        }
                    }
                }
                continue;
            }
            // A new switch state (another surface under the tyres): start over.
            em.key = u64::MAX;
            commands.entity(e).despawn();
        }
        mixer.apply(&bank.mix, ev.event);
        let (files, vol) = bank.resolve_case(&token, ev.event, &mut rng);
        if std::env::var("FC3_SFX_DEBUG").is_ok() {
            info!("loop {token}: {} file(s) at {:?}", files.len(), ev.at.map(|a| a.round()));
        }
        if files.is_empty() {
            continue;
        }
        let root = commands
            .spawn((
                LoopEmitter { key: ev.key, ttl: LOOP_TTL, volume: ev.volume, token, params: ev.params },
                Transform::from_translation(ev.at.unwrap_or(Vec3::ZERO)),
                Visibility::Visible,
            ))
            .id();
        // Looping voices run while the loop is refreshed; the rest (a splash
        // going under, an engine catching) play once as it starts.
        let d = match (ev.at, ear) {
            (Some(at), Some(ear)) => at.distance(ear),
            _ => 0.0,
        };
        for (f, blend, looping, falloff, gain, bus) in files {
            let heard = Heard { gain: vol * gain, volume: ev.volume, falloff, blend, params: ev.params, looping: true, placed: ev.at.is_some(), bus };
            let (v, speed) = heard.at(d);
            if std::env::var("FC3_SFX_DEBUG").is_ok() {
                info!("  {f} at {v:.3}, {d:.0} m");
            }
            let mode = if looping { PlaybackMode::Loop } else { PlaybackMode::Despawn };
            commands.spawn((
                AudioPlayer::<Sfx>(server.load(cache_path(&f))),
                PlaybackSettings { speed, ..settings(mode, v, ev.at.is_some()) },
                heard,
                Transform::IDENTITY,
                ChildOf(root),
            ));
        }
    }
    let dt = time.delta_secs().min(LOOP_TTL_STEP);
    for (e, mut em, _, _) in &mut emitters {
        em.ttl -= dt;
        if em.ttl <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// The island's sound regions (`soundregions_default.obj`): each has a
/// layered bed and pools of one-off calls (birds, insects, animals) for day
/// and night. Where the game paints them isn't recovered, so the region
/// comes from the ground: the terrain layer underfoot, the shore, the
/// heights, the open sea.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Region {
    Jungle,
    Forest,
    Field,
    Beach,
    Ocean,
    Peak,
}

impl Region {
    /// (bed, day calls, night calls); 0 = none.
    fn sounds(self) -> (u32, u32, u32) {
        match self {
            Region::Jungle => (0x0011_33BD, 0x0015_9609, 0x000A_6BA5),
            Region::Forest => (0x0000_585E, 0x0006_1874, 0x000A_6BA5),
            Region::Field => (0x0006_1A27, 0x0011_35AA, 0x000A_6BA4),
            Region::Beach => (0x0000_7874, 0x0006_182C, 0x0015_9385),
            Region::Ocean => (0x0001_74E1, 0, 0),
            Region::Peak => (0x0011_3374, 0, 0),
        }
    }

    fn at(ground: f32, layer: Option<&str>, on_water: bool) -> Region {
        use crate::sky::SEA_LEVEL;
        if on_water && ground < SEA_LEVEL - 4.0 {
            return Region::Ocean;
        }
        if ground < SEA_LEVEL + 5.0 {
            return Region::Beach;
        }
        if ground > SEA_LEVEL + 230.0 {
            return Region::Peak;
        }
        let l = layer.unwrap_or("").to_ascii_lowercase();
        if l.contains("sand") {
            Region::Beach
        } else if l.starts_with("south") || l.contains("grassland") {
            Region::Field
        } else if l.contains("jungle") || l.contains("deadleaves") || l.contains("bamboo") {
            Region::Jungle
        } else if l.contains("coastal") || l.contains("cliff") {
            Region::Forest
        } else {
            Region::Jungle
        }
    }
}

/// Region beds fade into each other at this rate (per second).
const REGION_FADE: f32 = 0.35;

/// A roof this close over the player's head (m) means indoors, where the
/// outdoor ambience drops to this share.
const INDOOR_ROOF: f32 = 8.0;
const INDOOR_LEVEL: f32 = 0.3;

/// Background jungle loop plus occasional birdsong and surf near the coast.
#[allow(clippy::too_many_arguments)]
fn ambience(
    time: Res<Time>,
    state: Res<State<crate::menu::GameState>>,
    players: Query<(Entity, &Transform, &crate::player::Player)>,
    heights: Res<crate::terrain::Heightfield>,
    spatial: avian3d::prelude::SpatialQuery,
    mut loops: MessageWriter<LoopSfx>,
    mut sfx: MessageWriter<PlaySfx>,
    (mut next_bird, mut outdoors, mut beds): (Local<f32>, Local<Option<f32>>, Local<Vec<(u32, f32)>>),
    mut rng: ResMut<Rng>,
    (probe, tod, driving): (crate::surfaces::SurfaceProbe, Res<crate::sky::TimeOfDay>, Res<crate::vehicles::Driving>),
) {
    if *state.get() == crate::menu::GameState::Paused {
        return;
    }
    let Ok((pe, p, player)) = players.single() else { return };
    // Not while the player is being put on the ground.
    if player.spawning {
        return;
    }
    // No terrain loaded under the player: out at sea.
    let ground = heights.height_at(p.translation).unwrap_or(crate::sky::SEA_LEVEL - 50.0);
    let near_sea = ground < crate::sky::SEA_LEVEL + 6.0;
    // Under a roof (a hut, a cave), the outside is muffled.
    let filter = avian3d::prelude::SpatialQueryFilter::from_excluded_entities([pe]);
    let roofed = spatial.cast_ray(p.translation + Vec3::Y * 0.8, Dir3::Y, INDOOR_ROOF, true, &filter).is_some();
    let out = outdoors.get_or_insert(1.0);
    let target = if roofed { INDOOR_LEVEL } else { 1.0 };
    *out += (target - *out) * (time.delta_secs() * 2.0).min(1.0);
    let out = *out;
    // The region's bed, faded in as the last one fades out.
    let region = Region::at(ground, probe.terrain_layer(p.translation), driving.vehicle.is_some() || p.translation.y < crate::sky::SEA_LEVEL + 3.0);
    let (bed, day_calls, night_calls) = region.sounds();
    let dt = time.delta_secs();
    if !beds.iter().any(|b| b.0 == bed) {
        beds.push((bed, 0.0));
    }
    for (event, level) in beds.iter_mut() {
        let target = if *event == bed { 1.0 } else { 0.0 };
        *level = (*level + (target - *level).signum() * REGION_FADE * dt).clamp(0.0, 1.0);
    }
    beds.retain(|b| b.1 > 0.0 || b.0 == bed);
    for &(event, level) in beds.iter() {
        if level > 0.01 {
            loops.write(LoopSfx { key: 0x5245_0000_0000_0000 | event as u64, event, at: None, volume: out * level * 0.35, ..default() });
        }
    }
    if near_sea {
        loops.write(LoopSfx { key: 2, event: ev::WAVES, at: None, volume: out * 0.4, ..default() });
    }
    // Its calls now and then, somewhere round about; the night has its own.
    let calls = if tod.daylight() > 0.3 { day_calls } else { night_calls };
    *next_bird -= dt;
    if *next_bird <= 0.0 && !roofed {
        *next_bird = rng.range(4.0, 12.0);
        if calls != 0 {
            let offset = Vec3::new(rng.range(-30.0, 30.0), rng.range(5.0, 15.0), rng.range(-30.0, 30.0));
            sfx.write(PlaySfx { event: calls, at: Some(p.translation + offset), volume: 0.5 });
        }
    }
    if std::env::var("FC3_REGION_DEBUG").is_ok() && (time.elapsed_secs() % 2.0) < dt {
        info!("sound region {region:?} (layer {:?}, ground {ground:.0})", probe.terrain_layer(p.translation));
    }
}
