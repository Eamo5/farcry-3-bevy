//! The interior volumes' own ambient (`fc3_formats::interiors`): within a
//! building's, a tunnel's or a cave's volume the game lights with the
//! ambient the volume names (a floor and a ceiling colour, by day and by
//! night) instead of the light probes, which would bleed the sky's blue
//! light indoors.
//!
//! The nearest volumes about the camera each get a small irradiance volume of
//! their own, a box turned and placed as the volume's bounds, of one voxel:
//! up-facing surfaces take the floor colour, down-facing the ceiling's,
//! walls between. Its intensity is negative, which the vendored `bevy_pbr`
//! takes for an interior's: within it, it stands in for the probes and the
//! sky (by its weight, so it fades in over its last half metre, as the
//! game's blurred mask does) rather than mixing with them.
//!
//! The game draws each volume's own closed model into a mask; its bounds
//! stand for it here, so only the volumes no bigger than a house are used
//! (the caves' sprawl over whole hillsides). `FC3_INTERIORS=0` leaves them
//! out; `FC3_INTERIOR_DEBUG` logs the one about the camera.

use bevy::asset::RenderAssetUsages;
use bevy::light::{IrradianceVolume, LightProbe};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use fc3_formats::interiors::Interior;

use crate::player::PlayerCamera;

pub struct InteriorsPlugin;

impl Plugin for InteriorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Interiors>().add_systems(PostUpdate, (load_interiors.run_if(resource_exists::<crate::data::CacheInfo>), place_interiors).chain());
    }
}

/// How many interiors light at once (Bevy binds eight irradiance volumes a
/// view, those whose centres are nearest the camera; the probes' near and
/// far volumes take two. With seven, a village's shacks pushed the far
/// volume out, its centre up to 34 m off, and the distant shade went blue.)
const SLOTS: usize = 6;
/// How far off (m, from its box) an interior is lit.
const REACH: f32 = 60.0;
/// Over how much of its edge (m) a volume fades in.
const SOFT: f32 = 0.5;

#[derive(Resource, Default)]
struct Interiors {
    loaded: bool,
    list: Vec<Interior>,
    slots: Vec<Slot>,
    next: f32,
}

struct Slot {
    entity: Entity,
    image: Handle<Image>,
    current: Option<usize>,
    /// The light it was last given.
    light: [Vec3; 3],
}

pub(crate) fn enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var("FC3_INTERIORS").as_deref() != Ok("0"));
    *ON
}

fn load_interiors(info: Res<crate::data::CacheInfo>, mut interiors: ResMut<Interiors>, mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    if interiors.loaded || !enabled() {
        return;
    }
    interiors.loaded = true;
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::INTERIORS_REL)).unwrap_or_default();
    let all = fc3_formats::interiors::from_text(&text);
    let total = all.len();
    // (The large ones by their shape, in the probes' volume: `probes`.)
    interiors.list = all.into_iter().filter(|i| !i.sprawls() && i.half.iter().all(|h| *h > 0.05)).collect();
    info!("interiors: {} of {total} volumes small enough to light", interiors.list.len());
    interiors.slots = (0..SLOTS)
        .map(|_| {
            let image = images.add(Image::new(
                Extent3d { width: 1, height: 2, depth_or_array_layers: 3 },
                TextureDimension::D3,
                vec![0; 6 * 8],
                TextureFormat::Rgba16Float,
                RenderAssetUsages::all(),
            ));
            let entity = commands
                .spawn((LightProbe::default(), IrradianceVolume { voxels: image.clone(), intensity: -1.0, affects_lightmapped_meshes: false }, Transform::default(), Visibility::Hidden))
                .id();
            Slot { entity, image, current: None, light: [Vec3::ZERO; 3] }
        })
        .collect();
}

/// How far a point is outside an interior's box (0 inside).
fn outside(i: &Interior, p: Vec3) -> f32 {
    let local = Quat::from_array(i.rotation).inverse() * (p - Vec3::from(i.centre));
    (local.abs() - Vec3::from(i.half)).max(Vec3::ZERO).length()
}

