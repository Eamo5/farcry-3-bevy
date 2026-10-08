//! The sun's lens flare: the game's flare sheet (the sun's glow and six
//! ghosts side by side) laid out along the line from the sun through the
//! middle of the view, while the sun is in view and nothing is in the way.
//! The pieces are small additive quads just in front of the camera.

use avian3d::prelude::*;
use bevy::light::NotShadowCaster;
use bevy::math::Affine2;
use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::cache_path;
use crate::player::{Player, PlayerCamera};

pub struct LensFlarePlugin;

impl Plugin for LensFlarePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(crate::menu::GameState::Loading), spawn_flare)
            .add_systems(Update, update_flare.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// One piece: its cell in the sheet (of 8), where it sits along the line
/// (1 at the sun, 0 mid-view, -1 opposite), its size (share of the view's
/// height) and tint.
const PIECES: [(u32, f32, f32, [f32; 3]); 7] = [
    (0, 1.0, 0.45, [1.0, 0.92, 0.8]),
    (2, 0.55, 0.12, [0.8, 0.85, 1.0]),
    (3, 0.28, 0.08, [1.0, 1.0, 1.0]),
    (4, -0.12, 0.11, [1.0, 0.9, 0.9]),
    (5, -0.38, 0.05, [0.9, 0.95, 1.0]),
    (6, -0.62, 0.13, [0.9, 1.0, 0.9]),
    (7, -0.95, 0.18, [1.0, 1.0, 1.0]),
];
const CELLS: f32 = 8.0;
/// How far in front of the eye the pieces sit (m).
const DEPTH: f32 = 0.5;

#[derive(Component)]
struct Flare {
    along: f32,
    size: f32,
    tint: [f32; 3],
    material: Handle<StandardMaterial>,
}

fn spawn_flare(
    mut commands: Commands,
    server: Res<AssetServer>,
    cams: Query<Entity, With<PlayerCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(cam) = cams.single() else { return };
    let sheet: Handle<Image> = server.load(cache_path(&cache::fx_rel("lens_flare")));
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    for (cell, along, size, tint) in PIECES {
        let material = materials.add(StandardMaterial {
            base_color: Color::BLACK,
            base_color_texture: Some(sheet.clone()),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            uv_transform: Affine2::from_scale_angle_translation(Vec2::new(1.0 / CELLS, 1.0), 0.0, Vec2::new(cell as f32 / CELLS, 0.0)),
            double_sided: true,
            cull_mode: None,
            fog_enabled: false,
            ..default()
        });
        commands.spawn((
            Flare { along, size, tint, material: material.clone() },
            Mesh3d(quad.clone()),
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden,
            NotShadowCaster,
            ChildOf(cam),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn update_flare(
    time: Res<Time>,
    tod: Res<crate::sky::TimeOfDay>,
    weather: Res<crate::clouds::Weather>,
    spatial: SpatialQuery,
    cams: Query<(&GlobalTransform, &Projection), With<PlayerCamera>>,
    players: Query<Entity, With<Player>>,
    mut flares: Query<(&Flare, &mut Transform, &mut Visibility)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut seen: Local<f32>,
) {
    let Ok((cam, projection)) = cams.single() else { return };
    let Projection::Perspective(p) = projection else { return };
    let sun = tod.sun_dir();
    let view = cam.rotation().inverse() * sun;
    let (tan_y, aspect) = ((p.fov * 0.5).tan(), p.aspect_ratio);
    let ndc = Vec2::new(view.x / (-view.z * tan_y * aspect), view.y / (-view.z * tan_y));
    let in_view = view.z < -0.05 && ndc.x.abs() < 1.15 && ndc.y.abs() < 1.15;
    // Nothing in the way: terrain, buildings, rocks.
    let eye = cam.translation();
    let filter = SpatialQueryFilter::from_excluded_entities(players.iter());
    let clear = in_view && Dir3::new(sun).ok().is_some_and(|d| spatial.cast_ray(eye, d, 3000.0, true, &filter).is_none());
    let target = if clear { 1.0 } else { 0.0 };
    *seen += (target - *seen) * (time.delta_secs() * 8.0).min(1.0);
    let strength = *seen * tod.daylight() * (1.0 - weather.overcast()) * (1.0 - 0.4 * ndc.length().min(1.0));
    for (f, mut t, mut vis) in &mut flares {
        if strength < 0.01 {
            *vis = Visibility::Hidden;
            continue;
        }
        *vis = Visibility::Inherited;
        let at = ndc * f.along;
        t.translation = Vec3::new(at.x * tan_y * aspect * DEPTH, at.y * tan_y * DEPTH, -DEPTH);
        t.scale = Vec3::splat(f.size * 2.0 * tan_y * DEPTH);
        if let Some(mut m) = materials.get_mut(&f.material) {
            let k = strength * 1.6;
            m.base_color = Color::linear_rgb(f.tint[0] * k, f.tint[1] * k, f.tint[2] * k);
        }
    }
}
