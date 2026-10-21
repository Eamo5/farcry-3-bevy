//! The game's sound mixings (`soundmixing.lib`): named mixes the game and
//! its scripts start and stop (the pause, being under water, a mission's
//! moment), each by events that push and pop mixer states (class 10). A
//! state sets parameters of the mixer's buses, fading to them and back:
//! their levels (0 and 2, dB) and commands (13 pause, 14 resume, 15 stop).
//! Each playing sound follows its bus and the buses above it
//! (`sound::hear`). A state also turns on the low-pass units on buses (under
//! water: 800 Hz), which the sounds below them run through as they play
//! (`sound_source`). The reverb unit (the reverb return's) takes the preset
//! the room the listener is in pushes (its `sndReverb`), else the sound
//! regions' (outdoors), and each bus sends its sounds to it (parameter 4,
//! dB); the sounds ring it themselves (`sound_source::Reverb`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use bevy::prelude::*;

use crate::menu::GameState;

/// The mixer as the manifest has it (see `fc3_formats::audio::SoundGraph`'s
/// mixer records).
#[derive(Default)]
pub struct MixData {
    /// Each bus's parent and own levels (dB).
    buses: HashMap<u32, (u32, f32, f32)>,
    /// Each state's bus changes: (bus, parameter, value, fade in, fade out).
    states: HashMap<u32, Vec<(u32, u32, f32, f32, f32)>>,
    /// The states each event pushes (true) and pops.
    actions: HashMap<u32, Vec<(bool, u32)>>,
    /// The mixings by name: their set and remove events (0: none); and the
    /// names by key (what rooms name them by).
    mixings: HashMap<String, (u32, u32)>,
    keys: HashMap<u64, String>,
    /// Each bus's filter units (low- and high-pass), and each unit's slot
    /// (see [`CUTOFFS`]).
    lowpass: HashMap<u32, Vec<u32>>,
    slots: HashMap<u32, u8>,
    high: HashSet<u32>,
    /// Each state's changes to effect units, like `states`.
    effects: HashMap<u32, Vec<(u32, u32, f32, f32, f32)>>,
    /// The reverb unit and its parameters' defaults; and the buses that
    /// send to it (a state sets their parameter 4), by slot (see [`SENDS`]).
    reverb: Option<(u32, Vec<(u32, f32)>)>,
    send_slots: HashMap<u32, u8>,
}

/// The reverb's parameters as the latest preset sets them (`f32` bits), and
/// whether one is on; for the playing sounds to read. By index: 0 on, 1 the
/// early reflections' delay (ms), 2 the tail's level (dB), 3 density (%),
/// 4 decay (s), 5 the treble's decay against the rest (%), 6 where the
/// treble starts (Hz), 7 the tail's delay (ms), 8 the dry level (dB), 9
/// diffusion (%), 12 the early reflections' level (dB). (Read from how they
/// vary between presets: a cave's early reflections come 2 to 7 ms on at
/// -2 dB, the jungle's 100 ms on at -20 dB, its treble from 4.8 kHz.)
pub const REVERB_PARAMS: usize = 16;
static REVERB: [AtomicU32; REVERB_PARAMS] = [const { AtomicU32::new(0) }; REVERB_PARAMS];
static REVERB_ON: AtomicBool = AtomicBool::new(false);
/// The listener is enclosed: in a room, cave or bunker with a reverb preset
/// of its own. Out in the open the reverb rests: the open-air presets (all
/// buses sent at 0 dB into a 5.5 s tail, read from how presets differ, not
/// from the engine) washed every shot and footstep, whose own echo is
/// authored into the sounds already (the far layers of their blends).
static ENCLOSED: AtomicBool = AtomicBool::new(false);
/// Each sending bus's send now (linear; 0: none), by slot.
static SENDS: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
/// The send slots on each sound's way to the mix, by file stem.
static SEND_SLOTS: LazyLock<RwLock<HashMap<String, Arc<[u8]>>>> = LazyLock::new(Default::default);

