//! What the enemies say (`fc3_formats::barks`): each enemy gets one of his
//! faction's voices, and speaks that voice's barks as his situation
//! changes: spotting the player, losing him, giving up the search,
//! reloading, throwing a grenade, running for the alarm, getting hurt,
//! burning and dying, with battle yells, search mutters and idle chatter in between. A
//! bark plays where he stands, and his face (if near) mouths it.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::combat::{DamageEvent, Died, Health};
use crate::npc::{Faction, Npc, NpcState};

pub struct BarksPlugin;

impl Plugin for BarksPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BarkTable>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_barks)
            .add_systems(Update, (assign_voices, bark).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// (voice, stimulus) -> (sound id, seconds) lines.
#[derive(Resource, Default)]
pub struct BarkTable {
    lines: HashMap<(String, String), Vec<(u32, f32)>>,
    /// The voices there are (`pirate_01`...).
    voices: Vec<String>,
}

impl BarkTable {
    /// One of a voice's lines for a stimulus (by a roll in 0..1): its sound
    /// and how long it lasts.
    pub(crate) fn line(&self, voice: &str, stimulus: &str, roll: f32) -> Option<(u32, f32)> {
        self.pick(voice, stimulus, roll)
    }

    fn pick(&self, voice: &str, stimulus: &str, roll: f32) -> Option<(u32, f32)> {
        let lines = self.lines.get(&(voice.to_string(), stimulus.to_string()))?;
        lines.get((roll * lines.len() as f32) as usize % lines.len().max(1)).copied()
    }
}

fn load_barks(info: Option<Res<crate::data::CacheInfo>>, mut table: ResMut<BarkTable>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(fc3_formats::barks::BARKS_REL)).map_err(anyhow::Error::from).and_then(|t| fc3_formats::barks::from_text(&t)) {
        Ok(barks) => {
            for b in barks {
                if !table.voices.contains(&b.voice) {
                    table.voices.push(b.voice.clone());
                }
                table.lines.insert((b.voice, b.stimulus), b.lines);
            }
            table.voices.sort();
            info!("barks: {} voices, {} situations", table.voices.len(), table.lines.len());
        }
        Err(e) => warn!("barks: {e:#}"),
    }
}

/// An enemy's voice, and when he may speak again.
#[derive(Component)]
pub struct Voice {
    pub voice: String,
    /// Speaking until then (seconds of game time).
    busy_until: f32,
    /// The next filler line (yell, mutter, chatter).
    next_filler: f32,
    last_state: NpcState,
    /// What he was doing last frame: reloading, between throws, running
    /// for the alarm.
    reloading: bool,
    throw_cooldown: f32,
    alarm: bool,
}

/// The voices of a faction (by their folder's name).
fn faction_voices(faction: Faction) -> &'static str {
    match faction {
        Faction::Pirate => "pirate_",
        Faction::Privateer => "privateer_",
        Faction::Rebel => "rebel_",
    }
}

/// Gives each enemy one of his faction's voices (by his entity, so it stays).
fn assign_voices(table: Res<BarkTable>, time: Res<Time>, npcs: Query<(Entity, &Npc), Without<Voice>>, mut commands: Commands) {
    if table.voices.is_empty() {
        return;
    }
    for (e, npc) in &npcs {
        let prefix = faction_voices(npc.faction);
        // Heavies have their faction's heavy voice where there is one; the
        // rest the numbered voices (not a boss's minions').
        let heavy = format!("{prefix}heavy");
        let choices: Vec<&String> = if npc.role.armoured() && table.voices.contains(&heavy) {
            table.voices.iter().filter(|v| **v == heavy).collect()
        } else {
            table.voices.iter().filter(|v| v.strip_prefix(prefix).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))).collect()
        };
        let Some(voice) = choices.get((e.to_bits() as usize).wrapping_mul(2654435761) % choices.len().max(1)) else { continue };
        let first = time.elapsed_secs() + 5.0 + (e.to_bits() % 17) as f32;
        commands.entity(e).insert(Voice {
            voice: voice.to_string(),
            busy_until: 0.0,
            next_filler: first,
            last_state: npc.state,
            reloading: npc.reload > 0.0,
            throw_cooldown: npc.throw_cooldown,
            alarm: npc.alarm_target.is_some(),
        });
    }
}

/// Barks farther than this (m) from the listener aren't played.
const EARSHOT: f32 = 45.0;
/// At most this many barks at once.
const AT_ONCE: usize = 2;

