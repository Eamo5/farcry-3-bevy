//! Being intoxicated (`CIntoxicatedMiniGame`): DOC_020's mushroom and its
//! hallucination's drunken walk (and the DLC's). While a script keeps it on,
//! the view sways and rolls by the gameplay's multipliers and the walk
//! wanders off its line (unless `DisableDrunkenControl`), easing in as it
//! starts and out as it stops. The settings are the GSF library's
//! (`gsf::IntoxicationSettings`), with the script's own values over them.
//! While the player watches their hands (`WatchBubbleHands`), its effects
//! play on both of them, the distortion and the light trail, for its time
//! (`WatchHandsDuration`), with its sounds as it starts and stops.

use std::collections::HashMap;

use bevy::prelude::*;

pub struct IntoxicationPlugin;

impl Plugin for IntoxicationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Intoxication>().add_systems(Update, run.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The gameplay a script started: its id and its own values.
pub struct Start {
    pub id: u64,
    pub params: HashMap<String, String>,
}

#[derive(Resource, Default)]
pub struct Intoxication {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    /// The scripts' generic events (`WatchCough`...): moves to play while
    /// intoxicated.
    pub events: Vec<String>,
    active: Option<(u64, fc3_formats::gsf::IntoxicationSettings)>,
    /// How strongly it holds the player now (0..1, easing in and out), and
    /// the settings it eases out with.
    strength: f32,
    fading: Option<fc3_formats::gsf::IntoxicationSettings>,
    t: f32,
    /// The view's sway (yaw, pitch; radians) and roll, for the camera.
    pub offset: Vec2,
    pub roll: f32,
    /// How far the walk wanders off where it is steered (radians).
    pub drift: f32,
    /// The effects on the hands, the time they have left and the sound as
    /// they stop.
    hands: Vec<Entity>,
    hands_left: f32,
    hands_stop: u32,
}

impl Intoxication {
    /// A checkpoint reload: sober again.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.events.clear();
        self.active = None;
        self.fading = None;
        self.strength = 0.0;
        self.offset = Vec2::ZERO;
        self.roll = 0.0;
        self.drift = 0.0;
    }
}

/// The sway at a multiplier of one: its reach (radians) and rate (Hz).
const SWAY_REACH: f32 = 0.12;
const SWAY_RATE: f32 = 1.0;
/// The roll with the sway, and the walk's wander at its fullest (radians)
/// and its rate per unit of `MovementspeedFactor` (Hz).
const ROLL: f32 = 0.35;
const WANDER: f32 = 0.3;
const WANDER_RATE: f32 = 0.04;
/// Seconds to ease in and out.
const EASE: f32 = 2.0;

