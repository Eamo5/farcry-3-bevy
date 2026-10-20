//! Weather: the game's cloud panoramas on a sky dome, cross-faded as the
//! weather drifts between clear, medium, dense, cumulonimbus and storm.
//! Storms dim the sun, thicken the air and bring rain (streaks around the
//! camera plus the game's rain ambience).

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{GeneratedEnvironmentMapLight, NotShadowCaster, NotShadowReceiver};

use crate::particles::{Particle, Particles, Sheet};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;
use fc3_formats::cache::{SKY_CLOUDS_REL, SKY_MOON_REL};

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;
use crate::player::PlayerCamera;
use crate::sky::{Sun, TimeOfDay, SEA_LEVEL};
use crate::weapons::Rng;

pub struct CloudsPlugin;

impl Plugin for CloudsPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "clouds.wgsl");
        app.add_plugins(MaterialPlugin::<CloudMaterial>::default()).insert_resource(Weather::from_env()).init_resource::<Daylight>().add_systems(
            Update,
            (
                setup_clouds.run_if(resource_exists::<CacheInfo>),
                update_weather,
                (update_clouds, weather_light).after(crate::sky::advance_time),
                follow_camera,
                rain,
            ),
        );
    }
}

/// Panorama indices: clear, medium, dense, cumulonimbus, storm.
const STORM: f32 = 4.0;

/// The sky's state, from 0 (clear) to 4 (storm), drifting toward a new
/// target every few minutes.
#[derive(Resource)]
pub struct Weather {
    pub state: f32,
    target: f32,
    change_in: f32,
    /// Dome rotation in turns.
    spin: f32,
    /// Direction the wind blows toward (radians about +Y).
    wind_angle: f32,
    /// The clear spell left before the next storm (s).
    storm_in: f32,
    /// Lightning: the last flash (1 as it strikes, fading), how bright a
    /// flash is, the wait to the next, and the flashes so far.
    flash: f32,
    flash_light: f32,
    next_flash: f32,
    pub flashes: u32,
}

/// Without the game's weather (an old cache): its single-player rhythm.
const WEATHER_TIMES: fc3_formats::environment::WeatherTimes = fc3_formats::environment::WeatherTimes {
    clear: (2000.0, 4840.0),
    storm: (120.0, 240.0),
    transition: 10.0,
    lightning: 24.0,
    lightning_speed: 12.0,
    lightning_gap: (4.0, 16.0),
};

impl Weather {
    /// `FC3_WEATHER=<0..4>` pins the sky (0 clear .. 4 storm), for testing.
    fn from_env() -> Self {
        match std::env::var("FC3_WEATHER").ok().and_then(|v| v.parse::<f32>().ok()) {
            Some(s) => Weather { state: s.clamp(0.0, STORM), target: s.clamp(0.0, STORM), change_in: f32::INFINITY, ..Weather::calm() },
            None => Weather::calm(),
        }
    }

    /// Light cloud, the first storm a clear spell away.
    fn calm() -> Self {
        Weather {
            state: 1.0,
            target: 1.0,
            change_in: 300.0,
            spin: 0.0,
            wind_angle: 0.9,
            storm_in: WEATHER_TIMES.clear.0,
            flash: 0.0,
            flash_light: 0.0,
            next_flash: 0.0,
            flashes: 0,
        }
    }

    /// The lightning's light now: what it multiplies the sky's light by,
    /// less one.
    pub fn lightning(&self) -> f32 {
        self.flash * self.flash_light
    }

    /// How stormy it is (0..1): darkness, rain and fog.
    pub fn storm(&self) -> f32 {
        smoothstep(3.0, STORM, self.state)
    }

    /// How much of the sky is clouded over (0..1).
    pub fn overcast(&self) -> f32 {
        smoothstep(1.0, 3.0, self.state)
    }

    /// The wind (m/s, toward), stronger under cloud and in storms.
    pub fn wind(&self) -> Vec3 {
        let speed = 2.0 + 4.0 * self.overcast() + 8.0 * self.storm();
        Vec3::new(self.wind_angle.sin(), 0.0, self.wind_angle.cos()) * speed
    }

    /// Exposure opened up under heavy cloud (EV), as the eye adjusts.
    pub fn exposure_shift(&self) -> f32 {
        0.4 * self.overcast() + 1.2 * self.storm()
    }
}

