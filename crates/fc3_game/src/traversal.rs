//! Getting around: zip lines, ladders and radio towers, plus the power
//! lines strung between poles.
//!
//! Cables come from the converted world (each pair of linked anchors) and
//! are drawn as sagging wires. Zip lines can be ridden from their high end;
//! ladders are climbed by walking into them; a radio tower's control panel,
//! once reached at the top, reveals the map around it and frees a weapon in
//! the shops.

use std::collections::HashMap;

use avian3d::prelude::ColliderDisabled;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use fc3_formats::traversal::{Cable, Traversal};

use crate::combat::Health;
use crate::controls::{Action, ActionState};
use crate::data::CacheInfo;
use crate::hud::{HudSet, HudState};
use crate::player::{Player, HALF_HEIGHT};

pub struct TraversalPlugin;

impl Plugin for TraversalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Ways>().init_resource::<Mount>().init_resource::<Unusable>().add_message::<TowerTaken>().add_systems(
            Update,
            (
                load_ways.run_if(resource_exists::<CacheInfo>),
                (zip_prompt, tower_prompt).before(HudSet).after(crate::turrets::man_turrets).run_if(traversal_controls_available),
                climb.run_if(traversal_controls_available),
                ride.run_if(traversal_controls_available),
            )
                .chain(),
        ).add_systems(PreUpdate, traversal_test.after(crate::controls::update_actions).before(crate::controls::apply_action_maps)
            .run_if(traversal_controls_available));
    }
}

