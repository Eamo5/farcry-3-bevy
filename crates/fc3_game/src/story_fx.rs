//! The story's placed particle systems (`CNewParticlesComponent` on story
//! entities: the fires, smoke and dust a mission's layers put up for its
//! set pieces), played as authored while their entity is in the world as
//! the story sees it (its layer present, not removed) and the player is
//! within reach. Those that start by themselves (`bAutoStart`) do; the rest
//! wait for a script (the `ParticleSystem` box:
//! `CNewParticlesComponent_PlayParticles`, and `..._StopParticles`,
//! `..._PauseParticles` and `..._CleanParticles` take them away). On the
//! world's own layer only those a script starts play here (`world_fx` plays
//! the rest it keeps).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::data::to_bevy;
use crate::player::Player;
use crate::story::StoryHost;

pub struct StoryFxPlugin;

impl Plugin for StoryFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StoryFx>().add_systems(Update, play_story_fx.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// How far off (m) a story effect plays.
const REACH: f32 = 160.0;

struct Placed {
    id: u64,
    at: Vec3,
    turn: Quat,
    name: &'static str,
    /// Starts by itself (and not on the world's own layer).
    auto: bool,
}

#[derive(Resource, Default)]
struct StoryFx {
    list: Option<Vec<Placed>>,
    playing: HashMap<u64, Entity>,
    next: f32,
}

/// Whether a story effect should play: as a script last switched it, else
/// as it starts by itself.
fn wanted(switched: Option<bool>, auto: bool) -> bool {
    switched.unwrap_or(auto)
}

fn play_story_fx(
    time: Res<Time>,
    host: Option<NonSend<StoryHost>>,
    players: Query<&Transform, With<Player>>,
    mut fx: ResMut<StoryFx>,
    mut commands: Commands,
) {
    let (Some(host), Ok(player)) = (host, players.single()) else { return };
    let now = time.elapsed_secs();
    if now < fx.next {
        return;
    }
    fx.next = now + 0.25;
    let fx = &mut *fx;
    let list = fx.list.get_or_insert_with(|| {
        let data = host.story.data();
        let list: Vec<Placed> = data
            .entities
            .values()
            .filter_map(|e| {
                let name = fc3_formats::story::particle_effect(&e.components)?.to_ascii_lowercase();
                // (Not the lamps' glows and beams: the lamps draw their own.)
                if name.contains("lightseffects.") || !crate::authored_fx::available(&name) {
                    return None;
                }
                Some(Placed {
                    id: e.id,
                    at: to_bevy(e.pos),
                    turn: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)),
                    name: crate::authored_fx::intern(&name),
                    auto: e.layer != fc3_formats::world::DEFAULT_MISSION_LAYER && fc3_formats::story::particle_autostarts(&e.components),
                })
            })
            .collect();
        info!("story effects: {} placed", list.len());
        list
    });
    let eye = player.translation;
    if std::env::var_os("FC3_FX_DEBUG").is_some() && (now % 2.0) < 0.25 {
        let names: Vec<(u64, &str)> = list.iter().filter(|p| fx.playing.contains_key(&p.id)).map(|p| (p.id, p.name)).take(20).collect();
        info!("story effects playing: {} ({names:?})", fx.playing.len());
    }
    for p in list.iter() {
        let d = p.at.distance(eye);
        let playing = fx.playing.get(&p.id).copied();
        // (A little past its reach before it goes, so it doesn't flicker at
        // the edge.)
        if d > REACH * 1.15 && playing.is_none() {
            continue;
        }
        let on = wanted(host.fx_switched.get(&p.id).copied(), p.auto) && host.story.is_loaded(p.id);
        match playing {
            None if on && d < REACH => {
                let e = commands.spawn((Transform::from_translation(p.at).with_rotation(p.turn), crate::authored_fx::AuthoredFx(p.name))).id();
                fx.playing.insert(p.id, e);
            }
            Some(e) if !on || d > REACH * 1.15 => {
                commands.entity(e).despawn();
                fx.playing.remove(&p.id);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wanted;

    /// DEN_040's chimney collapse waits for its script; the camp's lamps'
    /// glows start by themselves.
    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_set_pieces_wait_and_lamps_glow() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let data = fc3_story::data::StoryData::load(&root).unwrap();
        let chimney = &data.entities[&2077037341484000103].components;
        assert_eq!(fc3_formats::story::particle_effect(chimney).as_deref(), Some("fc3_environment_collapsecheminey.gfx_collapsechimney.breakbasechimney"));
        assert!(!fc3_formats::story::particle_autostarts(chimney));
        assert!(fc3_formats::story::particle_autostarts(&data.entities[&2073774852723977873].components));
    }

    /// A system that starts by itself plays until a script stops it; one
    /// that doesn't waits for a script to start it.
    #[test]
    fn scripts_start_and_stop_story_effects() {
        assert!(wanted(None, true) && !wanted(Some(false), true));
        assert!(!wanted(None, false) && wanted(Some(true), false));
    }
}
