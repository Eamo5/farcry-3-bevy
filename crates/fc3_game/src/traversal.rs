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
        app.init_resource::<Ways>().init_resource::<Mount>().add_message::<TowerTaken>().add_systems(
            Update,
            (
                load_ways.run_if(resource_exists::<CacheInfo>),
                traversal_test,
                (zip_prompt, tower_prompt).before(HudSet).after(crate::turrets::man_turrets),
                climb,
                ride,
            )
                .chain(),
        );
    }
}

/// The world's cables, ladders and towers.
#[derive(Resource, Default)]
pub struct Ways {
    loaded: bool,
    pub data: Traversal,
}

/// What the player is holding on to.
#[derive(Resource, Default, Clone, Copy, PartialEq)]
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

/// Offers the zip lines whose high end is in reach.
fn zip_prompt(
    mut actions: ResMut<ActionState>,
    settings: Res<crate::settings::Settings>,
    ways: Res<Ways>,
    mut mount: ResMut<Mount>,
    mut hud: ResMut<HudState>,
    mut players: Query<(Entity, &Transform, &mut Player, &Health)>,
    mut commands: Commands,
) {
    let Ok((e, t, mut p, h)) = players.single_mut() else { return };
    if *mount != Mount::None || hud.prompt.is_some() || h.dead || p.driving || p.noclip || p.swimming {
        return;
    }
    let feet = t.translation.y - HALF_HEIGHT;
    let near = ways.data.cables.iter().enumerate().filter(|(_, c)| c.zipline).find(|(_, c)| {
        let (start, _) = zip_ends(c);
        let d = Vec2::new(start.x - t.translation.x, start.z - t.translation.z).length();
        d < 2.5 && (0.3..4.5).contains(&(start.y - feet))
    });
    let Some((i, _)) = near else { return };
    hud.prompt = Some(format!("{}  ride the zip line", actions.label(Action::Interact, &settings)));
    if actions.just_pressed(Action::Interact) {
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

/// Walking into a ladder grabs it; forward and back climb, jump lets go,
/// and climbing past the top steps off onto whatever it leads to.
fn climb(
    time: Res<Time>,
    actions: Res<ActionState>,
    ways: Res<Ways>,
    spatial: avian3d::prelude::SpatialQuery,
    mut mount: ResMut<Mount>,
    mut players: Query<(Entity, &mut Transform, &mut Player, &Health)>,
    mut commands: Commands,
) {
    let Ok((e, mut t, mut p, h)) = players.single_mut() else { return };
    let dt = time.delta_secs();
    let feet = t.translation.y - HALF_HEIGHT;
    let forward = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    match *mount {
        Mount::None => {
            if h.dead || p.driving || p.noclip || p.swimming || actions.movement.y < 0.3 {
                return;
            }
            let near = ways.data.ladders.iter().enumerate().find(|(_, l)| {
                let base = Vec3::from(l.base);
                let to = Vec3::new(base.x - t.translation.x, 0.0, base.z - t.translation.z);
                to.length() < 0.8 && feet > base.y - 0.4 && feet < l.top - 0.3 && forward.dot(to.normalize_or(forward)) > 0.2
            });
            let Some((i, l)) = near else { return };
            let base = Vec3::from(l.base);
            let side = Vec3::new(t.translation.x - base.x, 0.0, t.translation.z - base.z).normalize_or(-Vec3::from(l.facing));
            *mount = Mount::Ladder { index: i, side };
            p.driving = true;
            p.velocity = Vec3::ZERO;
            commands.entity(e).insert(ColliderDisabled);
        }
        Mount::Ladder { index, side } => {
            let Some(l) = ways.data.ladders.get(index) else {
                *mount = Mount::None;
                return;
            };
            let base = Vec3::from(l.base);
            let climb = actions.movement.y;
            let mut y = feet + climb * 3.0 * dt;
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
                let landing = [-side, side].into_iter().find_map(|dir| {
                    let probe = Vec3::new(base.x, l.top + 1.0, base.z) + dir * 0.8;
                    spatial.cast_ray(probe, Dir3::NEG_Y, 2.6, true, &filter).map(|h| (dir, probe.y - h.distance))
                });
                match landing {
                    Some((dir, ground)) => {
                        t.translation = Vec3::new(base.x, ground + HALF_HEIGHT + 0.05, base.z) + dir * 0.8;
                        p.velocity = dir * 1.0;
                        release(&mut p, &mut mount, &mut commands);
                    }
                    None => t.translation = Vec3::new(base.x, l.top - 0.05 + HALF_HEIGHT, base.z) + side * 0.45,
                }
                return;
            }
            if y <= base.y && climb < 0.0 {
                y = base.y;
                release(&mut p, &mut mount, &mut commands);
            }
            t.translation = Vec3::new(base.x, y + HALF_HEIGHT, base.z) + side * 0.45;
        }
        Mount::Zip { .. } => {}
    }
}

/// A radio tower taken over, at its panel (the story hears of it).
#[derive(Message, Clone, Copy)]
pub struct TowerTaken {
    pub at: Vec3,
}

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
    players: Query<(&Transform, &Health, &crate::weapons::Inventory), With<Player>>,
    mut taken: MessageWriter<TowerTaken>,
) {
    let Ok((t, h, inv)) = players.single() else { return };
    if hud.prompt.is_some() || h.dead {
        return;
    }
    let Some((i, at)) = ways.data.towers.iter().enumerate().find(|(_, p)| Vec3::from(**p).distance(t.translation) < 2.6) else { return };
    if progress.towers.contains(&(i as u32)) {
        return;
    }
    hud.prompt = Some(format!("{}  take over the radio tower", actions.label(Action::Interact, &settings)));
    // (The story's autopilot takes the towers it is brought to.)
    if actions.just_pressed(Action::Interact) || std::env::var_os("FC3_STORY_AUTOPILOT").is_some() {
        actions.consume(Action::Interact);
        progress.towers.insert(i as u32);
        taken.write(TowerTaken { at: Vec3::from(*at) });
        explored.reveal(Vec3::from(*at), 1100.0);
        xp.write(crate::progression::XpGain { amount: 300, reason: "Radio tower" });
        let freed = catalog.freed_weapons(&inv.defs, progress.towers.len());
        let reward = if freed.is_empty() { String::new() } else { format!(" Free in shops: {}.", freed.join(", ")) };
        hud.notify(format!("Radio tower taken: map revealed.{reward}"));
    }
}

