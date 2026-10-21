//! Effects the scripts play on characters (`CAPI_Pawn.SpawnParticle`: the
//! Ink Monster's smoke arms, the waterfalls' spray over the player's head,
//! Grant's neck in the prologue): an authored effect on one of the
//! character's bones (the player's: the camera), kept until the script stops
//! it by the tag it gave (`StopToDestroyParticle`, `DestroyParticle`), when it
//! stops emitting and what it has put out lives out.
//!
//! Scripts name the effect by its path's hash (`GetPathID`), which is the
//! hash the cache files authored effects by, and the bone by its name's
//! (`GetStringID`), which is the rigs' own.

use std::collections::HashMap;

use bevy::prelude::*;

pub struct PawnFxPlugin;
impl Plugin for PawnFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PawnFx>().add_systems(Update, play_pawn_fx);
    }
}

/// A script's effect on a character.
#[derive(Debug, Clone)]
pub struct Request {
    pub pawn: u64,
    /// The effect, by its path's hash.
    pub effect: u64,
    /// The bone, by its name's hash.
    pub bone: u32,
    /// What the script stops it by.
    pub tag: u32,
    /// From the bone, in Dunia's axes.
    pub offset: [f32; 3],
}

/// The scripts' effects on characters: asked for, to stop, and playing.
#[derive(Resource, Default)]
pub struct PawnFx {
    pub requests: Vec<Request>,
    /// (pawn, tag).
    pub stops: Vec<(u64, u32)>,
    /// Every one stopped (a reload).
    pub clear: bool,
    playing: HashMap<(u64, u32), Entity>,
}

/// An effect's name by its path's hash, from its cached file (none when it
/// isn't cached).
fn effect_name(hash: u64) -> Option<&'static str> {
    static NAMES: std::sync::Mutex<Option<HashMap<u64, Option<&'static str>>>> = std::sync::Mutex::new(None);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    *names.get_or_insert_with(HashMap::new).entry(hash).or_insert_with(|| {
        let path = fc3_formats::cache::default_cache_root().join(fc3_formats::particle_effects::authored_rel_by_hash(hash));
        let text = std::fs::read_to_string(path).ok()?;
        let at = text.find("<PartSys Name=\"")? + "<PartSys Name=\"".len();
        let end = text[at..].find('"')?;
        Some(crate::authored_fx::intern(&text[at..at + end]))
    })
}

fn play_pawn_fx(
    mut fx: ResMut<PawnFx>,
    mut commands: Commands,
    cameras: Query<Entity, With<crate::player::PlayerCamera>>,
    actors: Query<(Entity, &crate::story_actors::StoryActor)>,
    rigs: Query<&crate::characters::CharacterRig>,
) {
    let fx = &mut *fx;
    if std::mem::take(&mut fx.clear) {
        for (_, e) in fx.playing.drain() {
            commands.entity(e).try_despawn();
        }
    }
    for key in std::mem::take(&mut fx.stops) {
        if let Some(e) = fx.playing.remove(&key) {
            commands.entity(e).try_despawn();
        }
    }
    let mut waiting = Vec::new();
    for r in std::mem::take(&mut fx.requests) {
        let Some(name) = effect_name(r.effect) else {
            debug!("pawn fx: effect {:016x} not cached", r.effect);
            continue;
        };
        // On the player, the camera; on a character, its bone (its head for
        // one it lacks, such as the camera), once its rig is in.
        let parent = if r.pawn == fc3_story::PLAYER {
            cameras.single().ok()
        } else {
            let Some((entity, actor)) = actors.iter().find(|(_, a)| a.id == r.pawn) else { continue };
            match actor.rig.map(|rig| rigs.get(rig)) {
                Some(Ok(rig)) => {
                    let joint = rig.joint(r.bone).or_else(|| rig.joint(fc3_formats::hash::name_hash("Head")));
                    Some(joint.map_or(entity, |(_, j)| j))
                }
                Some(Err(_)) => {
                    waiting.push(r);
                    continue;
                }
                None => Some(entity),
            }
        };
        let Some(parent) = parent else { continue };
        if let Some(e) = crate::authored_fx::attach(&mut commands, name, parent) {
            commands.entity(e).insert(Transform::from_translation(crate::data::to_bevy(r.offset)));
            debug!("pawn fx: {name} on {} ({:08x})", r.pawn, r.bone);
            if let Some(old) = fx.playing.insert((r.pawn, r.tag), e) {
                commands.entity(old).try_despawn();
            }
        }
    }
    fx.requests = waiting;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripts_effect_plays_on_the_player_until_stopped() {
        // The Ink Monster's smoke arms, by the name the scripts give (where
        // the cache has them).
        let effect = fc3_formats::hash::path_hash("FC3_BossFights.Citra_BossFight.Smoke_Arms");
        let Some(name) = effect_name(effect) else { return };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(PawnFxPlugin);
        let camera = app.world_mut().spawn(crate::player::PlayerCamera).id();
        let bone = fc3_formats::hash::name_hash("Camera");
        app.world_mut().resource_mut::<PawnFx>().requests.push(Request { pawn: fc3_story::PLAYER, effect, bone, tag: 7, offset: [0.0; 3] });
        app.update();
        let world = app.world_mut();
        let on: Vec<(&str, Entity)> = world.query::<(&crate::authored_fx::AuthoredFx, &ChildOf)>().iter(world).map(|(f, c)| (f.0, c.parent())).collect();
        assert_eq!(on, vec![(name, camera)]);
        app.world_mut().resource_mut::<PawnFx>().stops.push((fc3_story::PLAYER, 7));
        app.update();
        let world = app.world_mut();
        assert_eq!(world.query::<&crate::authored_fx::AuthoredFx>().iter(world).count(), 0);
    }
}
