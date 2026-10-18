//! People and animals set alight. Fire's harm (flames underfoot, a molotov,
//! the flamethrower) catches on whoever takes it: they burn for a while,
//! wearing the game's own human torch (the mercs' `CBurnComponent`'s
//! `psFireSpineParticles`), hurt by it each second, with its start and stop
//! sounds; water puts them out with a hiss of steam
//! (`psFireWaterSmokeParticles`). A body burns on until the fire dies.

use bevy::prelude::*;

use crate::combat::{DamageEvent, DamageKind, Health, Hit};

pub struct BurningPlugin;

impl Plugin for BurningPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (catch_fire, burn).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The human torch and the steam of one put out in water.
pub const HUMAN_TORCH: &str = "fc3_gameplay_fire.fc3_gameplay_pawnfire.fire_humantorch";
pub const WATER_SMOKE: &str = "fc3_gameplay_fire.fc3_gameplay_pawnfire.water_smoke";
/// The burning's start and stop sounds (`sndStartBurningSoundID`,
/// `sndStopBurningSoundID`).
const START_SOUND: u32 = 0x001B_0477;
const STOP_SOUND: u32 = 0x001B_0479;

/// How long one burns (s) and what the fire does to them each second.
const BURN_SECS: f32 = 6.0;
const BURN_DAMAGE: f32 = 30.0;
/// The bone the torch burns on (the chest), and where it is on a person
/// without their skeleton in (m up from their capsule's centre). An
/// animal's burns at its body's middle.
const SPINE: &str = "Spine2";
const CHEST: f32 = 0.4;

/// Alight: seconds left, the torch on them, who set it, and the wait to the
/// next hurt.
#[derive(Component)]
pub struct Aflame {
    left: f32,
    torch: Option<Entity>,
    instigator: Option<Entity>,
    tick: f32,
}

/// Fire's harm sets those it touches alight (people and animals; the player
/// takes it as his own harm).
fn catch_fire(
    mut hits: MessageReader<DamageEvent>,
    targets: Query<(&Health, &GlobalTransform, Has<Aflame>), Or<(With<crate::npc::Npc>, With<crate::wildlife::WildAnimal>)>>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
) {
    let mut lit = Vec::new();
    for hit in hits.read().filter(|h| h.kind == DamageKind::Fire) {
        let Ok((health, at, burning)) = targets.get(hit.target) else { continue };
        if burning || health.dead || lit.contains(&hit.target) {
            continue;
        }
        lit.push(hit.target);
        debug!("{} catches fire", hit.target);
        let torch = crate::authored_fx::available(HUMAN_TORCH)
            .then(|| commands.spawn((Transform::from_xyz(0.0, CHEST, 0.0), crate::authored_fx::AuthoredFx(HUMAN_TORCH), ChildOf(hit.target))).id());
        commands.entity(hit.target).insert(Aflame { left: BURN_SECS, torch, instigator: hit.instigator, tick: 1.0 });
        sfx.write(crate::sound::PlaySfx { event: START_SOUND, at: Some(at.translation()), volume: 1.0 });
    }
}

/// The fire burns on: it hurts each second, and goes out when it's had its
/// time or in water. The torch keeps to the chest, upright, as the body
/// moves and falls.
#[allow(clippy::too_many_arguments)]
fn burn(
    time: Res<Time>,
    heights: Res<crate::terrain::Heightfield>,
    mut burning: Query<(Entity, &mut Aflame, &GlobalTransform, &Health, Option<&crate::npc::Npc>)>,
    rigs: Query<&crate::characters::CharacterRig>,
    joints: Query<&GlobalTransform, Without<Aflame>>,
    mut torches: Query<&mut Transform, Without<Aflame>>,
    mut damage: MessageWriter<DamageEvent>,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let spine = fc3_formats::hash::name_hash(SPINE);
    for (e, mut fire, at, health, npc) in &mut burning {
        let p = at.translation();
        let chest = match npc {
            Some(npc) => npc.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(spine)).and_then(|(_, j)| joints.get(j).ok()).map(|j| j.translation()),
            None => Some(p),
        }
        .unwrap_or(p + Vec3::Y * CHEST);
        if let Some(mut t) = fire.torch.and_then(|torch| torches.get_mut(torch).ok()) {
            t.translation = at.affine().inverse().transform_point3(chest);
        }
        let drowned = crate::water::surface_at(p, &heights) > chest.y;
        fire.left -= dt;
        fire.tick -= dt;
        // (Not as it goes out: the hurt would light them again.)
        if fire.tick <= 0.0 && fire.left > 0.0 && !health.dead && !drowned {
            fire.tick += 1.0;
            damage.write(DamageEvent {
                target: e,
                amount: BURN_DAMAGE,
                kind: DamageKind::Fire,
                source: Some(p),
                instigator: fire.instigator,
                headshot: false,
                hit: Hit::Whole,
            });
        }
        if fire.left > 0.0 && !drowned {
            continue;
        }
        if let Some(torch) = fire.torch {
            commands.entity(torch).despawn();
        }
        if drowned {
            crate::authored_fx::play(&mut commands, WATER_SMOKE, chest);
        }
        sfx.write(crate::sound::PlaySfx { event: STOP_SOUND, at: Some(p), volume: 1.0 });
        debug!("{e}'s fire goes out{}", if drowned { " in water" } else { "" });
        commands.entity(e).remove::<Aflame>();
    }
}
