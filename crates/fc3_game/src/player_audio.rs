//! The player's own foley and the game's stings, from the sound settings of
//! the player archetype (`MainCharacter/PawnPlayer` in the entity library):
//! the detection sting, a level gained, cash looted, crouching, diving under
//! (a splash, then the muffled underwater bed) and coming up (a gasp after a
//! long dive), swimming strokes, wind rushing past in a fall, and dying.

use bevy::prelude::*;

use crate::combat::Health;
use crate::player::{Player, PlayerCamera};
use crate::sound::{LoopSfx, PlaySfx};

pub struct PlayerAudioPlugin;

impl Plugin for PlayerAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, player_audio.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// Event ids (field names recovered from their CRC-32 where known).
mod snd {
    /// `sndDetected`.
    pub const DETECTED: u32 = 0x0006_6CED;
    /// `sndLevelUp`.
    pub const LEVEL_UP: u32 = 0x0004_7221;
    /// `sndLootCash`.
    pub const LOOT_CASH: u32 = 0x0004_721A;
    /// `sndCrouchSound`.
    pub const CROUCH: u32 = 0x0045_129A;
    /// `sndUnderwaterStartSound` (sea): a splash and the underwater bed.
    pub const UNDERWATER: u32 = 0x0005_2617;
    /// `sndUnderwaterStopSound`, and the variant with a gasp for air.
    pub const SURFACE: u32 = 0x0005_2619;
    pub const SURFACE_GASP: u32 = 0x0005_2416;
    /// `sndDeathSound`.
    pub const DEATH: u32 = 0x0002_5613;
}

/// Riding a zip line (`sndOnZipline`, by speed in m/s, 0-20) and the hook
/// catching the line.
const ZIPLINE: u32 = 0x0002_6194;
const ZIP_HOOK: u32 = 0x0009_AC95;
const ZIP_SPEED: u32 = 0x0002_61B5;

/// Wind rushing past, by fall speed (`FALL_SPEED`, m/s, 0-40).
pub const FALLING: u32 = 0x0004_6FFA;
pub const FALL_SPEED: u32 = 0x0004_6FF7;

/// Under longer than this (s), the player comes up gasping.
const LONG_DIVE: f32 = 8.0;
/// The detection sting waits this long (s) after the last time.
const DETECTED_AGAIN: f32 = 12.0;
/// Seconds between swimming strokes at full speed.
const STROKE: f32 = 1.1;

#[derive(Default)]
struct Last {
    init: bool,
    detected: bool,
    since_detected: f32,
    level: u32,
    money: u32,
    crouching: bool,
    under: f32,
    dead: bool,
    stroke: f32,
    zipping: bool,
}

#[allow(clippy::too_many_arguments)]
fn player_audio(
    time: Res<Time>,
    mut last: Local<Last>,
    players: Query<(&Player, &Health)>,
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    npcs: Query<&crate::npc::Npc>,
    progress: Res<crate::progression::Progress>,
    driving: Res<crate::vehicles::Driving>,
    mount: Res<crate::traversal::Mount>,
    heights: Res<crate::terrain::Heightfield>,
    mut sfx: MessageWriter<PlaySfx>,
    mut loops: MessageWriter<LoopSfx>,
) {
    let (Ok((p, health)), Ok(cam)) = (players.single(), cams.single()) else { return };
    // Nothing while the player is being put on the ground.
    if p.spawning {
        return;
    }
    let dt = time.delta_secs();
    let ui = |event: u32, volume: f32| PlaySfx { event, at: None, volume };
    if !last.init {
        *last = Last {
            init: true,
            since_detected: DETECTED_AGAIN,
            level: progress.level,
            money: progress.money,
            crouching: p.crouching,
            dead: health.dead,
            ..default()
        };
    }
    // Spotted: the sting, once per encounter.
    let detected = npcs.iter().any(|n| n.hostile() && n.state == crate::npc::NpcState::Combat && n.sees_player);
    last.since_detected += dt;
    if detected && !last.detected && last.since_detected >= DETECTED_AGAIN {
        sfx.write(ui(snd::DETECTED, 0.7));
    }
    if detected {
        last.since_detected = 0.0;
    }
    last.detected = detected;
    if progress.level > last.level {
        sfx.write(ui(snd::LEVEL_UP, 0.8));
    }
    last.level = progress.level;
    // Money going up outside the shop is loot.
    if progress.money > last.money {
        sfx.write(ui(snd::LOOT_CASH, 0.6));
    }
    last.money = progress.money;
    if p.crouching != last.crouching && p.sliding.is_none() && p.grounded {
        sfx.write(ui(snd::CROUCH, 0.5));
    }
    last.crouching = p.crouching;
    if health.dead && !last.dead {
        sfx.write(ui(snd::DEATH, 0.9));
    }
    last.dead = health.dead;
    // Under water: the splash and the muffled bed; coming up again.
    let under = crate::water::submerged(cam.translation(), &heights, 0.05) && driving.vehicle.is_none();
    if under {
        last.under += dt;
        loops.write(LoopSfx { key: 0x5557_4154, event: snd::UNDERWATER, at: None, volume: 0.8, ..default() });
    } else if last.under > 0.4 {
        sfx.write(ui(if last.under > LONG_DIVE { snd::SURFACE_GASP } else { snd::SURFACE }, 0.8));
        last.under = 0.0;
    } else {
        last.under = 0.0;
    }
    // Strokes while swimming along the surface.
    let swim_speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    if p.swimming && !under && swim_speed > 0.8 {
        last.stroke -= dt * (swim_speed / 3.0).min(1.5);
        if last.stroke <= 0.0 {
            last.stroke = STROKE;
            let event = crate::sound::switched(crate::sound::ev::FOOTSTEP, crate::surfaces::value::DEEP_WATER, 1);
            sfx.write(ui(event, 0.45));
        }
    } else {
        last.stroke = 0.0;
    }
    // The line humming under the hook, faster and higher with speed.
    if let crate::traversal::Mount::Zip { speed, .. } = *mount {
        let params = [(ZIP_SPEED, speed), (0, 0.0), (0, 0.0), (0, 0.0)];
        if !last.zipping {
            sfx.write(ui(ZIP_HOOK, 0.7));
        }
        loops.write(LoopSfx { key: 0x5A49_5000, event: ZIPLINE, at: None, volume: 0.8, params, ..default() });
        last.zipping = true;
    } else {
        last.zipping = false;
    }
    // Wind rushing past in a fall, a wingsuit glide or under the canopy.
    let fall = if p.grounded || p.swimming || p.noclip || driving.vehicle.is_some() { 0.0 } else { p.velocity.length() };
    if fall > 6.0 {
        loops.write(LoopSfx {
            key: 0x5749_4E44,
            event: FALLING,
            at: None,
            volume: 0.9,
            params: [(FALL_SPEED, fall), (0, 0.0), (0, 0.0), (0, 0.0)],
            ..default()
        });
    }
}