// Traversal owns the capsule just like walking does. Menus and cinematic
// holds must suspend that ownership, including already-mounted movement.
fn traversal_controls_available(state: Res<State<crate::menu::GameState>>, hold: Res<crate::fp::Cinematic>) -> bool {
    *state.get() == crate::menu::GameState::Playing && hold.0 <= 0.0
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::menu::GameState;

    fn mounted_player() -> Player {
        Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, grounded: false, crouching: false,
            swimming: false, noclip: false, spawning: false, keep_height: false,
            last_landing_speed: 0.0, sprinting: false, spawned_at: 0.0, driving: true, armed_passenger: false,
            eye: 1.7, air: default(), mantle: None, mounted: None, sliding: None, dragging: false,
        }
    }

    #[test]
    fn ladder_velocity_tracks_motion_and_stops_at_a_blocked_top() {
        use avian3d::prelude::*;
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<ActionState>()
            .init_resource::<crate::mission_layers::MissionLayers>()
            .init_resource::<crate::vine::Vines>()
            .init_resource::<crate::player::PlayerBody>()
            .insert_resource(Ways { data: Traversal {
                ladders: vec![fc3_formats::traversal::Ladder {
                    id: 0, mission_layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
                    animation_package: 0,
                    base: [0.0; 3], top: 3.0, facing: [0.0, 0.0, 1.0], hand_over_hand: false,
                }], ..default()
            }, ..default() })
            .insert_resource(Mount::Ladder { index: 0, side: Vec3::Z });
        let e = app.world_mut().spawn((Transform::from_xyz(0.0, HALF_HEIGHT + 1.0, 0.45),
            Health::new(100.0), ColliderDisabled, mounted_player())).id();
        app.update();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(100));
        app.insert_resource(time);
        for axis in [1.0, 0.0, -1.0] {
            app.world_mut().resource_mut::<ActionState>().movement.y = axis;
            app.world_mut().run_system_once(climb).unwrap();
            assert!((app.world().get::<Player>(e).unwrap().velocity.y - axis * 3.0).abs() < 0.001);
        }
        app.world_mut().resource_mut::<ActionState>().movement.y = 1.0;
        for _ in 0..12 { app.world_mut().run_system_once(climb).unwrap(); }
        assert!((app.world().get::<Transform>(e).unwrap().translation.y - (3.0 - 0.05 + HALF_HEIGHT)).abs() < 0.001);
        assert_eq!(app.world().get::<Player>(e).unwrap().velocity, Vec3::ZERO);
        assert!(matches!(*app.world().resource::<Mount>(), Mount::Ladder { .. }));
    }

    #[test]
    fn walking_onto_a_ladder_at_its_top_turns_to_the_rungs_to_climb_down() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin, avian3d::prelude::PhysicsPlugins::default()))
            .init_resource::<ActionState>()
            .init_resource::<crate::mission_layers::MissionLayers>()
            .init_resource::<crate::vine::Vines>()
            .init_resource::<crate::player::PlayerBody>()
            .init_resource::<Mount>()
            .insert_resource(Ways { data: Traversal {
                ladders: vec![fc3_formats::traversal::Ladder {
                    id: 0, mission_layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
                    animation_package: 0,
                    base: [0.0; 3], top: 4.0, facing: [0.0, 0.0, -1.0], hand_over_hand: false,
                }], ..default()
            }, ..default() });
        // On top of the wall (the rungs face -z: it stands at -z), walking
        // back towards the edge (+z), facing it.
        let mut player = mounted_player();
        player.driving = false;
        player.yaw = std::f32::consts::PI;
        let e = app.world_mut().spawn((Transform::from_xyz(0.0, 4.0 + HALF_HEIGHT, -0.6), Health::new(100.0), player)).id();
        app.update();
        app.world_mut().resource_mut::<ActionState>().movement.y = 1.0;
        app.world_mut().run_system_once(climb).unwrap();
        assert!(matches!(*app.world().resource::<Mount>(), Mount::Ladder { side, .. } if side == Vec3::Z));
        let t = app.world().get::<Transform>(e).unwrap().translation;
        assert!(t.abs_diff_eq(Vec3::new(0.0, 3.4 + HALF_HEIGHT, 0.45), 1e-4), "{t:?}");
        // Facing the rungs (-z).
        let yaw = app.world().get::<Player>(e).unwrap().yaw;
        assert!(Vec3::new(-yaw.sin(), 0.0, -yaw.cos()).abs_diff_eq(Vec3::NEG_Z, 1e-4));
    }

    #[derive(Resource, Default)]
    struct Landing(Option<(Vec3, Vec3)>);

    fn probe_landing(spatial: avian3d::prelude::SpatialQuery, mut landing: ResMut<Landing>) {
        landing.0 = ladder_landing(&spatial, Vec3::ZERO, 3.0, Vec3::Z, &default());
    }

    #[test]
    fn ladder_exit_requires_capsule_clearance_and_tries_the_other_side() {
        use avian3d::prelude::*;
        use bevy::ecs::system::RunSystemOnce;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default(), bevy::mesh::MeshPlugin,
            bevy::transform::TransformPlugin, PhysicsPlugins::default()))
            .init_resource::<Landing>();
        app.world_mut().spawn((RigidBody::Static, Collider::cuboid(8.0, 1.0, 8.0), Transform::from_xyz(0.0, 2.5, 0.0)));
        let wall = app.world_mut().spawn((RigidBody::Static, Collider::cuboid(2.0, 3.0, 0.7), Transform::from_xyz(0.0, 4.5, -0.8))).id();
        for _ in 0..4 { app.update(); }
        app.world_mut().run_system_once(probe_landing).unwrap();
        let (dir, centre) = app.world().resource::<Landing>().0.expect("opposite side is clear");
        assert_eq!(dir, Vec3::Z);
        assert!((centre.y - (3.0 + HALF_HEIGHT + 0.05)).abs() < 0.001);

        let ceiling = app.world_mut().spawn((RigidBody::Static, Collider::cuboid(8.0, 0.2, 8.0), Transform::from_xyz(0.0, 4.6, 0.0))).id();
        for _ in 0..4 { app.update(); }
        app.world_mut().run_system_once(probe_landing).unwrap();
        assert!(app.world().resource::<Landing>().0.is_none(), "low ceiling must prevent dismount");

        app.world_mut().despawn(wall);
        app.world_mut().despawn(ceiling);
        for _ in 0..4 { app.update(); }
        app.world_mut().run_system_once(probe_landing).unwrap();
        assert_eq!(app.world().resource::<Landing>().0.unwrap().0, Vec3::NEG_Z);
    }

    #[test]
    fn mounted_zipline_freezes_in_menus_and_cinematics_then_resumes() {
        let mut app = App::new();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(100));
        app.insert_resource(time)
            .insert_resource(State::new(GameState::Playing))
            .init_resource::<crate::fp::Cinematic>()
            .init_resource::<ActionState>()
            .insert_resource(Ways { data: Traversal {
                cables: vec![Cable { zipline: true, a: [0.0, 20.0, 0.0], b: [0.0, 10.0, -100.0] }],
                ..default()
            }, ..default() })
            .insert_resource(Mount::Zip { cable: 0, s: 0.0, speed: 4.0 })
            .add_systems(Update, ride.run_if(traversal_controls_available));
        let e = app.world_mut().spawn((Transform::default(), Health::new(100.0), ColliderDisabled, mounted_player())).id();
        app.update();
        let position = app.world().get::<Transform>(e).unwrap().translation;
        let mounted = *app.world().resource::<Mount>();
        for state in [GameState::Paused, GameState::Map, GameState::Journal, GameState::Shop, GameState::MainMenu, GameState::Loading] {
            app.insert_resource(State::new(state));
            app.update();
            assert_eq!(app.world().get::<Transform>(e).unwrap().translation, position);
            assert_eq!(*app.world().resource::<Mount>(), mounted);
        }
        app.insert_resource(State::new(GameState::Playing));
        app.world_mut().resource_mut::<crate::fp::Cinematic>().0 = 0.2;
        app.update();
        assert_eq!(app.world().get::<Transform>(e).unwrap().translation, position);
        assert_eq!(*app.world().resource::<Mount>(), mounted);
        app.world_mut().resource_mut::<crate::fp::Cinematic>().0 = 0.0;
        app.update();
        assert_ne!(app.world().get::<Transform>(e).unwrap().translation, position);
        assert!(app.world().get::<Player>(e).unwrap().driving);
        assert!(app.world().get::<ColliderDisabled>(e).is_some());
    }
}

/// The world's cables, ladders and towers.
#[derive(Resource, Default)]
pub struct Ways {
    loaded: bool,
    pub data: Traversal,
}

/// What the player is holding on to.
#[derive(Resource, Default, Clone, Copy, PartialEq, Debug)]
pub enum Mount {
    #[default]
    None,
    /// Riding a zip line: distance travelled from the high end, and speed.
    Zip { cable: usize, s: f32, speed: f32 },
    /// On a ladder, on the side `side` points to.
    Ladder { index: usize, side: Vec3 },
}

/// Where a cable hangs at `t` (0..1 from `a` to `b`).
fn cable_point(c: &Cable, t: f32) -> Vec3 {
    let (a, b) = (Vec3::from(c.a), Vec3::from(c.b));
    let sag = a.distance(b) * if c.zipline { 0.012 } else { 0.025 };
    a.lerp(b, t) - Vec3::Y * sag * 4.0 * t * (1.0 - t)
}

/// A zip line's ends, high end first.
fn zip_ends(c: &Cable) -> (Vec3, Vec3) {
    let (a, b) = (Vec3::from(c.a), Vec3::from(c.b));
    if a.y >= b.y {
        (a, b)
    } else {
        (b, a)
    }
}