use crate::sky::smoothstep;

#[derive(ShaderType, Clone, Copy, Debug, Default)]
pub struct CloudParams {
    /// Direction toward the light (sun or moon); w: luminance of a fully lit cloud.
    pub light: Vec4,
    /// Light colour.
    pub colour: Vec4,
    /// Panorama cross-fade, dome rotation, storm, night.
    pub shape: Vec4,
    /// Direction toward the moon; w: visibility.
    pub moon: Vec4,
    /// Direction toward the sun; w: visibility.
    pub sun: Vec4,
    /// The world's sky dome colour from the zenith to the horizon, on the
    /// sun's side and the far side (scene colours that show as the presets'
    /// colours; see `shown_as`); w is 1 when the presets give one (else
    /// Bevy's atmosphere shows behind the clouds).
    pub sky_sun: [Vec4; SKY_SAMPLES],
    pub sky_far: [Vec4; SKY_SAMPLES],
    /// With the game's grade, a storm's cloud colour (after exposure) as the
    /// storm presets light it, and its fog's grey (w; 0 when not given).
    pub storm: Vec4,
    /// The storm fog over the sky toward the horizon: strength, power.
    pub storm_fog: Vec4,
}

/// Samples of each sky dome gradient (see `CloudParams::sky_sun`).
pub const SKY_SAMPLES: usize = 16;

/// A sky dome gradient at a time of day, as scene colours after exposure
/// (see `scene_colour`).
fn sky_samples(k: &fc3_formats::environment::KeyedGradient, t: f32, factor: Option<f32>) -> [Vec4; SKY_SAMPLES] {
    std::array::from_fn(|i| scene_colour(k.at(t, i as f32 / (SKY_SAMPLES - 1) as f32), factor).extend(1.0))
}

/// A sky colour from the presets as a scene colour after exposure: with the
/// game's grade, as the game lights it (the colour times the sky's
/// multiplier and the exposure, `SceneExposure::sky_factor`), which the
/// game's tone curve then shows as the game does; with AgX, the colour that
/// shows as picked (`shown_as`).
pub fn scene_colour(srgb: [f32; 3], factor: Option<f32>) -> Vec3 {
    match factor {
        Some(k) => Color::srgb(srgb[0], srgb[1], srgb[2]).to_linear().to_vec3() * k,
        None => shown_as(srgb),
    }
}

/// A storm's sky as the game's storm presets light it: the white dome at the
/// storm's sky multiplier, and the clouds over it in the storm's cloud colour
/// (as scene colours after exposure; `None` without the game's grade or the
/// presets). The clouds' own shading darkens it in the shader.
pub fn storm_sky(e: &fc3_formats::environment::Environment, t: f32, scene: &crate::sky::SceneExposure, shift: f32) -> Option<f32> {
    if e.storm_sky_hdr.0.is_empty() {
        return None;
    }
    let (exposure, _) = scene.game?;
    Some(e.storm_sky_hdr.at(t) * exposure * 2f32.powf(shift))
}

fn storm_cloud(e: &fc3_formats::environment::Environment, t: f32, scene: &crate::sky::SceneExposure, shift: f32) -> Option<Vec3> {
    if e.storm_cloud.0.is_empty() {
        return None;
    }
    let c = e.storm_cloud.at(t);
    Some(Color::srgb(c[0], c[1], c[2]).to_linear().to_vec3() * storm_sky(e, t, scene, shift)?)
}

/// The scene colour (after exposure) for the haze over distant land: the
/// sky's colour at the horizon, both sides averaged.
pub fn horizon_colour(env: &fc3_formats::environment::Environment, t: f32, factor: Option<f32>) -> Option<Vec3> {
    if env.sky_sun_side.0.is_empty() || env.sky_far_side.0.is_empty() {
        return None;
    }
    let (a, b) = (env.sky_sun_side.at(t, 1.0), env.sky_far_side.at(t, 1.0));
    Some(scene_colour(std::array::from_fn(|i| (a[i] + b[i]) * 0.5), factor))
}