#[allow(clippy::too_many_arguments)]
fn run(
    mut drunk: ResMut<Intoxication>,
    time: Res<Time>,
    info: Option<Res<crate::data::CacheInfo>>,
    mut tested: Local<bool>,
    mut anims: ResMut<crate::pawn_anims::PawnAnims>,
    (fp, rigs, mut sfx): (Res<crate::fp::FpState>, Query<&crate::characters::CharacterRig>, MessageWriter<crate::sound::PlaySfx>),
    (mut names, mut commands, mut test_event): (Local<HashMap<String, &'static str>>, Commands, Local<Option<String>>),
    host: Option<NonSendMut<crate::story::StoryHost>>,
) {
    let mut host = host;
    // FC3_DRUNK_TEST (debug): drunk from the start, at the library's values;
    // a value other than 1 is also sent as a generic event (`WatchCough`).
    if !*tested {
        if let Ok(test) = std::env::var("FC3_DRUNK_TEST") {
            *tested = true;
            drunk.pending.push(Start { id: u64::MAX, params: HashMap::new() });
            if test != "1" {
                *test_event = Some(test);
            }
        }
    }
    // (The test's event once the player's arms are in.)
    if fp.rig().is_some() {
        if let Some(event) = test_event.take() {
            drunk.events.push(event);
        }
    }
    let cancelled = std::mem::take(&mut drunk.cancelled);
    for start in std::mem::take(&mut drunk.pending) {
        if cancelled.contains(&start.id) {
            continue;
        }
        let settings = (|| -> anyhow::Result<_> {
            use anyhow::Context;
            let root = &info.as_ref().context("cache unavailable")?.root;
            fc3_formats::gsf::IntoxicationSettings::parse(&std::fs::read(root.join(fc3_formats::gsf::CACHE_REL))?)
        })();
        match settings {
            Ok(s) => {
                let s = s.with(|k| {
                    let v = start.params.get(k)?;
                    v.parse().ok().or(match v.as_str() {
                        "true" => Some(1.0),
                        "false" => Some(0.0),
                        _ => None,
                    })
                });
                info!("intoxicated ({}): {s:?}", start.id);
                drunk.active = Some((start.id, s));
            }
            Err(e) => warn!("intoxication: not played: {e:#}"),
        }
    }
    // Its generic events play its moves (the cough, watching the bubbles on
    // the hands), the scripts hearing nothing of them.
    for event in std::mem::take(&mut drunk.events) {
        if let Some(name) = drunk.active.as_ref().and_then(|(_, s)| s.moves.get(&event)) {
            info!("intoxicated: {event} plays {name}");
            // (The cough played out, the gameplay says so, as the engine
            // did: DOC_020's cave waits on it.)
            let done = (event == "WatchCough").then_some("Watch_Cough_Complete");
            anims.requests.push(crate::pawn_anims::Request::Play { pawn: fc3_story::PLAYER, name: name.clone(), looping: false, quiet: true, done });
        }
        // Watching the hands: their effects on both, for its time.
        if event == "WatchBubbleHands" {
            let Some(s) = drunk.active.as_ref().map(|(_, s)| s.clone()) else { continue };
            for e in std::mem::take(&mut drunk.hands) {
                commands.entity(e).try_despawn();
            }
            let rig = fp.rig().and_then(|r| rigs.get(r).ok());
            for hand in ["LeftHand", "RightHand"] {
                let Some((_, joint)) = rig.and_then(|r| r.joint(fc3_formats::hash::name_hash(hand))) else { continue };
                for effect in &s.hand_effects {
                    let name = *names.entry(effect.clone()).or_insert_with(|| Box::leak(effect.clone().into_boxed_str()));
                    drunk.hands.extend(crate::authored_fx::attach(&mut commands, name, joint));
                }
            }
            info!("intoxicated: {} effects on the hands for {} s", drunk.hands.len(), s.watch_hands);
            drunk.hands_left = s.watch_hands;
            drunk.hands_stop = s.watch_sounds.1;
            if s.watch_sounds.0 != 0 {
                sfx.write(crate::sound::PlaySfx { event: s.watch_sounds.0, at: None, volume: 1.0 });
            }
        }
    }
    // (Their time out, or the trip over: they stop.)
    if !drunk.hands.is_empty() {
        drunk.hands_left -= time.delta_secs();
        if drunk.hands_left <= 0.0 || drunk.active.is_none() {
            for e in std::mem::take(&mut drunk.hands) {
                commands.entity(e).try_despawn();
            }
            if drunk.hands_stop != 0 {
                sfx.write(crate::sound::PlaySfx { event: drunk.hands_stop, at: None, volume: 1.0 });
            }
            // (As the engine said, the watching over.)
            if let Some(h) = host.as_mut() {
                h.story.message("Watch_Bubble_Hands_Complete");
            }
        }
    }
    if drunk.active.as_ref().is_some_and(|(id, _)| cancelled.contains(id)) {
        let (id, s) = drunk.active.take().unwrap();
        info!("intoxicated ({id}): over");
        drunk.fading = Some(s);
    }
    let dt = time.delta_secs();
    let on = drunk.active.is_some();
    let step = (if on { dt } else { -dt }) / EASE;
    drunk.strength = (drunk.strength + step).clamp(0.0, 1.0);
    if drunk.strength <= 0.0 {
        drunk.fading = None;
        drunk.offset = Vec2::ZERO;
        drunk.roll = 0.0;
        drunk.drift = 0.0;
        return;
    }
    let Some(s) = drunk.active.as_ref().map(|(_, s)| s.clone()).or_else(|| drunk.fading.clone()) else { return };
    drunk.t += dt;
    let (t, k) = (drunk.t, drunk.strength);
    let tau = std::f32::consts::TAU;
    // Two beats each way, so it never quite repeats.
    let wave = |rate: f32, phase: f32| (tau * rate * t + phase).sin() + 0.5 * (tau * rate * 1.7 * t + phase * 2.3).sin();
    let [h_size, v_size, h_speed, v_speed] = s.sway;
    let yaw = SWAY_REACH * h_size * wave(SWAY_RATE * h_speed, 0.0);
    let pitch = SWAY_REACH * v_size * wave(SWAY_RATE * v_speed, 1.3);
    drunk.offset = Vec2::new(yaw, pitch) * k;
    drunk.roll = -yaw * ROLL * k;
    drunk.drift = if s.drunken_control { WANDER * k * wave(WANDER_RATE * s.speed, 0.7) / 1.5 } else { 0.0 };
}
