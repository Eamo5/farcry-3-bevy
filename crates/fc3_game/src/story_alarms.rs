//! The story's alarm boxes: `CAlarmPoint` entities on mission layers (a
//! camp's alarm a mission tells the player to switch off). Up close the
//! player can disable one; a guard of the story's in combat near a working
//! one runs for it and sets it off, as outpost guards do theirs. The story
//! hears either on the alarm's groups (`Alarm_Disabled`, `Alarm_Activated`,
//! and `Alarm_Deactivated` once the siren has run its course) and reads the
//! box's state back (`CAPI_Entity.GetAlarmState`). Outposts' own boxes stay
//! with `outposts`.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::combat::Health;
use crate::data::to_bevy;
use crate::npc::{Npc, NpcState};
use crate::player::Player;
use crate::story::StoryHost;
use crate::story_actors::StoryActor;

pub struct StoryAlarmsPlugin;

impl Plugin for StoryAlarmsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StoryAlarms>().add_systems(
            Update,
            (find_alarms, run_for_alarms, sound_alarms, autopilot_alarms, disable_alarms.before(crate::hud::HudSet).after(crate::turrets::man_turrets))
                .chain()
                .run_if(in_state(crate::menu::GameState::Playing)),
        );
    }
}

/// How near a guard in combat must be to run for a box.
const RUN_RANGE: f32 = 45.0;
/// How long a set-off box sounds before it falls quiet again.
const SIREN_SECS: f32 = 50.0;
/// How near the player must stand to reach a box.
const REACH: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    /// Sounding since then (game seconds).
    Active(u32),
    Disabled,
}

#[derive(Resource, Default)]
pub struct StoryAlarms {
    /// Every story alarm box (id, where), once the story's data is in.
    known: Option<Vec<(u64, Vec3)>>,
    state: HashMap<u64, State>,
    /// The guard running for each box.
    runner: HashMap<u64, Entity>,
}

impl StoryAlarms {
    fn idle(&self, id: u64) -> bool {
        self.state.get(&id).is_none_or(|s| *s == State::Idle)
    }
}

fn find_alarms(host: Option<NonSend<StoryHost>>, mut alarms: ResMut<StoryAlarms>) {
    if alarms.known.is_some() {
        return;
    }
    let Some(host) = host else { return };
    let known: Vec<(u64, Vec3)> = host
        .story
        .data()
        .entities
        .values()
        .filter(|e| e.layer != fc3_formats::world::DEFAULT_MISSION_LAYER && e.has("CAlarmPoint"))
        .map(|e| (e.id, to_bevy(e.pos)))
        .collect();
    info!("story alarms: {}", known.len());
    alarms.known = Some(known);
}

/// The boxes in the world now (their mission layer present).
fn present<'a>(alarms: &'a StoryAlarms, host: &'a StoryHost) -> impl Iterator<Item = (u64, Vec3)> + 'a {
    alarms.known.iter().flatten().copied().filter(|(id, _)| host.story.is_loaded(*id))
}

/// Sends the nearest story guard in combat to a working box, and sets the
/// box off when he gets there.
fn run_for_alarms(
    host: Option<NonSendMut<StoryHost>>,
    time: Res<Time>,
    mut alarms: ResMut<StoryAlarms>,
    mut guards: Query<(Entity, &Transform, &mut Npc, &Health, &StoryActor)>,
) {
    let Some(mut host) = host else { return };
    let boxes: Vec<(u64, Vec3)> = present(&alarms, &host).filter(|(id, _)| alarms.idle(*id)).collect();
    for (id, at) in boxes {
        if let Some(&runner) = alarms.runner.get(&id) {
            match guards.get(runner) {
                Ok((_, t, npc, h, actor)) if !h.dead => {
                    // Got there (the AI drops its target on arrival).
                    if npc.alarm_target.is_none() && Vec2::new(t.translation.x - at.x, t.translation.z - at.z).length() < 2.5 {
                        info!("story alarm {id} set off by {}", actor.id);
                        alarms.state.insert(id, State::Active(time.elapsed_secs() as u32));
                        alarms.runner.remove(&id);
                        host.story.alarm_event(id, 1, actor.id);
                    }
                    continue;
                }
                _ => {
                    alarms.runner.remove(&id);
                }
            }
        }
        let runner = guards
            .iter_mut()
            .filter(|(_, t, n, h, _)| !h.dead && n.state == NpcState::Combat && n.faction.hostile_to_player() && t.translation.distance(at) < RUN_RANGE)
            .min_by(|a, b| a.1.translation.distance(at).total_cmp(&b.1.translation.distance(at)));
        if let Some((e, _, mut npc, _, _)) = runner {
            npc.alarm_target = Some(at);
            alarms.runner.insert(id, e);
        }
    }
}