#[allow(clippy::too_many_arguments)]
fn bark(
    time: Res<Time>,
    table: Res<BarkTable>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut npcs: Query<(Entity, &Npc, &Transform, &Health, &mut Voice, Has<crate::burning::Aflame>)>,
    mut hits: MessageReader<DamageEvent>,
    mut died: MessageReader<Died>,
    listener: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    (mut sfx, library, mut faces): (MessageWriter<crate::sound::PlaySfx>, Option<ResMut<crate::faces::FaceLibrary>>, Query<&mut crate::faces::Face>),
    mut speaking: Local<Vec<f32>>,
) {
    let now = time.elapsed_secs();
    speaking.retain(|end| *end > now);
    let ear = listener.single().map(|l| l.translation()).unwrap_or(Vec3::splat(f32::MAX));
    let mut want: Vec<(Entity, &'static str, bool)> = Vec::new();
    // Hurt (when not speaking already) and killed (cutting in).
    for h in hits.read() {
        want.push((h.target, "reflex_wounded_ono", false));
    }
    for d in died.read() {
        want.push((d.entity, "reflex_death_ono", true));
    }
    for (e, npc, _, health, mut voice, aflame) in &mut npcs {
        if health.dead {
            continue;
        }
        // Alight: screaming, one cry after another.
        if aflame {
            if now >= voice.busy_until {
                want.push((e, "reflex_flames", true));
            }
            continue;
        }
        // What changed.
        let said = match (voice.last_state, npc.state) {
            (NpcState::Idle | NpcState::Suspicious | NpcState::Search, NpcState::Combat) => Some("reflex_threatseen"),
            (NpcState::Idle, NpcState::Suspicious) => Some("reflex_genericsurprise"),
            (NpcState::Combat, NpcState::Search) => Some("reflex_playervanished"),
            (NpcState::Search | NpcState::Suspicious, NpcState::Idle) => Some("inform_areaclear"),
            _ => None,
        };
        voice.last_state = npc.state;
        // What he starts doing: reloading, throwing, raising the alarm.
        let reloading = npc.reload > 0.0;
        let threw = npc.throw_cooldown > voice.throw_cooldown + 1.0;
        let alarm = npc.alarm_target.is_some();
        let doing = if alarm && !voice.alarm {
            Some("inform_activatingalarm")
        } else if threw {
            Some("inform_throwing_grenade")
        } else if reloading && !voice.reloading {
            Some("inform_reloading")
        } else {
            None
        };
        (voice.reloading, voice.throw_cooldown, voice.alarm) = (reloading, npc.throw_cooldown, alarm);
        let said = said.or(doing);
        if let Some(s) = said {
            want.push((e, s, false));
            continue;
        }
        // Fillers, now and then.
        if now >= voice.next_filler {
            let (stimulus, gap) = match npc.state {
                NpcState::Combat => (if rng.f32() < 0.4 { "generic_battle_charge" } else { "generic_battle_filler_yells" }, (6.0, 12.0)),
                NpcState::Search | NpcState::Suspicious => ("inform_searchingfiller", (8.0, 15.0)),
                NpcState::Idle => ("generic_idle_filler", (30.0, 70.0)),
            };
            voice.next_filler = now + rng.range(gap.0, gap.1);
            want.push((e, stimulus, false));
        }
    }
    let mut library = library;
    for (e, stimulus, urgent) in want {
        let Ok((_, npc, t, health, mut voice, aflame)) = npcs.get_mut(e) else { continue };
        // (Alight, the scream is the hurt.)
        if aflame && stimulus == "reflex_wounded_ono" {
            continue;
        }
        // (The dead say only their last.)
        if health.dead && stimulus != "reflex_death_ono" {
            continue;
        }
        if t.translation.distance(ear) > EARSHOT || (!urgent && (now < voice.busy_until || speaking.len() >= AT_ONCE)) {
            continue;
        }
        let Some((sound, seconds)) = table.pick(&voice.voice, stimulus, rng.f32()) else { continue };
        // FC3_BARK_DEBUG=1 (tests) logs each bark.
        if std::env::var_os("FC3_BARK_DEBUG").is_some() {
            info!("bark: {e:?} {} {stimulus} -> {sound:#x} ({seconds:.1}s)", voice.voice);
        }
        sfx.write(crate::sound::PlaySfx { event: sound, at: Some(t.translation + Vec3::Y * 0.7), volume: 1.0 });
        voice.busy_until = now + seconds;
        speaking.push(now + seconds);
        // His face mouths it, where the game has the lip-sync.
        if let (Some(lib), Some(rig)) = (library.as_mut(), npc.rig) {
            if let Ok(mut face) = faces.get_mut(rig) {
                face.voice = lib.clip(&fc3_formats::facial::voice_clip_path(sound)).map(|c| (c, 0.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_from_the_voices_lines() {
        let mut table = BarkTable::default();
        table.lines.insert(("pirate_01".into(), "reflex_threatseen".into()), vec![(10, 1.0), (11, 2.0)]);
        assert_eq!(table.pick("pirate_01", "reflex_threatseen", 0.0), Some((10, 1.0)));
        assert_eq!(table.pick("pirate_01", "reflex_threatseen", 0.99), Some((11, 2.0)));
        assert_eq!(table.pick("pirate_02", "reflex_threatseen", 0.5), None);
    }
}