/// One cable as a thin four-sided tube.
fn add_cable(c: &Cable, origin: Vec3, positions: &mut Vec<[f32; 3]>, normals: &mut Vec<[f32; 3]>, indices: &mut Vec<u32>) {
    const SEGMENTS: usize = 14;
    let radius = if c.zipline { 0.03 } else { 0.018 };
    let dir = (Vec3::from(c.b) - Vec3::from(c.a)).normalize_or(Vec3::X);
    let side = dir.cross(Vec3::Y).normalize_or(Vec3::Z);
    let up = side.cross(dir);
    let ring = [side, up, -side, -up];
    let base = positions.len() as u32;
    for i in 0..=SEGMENTS {
        let p = cable_point(c, i as f32 / SEGMENTS as f32) - origin;
        for n in ring {
            positions.push((p + n * radius).to_array());
            normals.push(n.to_array());
        }
    }
    for i in 0..SEGMENTS as u32 {
        for k in 0..4u32 {
            let a = base + i * 4 + k;
            let b = base + i * 4 + (k + 1) % 4;
            let (c2, d) = (a + 4, b + 4);
            indices.extend_from_slice(&[a, c2, b, b, c2, d]);
        }
    }
}

fn load_ways(
    info: Res<CacheInfo>,
    mut ways: ResMut<Ways>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    if ways.loaded {
        return;
    }
    ways.loaded = true;
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::TRAVERSAL_REL)).unwrap_or_default();
    ways.data = Traversal::from_text(&text);
    info!("{} cables, {} ladders, {} radio towers", ways.data.cables.len(), ways.data.ladders.len(), ways.data.towers.len());
    // Cables merged into one mesh per 512 m cell, drawn out to 800 m.
    const CELL: f32 = 512.0;
    let mut cells: HashMap<(i32, i32), Vec<&Cable>> = HashMap::new();
    for c in &ways.data.cables {
        let mid = (Vec3::from(c.a) + Vec3::from(c.b)) * 0.5;
        cells.entry(((mid.x / CELL).floor() as i32, (mid.z / CELL).floor() as i32)).or_default().push(c);
    }
    let material = materials.add(StandardMaterial { base_color: Color::srgb(0.08, 0.08, 0.08), perceptual_roughness: 0.6, ..default() });
    for ((cx, cz), list) in cells {
        let origin = Vec3::new((cx as f32 + 0.5) * CELL, 0.0, (cz as f32 + 0.5) * CELL);
        let (mut positions, mut normals, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        for c in list {
            add_cable(c, origin, &mut positions, &mut normals, &mut indices);
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_indices(Indices::U32(indices));
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(origin),
            VisibilityRange { start_margin: 0.0..0.0, end_margin: 700.0..800.0, use_aabb: true },
            NotShadowCaster,
            Name::new("cables"),
        ));
    }
}

/// Where scripts have made a usable thing unusable
/// (`CAPI_Entity.SetUsableEntityUsability`: JAS_060's radio tower zip line
/// until the tower is done): a zip line with an end there isn't offered.
#[derive(Resource, Default)]
pub struct Unusable(pub std::collections::HashMap<u64, Vec3>);

/// How near (m) an unusable thing is to the zip line end it holds.
const UNUSABLE_REACH: f32 = 8.0;

/// Offers the zip lines whose high end is in reach.
#[allow(clippy::too_many_arguments)]
fn zip_prompt(
    text: Option<Res<crate::hud::GameText>>,
    mut actions: ResMut<ActionState>,
    settings: Res<crate::settings::Settings>,
    ways: Res<Ways>,
    mut mount: ResMut<Mount>,
    mut hud: ResMut<HudState>,
    mut players: Query<(Entity, &Transform, &mut Player, &Health)>,
    mut commands: Commands,
    unusable: Option<Res<Unusable>>,
) {
    let Ok((e, t, mut p, h)) = players.single_mut() else { return };
    if *mount != Mount::None || hud.prompt.is_some() || h.dead || p.driving || p.noclip || p.swimming {
        return;
    }
    let feet = t.translation.y - HALF_HEIGHT;
    let held = |c: &Cable| unusable.as_ref().is_some_and(|u| u.0.values().any(|at| at.distance(Vec3::from(c.a)) < UNUSABLE_REACH || at.distance(Vec3::from(c.b)) < UNUSABLE_REACH));
    let near = ways.data.cables.iter().enumerate().filter(|(_, c)| c.zipline && !held(c)).find(|(_, c)| {
        let (start, _) = zip_ends(c);
        let d = Vec2::new(start.x - t.translation.x, start.z - t.translation.z).length();
        d < 2.5 && (0.3..4.5).contains(&(start.y - feet))
    });
    let Some((i, _)) = near else { return };
    hud.prompt = Some(text.as_deref().and_then(|t| t.prompt(crate::hud::prompt::ZIPLINE, &actions, &settings)).unwrap_or_else(|| format!("{}  ride the zip line", actions.label(Action::Interact, &settings))));
    if actions.used() {
        actions.consume(Action::Interact);
        *mount = Mount::Zip { cable: i, s: 0.0, speed: 4.0 };
        p.driving = true;
        p.velocity = Vec3::ZERO;
        commands.entity(e).insert(ColliderDisabled);
    }
}

