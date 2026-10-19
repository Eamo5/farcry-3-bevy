//! Encounters' conversations (`fc3_formats::conversations`): an encounter
//! whose script plays a conversation (an executioner taunting his victims,
//! pirates beating a man, a roadblock's banter) has its people say its
//! lines once the player comes within the script's radius. Each line goes
//! to the encounter's member whose voice it has a variant for (a pirate by
//! his bark voice, a victim by their sex), in turn, and the next follows
//! once it has been said (its sound's length).

use std::collections::HashMap;

use bevy::prelude::*;
use fc3_formats::conversations::{self as conv, Conversation};

use crate::combat::Health;
use crate::player::Player;
use crate::weapons::Rng;

pub struct EncounterTalkPlugin;

impl Plugin for EncounterTalkPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Conversations>()
            .add_message::<StartTalk>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_conversations)
            .add_systems(Update, (start_talks, talk).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

#[derive(Resource, Default)]
struct Conversations(HashMap<String, Conversation>);

fn load_conversations(info: Option<Res<crate::data::CacheInfo>>, mut convs: ResMut<Conversations>) {
    let Some(info) = info else { return };
    match std::fs::read_to_string(info.root.join(conv::CONVERSATIONS_REL)).map_err(anyhow::Error::from).and_then(|t| conv::from_text(&t)) {
        Ok(list) => {
            info!("{} conversations", list.len());
            convs.0 = list.into_iter().map(|c| (c.name.clone(), c)).collect();
        }
        Err(e) => warn!("conversations: {e:#}; re-run the converter"),
    }
}

/// An encounter set out with a conversation to say (by name), about where,
/// and from how near (0: our default).
#[derive(Message, Clone)]
pub struct StartTalk {
    pub group: u64,
    pub at: Vec3,
    pub name: String,
    pub near: f32,
}

/// From this near (m) when the script names no radius (ours).
const TALK_NEAR: f32 = 25.0;
/// A breath between lines (s), and a line's length when its sound's is not
/// known.
const GAP: f32 = 0.4;
const LINE: f32 = 3.0;
/// Past this (m) the player has left it: it is forgotten.
const FORGET: f32 = 300.0;

#[derive(Component)]
struct Talk {
    group: u64,
    at: Vec3,
    near: f32,
    lines: Vec<Vec<(String, u32)>>,
    line: usize,
    next: f32,
    started: bool,
}

fn start_talks(mut starts: MessageReader<StartTalk>, convs: Res<Conversations>, talks: Query<&Talk>, mut commands: Commands) {
    for s in starts.read() {
        let Some(c) = convs.0.get(&s.name) else { continue };
        if talks.iter().any(|t| t.group == s.group) {
            continue;
        }
        let near = if s.near > 0.0 { s.near } else { TALK_NEAR };
        commands.spawn(Talk { group: s.group, at: s.at, near, lines: c.lines.clone(), line: 0, next: 0.0, started: false });
    }
}

/// Whether a voice is a fighter's (pirate, privateer, rebel).
fn fighter_voice(v: &str) -> bool {
    ["pirate", "privateer", "rebel"].iter().any(|p| v.starts_with(p))
}

#[allow(clippy::too_many_arguments)]
fn talk(
    time: Res<Time>,
    mut talks: Query<(Entity, &mut Talk)>,
    npcs: Query<(&crate::npc::Npc, &Transform, &Health, Option<&crate::barks::Voice>)>,
    civilians: Query<(&crate::villagers::EncounterCivilian, &crate::villagers::Villager, &Transform, &Health)>,
    players: Query<&Transform, With<Player>>,
    bank: Option<Res<crate::sound::SoundBank>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut rng: ResMut<Rng>,
    (library, mut faces): (Option<ResMut<crate::faces::FaceLibrary>>, Query<&mut crate::faces::Face>),
    mut commands: Commands,
) {
    let mut library = library;
    let Ok(pt) = players.single() else { return };
    let now = time.elapsed_secs();
    for (e, mut t) in &mut talks {
        let d = pt.translation.distance(t.at);
        if d > FORGET {
            commands.entity(e).despawn();
            continue;
        }
        if !t.started {
            // (FC3_TALK_TEST: from twice as far, as encounters appear 35 m
            // off or more.)
            let near = if std::env::var_os("FC3_TALK_TEST").is_some() { t.near * 2.0 } else { t.near };
            if d > near {
                continue;
            }
            t.started = true;
            t.next = now;
            if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
                info!("encounter: talk at {:.0?} begins ({} lines)", t.at, t.lines.len());
            }
        }
        if now < t.next {
            continue;
        }
        let Some(line) = t.lines.get(t.line).cloned() else {
            commands.entity(e).despawn();
            continue;
        };
        let group = t.group;
        // Who says it: a fighter of the encounter with a voice the line has
        // (else any of them, by turns); a victim's line, one of its
        // civilians (in a voice of their sex).
        // (And their face, which mouths the line.)
        let speaker: Option<(Vec3, String, Option<Entity>)> = if line.iter().any(|(v, _)| fighter_voice(v)) {
            let mut members: Vec<(Vec3, String, Option<Entity>)> = npcs
                .iter()
                .filter(|(n, _, h, _)| n.marker_id == group && !h.dead)
                .filter_map(|(n, tr, _, v)| Some((tr.translation, v?.voice.clone(), n.rig)))
                .collect();
            members.sort_by(|a, b| a.1.cmp(&b.1));
            let own: Vec<_> = members.iter().filter(|(_, v, _)| line.iter().any(|(lv, _)| lv == v)).cloned().collect();
            let pool = if own.is_empty() { members } else { own };
            pool.get(t.line % pool.len().max(1)).cloned()
        } else {
            civilians.iter().find(|(c, _, _, h)| c.0 == group && !h.dead).map(|(_, v, tr, _)| {
                let female = v.female();
                let voice = line.iter().map(|(lv, _)| lv.clone()).find(|lv| lv.contains("female") == female).unwrap_or_default();
                (tr.translation, voice, v.rig())
            })
        };
        t.line += 1;
        let Some((at, voice, rig)) = speaker else { continue };
        let Some(sound) = conv::line_for(&line, &voice, rng.f32()) else { continue };
        sfx.write(crate::sound::PlaySfx { event: sound, at: Some(at + Vec3::Y * 0.7), volume: 1.0 });
        // The speaker's face mouths it, where the game has the lip-sync.
        let mut mouthed = false;
        if let (Some(lib), Some(mut face)) = (library.as_mut(), rig.and_then(|r| faces.get_mut(r).ok())) {
            face.voice = lib.clip(&fc3_formats::facial::voice_clip_path(sound)).map(|c| (c, 0.0));
            mouthed = face.voice.is_some();
        }
        if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
            let length = bank.as_ref().and_then(|b| b.length(sound)).unwrap_or(0.0);
            info!("encounter: {voice} says line {} ({sound:#x}, {length:.1} s{})", t.line, if mouthed { ", mouthed" } else { "" });
        }
        t.next = now + bank.as_ref().and_then(|b| b.length(sound)).unwrap_or(LINE) + GAP;
    }
}
