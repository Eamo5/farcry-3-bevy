//! Being intoxicated (`CIntoxicatedMiniGame`): DOC_020's mushroom and its
//! hallucination's drunken walk (and the DLC's). While a script keeps it on,
//! the view sways and rolls by the gameplay's multipliers and the walk
//! wanders off its line (unless `DisableDrunkenControl`), easing in as it
//! starts and out as it stops. The settings are the GSF library's
//! (`gsf::IntoxicationSettings`), with the script's own values over them.

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
}

impl Intoxication {
    /// A checkpoint reload: sober again.
    pub fn reset(&mut self) {
        self.pending.clear();
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

fn run(mut drunk: ResMut<Intoxication>, time: Res<Time>, info: Option<Res<crate::data::CacheInfo>>, mut tested: Local<bool>) {
    // FC3_DRUNK_TEST (debug): drunk from the start, at the library's values.
    if !*tested && std::env::var_os("FC3_DRUNK_TEST").is_some() {
        *tested = true;
        drunk.pending.push(Start { id: u64::MAX, params: HashMap::new() });
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
