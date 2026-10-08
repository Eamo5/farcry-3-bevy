//! The sky's light from the world's own sky colours. The cloud dome draws
//! the environment presets' sky (see `clouds.wgsl`); this renders the same
//! colours into a small cubemap that Bevy filters into the camera's diffuse
//! and specular environment light, so shade, fill and reflections take the
//! colour of the sky on screen instead of Bevy's physical atmosphere. Without
//! the presets (an old cache), the atmosphere's own probe lights the scene.

use bevy::asset::RenderAssetUsages;
use bevy::light::{AtmosphereEnvironmentMapLight, GeneratedEnvironmentMapLight};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
use fc3_formats::environment::Environment;

use crate::clouds::Weather;
use crate::player::PlayerCamera;
use crate::sky::{exposure_scale, GameEnvironment, TimeOfDay};

pub struct SkyProbePlugin;

impl Plugin for SkyProbePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, make_probe).add_systems(Update, (attach_probe, update_probe.after(crate::sky::advance_time)));
    }
}

/// Texels along a cubemap face (a power of two, as Bevy's filtering needs).
const SIZE: u32 = 32;
/// The ground below the horizon, as a share of the horizon's colour.
const GROUND: f32 = 0.3;
/// The probe's light against the sky's brightness on screen (the sky's fill
/// multiplier in `clouds::weather_light` was set for the atmosphere's dimmer
/// probe). `FC3_SKY_PROBE_GAIN` overrides it (tuning).
const GAIN: f32 = 0.7;

#[derive(Resource)]
struct SkyProbe {
    image: Handle<Image>,
    /// The hour, storm, exposure and indoors it was last drawn for.
    drawn: Option<(f32, f32, f32, f32)>,
}

fn make_probe(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut image = Image::new_fill(
        Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 6 },
        TextureDimension::D2,
        &[0; 8],
        TextureFormat::Rgba16Float,
        RenderAssetUsages::all(),
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
    commands.insert_resource(SkyProbe { image: images.add(image), drawn: None });
}

/// Lights the player's camera from the probe when the presets give a sky,
/// else from the atmosphere.
fn attach_probe(
    env: Res<GameEnvironment>,
    probe: Option<Res<SkyProbe>>,
    cameras: Query<Entity, (With<PlayerCamera>, Without<GeneratedEnvironmentMapLight>, Without<AtmosphereEnvironmentMapLight>)>,
    mut commands: Commands,
) {
    let Some(probe) = probe else { return };
    for e in &cameras {
        if has_sky(env.0.as_ref()) {
            commands.entity(e).insert(GeneratedEnvironmentMapLight { environment_map: probe.image.clone(), intensity: 1.0, ..default() });
        } else {
            commands.entity(e).insert(AtmosphereEnvironmentMapLight::default());
        }
    }
}

fn has_sky(env: Option<&Environment>) -> bool {
    env.is_some_and(|e| !e.sky_sun_side.0.is_empty() && !e.sky_far_side.0.is_empty())
}

/// Redraws the probe as the hour, the weather, the exposure or the shelter
/// overhead change.
fn update_probe(
    tod: Res<TimeOfDay>,
    scene: Res<crate::sky::SceneExposure>,
    weather: Res<Weather>,
    indoors: Res<crate::sky::Indoors>,
    env: Res<GameEnvironment>,
    probe: Option<ResMut<SkyProbe>>,
    mut images: ResMut<Assets<Image>>,
) {
    let (Some(mut probe), Some(e)) = (probe, env.0.as_ref().filter(|e| has_sky(Some(e)))) else { return };
    let storm = weather.storm();
    let ev100 = scene.ev100 - weather.exposure_shift();
    let now = (tod.hours, storm, ev100, indoors.0);
    if let Some((h, s, ev, i)) = probe.drawn {
        let hours = (now.0 - h).abs().min(24.0 - (now.0 - h).abs());
        if hours < 0.05 && (now.1 - s).abs() < 0.02 && (now.2 - ev).abs() < 0.05 && (now.3 - i).abs() < 0.05 {
            return;
        }
    }
    probe.drawn = Some(now);
    let Some(mut image) = images.get_mut(&probe.image) else { return };
    // With the game's grade, as bright as the game's probes make it.
    let gain = std::env::var("FC3_SKY_PROBE_GAIN").ok().and_then(|v| v.parse().ok()).or(scene.sky_light_factor()).unwrap_or(GAIN);
    image.data = Some(draw(e, tod.hours / 24.0, tod.sun_dir(), storm, indoors.0, gain / exposure_scale(ev100)));
}

/// The cubemap's texels (faces +X, -X, +Y, -Y, +Z, -Z), in cd/m².
fn draw(env: &Environment, t: f32, sun: Vec3, storm: f32, inside: f32, to_luminance: f32) -> Vec<u8> {
    // The two gradients sampled once along their length, as light: the
    // colours as picked (linear), not pushed through the inverse tone
    // mapping the visible dome uses, which oversaturates them for lighting.
    const STEPS: usize = 16;
    let side = |k: &fc3_formats::environment::KeyedGradient| -> [Vec3; STEPS] {
        std::array::from_fn(|i| {
            let c = k.at(t, i as f32 / (STEPS - 1) as f32);
            Color::srgb(c[0], c[1], c[2]).to_linear().to_vec3()
        })
    };
    let (near, far) = (side(&env.sky_sun_side), side(&env.sky_far_side));
    // The sky's light is less saturated than the sky (the presets' sky
    // light saturation: 0.65 by day, 1 at night).
    let saturation = env.sky_saturation.0.is_empty().then_some(1.0).unwrap_or_else(|| env.sky_saturation.at(t));
    let toward = Vec2::new(sun.x, sun.z).normalize_or(Vec2::X);
    let colour = |dir: Vec3| -> Vec3 {
        // As the cloud dome draws it (`dome` in `clouds.wgsl`).
        let elevation = dir.y.clamp(-1.0, 1.0).asin();
        let p = (1.0 - elevation.max(0.0) / std::f32::consts::FRAC_PI_2).clamp(0.0, 1.0) * (STEPS - 1) as f32;
        let (i, f) = (p.floor() as usize, p.fract());
        let j = (i + 1).min(STEPS - 1);
        let bearing = Vec2::new(dir.x, dir.z).normalize_or(Vec2::X);
        let mix = bearing.dot(toward) * 0.5 + 0.5;
        let c = far[i].lerp(far[j], f).lerp(near[i].lerp(near[j], f), mix);
        let grey = c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        let c = Vec3::splat(grey).lerp(c, saturation).lerp(Vec3::splat(grey * 0.55), storm);
        // Under a roof, what light gets in is mostly warm bounce off the
        // ground (as the ambient light, `clouds::weather_light`).
        let c = c.lerp(Vec3::new(1.0, 0.86, 0.7) * grey, inside * 0.8);
        if dir.y < 0.0 { c * GROUND } else { c }
    };
    let mut out = Vec::with_capacity((SIZE * SIZE * 6 * 8) as usize);
    for face in 0..6 {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let u = 2.0 * (x as f32 + 0.5) / SIZE as f32 - 1.0;
                let v = 2.0 * (y as f32 + 0.5) / SIZE as f32 - 1.0;
                let dir = match face {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                }
                .normalize();
                // Within half-float range.
                let c = (colour(dir) * to_luminance).min(Vec3::splat(60_000.0));
                for v in [c.x, c.y, c.z, 1.0] {
                    out.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
                }
            }
        }
    }
    out
}
