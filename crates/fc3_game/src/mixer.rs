//! The game's sound mixings (`soundmixing.lib`): named mixes the game and
//! its scripts start and stop (the pause, being under water, a mission's
//! moment), each by events that push and pop mixer states (class 10). A
//! state sets parameters of the mixer's buses, fading to them and back:
//! their levels (0 and 2, dB) and commands (13 pause, 14 resume, 15 stop).
//! Each playing sound follows its bus and the buses above it
//! (`sound::hear`). The states' effect units (filters) aren't applied.

use std::collections::{HashMap, HashSet};

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
    /// The mixings by name: their set and remove events (0: none).
    mixings: HashMap<String, (u32, u32)>,
}

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
            "mixstate" => {
                let Some(id) = it.next().and_then(hex) else { return };
                let sets = it
                    .filter_map(|t| {
                        let mut p = t.split(':');
                        Some((hex(p.next()?)?, p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?))
                    })
                    .collect();
                self.states.insert(id, sets);
            }
            "mixevent" => {
                let Some(id) = it.next().and_then(hex) else { return };
                let actions = it.filter_map(|t| t.get(1..).and_then(hex).map(|state| (t.starts_with('+'), state))).collect();
                self.actions.insert(id, actions);
            }
            "mixing" => {
                // priority, stacking with itself and with others, set,
                // remove, then the name (which may have spaces).
                let parts: Vec<&str> = rest.splitn(6, ' ').collect();
                let (Some(set), Some(remove), Some(name)) = (parts.get(3).and_then(|s| hex(s)), parts.get(4).and_then(|s| hex(s)), parts.get(5)) else {
                    return;
                };
                let event = |e: u32| if e == u32::MAX { 0 } else { e };
                self.mixings.insert(name.trim().to_string(), (event(set), event(remove)));
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

    /// The events that set and remove a mixing.
    pub fn mixing(&self, name: &str) -> Option<(u32, u32)> {
        self.mixings.get(name).copied()
    }
}

/// Mixings asked for by name (started, or stopped), from the game and its
/// scripts; the sound module plays their events.
#[derive(Resource, Default)]
pub struct Mixings(Vec<(String, bool)>);

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
}

/// The states pushed (the latest last), each bus level's way to what they
/// set, and the buses whose sounds are paused (or were stopped this frame).
#[derive(Resource, Default)]
pub struct Mixer {
    stack: Vec<u32>,
    levels: HashMap<(u32, u32), Ramp>,
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
/// handbook and the shop) and the player's head under water.
/// `FC3_MIXING_TEST=<name>[,<seconds>]` (tests) starts a mixing that many
/// seconds into play (default 10).
pub fn game_mixings(
    state: Res<State<GameState>>,
    breath: Option<Res<crate::syringes::Breath>>,
    (time, mut test): (Res<Time>, Local<Option<f32>>),
    mut on: Local<[bool; 2]>,
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
    let now = [matches!(state.get(), GameState::Paused | GameState::Map | GameState::Journal | GameState::Shop), breath.is_some_and(|b| b.under)];
    for (i, name) in ["Exclusive/Pause", "Exclusive/Underwater"].into_iter().enumerate() {
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