/// The camera's AgX display transform (the published polynomial fit of the
/// curve Bevy's lookup table holds): scene colour after exposure to linear
/// display colour.
fn agx(c: Vec3) -> Vec3 {
    let inset = Mat3::from_cols(
        Vec3::new(0.842_479_06, 0.042_328_24, 0.042_375_65),
        Vec3::new(0.078_433_6, 0.878_468_64, 0.078_433_6),
        Vec3::new(0.079_223_745, 0.079_166_13, 0.879_142_97),
    );
    let outset = Mat3::from_cols(
        Vec3::new(1.196_879, -0.052_896_85, -0.052_971_64),
        Vec3::new(-0.098_020_88, 1.151_903_1, -0.098_043_45),
        Vec3::new(-0.099_029_74, -0.098_961_18, 1.151_073_7),
    );
    let (lo, hi) = (-12.473_93f32, 4.026_069f32);
    let log = (inset * c.max(Vec3::splat(1e-10))).to_array().map(|v| (v.log2().clamp(lo, hi) - lo) / (hi - lo));
    let curve = |x: f32| {
        let (x2, x4) = (x * x, x * x * x * x);
        15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232
    };
    (outset * Vec3::from_array(log.map(curve))).max(Vec3::ZERO).powf(2.2)
}

/// The scene colour that shows on screen as `srgb` (a colour picked in the
/// game's editor) through the camera's AgX transform, found by iteration;
/// colours more saturated than AgX can show come out as near as it gets.
pub fn shown_as(srgb: [f32; 3]) -> Vec3 {
    let target = Color::srgb(srgb[0], srgb[1], srgb[2]).to_linear().to_vec3();
    let mut x = target;
    for _ in 0..48 {
        let shown = agx(x).max(Vec3::splat(1e-6));
        x *= (target / shown).clamp(Vec3::splat(0.5), Vec3::splat(2.0));
    }
    x.min(Vec3::splat(64.0))
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct CloudMaterial {
    #[uniform(0)]
    pub params: CloudParams,
    #[texture(1)]
    #[sampler(2)]
    pub a: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    pub b: Handle<Image>,
    #[texture(5)]
    #[sampler(6)]
    pub moon: Handle<Image>,
}

impl Material for CloudMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("clouds.wgsl")).with_source("embedded").into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }

    /// The dome sits at the camera; draw it before every other transparent thing.
    fn depth_bias(&self) -> f32 {
        -1.0e9
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // Seen from inside the dome.
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

#[derive(Component)]
struct CloudDome;

#[derive(Resource)]
struct Sky {
    material: Handle<CloudMaterial>,
    panoramas: [Handle<Image>; 5],
}

fn setup_clouds(
    mut done: Local<bool>,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
    mut commands: Commands,
) {
    if *done {
        return;
    }
    *done = true;
    let load = |rel: &str, wrap: ImageAddressMode| {
        server
            .load_builder()
            .with_settings(move |s: &mut ImageLoaderSettings| {
                s.is_srgb = true;
                s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: wrap,
                    address_mode_v: ImageAddressMode::ClampToEdge,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    ..default()
                });
                s.asset_usage = RenderAssetUsages::RENDER_WORLD;
            })
            .load(cache_path(rel))
    };
    // Panoramas wrap around the horizon.
    let panoramas = SKY_CLOUDS_REL.map(|rel| load(rel, ImageAddressMode::Repeat));
    let material = materials.add(CloudMaterial {
        params: CloudParams::default(),
        a: panoramas[0].clone(),
        b: panoramas[1].clone(),
        moon: load(SKY_MOON_REL, ImageAddressMode::ClampToEdge),
    });
    commands.spawn((
        CloudDome,
        Mesh3d(meshes.add(Sphere::new(16_000.0).mesh().uv(64, 32))),
        MeshMaterial3d(material.clone()),
        Transform::default(),
        NotShadowCaster,
        NotShadowReceiver,
        bevy::camera::visibility::NoFrustumCulling,
        Name::new("clouds"),
    ));
    commands.insert_resource(Sky { material, panoramas });
}

fn follow_camera(cameras: Query<&GlobalTransform, With<PlayerCamera>>, mut dome: Query<&mut Transform, With<CloudDome>>) {
    let (Ok(c), Ok(mut t)) = (cameras.single(), dome.single_mut()) else { return };
    t.translation = c.translation();
}

