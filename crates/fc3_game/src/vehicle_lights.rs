//! Vehicles' lights (`handling::Lights`, the archetype's
//! `VehicleLightSettings`): from the hour the game turns them on until the
//! one it turns them off, a vehicle someone drives lights its lamps (its
//! light switch puts the lit glass on its slots), throws its one dynamic
//! light ahead (a spot at the model's `DynamicLight` node, as far as its
//! range, in its cone), and its head and tail lamps glow, the tail ones at
//! their dimmed share until it brakes.

use bevy::prelude::*;
use fc3_formats::cache::{self, CachedBone};
use fc3_formats::handling::Lights;

use crate::cache_assets::{cache_path, Fc3Model};
use crate::light_switch::{respawn_part, worn, PartCopy};
use crate::original_shaders::OriginalSwap;
use crate::particles::{Particle, Particles, Sheet};
use crate::player::PlayerCamera;
use crate::vehicles::{handling_of, Vehicle, VehicleAssets, Wear};

pub struct VehicleLightsPlugin;

impl Plugin for VehicleLightsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (fit_lamps, switch_lamps, drive_beams, drive_glows).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// Spots handed to the nearest vehicles with their lights on.
const BEAMS: usize = 6;
/// How far from the camera a vehicle's beam is lit, and its lamps glow.
const BEAM_REACH: f32 = 160.0;
const GLOW_REACH: f32 = 400.0;
/// Without the game's grade: lux per game light unit, set by eye.
const UNIT_LUX: f32 = 400.0;

/// A vehicle's lamps, found in its model: its beam's pose and its lamps
/// (model space), and its light switch's slots.
#[derive(Component)]
pub(crate) struct Lamps {
    lights: &'static Lights,
    beam: Option<Transform>,
    heads: Vec<Vec3>,
    tails: Vec<Vec3>,
    /// The slots' materials off (by path) and lit.
    swaps: Vec<(String, Handle<StandardMaterial>)>,
    /// What the parts switched wore before (to give back).
    switched: Vec<(AssetId<StandardMaterial>, Handle<StandardMaterial>)>,
    pub(crate) on: bool,
}

/// A model's lamps: the leaf nodes named for a light (not its dynamic or
/// interior light), tail lamps by name (back, tail, red), the rest heads.
fn lamp_nodes(bones: &[CachedBone]) -> (Vec<Vec3>, Vec<Vec3>) {
    let named = |n: &str| {
        let n = n.to_ascii_lowercase();
        n.contains("light") && !n.contains("dynamic") && !n.contains("interior")
    };
    let (mut heads, mut tails) = (Vec::new(), Vec::new());
    for (i, b) in bones.iter().enumerate() {
        if !named(&b.name) || bones.iter().any(|c| c.parent == i as i32 && named(&c.name)) {
            continue;
        }
        let at = pose(bones, i).translation;
        let n = b.name.to_ascii_lowercase();
        if n.contains("back") || n.contains("tail") || n.contains("red") {
            tails.push(at);
        } else {
            heads.push(at);
        }
    }
    (heads, tails)
}

/// A node's model-space pose (as `weapons::node_pose`).
fn pose(bones: &[CachedBone], mut i: usize) -> Transform {
    let mut m = Mat4::IDENTITY;
    for _ in 0..bones.len() {
        let b = &bones[i];
        m = Mat4::from_rotation_translation(Quat::from_array(b.rotation), Vec3::from_array(b.translation)) * m;
        if b.parent < 0 || b.parent as usize == i || b.parent as usize >= bones.len() {
            break;
        }
        i = b.parent as usize;
    }
    Transform::from_matrix(m)
}