/// A set-off box sounds the camp's alarm until it falls quiet again.
fn sound_alarms(host: Option<NonSendMut<StoryHost>>, time: Res<Time>, mut alarms: ResMut<StoryAlarms>, mut loops: MessageWriter<crate::sound::LoopSfx>) {
    let Some(mut host) = host else { return };
    let now = time.elapsed_secs() as u32;
    let sounding: Vec<(u64, Vec3, u32)> = present(&alarms, &host)
        .filter_map(|(id, at)| match alarms.state.get(&id) {
            Some(State::Active(since)) => Some((id, at, *since)),
            _ => None,
        })
        .collect();
    for (id, at, since) in sounding {
        if now.saturating_sub(since) as f32 > SIREN_SECS {
            alarms.state.insert(id, State::Idle);
            host.story.alarm_event(id, 0, fc3_story::PLAYER);
            continue;
        }
        let key = 0x5341_0000_0000_0000 | (id & 0xFFFF_FFFF);
        loops.write(crate::sound::LoopSfx { key, event: crate::outposts::ALARM_LOOP, at: Some(at + Vec3::Y * 2.0), volume: 1.0, ..default() });
    }
}

/// The player switches a working box off up close.
#[allow(clippy::too_many_arguments)]
fn disable_alarms(
    host: Option<NonSendMut<StoryHost>>,
    actions: Res<crate::controls::ActionState>,
    (settings, text, time): (Res<crate::settings::Settings>, Option<Res<crate::hud::GameText>>, Res<Time>),
    mut held: Local<f32>,
    mut alarms: ResMut<StoryAlarms>,
    players: Query<(&Transform, &Health), With<Player>>,
    mut hud: ResMut<crate::hud::HudState>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    (mut xp, skills): (MessageWriter<crate::progression::XpGain>, Option<Res<crate::progression::Skills>>),
) {
    let Some(mut host) = host else { return };
    let Ok((pt, h)) = players.single() else { return };
    if h.dead || hud.prompt.is_some() {
        return;
    }
    let feet = pt.translation - Vec3::Y * crate::player::HALF_HEIGHT;
    let Some((id, at)) =
        present(&alarms, &host).filter(|(id, _)| !matches!(alarms.state.get(id), Some(State::Disabled))).find(|(_, at)| within_reach(feet, *at))
    else {
        return;
    };
    let prompt = text.as_deref().and_then(|t| t.prompt(crate::hud::prompt::DISABLE_ALARM, &actions, &settings));
    hud.prompt = Some(prompt.unwrap_or_else(|| format!("hold {}  disable alarm", actions.label(crate::controls::Action::Interact, &settings))));
    let interact = crate::controls::Action::Interact;
    if !crate::hud::held_long_enough(&mut held, actions.pressed(interact), actions.just_pressed(interact), time.delta_secs(), crate::hud::ALARM_HOLD) {
        return;
    }
    info!("story alarm {id} disabled");
    alarms.state.insert(id, State::Disabled);
    alarms.runner.remove(&id);
    host.story.alarm_event(id, 2, fc3_story::PLAYER);
    hud.notify("Alarm disabled".to_string());
    sfx.write(crate::sound::PlaySfx { event: crate::outposts::ALARM_OFF, at: Some(at), volume: 1.0 });
    let amount = skills.as_ref().and_then(|s| s.xp_reward("DisableAlarm")).unwrap_or(0.0) as u32;
    if amount > 0 {
        xp.write(crate::progression::XpGain { amount, reason: "Alarm disabled" });
    }
}