/// A fair-weather sky every few minutes: mostly light cloud, sometimes
/// heavy. A storm comes after a clear spell as long as the game's weather
/// says (`CEnvironmentWeather`: 2,000 to 4,840 s), gathers into heavy
/// cloud, comes on over its transition (10 s) and lasts as long as it says
/// (120 to 240 s), its lightning striking every few seconds (4 to 16).
fn update_weather(time: Res<Time>, mut weather: ResMut<Weather>, mut rng: ResMut<Rng>, env: Res<crate::sky::GameEnvironment>) {
    let dt = time.delta_secs();
    let mut times = env.0.as_ref().and_then(|e| e.weather).unwrap_or(WEATHER_TIMES);
    // `FC3_LIGHTNING_GAP=<s>` (tests): a flash that often.
    if let Some(gap) = std::env::var("FC3_LIGHTNING_GAP").ok().and_then(|v| v.parse::<f32>().ok()) {
        times.lightning_gap = (gap, gap);
    }
    let fair = |rng: &mut Rng| match rng.f32() {
        r if r < 0.27 => 0.0,
        r if r < 0.65 => 1.0,
        r if r < 0.87 => 2.0,
        _ => 3.0,
    };
    let range = |rng: &mut Rng, (a, b): (f32, f32)| rng.range(a, b.max(a));
    // (`FC3_WEATHER` pins it.)
    if weather.change_in.is_finite() {
        if weather.target == STORM {
            // The storm's time runs once it has gathered.
            if weather.state >= 3.0 {
                weather.change_in -= dt;
            }
            if weather.change_in <= 0.0 {
                weather.target = fair(&mut rng);
                weather.change_in = rng.range(240.0, 540.0);
                weather.storm_in = range(&mut rng, times.clear);
            }
        } else {
            weather.change_in -= dt;
            weather.storm_in -= dt;
            if weather.storm_in <= 0.0 {
                weather.target = STORM;
                weather.change_in = range(&mut rng, times.storm);
            } else if weather.change_in <= 0.0 {
                weather.target = fair(&mut rng);
                weather.change_in = rng.range(240.0, 540.0);
            }
        }
    }
    // Clouds come and go over a minute or two; a storm comes on and clears
    // over its transition.
    let stormy = weather.state >= 3.0 && (weather.target == STORM || weather.state > 3.0);
    let step = dt / if stormy { times.transition.max(1.0) } else { 90.0 };
    weather.state += (weather.target - weather.state).clamp(-step, step);
    // Lightning while the storm is on: each flash fading at its speed.
    weather.flash *= (-times.lightning_speed.max(0.1) * dt).exp();
    weather.flash_light = times.lightning;
    if weather.storm() > 0.5 && times.lightning > 0.0 {
        weather.next_flash -= dt;
        if weather.next_flash <= 0.0 {
            weather.flash = 1.0;
            weather.flashes = weather.flashes.wrapping_add(1);
            weather.next_flash = range(&mut rng, times.lightning_gap);
        }
    }
    // About one turn every two hours.
    weather.spin = (weather.spin + dt / 7200.0).fract();
    // The wind veers slowly.
    weather.wind_angle += rng.range(-1.0, 1.0) * dt * 0.02;
}

/// How much daylight there is (0 at night, 1 by day), from the sun's height.
#[derive(Resource, Default)]
pub struct Daylight(pub f32);