fn fit_lamps(
    assets: Option<Res<VehicleAssets>>,
    models: Res<Assets<Fc3Model>>,
    server: Res<AssetServer>,
    vehicles: Query<(Entity, &Vehicle), Without<Lamps>>,
    mut commands: Commands,
) {
    let Some(assets) = assets else { return };
    for (e, v) in &vehicles {
        let Some(lights) = handling_of(v.def()).and_then(|h| h.lights.as_ref()) else { continue };
        let Some(model) = assets.models.get(v.def()).and_then(|h| models.get(h)) else { continue };
        let (heads, tails) = lamp_nodes(&model.bones);
        let swaps = lights
            .swaps
            .iter()
            .map(|(slot, file)| {
                let off = cache_path(&cache::material_rel(&fc3_formats::world::material_override_path(slot)));
                (off, server.load(cache_path(&cache::material_rel(file))))
            })
            .collect();
        commands.entity(e).insert(Lamps {
            lights,
            beam: model.bones.iter().position(|b| b.name.eq_ignore_ascii_case("DynamicLight")).map(|i| pose(&model.bones, i)),
            heads,
            tails,
            swaps,
            switched: Vec::new(),
            on: false,
        });
    }
}

/// Lights a vehicle's lamps while someone drives it in the hours the game
/// has them on, and puts them out again.
#[allow(clippy::type_complexity)]
fn switch_lamps(
    clock: Res<crate::sky::TimeOfDay>,
    server: Res<AssetServer>,
    mut vehicles: Query<(Entity, &Vehicle, &Wear, &mut Lamps)>,
    tree: Query<&Children>,
    parts: Query<(Option<&MeshMaterial3d<StandardMaterial>>, Option<&OriginalSwap>, PartCopy)>,
    mut commands: Commands,
) {
    for (root, v, wear, mut lamps) in &mut vehicles {
        let want = v.running() && !wear.wrecked && lamps.lights.on_at(clock.hours);
        if want == lamps.on {
            continue;
        }
        lamps.on = want;
        let mut stack = vec![root];
        let mut switched = std::mem::take(&mut lamps.switched);
        while let Some(e) = stack.pop() {
            if let Ok(children) = tree.get(e) {
                stack.extend(children.iter());
            }
            let Ok((standard, original, copy)) = parts.get(e) else { continue };
            let Some(now) = worn(standard, original) else { continue };
            if want {
                let path = server.get_path(now).map(|p| p.to_string());
                let Some((_, lit)) = lamps.swaps.iter().find(|(off, _)| path.as_deref() == Some(off.as_str())) else { continue };
                let Some(off) = server.get_id_handle(now) else { continue };
                switched.push((lit.id(), off));
                respawn_part(&mut commands, e, &copy, lit.clone());
            } else if let Some(i) = switched.iter().position(|(lit, _)| *lit == now) {
                let off = switched[i].1.clone();
                respawn_part(&mut commands, e, &copy, off);
            }
        }
        if want {
            lamps.switched = switched;
        }
    }
}

/// The lamps' light: warm, as their lit glass.
const BEAM_COLOUR: Vec3 = Vec3::new(1.0, 0.93, 0.78);