/// `FC3_TRAVERSAL_TEST=zip|ladder|tower`: puts the player at the nearest
/// zip line, ladder or tower panel, works the controls and logs progress.
#[allow(clippy::too_many_arguments)]
fn traversal_test(
    time: Res<Time>,
    ways: Res<Ways>,
    mount: Res<Mount>,
    progress: Res<crate::progression::Progress>,
    mut actions: ResMut<ActionState>,
    mut stage: Local<u32>,
    mut next_log: Local<f32>,
    mut players: Query<(&mut Transform, &mut Player, &Health)>,
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
    let face = |p: &mut Player, from: Vec3, to: Vec3| {
        let d = to - from;
        p.yaw = (-d.x).atan2(-d.z);
    };
    let base = if kind == "zip_takedown" { "zip" } else { kind.as_str() };
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
            let Some(l) = ways
                .data
                .ladders
                .iter()
                .filter(|l| l.top - l.base[1] > 4.0)
                .min_by(|a, b| Vec3::from(a.base).distance(here).total_cmp(&Vec3::from(b.base).distance(here)))
            else {
                return;
            };
            let base = Vec3::from(l.base);
            let side = Vec3::from(l.facing);
            t.translation = base + side * 0.7 + Vec3::Y * (HALF_HEIGHT + 0.05);
            face(&mut p, t.translation, base);
            info!("ladder test: foot {base:?}, top {:.1}", l.top);
            *stage = 1;
        }
        (1, "ladder") => actions.movement.y = 1.0,
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
        if kind == "zip_takedown" {
            if let Some((n, nh)) = npcs.iter().min_by(|a, b| a.0.translation.distance(t.translation).total_cmp(&b.0.translation.distance(t.translation))) {
                info!("zip test: nearest enemy {:.1?} off, dead {}", n.translation - t.translation, nh.dead);
            }
        }
    }
}
