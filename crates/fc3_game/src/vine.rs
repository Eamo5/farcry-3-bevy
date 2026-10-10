//! Mission vine gameplay: binds the exact scripted ladder and relocates the
//! player's pose between the authored anchor frames during its stand-up clip.

use std::collections::HashMap;
use bevy::prelude::*;
use crate::{cache_assets::{cache_path, Fc3Clip, Fc3Skeleton}, cinema::{Built, Cinema}, player::{Player, PlayerCamera},
    story::StoryHost, traversal::{Mount, Ways}};
use fc3_formats::cinema::{Clip, ClipKind, Group, GroupKind, Sequence};

pub struct VinePlugin;
impl Plugin for VinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Vines>().add_systems(Update,
            run.after(crate::story::StorySet).before(crate::cinema::CinemaSet)
                .run_if(in_state(crate::menu::GameState::Playing)))
            .add_systems(PreUpdate, restore_shake)
            .add_systems(PostUpdate, (align_dolly_view, shake_view).chain().after(crate::cinema::scene_view)
                .before(bevy::transform::TransformSystems::Propagate));
    }
}

pub struct Start {
    pub id: u64,
    pub params: HashMap<String, String>,
}

#[derive(Resource, Default)]
pub struct Vines {
    pub pending: Vec<Start>,
    pub cancelled: Vec<u64>,
    active: Option<Vine>,
}
impl Vines {
    pub fn reset(&mut self) { self.pending.clear(); self.cancelled.clear(); self.active = None; }
    pub fn owns_ladder(&self, id: u64) -> bool { self.active.as_ref().is_some_and(|v| v.ladder == id) }
}

enum Phase { Waiting, Climbing(f32), Dolly(f32), Animation }
struct Vine {
    id: u64,
    ladder: u64,
    first: Transform,
    second: Transform,
    settings: fc3_formats::gsf::VineSettings,
    clip: String,
    handle: Handle<Fc3Clip>,
    shake: Handle<Fc3Clip>,
    skeleton: Handle<Fc3Skeleton>,
    phase: Phase,
    destination: Transform,
    climb_yaw: f32,
    look_start: Vec2,
}

/// Keep the climbed offset and facing, rather than snapping to the second
/// anchor's origin. Both anchors describe coordinate frames, not endpoints.
fn relocate(pose: Transform, first: Transform, second: Transform) -> Transform {
    Transform::from_matrix(second.to_matrix() * first.to_matrix().inverse() * pose.to_matrix())
}

fn stand_up_origin(mut capsule: Transform, first: Transform, second: Transform) -> Transform {
    capsule.translation.y -= crate::player::HALF_HEIGHT;
    let mut origin = relocate(capsule, first, second);
    // The authored vine relocation applies a local X quarter-turn after
    // changing anchor frames: the horizontal vine becomes a standing pose.
    origin.rotation *= Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    origin
}