/// Hands the pooled spots to the nearest vehicles with their lights on and
/// points them ahead of each.
#[allow(clippy::type_complexity)]
fn drive_beams(
    mut pool: Local<Vec<Entity>>,
    scene: Res<crate::sky::SceneExposure>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    vehicles: Query<(&GlobalTransform, &Lamps)>,
    mut spots: Query<(&mut SpotLight, &mut Transform, &mut Visibility)>,
    mut commands: Commands,
) {
    if pool.is_empty() {
        *pool = (0..BEAMS)
            .map(|_| commands.spawn((SpotLight { intensity: 0.0, ..default() }, Transform::default(), Visibility::Hidden)).id())
            .collect();
        return;
    }
    let Ok(cam) = cameras.single() else { return };
    let eye = cam.translation();
    let mut lit: Vec<(f32, Transform, &Lights)> = vehicles
        .iter()
        .filter(|(_, l)| l.on)
        .filter_map(|(t, l)| {
            let pose = t.compute_transform() * l.beam?;
            let d = pose.translation.distance(eye);
            (d < BEAM_REACH).then_some((d, pose, l.lights))
        })
        .collect();
    lit.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (i, &e) in pool.iter().enumerate() {
        let Ok((mut spot, mut tr, mut vis)) = spots.get_mut(e) else { continue };
        let Some((d, pose, l)) = lit.get(i) else {
            *vis = Visibility::Hidden;
            continue;
        };
        // The game lights in its units, full to the fade's start and gone by
        // the range; Bevy's light falls as 1/d², so it's matched four
        // fifths of the way to where the fall begins (brighter nearer,
        // dimmer past it).
        let unit = scene.unit.unwrap_or(UNIT_LUX);
        let matched = (l.fade_start * 0.8).max(1.0);
        let fade = 1.0 - crate::sky::smoothstep(BEAM_REACH - 30.0, BEAM_REACH, *d);
        spot.intensity = 4.0 * std::f32::consts::PI * unit * l.intensity * matched * matched * fade;
        spot.color = Color::linear_rgb(BEAM_COLOUR.x, BEAM_COLOUR.y, BEAM_COLOUR.z);
        spot.range = l.range;
        // (The data's angles are the cone's full width.)
        spot.outer_angle = (l.outer_angle * 0.5).to_radians().min(1.5);
        spot.inner_angle = (l.inner_angle * 0.5).to_radians().min(spot.outer_angle * 0.95);
        *tr = *pose;
        *vis = Visibility::Inherited;
    }
}

/// Draws a glow on each lit lamp in reach: white ahead, red behind (at the
/// dimmed share until the brakes go on).
fn drive_glows(
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    vehicles: Query<(&GlobalTransform, &Vehicle, &Lamps)>,
    mut particles: ResMut<Particles>,
) {
    let Ok(cam) = cameras.single() else { return };
    let eye = cam.translation();
    for (t, v, lamps) in &vehicles {
        if !lamps.on {
            continue;
        }
        let tail = if v.braking() { 1.0 } else { lamps.lights.brake_dim };
        for (points, colour, strength, size) in [(&lamps.heads, BEAM_COLOUR, 0.9, 0.45), (&lamps.tails, Vec3::new(1.0, 0.08, 0.04), 0.7 * tail, 0.3)] {
            for p in points {
                let at = t.transform_point(*p);
                let d = at.distance(eye);
                if d > GLOW_REACH || d < 1.5 {
                    continue;
                }
                let fade = 1.0 - crate::sky::smoothstep(GLOW_REACH - 80.0, GLOW_REACH, d);
                let mut g = Particle::new(Sheet::Glow, at, 1.0, size * (d / 25.0).max(1.0).sqrt());
                g.colour = colour.extend(strength * fade);
                particles.sprite(g);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bone(name: &str, parent: i32, at: [f32; 3]) -> CachedBone {
        CachedBone { name: name.into(), parent, translation: at, rotation: [0.0, 0.0, 0.0, 1.0], ..Default::default() }
    }

    #[test]
    fn lamps_are_the_light_leaves() {
        let bones = vec![
            bone("4x4", -1, [0.0; 3]),
            bone("HeadLight_Front_Right", 0, [0.5, 1.0, -2.4]),
            bone("HeadLight_FR", 1, [0.0, 0.0, -0.04]),
            bone("TailLight_Back_Left", 0, [-0.8, 1.1, 1.4]),
            bone("BackLight_BL", 3, [0.0, 0.0, 0.03]),
            bone("DynamicLight", 0, [0.0, 1.1, -2.9]),
            bone("InteriorLight", 0, [0.0, 3.5, 0.0]),
            bone("Heaflight_L_FX", 0, [-0.7, 0.8, -2.0]),
        ];
        let (heads, tails) = lamp_nodes(&bones);
        assert_eq!(heads.len(), 2, "{heads:?}");
        assert_eq!(tails.len(), 1, "{tails:?}");
        assert!((heads[0] - Vec3::new(0.5, 1.0, -2.44)).length() < 1e-4);
    }
}