fn update_clouds(
    weather: Res<Weather>,
    (scene, indoors): (Res<crate::sky::SceneExposure>, Res<crate::sky::Indoors>),
    tod: Res<TimeOfDay>,
    env: Res<crate::sky::GameEnvironment>,
    sky: Option<Res<Sky>>,
    suns: Query<(&GlobalTransform, &DirectionalLight), With<Sun>>,
    mut daylight: ResMut<Daylight>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    daylight.0 = tod.daylight();
    let (Some(sky), Ok((sun, light))) = (sky, suns.single()) else { return };
    let Some(mut m) = materials.get_mut(&sky.material) else { return };
    let i = (weather.state.floor() as usize).min(3);
    if m.a != sky.panoramas[i] {
        m.a = sky.panoramas[i].clone();
        m.b = sky.panoramas[i + 1].clone();
    }
    let elevation = tod.sun_dir().y;
    // On-screen brightness of a fully lit cloud: bright by day, dimming through
    // dusk to moonlit grey; expressed as luminance under the current exposure.
    let display = 0.08 + 0.44 * smoothstep(-0.15, 0.12, elevation);
    let ev100 = scene.ev100 - weather.exposure_shift();
    let luminance = display / crate::sky::exposure_scale(ev100) * (1.0 - 0.6 * weather.storm()) * (1.0 + weather.lightning());
    let colour = if elevation > -0.12 {
        // Low sun reddens the light.
        Vec3::new(1.0, 0.55, 0.32).lerp(Vec3::new(1.0, 0.97, 0.92), smoothstep(-0.02, 0.3, elevation))
    } else {
        light.color.to_linear().to_vec3()
    };
    let night = smoothstep(-0.02, -0.2, elevation);
    let moon = tod.moon_dir();
    // The sky dome's colours for the hour (none from an old cache).
    let t = tod.hours / 24.0;
    let (sky_sun, sky_far) = match env.0.as_ref().filter(|e| !e.sky_sun_side.0.is_empty() && !e.sky_far_side.0.is_empty()) {
        Some(e) => {
            // Shown under the camera's exposure (`weather_light` opens it up).
            let factor = scene.sky_factor(weather.exposure_shift() + 0.8 * indoors.0);
            (sky_samples(&e.sky_sun_side, t, factor), sky_samples(&e.sky_far_side, t, factor))
        }
        None => ([Vec4::ZERO; SKY_SAMPLES], [Vec4::ZERO; SKY_SAMPLES]),
    };
    let sun_dir = tod.sun_dir();
    m.params = CloudParams {
        light: (-sun.forward().as_vec3()).extend(luminance),
        colour: colour.extend(1.0),
        shape: Vec4::new(weather.state - i as f32, weather.spin, weather.storm(), night),
        moon: moon.extend(smoothstep(-0.02, 0.06, moon.y)),
        sun: sun_dir.extend(smoothstep(-0.04, 0.01, sun_dir.y)),
        sky_sun,
        sky_far,
        storm: env
            .0
            .as_ref()
            .and_then(|e| {
                let shift = weather.exposure_shift() + 0.8 * indoors.0;
                // (Lit up by a lightning flash.)
                let flash = 1.0 + weather.lightning();
                Some((storm_cloud(e, t, &scene, shift)? * flash).extend((storm_sky(e, t, &scene, shift)? * e.storm_fog_sky * flash).max(1e-4)))
            })
            .unwrap_or(Vec4::ZERO),
        storm_fog: env.0.as_ref().map_or(Vec4::ZERO, |e| Vec4::new(e.storm_sky_fog.0, e.storm_sky_fog.1, 0.0, 0.0)),
    };
}

/// Clouds dim the sun and the sky light; the eye adjusts part of the way.
/// Indoors, the sky's blue light mostly stays out and a warm bounce fills in.
fn weather_light(
    weather: Res<Weather>,
    scene: Res<crate::sky::SceneExposure>,
    indoors: Res<crate::sky::Indoors>,
    mut ambient: ResMut<bevy::light::GlobalAmbientLight>,
    mut suns: Query<&mut DirectionalLight, With<Sun>>,
    mut cameras: Query<
        (Option<&mut bevy::light::EnvironmentMapLight>, Option<&mut GeneratedEnvironmentMapLight>, &mut bevy::camera::Exposure),
        With<PlayerCamera>,
    >,
    tod: Res<crate::sky::TimeOfDay>,
) {
    let (overcast, storm) = (weather.overcast(), weather.storm());
    if let Ok(mut light) = suns.single_mut() {
        light.illuminance *= (1.0 - 0.3 * overcast) * (1.0 - 0.75 * storm);
    }
    let inside = indoors.0;
    // A lightning flash lights everything from the sky (less indoors).
    let flash = 1.0 + weather.lightning() * (1.0 - 0.8 * inside);
    // `advance_time` set this frame's ambient level; warm it toward bounce light.
    ambient.color = Color::srgb(0.75, 0.85, 1.0).mix(&Color::srgb(1.0, 0.86, 0.7), inside);
    ambient.brightness *= (1.0 + 0.6 * inside) * flash;
    // `FC3_SKY_FILL` overrides it (tuning).
    // With the game's grade the probe is already as bright as the game's.
    let fill: f32 = std::env::var("FC3_SKY_FILL").ok().and_then(|v| v.parse().ok()).unwrap_or(if scene.game.is_some() { 1.0 } else { SKY_FILL });
    for (env, generated, mut exposure) in &mut cameras {
        // The sky's light (set on the filtered map's source too, which the
        // atmosphere only copies when it first makes it): a tropical day's
        // bright fill by day, dimmer in storms and indoors.
        let day = smoothstep(-0.05, 0.3, tod.sun_dir().y);
        let intensity = (1.0 + (fill - 1.0) * day) * (1.0 - 0.45 * storm) * (1.0 - 0.65 * inside) * flash;
        if let Some(mut env) = env {
            if (env.intensity - intensity).abs() > 0.005 {
                env.intensity = intensity;
            }
        }
        if let Some(mut generated) = generated {
            if (generated.intensity - intensity).abs() > 0.005 {
                generated.intensity = intensity;
            }
        }
        // `advance_time` set this frame's exposure; open up under heavy cloud
        // and in the dimmer light indoors.
        exposure.ev100 -= weather.exposure_shift() + 0.8 * inside;
    }
}