#[allow(clippy::too_many_arguments)]
fn run(
    host: Option<NonSendMut<StoryHost>>, time: Res<Time>, info: Option<Res<crate::data::CacheInfo>>,
    mut vines: ResMut<Vines>, ways: Res<Ways>, mut mount: ResMut<Mount>,
    mut cinema: ResMut<Cinema>, mut hold: ResMut<crate::fp::Cinematic>,
    server: Res<AssetServer>, clips: Res<Assets<Fc3Clip>>, moves: Res<crate::qte::MoveTable>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &crate::combat::Health)>, mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    let Ok((entity, mut pose, mut player, health)) = players.single_mut() else { return };
    let cancelled = std::mem::take(&mut vines.cancelled);
    for &id in &cancelled {
        if vines.active.as_ref().is_some_and(|v| v.id == id) {
            cinema.stop_built(&format!("vine_{id}"));
            if vines.active.as_ref().is_some_and(|v| mounted_on(&mount, &ways, v.ladder)
                || matches!(v.phase, Phase::Dolly(_) | Phase::Animation)) {
                *mount = Mount::None;
                player.driving = false;
                player.velocity = Vec3::ZERO;
                commands.entity(entity).remove::<avian3d::prelude::ColliderDisabled>();
            }
            vines.active = None;
        }
    }
    for start in std::mem::take(&mut vines.pending) {
        if cancelled.contains(&start.id) { continue; }
        let prepared = (|| -> anyhow::Result<Vine> {
            use anyhow::{Context, ensure};
            ensure!(vines.active.is_none(), "another vine gameplay is active");
            let settings = fc3_formats::gsf::VineSettings::parse(&std::fs::read(
                info.as_ref().context("cache unavailable")?.root.join(fc3_formats::gsf::CACHE_REL))?)?;
            let id = |key: &str| -> anyhow::Result<u64> { Ok(start.params.get(key).context("missing vine binding")?.parse()?) };
            let data = host.story.data();
            let anchor = |key: &str| -> anyhow::Result<Transform> {
                let e = data.entities.get(&id(key)?).context("missing vine anchor entity")?;
                Ok(Transform { translation: crate::data::to_bevy(e.pos),
                    rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(e.angles)), scale: Vec3::ONE })
            };
            let ladder = id("Ladder")?;
            ensure!(ladder != 0 && ladder != u64::MAX, "invalid vine ladder binding");
            ensure!(ways.data.ladders.iter().any(|l| l.id == ladder), "vine ladder missing from traversal cache");
            let states = moves.0.get(&settings.stand_up_move).context("missing vine stand-up move")?;
            ensure!(states.len() == 1, "vine stand-up move needs an unambiguous clip");
            let clip = states[0].1.clone();
            let handle = server.load(cache_path(&fc3_formats::cache::anim_rel(&clip)));
            let shake_states = moves.0.get(&settings.camera_shake_move).context("missing vine camera-shake move")?;
            ensure!(shake_states.len() == 1, "vine camera-shake move needs an unambiguous clip");
            let shake = server.load(cache_path(&fc3_formats::cache::anim_rel(&shake_states[0].1)));
            let skeleton = server.load(cache_path(&fc3_formats::cache::skeleton_rel(
                r"graphics\actors_\_skeleton_master\hips_ref.skeleton")));
            Ok(Vine { id: start.id, ladder, first: anchor("First_Vine_Point")?, second: anchor("Second_Vine_Point")?,
                settings, clip, handle, shake, skeleton, phase: Phase::Waiting, destination: Transform::IDENTITY,
                climb_yaw: 0.0, look_start: Vec2::ZERO })
        })();
        match prepared {
            Ok(v) => { info!("vine: armed gameplay {} on ladder {}", v.id, v.ladder); vines.active = Some(v); }
            Err(e) => warn!("vine: cannot start gameplay {}: {e:#}", start.id),
        }
    }
    let Some(v) = vines.active.as_mut() else { return };
    if health.dead || !host.story.is_loaded(v.ladder) {
        cinema.stop_built(&format!("vine_{}", v.id));
        if mounted_on(&mount, &ways, v.ladder) || matches!(v.phase, Phase::Dolly(_) | Phase::Animation) {
            *mount = Mount::None;
            player.driving = false;
            player.velocity = Vec3::ZERO;
            commands.entity(entity).remove::<avian3d::prelude::ColliderDisabled>();
        }
        vines.active = None;
        return;
    }
    let dt = time.delta_secs();
    if dt <= 0.0 { return; }
    let mounted = mounted_on(&mount, &ways, v.ladder);
    match &mut v.phase {
        Phase::Waiting if mounted => {
            v.climb_yaw = player.yaw;
            v.phase = Phase::Climbing(0.0);
            info!("vine: climb started");
        }
        Phase::Waiting => {}
        Phase::Climbing(t) => {
            if !mounted { v.phase = Phase::Waiting; return; }
            *t += dt;
            if *t >= v.settings.climb_max_time && clips.get(&v.handle).is_some() {
                v.destination = stand_up_origin(*pose, v.first, v.second);
                v.look_start = Vec2::new(player.yaw, player.pitch);
                v.phase = Phase::Dolly(0.0);
                hold.0 = hold.0.max(0.2);
                player.velocity = Vec3::ZERO;
                host.story.message("dolly_zoom_started");
                info!("vine: dolly phase, destination {:?}", v.destination.translation);
            }
        }
        Phase::Dolly(t) => {
            hold.0 = hold.0.max(0.2);
            *t += dt;
            if *t >= v.settings.dolly_duration {
                let Some(clip) = clips.get(&v.handle) else { return };
                let duration = clip.0.duration;
                let name = format!("vine_{}", v.id);
                // A gameplay move starts from the pawn frame, unlike a
                // cinematic scene authored around the clip's absolute root.
                let origin = Transform::from_matrix(v.destination.to_matrix()
                    * crate::cinema::clip_root(&clip.0, 0.0).to_matrix().inverse());
                info!("vine: stand-up rotation {:?}, clip origin {:?}", v.destination.rotation, origin);
                cinema.play_built_tracked(Built { name: name.clone(), origin, bindings: HashMap::new(),
                    seq: Sequence { name, duration, cutscene: true, groups: vec![Group {
                        uid: 1, kind: GroupKind::Player, name: "player".into(), archetype: 0,
                        clips: vec![Clip { start: 0.0, duration, kind: ClipKind::Animation {
                            source: v.clip.clone(), from: 0, to: (duration * 30.0) as u32, blend: 0.15, anchored: false,
                        }}],
                    }], ..default() },
                });
                host.story.message("DisablePostFX");
                v.phase = Phase::Animation;
            }
        }
        Phase::Animation => {
            hold.0 = hold.0.max(0.2);
            if cinema.take_built_finished(&format!("vine_{}", v.id)) {
                // The player-track scene already carried the capsule to its
                // final camera pose. Restoring the pre-animation destination
                // here would undo the stand-up motion.
                pose.rotation = Quat::from_rotation_y(player.yaw);
                player.velocity = Vec3::ZERO;
                player.driving = false;
                player.spawning = true;
                player.keep_height = true;
                *mount = Mount::None;
                commands.entity(entity).remove::<avian3d::prelude::ColliderDisabled>();
                let id = v.id;
                vines.active = None;
                host.story.gameplay_ended(id);
                host.story.message("vine_climbing_ended");
                info!("vine: transition completed at {:?}", pose.translation);
            } else if cinema.progress(&format!("vine_{}", v.id)).is_none() {
                // A scene canceled outside GSF must relinquish control, but
                // must not notify Lua that the vine transition succeeded.
                warn!("vine: stand-up playback interrupted for gameplay {}", v.id);
                *mount = Mount::None;
                player.driving = false;
                player.velocity = Vec3::ZERO;
                commands.entity(entity).remove::<avian3d::prelude::ColliderDisabled>();
                vines.active = None;
            }
        }
    }
}