/// The reverb's parameters now, if a preset has it on (`FC3_REVERB=0`
/// turns it off).
pub fn reverb() -> Option<[f32; REVERB_PARAMS]> {
    if !REVERB_ON.load(Ordering::Relaxed) || !ENCLOSED.load(Ordering::Relaxed) {
        return None;
    }
    Some(std::array::from_fn(|i| f32::from_bits(REVERB[i].load(Ordering::Relaxed))))
}

/// A sound's send to the reverb now (linear): its buses' strongest.
pub fn send(slots: &[u8]) -> f32 {
    slots.iter().map(|&s| f32::from_bits(SENDS[s as usize].load(Ordering::Relaxed))).fold(0.0, f32::max)
}

/// The send slots a sound file goes through (none until the manifest is
/// read).
pub fn sends_of(stem: &str) -> Arc<[u8]> {
    SEND_SLOTS.read().ok().and_then(|m| m.get(stem).cloned()).unwrap_or_else(|| Arc::from([]))
}

/// The low-pass units' cutoffs now (Hz as `f32` bits; 0: open), by slot,
/// for the playing sounds to read.
const SLOTS: usize = 64;
static CUTOFFS: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
/// Which slots are high-passes (the rest low-passes).
static HIGH: [AtomicBool; SLOTS] = [const { AtomicBool::new(false) }; SLOTS];

/// The low-pass slots on each sound's way to the mix (its bus's and those
/// above it), by file stem, for the sound loader.
static FILTERS: LazyLock<RwLock<HashMap<String, Arc<[u8]>>>> = LazyLock::new(Default::default);

/// The low-pass slots a sound file goes through (none until the manifest is
/// read).
pub fn filters_of(stem: &str) -> Arc<[u8]> {
    FILTERS.read().ok().and_then(|m| m.get(stem).cloned()).unwrap_or_else(|| Arc::from([]))
}

/// The cutoffs (Hz) for a sound going through `slots`: the lowest low-pass
/// on and the highest high-pass on, if any.
pub fn cutoffs(slots: &[u8]) -> (Option<f32>, Option<f32>) {
    let (mut low, mut high): (Option<f32>, Option<f32>) = (None, None);
    for &s in slots {
        let hz = f32::from_bits(CUTOFFS[s as usize].load(Ordering::Relaxed));
        if hz > 0.0 {
            if HIGH[s as usize].load(Ordering::Relaxed) {
                high = Some(high.map_or(hz, |h| h.max(hz)));
            } else {
                low = Some(low.map_or(hz, |l| l.min(hz)));
            }
        }
    }
    (low, high)
}

/// A low-pass unit's cutoff when open, and a high-pass unit's (Hz).
const OPEN: f32 = 20_000.0;
const OPEN_HIGH: f32 = 20.0;