/// Slides along the zip line, faster the steeper it runs; jump to let go.
fn ride(
    time: Res<Time>,
    actions: Res<ActionState>,
    ways: Res<Ways>,
    mut mount: ResMut<Mount>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &Health)>,
    mut commands: Commands,
) {
    let Mount::Zip { cable, mut s, mut speed } = *mount else { return };
    let Ok((e, mut t, mut p, h)) = players.single_mut() else { return };
    let dt = time.delta_secs();
    let Some(c) = ways.data.cables.get(cable) else {
        *mount = Mount::None;
        return;
    };
    let (start, end) = zip_ends(c);
    let len = start.distance(end).max(1.0);
    let slope = (start.y - end.y) / len;
    speed = (speed + (9.81 * slope - 0.006 * speed * speed) * dt).clamp(3.0, 16.0);
    s += speed * dt;
    // Parametrised high end first, whichever end the cable lists first.
    let from_a = Vec3::from(c.a).y >= Vec3::from(c.b).y;
    let at = |s: f32| {
        let u = (s / len).clamp(0.0, 1.0);
        cable_point(c, if from_a { u } else { 1.0 - u })
    };
    let hang = at(s) - Vec3::Y * (2.05 - HALF_HEIGHT);
    t.translation = hang;
    let dir = (at(s + 0.5) - at(s)).normalize_or(Vec3::NEG_Z);
    let done = s >= len - 1.5 || h.dead || actions.just_pressed(Action::Jump);
    if done {
        *mount = Mount::None;
        p.driving = false;
        p.velocity = dir * speed * 0.6;
        commands.entity(e).remove::<ColliderDisabled>();
    } else {
        *mount = Mount::Zip { cable, s, speed };
    }
}

/// Climbing speed on a ladder without the game's figures (m/s).
const LADDER_SPEED: f32 = 3.0;

/// Walking into a ladder grabs it; forward and back climb, jump lets go,
/// and climbing past the top steps off onto whatever it leads to.
fn climb(
    time: Res<Time>,
    actions: Res<ActionState>,
    ways: Res<Ways>,
    vines: Res<crate::vine::Vines>,
    host: Option<NonSend<crate::story::StoryHost>>,
    layers: Res<crate::mission_layers::MissionLayers>,
    spatial: avian3d::prelude::SpatialQuery,
    mut mount: ResMut<Mount>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &Health)>,
    body: Res<crate::player::PlayerBody>,
    mut commands: Commands,
) {
    let Ok((e, mut t, mut p, h)) = players.single_mut() else { return };
    let dt = time.delta_secs();
    let feet = t.translation.y - HALF_HEIGHT;
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let available = |l: &fc3_formats::traversal::Ladder| {
        (l.mission_layer == fc3_formats::world::DEFAULT_MISSION_LAYER || layers.present.contains(&l.mission_layer))
        && (l.id == 0 || host.as_ref().is_none_or(|h| {
            !h.story.data().entities.contains_key(&l.id) || h.story.is_loaded(l.id)
        }))
    };
    match *mount {
        Mount::None => {
            if h.dead || p.driving || p.noclip || p.swimming || actions.movement.y < 0.3 {
                return;
            }
            let towards = |l: &fc3_formats::traversal::Ladder, reach: f32| {
                let base = Vec3::from(l.base);
                let to = Vec3::new(base.x - t.translation.x, 0.0, base.z - t.translation.z);
                to.length() < reach && forward.dot(to.normalize_or(forward)) > 0.2
            };
            let near = ways.data.ladders.iter().enumerate().find(|(_, l)| {
                available(l) && towards(l, 0.8) && feet > l.base[1] - 0.4 && feet < l.top - 0.3
            });
            // Or walking onto it at its top, to climb down.
            let from_top = || ways.data.ladders.iter().enumerate().find(|(_, l)| available(l) && towards(l, 1.0) && (feet - l.top).abs() < 0.6);
            let (i, l, top) = match near {
                Some((i, l)) => (i, l, false),
                None => match from_top() {
                    Some((i, l)) => (i, l, true),
                    None => return,
                },
            };
            let base = Vec3::from(l.base);
            let facing = Vec3::from(l.facing);
            let side = if top { -facing } else { Vec3::new(t.translation.x - base.x, 0.0, t.translation.z - base.z).normalize_or(-facing) };
            *mount = Mount::Ladder { index: i, side };
            p.driving = true;
            p.velocity = Vec3::ZERO;
            commands.entity(e).insert(ColliderDisabled);
            if top {
                // Turned round to the rungs, just under the top.
                p.yaw = (-facing.x).atan2(-facing.z);
                t.translation = Vec3::new(base.x, l.top - 0.6 + HALF_HEIGHT, base.z) + side * 0.45;
                info!("ladder: on {} from the top", l.id);
            }
        }
        Mount::Ladder { index, side } => {
            let Some(l) = ways.data.ladders.get(index).filter(|l| available(l)) else {
                *mount = Mount::None;
                p.driving = false;
                p.velocity = Vec3::ZERO;
                commands.entity(e).remove::<ColliderDisabled>();
                return;
            };
            let base = Vec3::from(l.base);
            let climb = actions.movement.y;
            // (The player's own pace on this kind of ladder.)
            let speed = body.0.climb.map(|c| c.ladder[usize::from(l.hand_over_hand)][0]).filter(|s| *s > 0.0).unwrap_or(LADDER_SPEED);
            let mut y = feet + climb * speed * dt;
            let release = |p: &mut Player, mount: &mut Mount, commands: &mut Commands| {
                *mount = Mount::None;
                p.driving = false;
                commands.entity(e).remove::<ColliderDisabled>();
            };
            if h.dead || actions.just_pressed(Action::Jump) {
                // Push off backwards.
                p.velocity = side * 2.5 + Vec3::Y * 1.5;
                release(&mut p, &mut mount, &mut commands);
                return;
            }
            if y >= l.top - 0.05 && climb > 0.0 {
                // Over the top onto the landing: beyond the ladder, else back on
                // this side; with nothing to stand on, stay at the top.
                let filter = avian3d::prelude::SpatialQueryFilter::from_excluded_entities([e]);
                let landing = if vines.owns_ladder(l.id) { None } else { ladder_landing(&spatial, base, l.top, side, &filter) };
                match landing {
                    Some((dir, centre)) => {
                        t.translation = centre;
                        p.velocity = dir * 1.0;
                        release(&mut p, &mut mount, &mut commands);
                    }
                    None => {
                        let y = l.top - 0.05;
                        p.velocity = Vec3::Y * climb_velocity(t.translation.y, y + HALF_HEIGHT, dt);
                        t.translation = Vec3::new(base.x, y + HALF_HEIGHT, base.z) + side * 0.45;
                    }
                }
                return;
            }
            if y <= base.y && climb < 0.0 {
                y = base.y;
                p.velocity = Vec3::ZERO;
                release(&mut p, &mut mount, &mut commands);
            } else {
                p.velocity = Vec3::Y * climb_velocity(t.translation.y, y + HALF_HEIGHT, dt);
            }
            t.translation = Vec3::new(base.x, y + HALF_HEIGHT, base.z) + side * 0.45;
        }
        Mount::Zip { .. } => {}
    }
}