/// How much brighter than the physical sky the sky's fill light is by day
/// (the game's shade is light and blue; a clear sky alone leaves it murky).
const SKY_FILL: f32 = 2.0;

/// Rain streaks (the game's water-drop sprite) around the camera while it's
/// stormy, and the rain ambience of the surroundings.
fn rain(
    time: Res<Time>,
    weather: Res<Weather>,
    mut rng: ResMut<Rng>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    heights: Res<crate::terrain::Heightfield>,
    mut particles: ResMut<Particles>,
    mut carry: Local<f32>,
    mut loops: MessageWriter<crate::sound::LoopSfx>,
    regions: Option<Res<crate::sound::SoundRegions>>,
) {
    let dt = time.delta_secs();
    let Ok(cam) = cameras.single() else { return };
    let c = cam.translation();
    let underwater = crate::water::submerged(c, &heights, 0.0);
    let intensity = if underwater { 0.0 } else { smoothstep(0.3, 1.0, weather.storm()) };
    if intensity > 0.01 {
        // The sound region's rain (else the coast's or the forest's), its
        // layers as heavy as the rain (its parameter, 0..1).
        let ground = heights.height_at(c).unwrap_or(0.0);
        let fallback = if ground < SEA_LEVEL + 6.0 { crate::sound::ev::RAIN_COAST } else { crate::sound::ev::RAIN_FOREST };
        let event = regions.as_deref().and_then(|r| r.here()).map(|r| r.rain).filter(|e| *e != 0).unwrap_or(fallback);
        let params = [(crate::sound::RAIN_LEVEL, intensity), (0, 0.0), (0, 0.0), (0, 0.0)];
        loops.write(crate::sound::LoopSfx { key: 0x5241_494E, event, at: None, volume: 0.8, params, ..default() });
    }
    // Drops per second, slanted by the wind; each falls for about a second.
    *carry += intensity * 1500.0 * dt;
    let velocity = Vec3::new(0.0, -14.0, 0.0) + weather.wind() * 0.35;
    while *carry >= 1.0 {
        *carry -= 1.0;
        let mut flat = Vec2::new(rng.range(-16.0, 16.0), rng.range(-16.0, 16.0));
        // Keep streaks off the lens.
        if flat.length() < 1.5 {
            flat = flat.normalize_or(Vec2::X) * 1.5;
        }
        let mut p = Particle::new(Sheet::Drop, c + Vec3::new(flat.x, rng.range(2.0, 14.0), flat.y), 1.2, 0.22);
        p.vel = velocity * rng.range(0.9, 1.1);
        p.stretch = 0.035;
        p.fade_in = 0.1;
        p.fade_out = 0.8;
        p.colour = Vec4::new(0.8, 0.85, 0.9, 0.45);
        particles.spawn(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_colours_show_as_picked() {
        // A noon horizon and a dusk glow come back through AgX as picked.
        for srgb in [[0.54, 0.65, 0.75], [0.96, 0.80, 0.58], [0.11, 0.18, 0.25]] {
            let shown = Color::LinearRgba(LinearRgba::from_vec3(agx(shown_as(srgb)))).to_srgba();
            let got = [shown.red, shown.green, shown.blue];
            assert!(got.iter().zip(srgb).all(|(a, b)| (a - b).abs() < 0.01), "{srgb:?} -> {got:?}");
        }
    }
}