impl MixData {
    /// Reads one of the manifest's mixer records: its kind (`bus`,
    /// `mixstate`, `mixevent`, `mixing`) and the rest of its line.
    pub fn read(&mut self, kind: &str, rest: &str) {
        let hex = |s: &str| u32::from_str_radix(s, 16).ok();
        let mut it = rest.split_whitespace();
        match kind {
            "bus" => {
                let v: Vec<&str> = it.collect();
                if let [id, parent, a, b] = v[..] {
                    if let (Some(id), Some(parent), Ok(a), Ok(b)) = (hex(id), hex(parent), a.parse(), b.parse()) {
                        self.buses.insert(id, (parent, a, b));
                    }
                }
            }
            "fx" => {
                // unit, bus, kind (0 a low-pass, 2 a high-pass), default
                let v: Vec<&str> = it.collect();
                if let [unit, bus, kind @ ("0" | "2"), _] = v[..] {
                    if let (Some(unit), Some(bus)) = (hex(unit), hex(bus)) {
                        self.lowpass.entry(bus).or_default().push(unit);
                        let next = self.slots.len();
                        if next < SLOTS {
                            let slot = *self.slots.entry(unit).or_insert(next as u8);
                            if kind == "2" {
                                self.high.insert(unit);
                            }
                            HIGH[slot as usize].store(kind == "2", Ordering::Relaxed);
                        }
                    }
                }
            }
            "mixstate" | "mixfx" => {
                let Some(id) = it.next().and_then(hex) else { return };
                let sets = it
                    .filter_map(|t| {
                        let mut p = t.split(':');
                        Some((hex(p.next()?)?, p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?))
                    })
                    .collect();
                if kind == "mixfx" {
                    self.effects.insert(id, sets);
                } else {
                    // (A bus a state sets parameter 4 of sends to the reverb.)
                    for &(bus, param, ..) in &sets {
                        let next = self.send_slots.len();
                        if param == 4 && next < SLOTS {
                            self.send_slots.entry(bus).or_insert(next as u8);
                        }
                    }
                    self.states.insert(id, sets);
                }
            }
            "reverb" => {
                // unit, then parameter:default...
                let Some(unit) = it.next().and_then(hex) else { return };
                let params = it.filter_map(|t| t.split_once(':')).filter_map(|(p, v)| Some((p.parse().ok()?, v.parse().ok()?))).collect();
                self.reverb = Some((unit, params));
            }
            "mixevent" => {
                let Some(id) = it.next().and_then(hex) else { return };
                let actions = it.filter_map(|t| t.get(1..).and_then(hex).map(|state| (t.starts_with('+'), state))).collect();
                self.actions.insert(id, actions);
            }
            "mixing" => {
                // priority, stacking with itself and with others, set,
                // remove, the key (newer manifests), then the name (which
                // may have spaces).
                let parts: Vec<&str> = rest.splitn(7, ' ').collect();
                let (Some(set), Some(remove)) = (parts.get(3).and_then(|s| hex(s)), parts.get(4).and_then(|s| hex(s))) else { return };
                let key = parts.get(5).filter(|k| k.len() == 16).and_then(|k| u64::from_str_radix(k, 16).ok());
                let name = if key.is_some() { parts.get(6).map(|n| n.to_string()) } else { rest.splitn(6, ' ').nth(5).map(|n| n.to_string()) };
                let Some(name) = name.map(|n| n.trim().to_string()) else { return };
                let event = |e: u32| if e == u32::MAX { 0 } else { e };
                self.mixings.insert(name.clone(), (event(set), event(remove)));
                if let Some(key) = key {
                    self.keys.insert(key, name);
                }
            }
            _ => {}
        }
    }

    /// A bus parameter's level when no state sets it (a bus muted outright
    /// counts as 0 dB, as in the sounds' gains: a state raises it).
    fn base(&self, bus: u32, param: u32) -> f32 {
        let Some(&(_, a, b)) = self.buses.get(&bus) else { return 0.0 };
        match param {
            0 if a > -90.0 => a,
            2 => b,
            _ => 0.0,
        }
    }

    /// The events that set and remove a mixing, by its name (or `#` and
    /// its key in hex, as rooms name it).
    pub fn mixing(&self, name: &str) -> Option<(u32, u32)> {
        let name = match name.strip_prefix('#').and_then(|k| u64::from_str_radix(k, 16).ok()) {
            Some(key) => self.keys.get(&key)?.as_str(),
            None => name,
        };
        self.mixings.get(name).copied()
    }