/// How near a box must be for the autopilot to go and switch it off.
const ALARM_REACH: f32 = 150.0;
/// How often it is put beside a box before it leaves that one be.
const ALARM_TRIES: u32 = 8;

/// Whether a player standing at `feet` can reach a box at `at` (its base).
fn within_reach(feet: Vec3, at: Vec3) -> bool {
    Vec2::new(feet.x - at.x, feet.z - at.z).length() < REACH && (feet.y - at.y).abs() < 2.5
}

/// Tests (`FC3_STORY_AUTOPILOT`): once the story has gone quiet waiting for
/// an alarm to be switched off, the player goes up to a working box near
/// (`ALARM_REACH`) and presses to disable it (before `disable_alarms`, which
/// sees the press the same frame). Not while a scene holds the view or plays
/// the player: BAM_030's closing scene had him carried off to an outpost's
/// box across the island.
fn autopilot_alarms(
    host: Option<NonSendMut<StoryHost>>,
    alarms: Res<StoryAlarms>,
    cinema: Res<crate::cinema::Cinema>,
    mut players: Query<(&mut Transform, &mut Player)>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut tries: Local<std::collections::HashMap<u64, u32>>,
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() {
        return;
    }
    let Some(mut host) = host else { return };
    // (Not while tailing a character: he'd see the player go.)
    if host.quiet() < 6.0 || !host.story.awaits_group_event("Alarm_Disabled") || !host.story.tail_targets().is_empty() {
        return;
    }
    if cinema.view.is_some() || cinema.plays_player() {
        return;
    }
    let Ok((mut t, mut p)) = players.single_mut() else { return };
    // (Not from a seat: put beside the box, he's back in it the next frame,
    // and the tries kept the story from ever going quiet for him to get
    // out: SAM_015, driven into the fuel depot.)
    if p.driving {
        return;
    }
    let feet = t.translation - Vec3::Y * crate::player::HALF_HEIGHT;
    // (A box he never gets to after a few tries is left be.)
    let Some((id, at)) =
        present(&alarms, &host).find(|(id, at)| alarms.idle(*id) && at.distance(feet) < ALARM_REACH && tries.get(id).is_none_or(|n| *n < ALARM_TRIES))
    else {
        return;
    };
    if !within_reach(feet, at) {
        info!("story autopilot: disable alarm {id}");
        // Each try from another side (the first one's may be behind its
        // wall, where he drops out of reach).
        let n = tries.entry(id).or_default();
        let side = Quat::from_rotation_y(*n as f32 * std::f32::consts::FRAC_PI_2) * (feet - at).with_y(0.0).normalize_or(Vec3::Z);
        *n += 1;
        crate::story::teleport(&mut t, &mut p, crate::data::to_dunia(at + side * 1.2), 0.0);
        host.reset_quiet();
        return;
    }
    let to = at - feet;
    p.yaw = (-to.x).atan2(-to.z);
    actions.press(crate::controls::Action::Interact);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box is in reach standing beside it on its own level, not from a
    /// few steps off or a floor above.
    #[test]
    fn a_box_is_reached_beside_it() {
        let at = Vec3::new(10.0, 5.0, 10.0);
        assert!(within_reach(Vec3::new(11.2, 5.0, 10.0), at));
        assert!(!within_reach(Vec3::new(13.0, 5.0, 10.0), at));
        assert!(!within_reach(Vec3::new(10.5, 9.0, 10.0), at));
    }
}