fn mounted_on(mount: &Mount, ways: &Ways, id: u64) -> bool {
    matches!(*mount, Mount::Ladder { index, .. } if ways.data.ladders.get(index).is_some_and(|l| l.id == id))
}

#[derive(Component)]
struct ShakenView { base: Transform, applied: Transform }

fn dolly_angles(start: Vec2, facing: f32, upper_degrees: f32, elapsed: f32, duration: f32) -> Vec2 {
    let fraction = if duration > 0.0 { (elapsed / duration).clamp(0.0, 1.0) } else { 1.0 };
    let turn = (facing - start.x + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    Vec2::new(start.x + turn * fraction, start.y + (upper_degrees.to_radians() - start.y) * fraction)
}

/// The original transition aligns the view toward the top while its dolly
/// timer runs (Camera_Look_Up_Time), before playing the stand-up animation.
fn align_dolly_view(
    vines: Res<Vines>, cinema: Res<Cinema>, players: Query<&Player>,
    mut cameras: Query<&mut Transform, With<PlayerCamera>>,
) {
    if cinema.view.is_some() { return; }
    let Some(v) = &vines.active else { return };
    let Phase::Dolly(t) = v.phase else { return };
    let (Ok(player), Ok(mut view)) = (players.single(), cameras.single_mut()) else { return };
    let angles = dolly_angles(v.look_start, v.climb_yaw, v.settings.upper_look, t, v.settings.look_up_time);
    view.rotation = Quat::from_rotation_y(angles.x - player.yaw) * Quat::from_rotation_x(angles.y);
}

/// Retract our previous offset before any player/scene camera updates. If a
/// reload replaced the transform already, its new pose takes precedence.
fn restore_shake(mut commands: Commands, mut cameras: Query<(Entity, &mut Transform, &ShakenView)>) {
    for (entity, mut view, shake) in &mut cameras {
        if *view == shake.applied { *view = shake.base; }
        commands.entity(entity).remove::<ShakenView>();
    }
}

/// Apply the authored camera's relative motion, never its absolute bind pose.
fn shake_view(
    vines: Res<Vines>, cinema: Res<Cinema>, clips: Res<Assets<Fc3Clip>>, skeletons: Res<Assets<Fc3Skeleton>>,
    mut cameras: Query<(Entity, &mut Transform), With<PlayerCamera>>, mut commands: Commands,
) {
    if cinema.view.is_some() { return; }
    let Some(v) = &vines.active else { return };
    let Phase::Dolly(t) = v.phase else { return };
    let start = (v.settings.dolly_duration - v.settings.shake_before).max(0.0);
    if t < start { return; }
    let (Some(clip), Some(skeleton)) = (clips.get(&v.shake), skeletons.get(&v.skeleton)) else { return };
    let camera = |time| crate::cinema::bone_world(&skeleton.0, &clip.0, time, "Camera")
        .map(|bone| crate::cinema::clip_root(&clip.0, time).to_matrix() * bone);
    let (Some(first), Some(now)) = (camera(0.0), camera((t - start).min(clip.0.duration))) else { return };
    let (_, rotation, translation) = (first.inverse() * now).to_scale_rotation_translation();
    if let Ok((entity, mut view)) = cameras.single_mut() {
        let base = *view;
        *view = base * Transform { translation, rotation, scale: Vec3::ONE };
        commands.entity(entity).insert(ShakenView { base, applied: *view });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dolly_alignment_uses_authored_time_and_shortest_yaw_without_moving_the_body() {
        let mut app = App::new();
        app.init_resource::<Vines>().init_resource::<Cinema>().add_systems(Update, align_dolly_view);
        let start = Vec2::new(179.0f32.to_radians(), -30.0f32.to_radians());
        let mut v = active_vine();
        v.phase = Phase::Dolly(0.2);
        v.look_start = start;
        v.climb_yaw = -179.0f32.to_radians();
        v.settings.upper_look = 10.0;
        v.settings.look_up_time = 0.4;
        app.world_mut().resource_mut::<Vines>().active = Some(v);
        let body = app.world_mut().spawn((Transform::from_xyz(1.0, 94.0, 2.0), Player {
            velocity: Vec3::ZERO, yaw: start.x, pitch: start.y, grounded: false, crouching: false, swimming: false,
            noclip: false, spawning: false, keep_height: false, last_landing_speed: 0.0, sprinting: false,
            spawned_at: 0.0, driving: true, eye: crate::player::EYE_HEIGHT, air: crate::player::AirMode::None,
            mantle: None, mounted: None, sliding: None, dragging: false,
        })).id();
        let camera = app.world_mut().spawn((PlayerCamera, Transform::from_xyz(0.0, 0.72, 0.0))).id();
        app.update();
        let at = *app.world().get::<Transform>(camera).unwrap();
        let expected = Quat::from_rotation_y(1.0f32.to_radians()) * Quat::from_rotation_x(-10.0f32.to_radians());
        assert!(at.rotation.abs_diff_eq(expected, 1e-5));
        assert_eq!(at.translation, Vec3::new(0.0, 0.72, 0.0));
        assert_eq!(app.world().get::<Transform>(body).unwrap().translation, Vec3::new(1.0, 94.0, 2.0));
        assert_eq!(app.world().get::<Player>(body).unwrap().pitch, start.y);
        app.world_mut().resource_mut::<Vines>().active.as_mut().unwrap().phase = Phase::Dolly(2.0);
        app.update();
        let held = *app.world().get::<Transform>(camera).unwrap();
        let expected = Quat::from_rotation_y(2.0f32.to_radians()) * Quat::from_rotation_x(10.0f32.to_radians());
        assert!(held.rotation.abs_diff_eq(expected, 1e-5));
        app.world_mut().resource_mut::<Cinema>().view = Some((Transform::IDENTITY, 60.0));
        *app.world_mut().get_mut::<Transform>(camera).unwrap() = Transform::IDENTITY;
        app.update();
        assert_eq!(*app.world().get::<Transform>(camera).unwrap(), Transform::IDENTITY);
        assert!(dolly_angles(start, start.x, 10.0, 0.0, 0.0).is_finite());
    }

    fn active_vine() -> Vine {
        Vine { id: 408, ladder: 123, first: Transform::IDENTITY, second: Transform::IDENTITY,
            clip: String::new(), handle: Handle::default(), shake: Handle::default(), skeleton: Handle::default(),
            phase: Phase::Animation, destination: Transform::IDENTITY, climb_yaw: 0.0, look_start: Vec2::ZERO,
            settings: fc3_formats::gsf::VineSettings {
                stand_up_move: String::new(), camera_shake_move: String::new(), teleport_particle: String::new(),
                teleport_postfx: String::new(), dolly_postfx: String::new(), camera_speed: 1.0,
                horizontal_limits: [-30.0, 30.0], vertical_limits: [-60.0, 0.0], particle_after: 0.0,
                particle_before: 0.0, particle_offset_z: 0.0, postfx_before: -1.0, shake_before: 0.0,
                camera_blend_time: 0.0, upper_look: 0.0, look_up_time: 0.0, climb_max_time: 6.0, dolly_duration: 2.5,
            } }
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE with the authored vine camera-shake clip"]
    fn installed_shake_uses_the_authored_camera_and_yields_to_cinematics() {
        let root = std::path::PathBuf::from(std::env::var_os("FC3_TEST_CACHE").expect("FC3_TEST_CACHE"));
        let clip = fc3_formats::anim::parse_clip(&std::fs::read(root.join(fc3_formats::cache::anim_rel(
            r"graphics\characters\_common\animations\exoticgameplay\ex_doc_01\vineclimbing\vine_hallucination_camerashake.mab"))).unwrap()).unwrap();
        let skeleton = fc3_formats::anim::parse_skeleton(&std::fs::read(root.join(fc3_formats::cache::skeleton_rel(
            r"graphics\actors_\_skeleton_master\hips_ref.skeleton"))).unwrap()).unwrap();
        assert!((clip.duration - 1.0).abs() < 0.001);
        let mut app = App::new();
        app.init_resource::<Vines>().init_resource::<Cinema>()
            .init_resource::<Assets<Fc3Clip>>().init_resource::<Assets<Fc3Skeleton>>()
            .add_systems(PreUpdate, restore_shake).add_systems(Update, shake_view);
        let mut vine = active_vine();
        vine.shake = app.world_mut().resource_mut::<Assets<Fc3Clip>>().add(Fc3Clip(std::sync::Arc::new(clip)));
        vine.skeleton = app.world_mut().resource_mut::<Assets<Fc3Skeleton>>().add(Fc3Skeleton(std::sync::Arc::new(skeleton)));
        vine.settings.shake_before = 1.0;
        vine.phase = Phase::Dolly(1.4);
        app.world_mut().resource_mut::<Vines>().active = Some(vine);
        let base = Transform::from_xyz(0.0, 0.72, 0.0).with_rotation(Quat::from_rotation_x(0.2));
        let camera = app.world_mut().spawn((PlayerCamera, base)).id();
        app.update();
        assert_eq!(*app.world().get::<Transform>(camera).unwrap(), base);
        app.world_mut().resource_mut::<Vines>().active.as_mut().unwrap().phase = Phase::Dolly(1.8);
        app.update();
        let shaken = *app.world().get::<Transform>(camera).unwrap();
        assert!(shaken.rotation.is_finite());
        assert!(base.rotation.angle_between(shaken.rotation) > 0.001, "authored clip must move the view");
        assert!(shaken.translation.is_finite());
        app.update();
        assert_eq!(*app.world().get::<Transform>(camera).unwrap(), shaken, "paused shake must not accumulate");
        app.world_mut().resource_mut::<Cinema>().view = Some((Transform::IDENTITY, 60.0));
        app.update();
        assert_eq!(*app.world().get::<Transform>(camera).unwrap(), base);
        app.world_mut().resource_mut::<Cinema>().view = None;
        app.update();
        assert_eq!(*app.world().get::<Transform>(camera).unwrap(), shaken);
        app.world_mut().resource_mut::<Vines>().reset();
        app.update();
        assert_eq!(*app.world().get::<Transform>(camera).unwrap(), base);
    }

    #[test]
    fn cancellation_and_layer_loss_release_animation_ownership_without_a_mount() {
        for cancel in [true, false] {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
                .init_asset::<Fc3Clip>().init_resource::<Vines>().init_resource::<Ways>()
                .init_resource::<Mount>().init_resource::<Cinema>().init_resource::<crate::fp::Cinematic>()
                .init_resource::<crate::qte::MoveTable>().add_systems(Update, run);
            let pack = fc3_formats::scripts::ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap();
            let story = fc3_story::Story::new(pack, fc3_story::data::StoryData::default()).unwrap();
            app.insert_non_send(StoryHost::new(story, None));
            let entity = app.world_mut().spawn((Transform::from_xyz(1.0, 94.0, 2.0), Player {
                velocity: Vec3::Y, yaw: 0.0, pitch: 0.0, grounded: false, crouching: false, swimming: false,
                noclip: false, spawning: false, keep_height: false, last_landing_speed: 0.0, sprinting: false,
                spawned_at: 0.0, driving: true, eye: crate::player::EYE_HEIGHT, air: crate::player::AirMode::None,
                mantle: None, mounted: None, sliding: None, dragging: false,
            }, crate::combat::Health::new(100.0), avian3d::prelude::ColliderDisabled)).id();
            let mut vines = app.world_mut().resource_mut::<Vines>();
            vines.active = Some(active_vine());
            if cancel { vines.cancelled.push(408); }
            drop(vines);
            app.world_mut().resource_mut::<Cinema>().play_built_tracked(Built {
                name: "vine_408".into(), origin: Transform::IDENTITY, bindings: HashMap::new(),
                seq: Sequence { duration: 3.4, ..default() },
            });
            app.update();
            assert!(app.world().resource::<Vines>().active.is_none());
            assert!(!app.world().get::<Player>(entity).unwrap().driving);
            assert_eq!(app.world().get::<Player>(entity).unwrap().velocity, Vec3::ZERO);
            assert!(app.world().get::<avian3d::prelude::ColliderDisabled>(entity).is_none());
            assert_eq!(app.world().get::<Transform>(entity).unwrap().translation, Vec3::new(1.0, 94.0, 2.0));
            assert!(app.world().resource::<Cinema>().progress("vine_408").is_none());
            assert!(!app.world_mut().resource_mut::<Cinema>().take_built_finished("vine_408"));
        }
    }

    #[test]
    fn relocation_preserves_the_player_pose_relative_to_rotated_anchors() {
        let first = Transform::from_xyz(20.0, 30.0, 40.0).with_rotation(Quat::from_rotation_y(0.7));
        let second = Transform::from_xyz(-12.0, 8.0, 63.0).with_rotation(Quat::from_rotation_y(-1.2));
        let relative = Transform::from_xyz(0.5, 6.0, -0.7).with_rotation(Quat::from_rotation_y(0.3));
        let pose = Transform::from_matrix(first.to_matrix() * relative.to_matrix());
        let moved = relocate(pose, first, second);
        let local = second.to_matrix().inverse() * moved.to_matrix();
        assert!(local.abs_diff_eq(relative.to_matrix(), 0.0001));
        assert!(relocate(moved, second, first).to_matrix().abs_diff_eq(pose.to_matrix(), 0.0001));
    }
}