/// The light (cd/m², before the exposure) an interior gives surfaces facing
/// up, down and sideways, by how much day it is.
pub(crate) fn ambient_light(i: &Interior, day: f32, unit: f32) -> [Vec3; 3] {
    let colour = |c: [f32; 3]| Color::srgb(c[0], c[1], c[2]).to_linear().to_vec3();
    let (d, n) = (i.ambient.day, i.ambient.night);
    let floor = colour(n.0).lerp(colour(d.0), day) * (n.2 + (d.2 - n.2) * day);
    let ceiling = colour(n.1).lerp(colour(d.1), day) * (n.2 + (d.2 - n.2) * day);
    // (The game's light has no 1/π on its diffuse; Bevy's does.)
    let k = unit / std::f32::consts::PI;
    [floor * k, ceiling * k, (floor + ceiling) * 0.5 * k]
}

/// Gives the slots to the nearest interiors and sets their light (a few
/// times a second).
#[allow(clippy::too_many_arguments)]
fn place_interiors(
    time: Res<Time>,
    mut interiors: ResMut<Interiors>,
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    scene: Res<crate::sky::SceneExposure>,
    daylight: Res<crate::clouds::Daylight>,
    host: Option<NonSend<crate::story::StoryHost>>,
    mut images: ResMut<Assets<Image>>,
    mut probes: Query<(&mut Transform, &mut LightProbe, &mut Visibility)>,
) {
    let Ok(cam) = cams.single() else { return };
    if interiors.slots.is_empty() || time.elapsed_secs() < interiors.next {
        return;
    }
    interiors.next = time.elapsed_secs() + 0.25;
    let at = cam.translation();
    let interiors = &mut *interiors;
    // (Without the game's grade, nothing to measure their light by.)
    let Some(unit) = scene.unit else { return };
    let mut near: Vec<(f32, usize)> = interiors
        .list
        .iter()
        .enumerate()
        .filter(|(_, i)| Vec3::from(i.centre).distance(at) < REACH + Vec3::from(i.half).length())
        .filter(|(_, i)| i.layer == fc3_formats::world::DEFAULT_MISSION_LAYER || host.as_ref().is_some_and(|h| h.story.is_loaded(i.entity)))
        .map(|(k, i)| (outside(i, at), k))
        .filter(|(d, _)| *d < REACH)
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(SLOTS);
    let wanted: Vec<usize> = near.iter().map(|(_, k)| *k).collect();
    for s in interiors.slots.iter_mut() {
        if s.current.is_some_and(|k| !wanted.contains(&k)) {
            s.current = None;
        }
    }
    for k in &wanted {
        if interiors.slots.iter().any(|s| s.current == Some(*k)) {
            continue;
        }
        if let Some(s) = interiors.slots.iter_mut().find(|s| s.current.is_none()) {
            s.current = Some(*k);
            s.light = [Vec3::NAN; 3];
        }
    }
    let debug = std::env::var_os("FC3_INTERIOR_DEBUG").is_some();
    for s in &mut interiors.slots {
        let Ok((mut t, mut probe, mut vis)) = probes.get_mut(s.entity) else { continue };
        let Some(i) = s.current.map(|k| &interiors.list[k]) else {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            continue;
        };
        let size = Vec3::from(i.half) * 2.0;
        let placed = Transform { translation: Vec3::from(i.centre), rotation: Quat::from_array(i.rotation), scale: size };
        if *t != placed {
            *t = placed;
            probe.falloff = (Vec3::splat(SOFT) / size).min(Vec3::splat(0.5));
        }
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
        let light = ambient_light(i, daylight.0, unit);
        let changed = s.light.iter().zip(&light).any(|(a, b)| !(a.distance(*b) <= 0.01 * b.length().max(1e-3)));
        if changed {
            s.light = light;
            if let Some(mut img) = images.get_mut(&s.image) {
                let mut data = vec![0u8; 6 * 8];
                for (f, n) in crate::probes::SIDES.iter().enumerate() {
                    let c = if n.y > 0.5 { light[0] } else if n.y < -0.5 { light[1] } else { light[2] };
                    crate::probes::put_side(&mut data, [1, 1, 1], [0, 0, 0], f, c);
                }
                img.data = Some(data);
            }
            if debug && outside(i, at) <= 0.0 {
                info!("interiors: in {:016x} ({:?} m, layer {:x}): floor {:?}, ceiling {:?} cd/m2", i.entity, size, i.layer, light[0], light[1]);
            }
        }
    }
}