/// How far over a ladder's top its landing may stand.
const LANDING_LIP: f32 = 1.2;

fn climb_velocity(from: f32, to: f32, dt: f32) -> f32 {
    if dt > 0.0 { (to - from) / dt } else { 0.0 }
}

/// A ground hit alone is not a landing: the capsule also needs headroom
/// and room beside the ladder. Reject steep rock faces and enclosed ledges.
/// (The ground may stand a little over the ladder's top, a lip pulled up
/// over: up to `LANDING_LIP` metres.)
fn ladder_landing(
    spatial: &avian3d::prelude::SpatialQuery,
    base: Vec3,
    top: f32,
    side: Vec3,
    filter: &avian3d::prelude::SpatialQueryFilter,
) -> Option<(Vec3, Vec3)> {
    let capsule = avian3d::prelude::Collider::capsule(crate::player::CAPSULE_RADIUS, crate::player::CAPSULE_LENGTH);
    [-side, side].into_iter().find_map(|dir| {
        let probe = Vec3::new(base.x, top + LANDING_LIP + 0.4, base.z) + dir * 0.8;
        let ground = spatial.cast_ray(probe, Dir3::NEG_Y, LANDING_LIP + 2.0, true, filter)?;
        if ground.normal.y < crate::player::MIN_GROUND_NORMAL_Y { return None; }
        let centre = probe.with_y(probe.y - ground.distance + HALF_HEIGHT + 0.05);
        spatial.shape_intersections(&capsule, centre, Quat::IDENTITY, filter).is_empty().then_some((dir, centre))
    })

}

/// A radio tower taken over, at its panel (the story hears of it).
#[derive(Message, Clone, Copy)]
pub struct TowerTaken {
    pub at: Vec3,
}

/// The original HUD's card for a radio tower taken (the game's strings),
/// and the count under it ("%d of 18").
const CARD_TOWER: u32 = 156250;
const CARD_TOWERS: u32 = 156252;

/// At a radio tower's panel: take the tower over.
#[allow(clippy::too_many_arguments)]
fn tower_prompt(
    mut actions: ResMut<ActionState>,
    settings: Res<crate::settings::Settings>,
    ways: Res<Ways>,
    catalog: Res<crate::progression::ShopCatalog>,
    mut progress: ResMut<crate::progression::Progress>,
    mut explored: ResMut<crate::map::Explored>,
    mut hud: ResMut<HudState>,
    mut xp: MessageWriter<crate::progression::XpGain>,
    players: Query<(&Transform, &Health, &crate::weapons::Inventory, &Player)>,
    mut taken: MessageWriter<TowerTaken>,
    skills: Option<Res<crate::progression::Skills>>,
    mut story_ui: ResMut<crate::story::StoryUi>,
) {
    let Ok((t, h, inv, p)) = players.single() else { return };
    // (Not while the world comes in round him: the autopilot took a tower
    // it was put beside behind the loading screen.)
    if hud.prompt.is_some() || h.dead || p.spawning {
        return;
    }
    let Some((i, at)) = ways.data.towers.iter().enumerate().find(|(_, p)| Vec3::from(**p).distance(t.translation) < 2.6) else { return };
    if progress.towers.contains(&(i as u32)) {
        return;
    }
    hud.prompt = Some(format!("{}  take over the radio tower", actions.label(Action::Interact, &settings)));
    // (The story's autopilot takes the towers it is brought to.)
    if actions.used() || std::env::var_os("FC3_STORY_AUTOPILOT").is_some() {
        actions.consume(Action::Interact);
        progress.towers.insert(i as u32);
        taken.write(TowerTaken { at: Vec3::from(*at) });
        explored.reveal(Vec3::from(*at), 1100.0);
        // (The game's own reward for a tower.)
        let amount = skills.as_ref().and_then(|s| s.xp_reward("RadioTower")).unwrap_or(0.0) as u32;
        if amount > 0 {
            xp.write(crate::progression::XpGain { amount, reason: "Radio tower" });
        }
        let freed = catalog.freed_weapons(&inv.defs, progress.towers.len());
        let reward = if freed.is_empty() { String::new() } else { format!(" Free in shops: {}.", freed.join(", ")) };
        hud.notify_native(format!("Radio tower taken: map revealed.{reward}"));
        story_ui.cards.push(crate::story::MissionCard::Event { text: CARD_TOWER, count: Some((CARD_TOWERS, progress.towers.len())) });
    }
}