    /// Hands the sound loader each file's low-pass slots (by its bus).
    pub fn publish_filters<'a>(&self, files: impl Iterator<Item = (&'a String, Option<u32>)>) {
        let mut by_bus: HashMap<u32, Arc<[u8]>> = HashMap::new();
        let mut out = HashMap::new();
        for (stem, bus) in files {
            let Some(bus) = bus else { continue };
            let slots = by_bus
                .entry(bus)
                .or_insert_with(|| {
                    let mut slots = Vec::new();
                    let mut at = Some(bus);
                    for _ in 0..16 {
                        let Some(b) = at else { break };
                        slots.extend(self.lowpass.get(&b).into_iter().flatten().filter_map(|u| self.slots.get(u)));
                        at = self.buses.get(&b).map(|x| x.0).filter(|p| *p != b);
                    }
                    slots.into()
                })
                .clone();
            if !slots.is_empty() {
                out.insert(stem.clone(), slots);
            }
        }
        if let Ok(mut m) = FILTERS.write() {
            *m = out;
        }
    }

    /// Hands the sound loader each file's send slots (by its bus).
    pub fn publish_sends<'a>(&self, files: impl Iterator<Item = (&'a String, Option<u32>)>) {
        let mut by_bus: HashMap<u32, Arc<[u8]>> = HashMap::new();
        let mut out = HashMap::new();
        for (stem, bus) in files {
            let Some(bus) = bus else { continue };
            let slots = by_bus
                .entry(bus)
                .or_insert_with(|| {
                    let mut slots = Vec::new();
                    let mut at = Some(bus);
                    for _ in 0..16 {
                        let Some(b) = at else { break };
                        slots.extend(self.send_slots.get(&b));
                        at = self.buses.get(&b).map(|x| x.0).filter(|p| *p != b);
                    }
                    slots.into()
                })
                .clone();
            if !slots.is_empty() {
                out.insert(stem.clone(), slots);
            }
        }
        if let Ok(mut m) = SEND_SLOTS.write() {
            *m = out;
        }
    }
}

/// Mixings asked for by name (started, or stopped), from the game and its
/// scripts, and sounds to play with them (a room's going in and out); the
/// sound module plays their events.
#[derive(Resource, Default)]
pub struct Mixings(Vec<(String, bool)>, Vec<u32>);

impl Mixings {
    pub fn start(&mut self, name: &str) {
        self.0.push((name.to_string(), true));
    }

    pub fn stop(&mut self, name: &str) {
        self.0.push((name.to_string(), false));
    }

    pub fn take(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.0)
    }

    pub fn take_sounds(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.1)
    }
}

/// The world's rooms (`fc3_formats::rooms`), by 64 m cell, and the one the
/// listener is in.
#[derive(Resource, Default)]
pub struct Rooms {
    rooms: Vec<fc3_formats::rooms::Room>,
    cells: HashMap<(i32, i32), Vec<usize>>,
    inside: Option<usize>,
    /// The ambience regions' beds, by index.
    beds: HashMap<u32, u32>,
    /// The reverb preset outdoors (the one most of the sound regions have),
    /// and the one pushed now.
    outdoors: u32,
    reverb: Option<u32>,
}

impl Rooms {
    /// The region of the room the listener is in, where it has one of its
    /// own (a cave, a bunker).
    pub fn region(&self) -> Option<u32> {
        self.rooms.get(self.inside?)?.region
    }

    /// The bed of the room the listener is in, where it has a region of its
    /// own (a cave, a bunker).
    pub fn bed(&self) -> Option<u32> {
        let region = self.rooms.get(self.inside?)?.region?;
        self.beds.get(&region).copied().filter(|b| *b != 0)
    }
}

const ROOM_CELL: f32 = 64.0;

pub fn load_rooms(mut commands: Commands, info: Res<crate::data::CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::ROOMS_REL)).unwrap_or_default();
    let rooms: Vec<fc3_formats::rooms::Room> = text.lines().filter_map(fc3_formats::rooms::Room::from_line).collect();
    let regions: Vec<fc3_formats::rooms::SoundRegion> = text.lines().filter_map(fc3_formats::rooms::SoundRegion::from_line).collect();
    let beds = regions.iter().map(|r| (r.index, r.bed)).collect();
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for r in regions.iter().filter(|r| r.reverb != 0 && r.reverb != u32::MAX) {
        *counts.entry(r.reverb).or_default() += 1;
    }
    let outdoors = counts.into_iter().max_by_key(|(r, n)| (*n, *r)).map_or(0, |(r, _)| r);
    let mut cells: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, r) in rooms.iter().enumerate() {
        let cell = |v: f32| (v / ROOM_CELL).floor() as i32;
        for x in cell(r.min[0])..=cell(r.max[0]) {
            for z in cell(r.min[2])..=cell(r.max[2]) {
                cells.entry((x, z)).or_default().push(i);
            }
        }
    }
    info!("{} rooms", rooms.len());
    commands.insert_resource(Rooms { rooms, cells, inside: None, beds, outdoors, reverb: None });
}

