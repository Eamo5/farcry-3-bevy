//! The story's combat helpers' targets (`CombatHelpers/TargetMarker_*`, an
//! entity with a `CTargetComponent`): places the AI fires at to stage a
//! fight the player comes upon (WIL_050's pirates loosing off at a rocket's
//! mark, HOY_010's waves at their bombs, CIT_050's fake targets). A marker
//! is on as authored (`bActive`) or as a script switches it
//! (`SetTargetActive <marker> <0|1>`, the `TargetModifier` box), and while
//! it's in the world the player's foes near it (within its `fMaxRange`, the
//! nearest `MaxShooters`) not fighting him, or not seeing him, turn to it
//! and fire bursts: the muzzle's flash, a tracer, the gun's sound. Its
//! `GroupFilter` names whom it stands for (WIL_050's Oliver), who don't
//! shoot at it. No one is hurt.

use std::collections::HashMap;

use bevy::light::NotShadowCaster;
use bevy::prelude::*;

use crate::data::to_bevy;

pub struct StagedFirePlugin;

impl Plugin for StagedFirePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TargetMarkers>()
            .add_systems(Update, stage_fire.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A marker's settings, from its `CTargetComponent`.
#[derive(Clone, Copy, Debug)]
struct Marker {
    at: [f32; 3],
    reach: f32,
    shooters: usize,
    /// The group it stands for (`#%08X`), if it names one.
    group: Option<u32>,
    active: bool,
}

#[derive(Resource, Default)]
pub struct TargetMarkers {
    /// Markers a script switched on or off (`SetTargetActive`).
    pub switched: HashMap<u64, bool>,
    markers: Option<HashMap<u64, Marker>>,
    /// Shooters' bursts: when the next starts, and when the current ends.
    bursts: HashMap<u64, (f32, f32)>,
    /// Markers told of (the first time they had shooters).
    told: std::collections::HashSet<u64>,
}

/// A burst's length and the pause after it (s), and its shots' pace.
const BURST: (f32, f32) = (0.8, 2.0);
const PAUSE: (f32, f32) = (1.5, 4.0);
const SHOT_EVERY: f32 = 0.12;

fn read_markers(data: &fc3_story::data::StoryData) -> HashMap<u64, Marker> {
    data.entities
        .values()
        .filter_map(|e| {
            let target = e.component("CTargetComponent")?.child("Target")?;
            let flag = |name: &str| target.field(name).and_then(|b| b.first().copied()).is_some_and(|b| b != 0);
            Some((
                e.id,
                Marker {
                    at: e.pos,
                    reach: target.f32("fMaxRange").filter(|r| *r > 0.0).unwrap_or(40.0),
                    shooters: target.u32("MaxShooters").map_or(1, |n| n.clamp(1, 6) as usize),
                    group: target.u32("GroupFilter").filter(|&g| g != 0 && g != u32::MAX),
                    active: flag("bActive"),
                },
            ))
        })
        .collect()
}

#[allow(clippy::type_complexity)]
fn stage_fire(
    host: Option<NonSend<crate::story::StoryHost>>,
    time: Res<Time>,
    mut markers: ResMut<TargetMarkers>,
    mut shooters: Query<(&crate::story_actors::StoryActor, &mut Transform, &mut crate::npc::Npc)>,
    fx: Option<Res<crate::weapons::EffectAssets>>,
    mut particles: ResMut<crate::particles::Particles>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut commands: Commands,
    mut sfx: MessageWriter<crate::sound::PlaySfx>,
) {
    let (Some(host), Some(fx)) = (host, fx) else { return };
    let now = time.elapsed_secs();
    let markers = &mut *markers;
    let all = markers.markers.get_or_insert_with(|| read_markers(&host.story.data()));
    if all.is_empty() {
        return;
    }
    let mut firing: Vec<u64> = Vec::new();
    for (&id, m) in all.iter() {
        if !markers.switched.get(&id).copied().unwrap_or(m.active) || !host.story.is_loaded(id) {
            continue;
        }
        let at = to_bevy(m.at);
        let group: Option<Vec<u64>> = m.group.map(|g| host.story.group_members(&format!("#{g:08X}")));
        let mut near: Vec<(u64, f32)> = shooters
            .iter()
            .filter(|(a, t, npc)| {
                // (The player's foes; fighting him, while they don't see
                // him: WIL_050's guards fire at Oliver's stand-in while the
                // player snipes them from the hill. Not those the marker
                // stands for, its group: Oliver. Those standing: one walking
                // his rounds isn't turned about.)
                !a.dead
                    && !a.seated
                    && !a.in_scene
                    && npc.hostile()
                    && npc.velocity.length() < 1.0
                    && (npc.state != crate::npc::NpcState::Combat || !npc.sees_player)
                    && t.translation.distance(at) < m.reach
                    && group.as_ref().is_none_or(|g| !g.contains(&a.id))
            })
            .map(|(a, t, _)| (a.id, t.translation.distance(at)))
            .collect();
        near.sort_by(|a, b| a.1.total_cmp(&b.1));
        if !near.is_empty() && markers.told.insert(id) {
            info!("staged fire: {} on, {} shooting at it", host.story.data().name_of(id).unwrap_or("?"), near.len().min(m.shooters));
        }
        for (shooter, _) in near.into_iter().take(m.shooters) {
            firing.push(shooter);
            let Some((a, mut t, mut npc)) = shooters.iter_mut().find(|(a, ..)| a.id == shooter) else { continue };
            let (next, until) = *markers.bursts.entry(a.id).or_insert((now + rng.range(0.0, PAUSE.1), 0.0));
            // Turned to it; a burst now and then.
            let to = (at - t.translation).with_y(0.0);
            if to.length() > 0.1 {
                npc.yaw = (-to.x).atan2(-to.z);
                t.rotation = Quat::from_rotation_y(npc.yaw);
            }
            if now >= next && until < now {
                let len = rng.range(BURST.0, BURST.1);
                markers.bursts.insert(a.id, (now + len + rng.range(PAUSE.0, PAUSE.1), now + len));
                continue;
            }
            if now >= until || (now / SHOT_EVERY) as u32 == ((now - time.delta_secs()) / SHOT_EVERY) as u32 {
                continue;
            }
            let forward = Vec3::new(-npc.yaw.sin(), 0.0, -npc.yaw.cos());
            let muzzle = t.translation + Vec3::Y * 0.35 + forward * 0.7;
            let aim = at + Vec3::new(rng.range(-1.5, 1.5), rng.range(0.0, 1.8), rng.range(-1.5, 1.5));
            let dir = (aim - muzzle).normalize_or(forward);
            let weapon = match npc.role {
                crate::npc::Role::Heavy => "PKM",
                crate::npc::Role::Sniper => "SVD",
                crate::npc::Role::Shotgunner => "M133",
                _ => "AK47",
            };
            crate::weapons::flash_3p_into(&mut commands, &mut particles, &mut rng, weapon, muzzle, dir, 1.0);
            let end = muzzle + dir * muzzle.distance(aim);
            let len = muzzle.distance(end);
            if len > 1.0 {
                commands.spawn((
                    Mesh3d(fx.tracer_mesh.clone()),
                    MeshMaterial3d(fx.tracer_mat.clone()),
                    Transform::from_translation((muzzle + end) * 0.5).looking_at(end, Vec3::Y).with_scale(Vec3::new(1.0, 1.0, len)),
                    crate::weapons::Effect { age: 0.0, life: 0.05, grow: 0.0, light: false },
                    NotShadowCaster,
                ));
            }
            let event = match npc.role {
                crate::npc::Role::Shotgunner => crate::sound::ev::SHOTGUN_3P,
                crate::npc::Role::Sniper => crate::sound::ev::SNIPER_3P,
                crate::npc::Role::Heavy => crate::sound::ev::LMG_3P,
                _ => crate::sound::ev::RIFLE_3P,
            };
            sfx.write(crate::sound::PlaySfx { event, at: Some(muzzle), volume: 0.8 });
        }
    }
    markers.bursts.retain(|id, _| firing.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker reads its reach, shooters, group and whether it starts on.
    #[test]
    fn markers_read_their_target_settings() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let mut data = fc3_story::data::StoryData::default();
        let target = Object {
            name_hash: name_hash("Target"),
            fields: vec![
                (name_hash("bActive"), vec![1]),
                (name_hash("fMaxRange"), 25.0f32.to_le_bytes().to_vec()),
                (name_hash("MaxShooters"), 3u32.to_le_bytes().to_vec()),
                (name_hash("GroupFilter"), 0x2e69_beafu32.to_le_bytes().to_vec()),
            ],
            children: vec![],
        };
        let component = Arc::new(Object { name_hash: name_hash("CTargetComponent"), fields: vec![], children: vec![Arc::new(target)] });
        data.entities.insert(7, fc3_story::data::Entry {
            id: 7, name: "CombatHelpers/TargetMarker_0".into(), class: "CEntity".into(), pos: [1.0, 2.0, 3.0], angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER, origin: fc3_story::data::Origin::Global,
            components: Arc::new(Object { children: vec![component], ..Default::default() }), fields: Arc::new(Object::default()),
        });
        let m = read_markers(&data)[&7];
        assert!(m.active);
        assert_eq!((m.reach, m.shooters, m.group, m.at), (25.0, 3, Some(0x2e69_beaf), [1.0, 2.0, 3.0]));
    }
}