/// `FC3_TRAVERSAL_TEST=zip|ladder|tower`: puts the player at the nearest
/// zip line, ladder or tower panel, works the controls and logs progress.
#[allow(clippy::too_many_arguments)]
fn traversal_test(
    time: Res<Time>,
    ways: Res<Ways>,
    layers: Res<crate::mission_layers::MissionLayers>,
    mount: Res<Mount>,
    progress: Res<crate::progression::Progress>,
    mut actions: ResMut<ActionState>,
    mut stage: Local<u32>,
    mut next_log: Local<f32>,
    mut ladder_time: Local<f32>,
    mut walk_waypoint: Local<usize>,
    (mut next_walk_jump, mut last_here, mut leg, mut press_until, mut stalled): (Local<f32>, Local<Option<Vec3>>, Local<u8>, Local<f32>, Local<f32>),
    (nav, mut nav_route): (Res<crate::nav::NavMesh>, Local<Option<(u8, Vec<Vec2>)>>),
    mut players: Query<(&mut Transform, &mut Player, &Health)>,
    cameras: Query<&Transform, (With<crate::player::PlayerCamera>, Without<Player>)>,
    (mut spawns, td, npcs, heights): (
        MessageWriter<crate::npc::SpawnEnemy>,
        Res<crate::stealth::Takedown>,
        Query<(&Transform, &Health), (With<crate::npc::Npc>, Without<Player>)>,
        Res<crate::terrain::Heightfield>,
    ),
) {
    let Ok(kind) = std::env::var("FC3_TRAVERSAL_TEST") else { return };
    let Ok((mut t, mut p, h)) = players.single_mut() else { return };
    if p.spawning || !ways.loaded {
        return;
    }
    let now = time.elapsed_secs();
    let here = t.translation;
    if h.dead && *stage == 3 {
        warn!("traversal test: post-dismount walk aborted on death at {here:?}");
        *stage = 4;
    }
    let face = |p: &mut Player, from: Vec3, to: Vec3| {
        let d = to - from;
        p.yaw = (-d.x).atan2(-d.z);
    };
    let base = if kind == "zip_takedown" { "zip" } else { kind.as_str() };
    // The walk's legs: FC3_TRAVERSAL_WALK_TO from the ladder; then
    // FC3_TRAVERSAL_WALK_AFTER once the story moves him elsewhere (a jump of
    // over 20 m), ending on the interaction; then FC3_TRAVERSAL_WALK_THEN.
    let moved = last_here.is_some_and(|l| l.distance(here) > TELEPORTED);
    *last_here = Some(here);
    let after = std::env::var("FC3_TRAVERSAL_WALK_AFTER").ok();
    if base == "ladder" && moved && *stage >= 3 && *leg == 0 && after.is_some() {
        *leg = 1;
        *walk_waypoint = 0;
        *stage = 3;
        info!("traversal test: moved to {here:?}; walking on from there");
    }
    match (*stage, base) {
        (0, "zip") => {
            // (For the takedown, one with room under it partway along.)
            let roomy = |c: &Cable| {
                let (a, b) = zip_ends(c);
                (6..19).any(|k| {
                    let p = a.lerp(b, k as f32 / 20.0);
                    heights.height_at(p).is_some_and(|g| (3.0..10.0).contains(&(p.y - g)))
                })
            };
            let Some(c) = ways
                .data
                .cables
                .iter()
                .filter(|c| c.zipline && (kind != "zip_takedown" || roomy(c)))
                .min_by(|a, b| zip_ends(a).0.distance(here).total_cmp(&zip_ends(b).0.distance(here)))
            else {
                return;
            };
            let (start, end) = zip_ends(c);
            let back = (start - end).with_y(0.0).normalize_or(Vec3::X);
            t.translation = start + back * 0.8 - Vec3::Y * (2.0 - HALF_HEIGHT);
            face(&mut p, t.translation, end);
            info!("zip test: at {start:?}, {:.0} m to {end:?}", start.distance(end));
            // `zip_takedown`: an enemy waits under the line, where the ground
            // is a few metres below it.
            if kind == "zip_takedown" {
                let under = (6..19).map(|k| start.lerp(end, k as f32 / 20.0)).find(|p| heights.height_at(*p).is_some_and(|g| (3.0..10.0).contains(&(p.y - g))));
                match under {
                    Some(at) => spawns.write(crate::npc::SpawnEnemy {
                        at,
                        hunting: None,
                        garrison: None,
                        faction: crate::npc::Faction::Pirate,
                        role: Some(crate::npc::Role::Assault),
                        ride: None,
                    }),
                    None => {
                        info!("zip test: no ground close under this line");
                        return;
                    }
                };
            }
            *stage = 1;
        }
        (1, "zip") => {
            actions.press(Action::Interact);
            *stage = 2;
        }
        (0, "ladder") => {
            let requested = std::env::var("FC3_TRAVERSAL_LADDER_ID").ok().and_then(|s| s.parse::<u64>().ok());
            let Some(l) = ways
                .data
                .ladders
                .iter()
                .filter(|l| requested.is_none_or(|id| id == l.id))
                .filter(|l| l.mission_layer == fc3_formats::world::DEFAULT_MISSION_LAYER || layers.present.contains(&l.mission_layer))
                .filter(|l| l.top - l.base[1] > 4.0)
                .min_by(|a, b| Vec3::from(a.base).distance(here).total_cmp(&Vec3::from(b.base).distance(here)))
            else {
                return;
            };
            let base = Vec3::from(l.base);
            // (A ladder faces its wall: the climber stands behind it.)
            let side = -Vec3::from(l.facing);
            t.translation = base + side * 0.7 + Vec3::Y * (HALF_HEIGHT + 0.05);
            face(&mut p, t.translation, base);
            info!("ladder test: entity {}, foot {base:?}, top {:.1}", l.id, l.top);
            *stage = 1;
        }
        (1, "ladder") => {
            actions.movement.y = 1.0;
            if matches!(*mount, Mount::Ladder { .. }) { *stage = 2; *ladder_time = 0.0; }
        }
        (2, "ladder") => {
            if matches!(*mount, Mount::Ladder { .. }) {
                *ladder_time += time.delta_secs();
                // Exercise all authored vine locomotion clips through input,
                // with enough initial height to slide without dismounting.
                actions.movement.y = if std::env::var("FC3_TRAVERSAL_LADDER_MOTION").as_deref() == Ok("cycle") {
                    match *ladder_time {
                        t if t < 1.5 => 1.0,
                        t if t < 2.25 => 0.0,
                        t if t < 3.0 => -1.0,
                        _ => 1.0,
                    }
                } else { 1.0 };
            }
            else { *stage = 3; }
        }
        (3, "ladder") => {
            // Optional post-dismount probe: ordinary look/walk controls only.
            // The initial ladder fixture is still a repositioned test setup.
            let route = match *leg {
                0 => std::env::var("FC3_TRAVERSAL_WALK_TO").ok(),
                1 => after.clone(),
                // (Its legs, one after another, split by `|`.)
                n => std::env::var("FC3_TRAVERSAL_WALK_THEN").ok().and_then(|s| s.split('|').nth(n as usize - 2).map(str::to_string)),
            };
            // (A leg `nav:<x>,<y>,<z>` goes the game's navigation mesh's way
            // there from where it starts.)
            let route = match route.as_deref().and_then(|r| r.strip_prefix("nav:")) {
                Some(goal) => {
                    if nav_route.as_ref().is_none_or(|(l, _)| *l != *leg) {
                        let goal: Vec<f32> = goal.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                        let goal = (goal.len() == 3).then(|| Vec3::new(goal[0], goal[1], goal[2]));
                        // (From his feet: the mesh lies on the ground.)
                        let feet = here - Vec3::Y * HALF_HEIGHT;
                        let Some(path) = goal.and_then(|g| nav.path(feet, g)) else {
                            if (now * 0.2).fract() < time.delta_secs() * 0.2 {
                                info!(
                                    "traversal test: no navigation mesh way to {goal:?} (mesh at him {:?}, at the goal {:?})",
                                    nav.height_at(feet),
                                    goal.and_then(|g| nav.height_at(g))
                                );
                            }
                            return;
                        };
                        info!("traversal test: navigation mesh way, {} points: {path:.1?}", path.len());
                        *nav_route = Some((*leg, path.iter().map(|p| Vec2::new(p.x, p.z)).collect()));
                    }
                    nav_route.as_ref().map(|(_, r)| r.iter().map(|p| format!("{},{}", p.x, p.y)).collect::<Vec<_>>().join(";"))
                }
                None => route,
            };
            if let Some(target) = route {
                let target_text = target.clone();
                if let Some(route) = walk_route(&target) {
                    let Some(target) = route.get(*walk_waypoint) else { *stage = 4; return };
                    // Narrow doorways need a tighter approach than the default
                    // outdoor probe. Both options still use ordinary inputs.
                    let precise_from = std::env::var("FC3_TRAVERSAL_WALK_RADIUS_FROM").ok()
                        .and_then(|s| s.parse::<usize>().ok()).unwrap_or(1).max(1);
                    let radius = std::env::var("FC3_TRAVERSAL_WALK_RADIUS").ok()
                        .and_then(|s| s.parse::<f32>().ok()).filter(|r| r.is_finite() && (0.05..=0.6).contains(r)).unwrap_or(0.6);
                    let radius = if *walk_waypoint + 1 >= precise_from { radius } else { 0.6 };
                    // (The later legs end on their points: a trigger to use.)
                    let radius = if *leg > 0 && *walk_waypoint + 1 == route.len() { AFTER_END_RADIUS } else { radius };
                    let crouch = std::env::var("FC3_TRAVERSAL_WALK_CROUCH_FROM").ok()
                        .and_then(|s| s.parse::<usize>().ok()).is_some_and(|from| from > 0 && *walk_waypoint + 1 >= from);
                    let crouch = crouch && std::env::var("FC3_TRAVERSAL_WALK_CROUCH_UNTIL").ok()
                        .and_then(|s| s.parse::<usize>().ok()).is_none_or(|until| *walk_waypoint < until);
                    // (A point written `x,zc` is walked to crouched, one written
                    // `x,zd` is gone to down any ladder taken on the way, and one
                    // written `x,zp` is reached precisely: a doorway.)
                    let mark = |m: char| target_text.split(';').nth(*walk_waypoint).is_some_and(|p| p.trim().ends_with(m));
                    let radius = if mark('p') { radius.min(0.2) } else { radius };
                    let crouch = crouch || mark('c');
                    if crouch { actions.hold(Action::Crouch); }
                    if mark('d') && matches!(*mount, Mount::Ladder { .. }) {
                        actions.movement.y = -1.0;
                        return;
                    }
                    let delta = Vec3::new(target.x - here.x, 0.0, target.y - here.z);
                    if delta.length() < radius {
                        *walk_waypoint += 1;
                        if *walk_waypoint == route.len() {
                            info!("traversal test: post-dismount walk reached {here:?}");
                            *stage = 4;
                        } else {
                            info!("traversal test: waypoint {}/{} reached {here:?}", *walk_waypoint, route.len());
                        }
                    } else {
                        let desired = (-delta.x).atan2(-delta.z);
                        let turn = (p.yaw - desired + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                        actions.look.x = turn.clamp(-time.delta_secs() * 2.0, time.delta_secs() * 2.0);
                        if turn.abs() < 0.2 {
                            actions.movement.y = 1.0;
                            // (Held up a moment while pushing on, not just
                            // setting off from a standstill.)
                            let slow = Vec2::new(p.velocity.x, p.velocity.z).length() < 0.5;
                            *stalled = if slow { *stalled + time.delta_secs() } else { 0.0 };
                            if std::env::var("FC3_TRAVERSAL_WALK_JUMP").as_deref() == Ok("1")
                                && !crouch && p.grounded && !p.driving && now >= *next_walk_jump
                                && *stalled > 0.4 {
                                actions.press(Action::Jump);
                                *next_walk_jump = now + 1.0;
                                info!("traversal test: ordinary jump probe at {here:?}");
                            }
                        }
                    }
                }
            }
        }
        // (With a route on, it waits to be moved there; the last leg ends
        // the walk.)
        (4, "ladder") if after.is_some() && *leg == 0 => {}
        (4, "ladder") if *leg >= 2 => {
            let legs = std::env::var("FC3_TRAVERSAL_WALK_THEN").map_or(0, |s| s.split('|').count());
            if (*leg as usize - 1) < legs {
                *leg += 1;
                *walk_waypoint = 0;
                *stage = 3;
                info!("traversal test: walking on, leg {}", *leg);
            } else {
                *stage = 6;
            }
        }
        (4, "ladder") => {
            // Optional end-of-route interaction: aim at a world point and
            // press the ordinary action once, without selecting an entity.
            if let Some(target) = std::env::var("FC3_TRAVERSAL_INTERACT_AT").ok().and_then(|s| {
                let values: Vec<f32> = s.split(',').filter_map(|v| v.parse().ok()).collect();
                (values.len() == 3 && values.iter().all(|v| v.is_finite())).then(|| Vec3::new(values[0], values[1], values[2]))
            }) {
                let eye = here + Vec3::Y * (p.eye - HALF_HEIGHT);
                face(&mut p, eye, target);
                *stage = 5;
            }
        }
        (5, "ladder") => {
            // (Held a while, as using a mission trigger takes.)
            if *press_until == 0.0 {
                *press_until = now + INTERACT_HOLD;
                info!("traversal test: ordinary end-of-route interaction");
            }
            actions.hold(Action::Interact);
            if now >= *press_until {
                *stage = 6;
                if std::env::var_os("FC3_TRAVERSAL_WALK_THEN").is_some() {
                    *leg = 2;
                    *walk_waypoint = 0;
                    *stage = 3;
                    info!("traversal test: walking the last leg");
                }
            }
        }
        (0, "tower") => {
            let Some(at) = ways.data.towers.iter().map(|p| Vec3::from(*p)).min_by(|a, b| a.distance(here).total_cmp(&b.distance(here))) else { return };
            t.translation = at + Vec3::new(1.0, HALF_HEIGHT, 0.0);
            face(&mut p, t.translation, at);
            info!("tower test: panel at {at:?}");
            *stage = 1;
        }
        (1, "tower") => {
            actions.press(Action::Interact);
            *stage = 2;
        }
        _ => {}
    }
    if *stage > 0 && now > *next_log {
        *next_log = now + 0.5;
        let state = match *mount {
            Mount::None => "on foot".to_string(),
            Mount::Zip { s, speed, .. } => format!("zip {s:.1} m at {speed:.1} m/s{}", if td.candidate.is_some() { " (over an enemy)" } else { "" }),
            Mount::Ladder { .. } => "ladder".to_string(),
        };
        info!("traversal test: {state}, at {:?}, health {:.0}, towers {}", t.translation, h.current, progress.towers.len());
        debug!("traversal pose: yaw {} pitch {} eye {} rotation {:?}, driving {} spawning {}, velocity {:?}", p.yaw, p.pitch, p.eye, t.rotation, p.driving, p.spawning, p.velocity);
        debug!("traversal camera: {:?}", cameras.single().ok());
        if kind == "zip_takedown" {
            if let Some((n, nh)) = npcs.iter().min_by(|a, b| a.0.translation.distance(t.translation).total_cmp(&b.0.translation.distance(t.translation))) {
                info!("zip test: nearest enemy {:.1?} off, dead {}", n.translation - t.translation, nh.dead);
            }
        }
    }
}

/// Test-only horizontal route; reject malformed points rather than silently
/// dropping coordinates and steering to a different location.
/// A move this far (m) in a frame is the story placing him elsewhere.
const TELEPORTED: f32 = 20.0;
/// How near (m) the route on's last point he stops.
const AFTER_END_RADIUS: f32 = 0.2;
/// How long (s) the end-of-route interaction is held.
const INTERACT_HOLD: f32 = 1.0;

fn walk_route(text: &str) -> Option<Vec<Vec2>> {
    text.split(';').map(|point| {
        let (x, z) = point.trim().trim_end_matches(['c', 'd', 'p']).split_once(',')?;
        let point = Vec2::new(x.trim().parse().ok()?, z.trim().parse().ok()?);
        point.is_finite().then_some(point)
    }).collect()
}

#[cfg(test)]
mod route_tests {
    use super::*;

    #[test]
    fn route_preserves_order_and_rejects_malformed_coordinates() {
        assert_eq!(walk_route("-1877,-2500; -1875, -2488"), Some(vec![Vec2::new(-1877.0, -2500.0), Vec2::new(-1875.0, -2488.0)]));
        // A crouched point keeps its place.
        assert_eq!(walk_route("1,2c;3,4"), Some(vec![Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)]));
        assert_eq!(walk_route("1,2p;3,4d"), Some(vec![Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)]));
        for text in ["", "1,2;", "1,2,3", "1,no,2", "NaN,2", "1,inf", "1,2;;3,4"] {
            assert!(walk_route(text).is_none(), "accepted {text}");
        }
    }
}