/// A room's mixing (`#` and its key), unless it has none.
fn room_mixing_name(r: &fc3_formats::rooms::Room) -> Option<String> {
    (r.mixing != 0 && r.mixing != u64::from(u32::MAX)).then(|| format!("#{:016x}", r.mixing))
}

/// The room the listener is in (the smallest of those it is in): its
/// mixing on while inside, and its sounds of going in and out.
pub fn room_mixings(
    rooms: Option<ResMut<Rooms>>,
    listener: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    mut mixings: ResMut<Mixings>,
    regions: Option<Res<crate::sound::SoundRegions>>,
) {
    let (Some(mut rooms), Ok(ear)) = (rooms, listener.single()) else { return };
    let p = ear.translation().to_array();
    let cell = ((p[0] / ROOM_CELL).floor() as i32, (p[2] / ROOM_CELL).floor() as i32);
    let now = rooms
        .cells
        .get(&cell)
        .into_iter()
        .flatten()
        .copied()
        .filter(|&i| rooms.rooms[i].contains(p))
        .min_by(|&a, &b| rooms.rooms[a].size().total_cmp(&rooms.rooms[b].size()));
    // The reverb preset: the room's own while inside one that has it, else
    // the sound region's the listener is in, else the outdoors' (its event
    // pushes the mixer's reverb state; the latest pushed wins).
    let set = |r: &u32| *r != 0 && *r != u32::MAX;
    let outside = regions.as_deref().and_then(|r| r.here()).map(|r| r.reverb).filter(set).unwrap_or(rooms.outdoors);
    let own = now.map(|i| rooms.rooms[i].reverb).filter(set);
    ENCLOSED.store(own.is_some(), Ordering::Relaxed);
    let reverb = own.unwrap_or(outside);
    if reverb != 0 && rooms.reverb != Some(reverb) {
        debug!("room: reverb preset {reverb:08x}");
        mixings.1.push(reverb);
        rooms.reverb = Some(reverb);
    }
    if now == rooms.inside {
        return;
    }
    let (was, is) = (rooms.inside.map(|i| &rooms.rooms[i]), now.map(|i| &rooms.rooms[i]));
    let (was_mix, is_mix) = (was.and_then(room_mixing_name), is.and_then(room_mixing_name));
    if was_mix != is_mix {
        if let Some(m) = &was_mix {
            mixings.stop(m);
        }
        if let Some(m) = &is_mix {
            mixings.start(m);
        }
    }
    mixings.1.extend(was.map(|r| r.exit).into_iter().chain(is.map(|r| r.enter)).filter(|e| *e != 0));
    debug!("room: {:?} -> {:?} ({was_mix:?} -> {is_mix:?})", rooms.inside, now);
    rooms.inside = now;
}

/// The states pushed (the latest last), each bus level's way to what they
/// set, and the buses whose sounds are paused (or were stopped this frame).
#[derive(Resource, Default)]
pub struct Mixer {
    stack: Vec<u32>,
    levels: HashMap<(u32, u32), Ramp>,
    /// The low-pass units' cutoffs (log2 Hz) on their way.
    cutoffs: HashMap<u32, Ramp>,
    paused: HashSet<u32>,
    stopped: HashSet<u32>,
}

/// A level moving from one value to another over `len` s; `back` is how
/// long it takes to return once the state setting it goes.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    now: f32,
    from: f32,
    to: f32,
    t: f32,
    len: f32,
    back: f32,
}

impl Mixer {
    /// The states `event` pushes and pops, and its commands to the buses.
    pub fn apply(&mut self, mix: &MixData, event: u32) {
        for &(push, state) in mix.actions.get(&event).into_iter().flatten() {
            self.stack.retain(|s| *s != state);
            let sets = mix.states.get(&state).into_iter().flatten();
            if push {
                self.stack.push(state);
                for &(bus, param, ..) in sets {
                    match param {
                        13 => {
                            self.paused.insert(bus);
                        }
                        14 => {
                            self.paused.remove(&bus);
                        }
                        15 => {
                            self.stopped.insert(bus);
                        }
                        _ => {}
                    }
                }
            } else {
                for &(bus, ..) in sets.filter(|s| s.1 == 13) {
                    self.paused.remove(&bus);
                }
            }
            debug!("mixer: {} {state:08x} (event {event:08x})", if push { "push" } else { "pop" });
        }
    }

    /// Moves each bus level toward what the states set (the latest state
    /// wins), over their fades.
    fn update(&mut self, mix: &MixData, dt: f32) {
        let mut want: HashMap<(u32, u32), (f32, f32, f32)> = HashMap::new();
        for state in &self.stack {
            for &(bus, param, value, fade_in, fade_out) in mix.states.get(state).into_iter().flatten() {
                if param == 0 || param == 2 {
                    want.insert((bus, param), (value, fade_in, fade_out));
                }
            }
        }
        let keys: HashSet<(u32, u32)> = self.levels.keys().chain(want.keys()).copied().collect();
        for key in keys {
            let base = mix.base(key.0, key.1);
            let r = self.levels.entry(key).or_insert(Ramp { now: base, from: base, to: base, t: 0.0, len: 0.0, back: 0.0 });
            let (to, len) = match want.get(&key) {
                Some(&(value, fade_in, fade_out)) => {
                    r.back = fade_out;
                    (value, fade_in)
                }
                None => (base, r.back),
            };
            if (r.to - to).abs() > 1e-4 {
                *r = Ramp { from: r.now, to, t: 0.0, len, ..*r };
            }
            r.t += dt;
            r.now = if r.len <= 0.0 || r.t >= r.len { r.to } else { r.from + (r.to - r.from) * r.t / r.len };
        }
        self.levels.retain(|k, r| want.contains_key(k) || (r.now - mix.base(k.0, k.1)).abs() > 1e-4);

        // The filter units: on (parameter 0) at a cutoff (1) as the latest
        // state has them, else open.
        let open_of = |unit: u32| if mix.high.contains(&unit) { OPEN_HIGH.log2() } else { OPEN.log2() };
        let mut cut: HashMap<u32, (Option<bool>, Option<f32>, f32, f32)> = HashMap::new();
        for state in &self.stack {
            for &(unit, param, value, fade_in, fade_out) in mix.effects.get(state).into_iter().flatten() {
                if !mix.slots.contains_key(&unit) {
                    continue;
                }
                let e = cut.entry(unit).or_insert((None, None, 0.0, 0.0));
                match param {
                    0 => e.0 = Some(value > 0.5),
                    1 => e.1 = Some(value),
                    _ => continue,
                }
                (e.2, e.3) = (fade_in, fade_out);
            }
        }
        let units: HashSet<u32> = self.cutoffs.keys().chain(cut.keys()).copied().collect();
        for unit in units {
            let open = open_of(unit);
            let r = self.cutoffs.entry(unit).or_insert(Ramp { now: open, from: open, to: open, t: 0.0, len: 0.0, back: 0.0 });
            let (to, len) = match cut.get(&unit) {
                Some(&(Some(true), Some(hz), fade_in, fade_out)) => {
                    r.back = fade_out;
                    (hz.clamp(OPEN_HIGH, OPEN).log2(), fade_in)
                }
                _ => (open, r.back),
            };
            if (r.to - to).abs() > 1e-4 {
                *r = Ramp { from: r.now, to, t: 0.0, len, ..*r };
            }
            r.t += dt;
            r.now = if r.len <= 0.0 || r.t >= r.len { r.to } else { r.from + (r.to - r.from) * r.t / r.len };
            if let Some(&slot) = mix.slots.get(&unit) {
                let hz = if (r.now - open).abs() < 0.01 { 0.0 } else { r.now.exp2() };
                CUTOFFS[slot as usize].store(hz.to_bits(), Ordering::Relaxed);
            }
        }
        self.cutoffs.retain(|u, r| cut.contains_key(u) || (r.now - open_of(*u)).abs() >= 0.01);

        // The reverb: its unit's parameters as the latest preset has them
        // (over its defaults), on once one has set them; and each bus's
        // send as the latest state sets it.
        if let Some((unit, defaults)) = &mix.reverb {
            let mut p = [0.0f32; REVERB_PARAMS];
            for &(i, v) in defaults.iter().filter(|(i, _)| (*i as usize) < REVERB_PARAMS) {
                p[i as usize] = v;
            }
            let mut set = false;
            for state in &self.stack {
                for &(u, param, value, ..) in mix.effects.get(state).into_iter().flatten() {
                    if u == *unit && (param as usize) < REVERB_PARAMS {
                        p[param as usize] = value;
                        set = true;
                    }
                }
            }
            for (slot, v) in REVERB.iter().zip(p) {
                slot.store(v.to_bits(), Ordering::Relaxed);
            }
            let off = std::env::var("FC3_REVERB").is_ok_and(|v| v == "0");
            REVERB_ON.store(set && p[0] > 0.5 && !off, Ordering::Relaxed);
        }
        let mut sends: HashMap<u32, f32> = HashMap::new();
        for state in &self.stack {
            for &(bus, param, value, ..) in mix.states.get(state).into_iter().flatten() {
                if param == 4 {
                    sends.insert(bus, value);
                }
            }
        }
        for (bus, &slot) in &mix.send_slots {
            let gain = sends.get(bus).map_or(0.0, |db| 10f32.powf(db / 20.0));
            SENDS[slot as usize].store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    /// The mixer's change to the sounds of `bus` (dB, beside the levels
    /// already in their gain), whether they're paused, and whether they were
    /// stopped.
    pub fn on_bus(&self, mix: &MixData, bus: u32) -> (f32, bool, bool) {
        let (mut db, mut paused, mut stopped) = (0.0, false, false);
        let mut at = Some(bus);
        for _ in 0..16 {
            let Some(b) = at else { break };
            paused |= self.paused.contains(&b);
            stopped |= self.stopped.contains(&b);
            for param in [0, 2] {
                if let Some(r) = self.levels.get(&(b, param)) {
                    db += r.now - mix.base(b, param);
                }
            }
            at = mix.buses.get(&b).map(|x| x.0).filter(|p| *p != b);
        }
        (db, paused, stopped)
    }

    /// The filter units turned on now, with their cutoffs (Hz).
    pub fn filters_on(&self) -> Vec<(u32, f32)> {
        self.cutoffs.iter().filter(|(_, r)| r.now < OPEN.log2() - 0.01 && r.now > OPEN_HIGH.log2() + 0.01).map(|(u, r)| (*u, r.now.exp2())).collect()
    }

    /// Back to the mix as authored, every state gone (a load, the main
    /// menu: nothing a story or a room pushed carries over).
    pub fn reset(&mut self) {
        *self = Mixer::default();
        for c in CUTOFFS.iter().chain(&SENDS) {
            c.store(0, Ordering::Relaxed);
        }
        REVERB_ON.store(false, Ordering::Relaxed);
    }

    /// Forgets this frame's stops (once every sound has heard them).
    pub fn clear_stops(&mut self) {
        self.stopped.clear();
    }

    /// Moves the levels on (real time: the pause's own fades run while the
    /// game is paused).
    pub fn advance(&mut self, mix: &MixData, dt: f32) {
        self.update(mix, dt);
    }
}

/// The game's own mixings: the pause (over the pause menu, the map, the
/// handbook and the shop), the player's head under water, health in its
/// last bar (the world muffled), riding a vehicle, and a fight (enemies in
/// combat with the player, as the music has it: the beds thinned).
/// Clears the mixer as a game loads or the main menu comes up (see
/// [`Mixer::reset`]), and the rooms' notion of where the listener is.
pub fn reset_mixer(mut mixer: ResMut<Mixer>, rooms: Option<ResMut<Rooms>>) {
    mixer.reset();
    if let Some(mut rooms) = rooms {
        (rooms.inside, rooms.reverb) = (None, None);
    }
}

/// `FC3_MIXING_TEST=<name>[,<seconds>]` (tests) starts a mixing that many
/// seconds into play (default 10).
pub fn game_mixings(
    state: Res<State<GameState>>,
    breath: Option<Res<crate::syringes::Breath>>,
    (time, mut test): (Res<Time>, Local<Option<f32>>),
    (players, driving, music): (
        Query<&crate::combat::Health, With<crate::player::Player>>,
        Option<Res<crate::vehicles::Driving>>,
        Option<Res<crate::music::Music>>,
    ),
    mut on: Local<[bool; 5]>,
    mut mixings: ResMut<Mixings>,
) {
    if let Ok(spec) = std::env::var("FC3_MIXING_TEST") {
        let (name, after) = spec.split_once(',').map_or((spec.as_str(), 10.0), |(n, s)| (n, s.parse().unwrap_or(10.0)));
        if *state.get() == GameState::Playing && test.is_some_and(|t| t >= 0.0) {
            let t = test.get_or_insert(0.0);
            *t += time.delta_secs();
            if *t >= after {
                info!("FC3_MIXING_TEST: {name}");
                mixings.start(name);
                *t = -1.0;
            }
        } else if test.is_none() && *state.get() == GameState::Playing {
            *test = Some(0.0);
        }
    }
    let playing = *state.get() == GameState::Playing;
    let now = [
        matches!(state.get(), GameState::Paused | GameState::Map | GameState::Journal | GameState::Shop),
        breath.is_some_and(|b| b.under),
        playing && players.single().is_ok_and(|h| !h.dead && h.current < crate::combat::HEALTH_SEGMENT),
        playing && driving.is_some_and(|d| d.vehicle.is_some()),
        playing && music.is_some_and(|m| m.in_combat()),
    ];
    for (i, name) in ["Exclusive/Pause", "Exclusive/Underwater", "Compatible/Health_Low", "Compatible/In_Vehicles", "Compatible/Combat"].into_iter().enumerate()
    {
        if now[i] != on[i] {
            if now[i] {
                mixings.start(name);
            } else {
                mixings.stop(name);
            }
        }
    }
    *on = now;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> MixData {
        let mut m = MixData::default();
        m.read("bus", "00005000 00004000 -2 0");
        m.read("bus", "00005001 00005000 0 -3");
        // Duck the parent bus over a second (back over two); pause the child.
        m.read("mixstate", "00000100 00005000:0:-12:1:2 00005001:13:1:0:0");
        m.read("mixevent", "00000010 +00000100");
        m.read("mixevent", "00000011 -00000100");
        m.read("mixing", "1 1 1 00000010 00000011 Exclusive/Some Name");
        m
    }

    #[test]
    fn states_fade_bus_levels_and_pause_their_sounds() {
        let mix = data();
        assert_eq!(mix.mixing("Exclusive/Some Name"), Some((0x10, 0x11)));
        let mut mixer = Mixer::default();
        mixer.apply(&mix, 0x10);
        mixer.advance(&mix, 0.5);
        // Halfway from -2 to -12 on the parent: -5 dB on the child's sounds.
        let (db, paused, _) = mixer.on_bus(&mix, 0x5001);
        assert!((db + 5.0).abs() < 1e-3 && paused, "{db} {paused}");
        mixer.advance(&mix, 1.0);
        assert!((mixer.on_bus(&mix, 0x5001).0 + 10.0).abs() < 1e-3);
        // Popped: back over two seconds, and playing again.
        mixer.apply(&mix, 0x11);
        mixer.advance(&mix, 1.0);
        let (db, paused, _) = mixer.on_bus(&mix, 0x5001);
        assert!((db + 5.0).abs() < 1e-3 && !paused, "{db} {paused}");
        mixer.advance(&mix, 1.5);
        assert_eq!(mixer.on_bus(&mix, 0x5001).0, 0.0);
    }
}
