//! Sun, sky and time of day (clouds and weather are in `clouds`, the sea in `water`).

use bevy::camera::Exposure;
use bevy::light::atmosphere::ScatteringMedium;
use bevy::light::Atmosphere;
use bevy::light::{CascadeShadowConfigBuilder, GlobalAmbientLight, SunDisk};
use bevy::pbr::AtmosphereSettings;
use bevy::prelude::*;

/// Ocean surface height in Bevy Y (Dunia Z), derived from floating props.
pub const SEA_LEVEL: f32 = 100.0;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(TimeOfDay { hours: 10.5, speed: 0.0 })
            .insert_resource(load_environment())
            .insert_resource(GlobalAmbientLight { color: Color::srgb(0.75, 0.85, 1.0), brightness: 150.0, ..default() })
            .init_resource::<Indoors>()
            .init_resource::<SceneExposure>()
            .add_systems(Startup, setup_sky)
            .add_systems(Update, (advance_time, indoors));
    }
}

/// How enclosed the view is: 0 in the open, 1 under a roof with walls
/// round about (eased). Indoors the sky's light is mostly shut out and what
/// gets in is warm light bounced off the ground.
#[derive(Resource, Default)]
pub struct Indoors(pub f32);

/// A roof this close overhead (m), and walls this close around.
const ROOF: f32 = 8.0;
const WALLS: f32 = 6.0;

fn indoors(
    time: Res<Time>,
    spatial: avian3d::prelude::SpatialQuery,
    cams: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    players: Query<Entity, With<crate::player::Player>>,
    mut indoors: ResMut<Indoors>,
) {
    let Ok(cam) = cams.single() else { return };
    let eye = cam.translation();
    let filter = avian3d::prelude::SpatialQueryFilter::from_excluded_entities(players.iter());
    let hit = |dir: Dir3, reach: f32| spatial.cast_ray(eye, dir, reach, true, &filter).is_some();
    // A roof alone (a canopy, an awning) shades a little; with walls it's a room.
    let target = if eye.y > SEA_LEVEL && hit(Dir3::Y, ROOF) {
        let walls = [Dir3::X, Dir3::NEG_X, Dir3::Z, Dir3::NEG_Z].into_iter().filter(|d| hit(*d, WALLS)).count();
        if walls >= 2 {
            0.4 + 0.15 * walls as f32
        } else {
            0.1
        }
    } else {
        0.0
    };
    indoors.0 += (target - indoors.0) * (time.delta_secs() * 2.5).min(1.0);
}

/// The world's outdoor environment over the day (the game's presets, from
/// the cache; absent in an old cache).
#[derive(Resource, Default)]
pub struct GameEnvironment(pub Option<fc3_formats::environment::Environment>);

fn load_environment() -> GameEnvironment {
    let path = fc3_formats::cache::default_cache_root().join(fc3_formats::cache::ENVIRONMENT_REL);
    GameEnvironment(std::fs::read_to_string(path).ok().map(|t| fc3_formats::environment::Environment::from_text(&t)))
}

/// Without the game's grade (Bevy's AgX instead): exposure at noon and under
/// the moon, set by eye for AgX. With it, the exposure comes from the presets
/// (see `NOON_SUN_LUX`).
const NOON_EV100: f32 = 15.0;
const NIGHT_EV100: f32 = 6.0;
/// The light the noon exposure is set for (sun and sky, lux).
const NOON_LUX: f32 = 90_600.0;
/// The moon's light (lux).
const MOON_LUX: f32 = 40.0;

/// A colour from the presets in linear RGB: they are picked in sRGB (flagged
/// as colours, which the engine decodes before lighting with them).
fn linear(c: [f32; 3]) -> [f32; 3] {
    let l = Color::srgb(c[0], c[1], c[2]).to_linear();
    [l.red, l.green, l.blue]
}

/// The hue of a colour from the presets (brightest channel at 1), or `None`
/// when it is (nearly) black.
fn hue(c: [f32; 3]) -> Option<Color> {
    let c = linear(c);
    let m = c[0].max(c[1]).max(c[2]);
    (m > 0.003).then(|| Color::linear_rgb(c[0] / m, c[1] / m, c[2] / m))
}

/// The exposure for the hour, before the weather and shelter open it up
/// (`clouds::weather_light`), and what the sky's colours are shown at.
#[derive(Resource, Clone, Copy)]
pub struct SceneExposure {
    /// Camera exposure (EV100).
    pub ev100: f32,
    /// With the game's grade: the exposure the game's eye adaptation settles
    /// on (a multiplier on the scene) and the sky dome's brightness
    /// multiplier, both from the presets.
    pub game: Option<(f32, f32)>,
    /// With the game's grade: the shade's light from the sky as the game's
    /// light probes give it (their scale times their multiplier), relative to
    /// the sky dome's colours.
    pub probe: Option<f32>,
    /// With the game's grade: lux per game light unit (see `NOON_SUN_LUX`),
    /// for the world's lamps.
    pub unit: Option<f32>,
}

impl Default for SceneExposure {
    fn default() -> Self {
        SceneExposure { ev100: NOON_EV100, game: None, probe: None, unit: None }
    }
}

impl SceneExposure {
    /// What a sky colour from the presets (linear) is multiplied by to give
    /// its scene colour after exposure, as the game shows it, with the
    /// exposure opened up by `shift` EV; `None` without the game's grade
    /// (the colours then go through the inverse of AgX).
    pub fn sky_factor(&self, shift: f32) -> Option<f32> {
        self.game.map(|(exposure, sky)| exposure * sky * 2f32.powf(shift))
    }

    /// What the sky's colours (linear) are multiplied by for the light they
    /// give the shade, as a scene colour after the exposure: the sky as the
    /// game draws it, as much as its light probes pass on.
    pub fn sky_light_factor(&self) -> Option<f32> {
        Some(self.sky_factor(0.0)? * self.probe?)
    }
}

/// With the game's grade, the light follows the presets as the game's does:
/// the sun's or moon's colour times the light multiplier, in game units, at
/// the same strength whatever the sun's height (the colour does the
/// dawning). Game units become lux at the noon sun's and the moon's
/// brightness here (`NOON_SUN_LUX`, `MOON_LUX`), easing from one to the other
/// in the few dark minutes at 6:00 and 18:00 when the presets hand the light
/// over. The exposure is then the game's: its adapted multiplier
/// (`curveExposureMin`, where a typical scene settles) on the light in game
/// units, which with the game's own tone curve shows the scene as the game
/// does, day and night.
const NOON_SUN_LUX: f32 = 90_000.0;

/// Luminance weights (linear Rec. 709).
const LUMA: Vec3 = Vec3::new(0.2126, 0.7152, 0.0722);

/// The light in game units: sun and moon colours (linear) and the multiplier.
fn game_light(e: &fc3_formats::environment::Environment, t: f32) -> (Vec3, Vec3, f32) {
    (Vec3::from_array(linear(e.sun_color.at(t))), Vec3::from_array(linear(e.moon_color.at(t))), e.light_hdr.at(t))
}

/// Lux per game light unit, by the sun's height (see `NOON_SUN_LUX`).
fn lux_per_unit(e: &fc3_formats::environment::Environment, elevation: f32) -> f32 {
    let key = |t: f32| {
        let (sun, moon, hdr) = game_light(e, t);
        ((sun + moon).dot(LUMA) * hdr).max(1e-4)
    };
    let (day, night) = (NOON_SUN_LUX / key(0.5), MOON_LUX / key(0.0));
    (night.ln() + (day.ln() - night.ln()) * smoothstep(-0.06, 0.06, elevation)).exp()
}

/// In-game clock; `speed` is game hours per real second.
#[derive(Resource)]
pub struct TimeOfDay {
    pub hours: f32,
    pub speed: f32,
}

/// The sky's turning axis (level, at yaw 240: Dunia yaw counts
/// counter-clockwise from north), in Bevy space.
pub const SKY_AXIS: Vec3 = Vec3::new(0.866_025_4, 0.0, 0.5);
/// Where the sun rises: across the axis, toward 60 degrees from east to
/// north (Dunia), in Bevy space. The sky turns it overhead by noon.
pub const SUN_RISE: Vec3 = Vec3::new(0.5, 0.0, -0.866_025_4);
/// `SunRiseTimeHour`.
const SUNRISE_HOUR: f32 = 6.0;

/// Normal clock speed: one game hour every four minutes.
pub const CLOCK_SPEED: f32 = 1.0 / 240.0;

impl TimeOfDay {
    /// How far the sky has turned since sunrise (radians; half a turn by 18:00).
    fn sky_angle(&self) -> f32 {
        (self.hours - SUNRISE_HOUR) / 12.0 * std::f32::consts::PI
    }

    /// Direction toward the sun. The world's sky turns about a level axis
    /// (`RotationAxisYaw` 240 in the world's environment settings), so the
    /// sun rises toward NNE, passes overhead at noon and sets toward SSW.
    /// The terrain's horizon maps are traced along the same line.
    pub fn sun_dir(&self) -> Vec3 {
        Quat::from_axis_angle(SKY_AXIS, self.sky_angle()) * SUN_RISE
    }

    /// Direction toward the moon: on the same turning sky, 25 degrees up in
    /// the south at sunrise (`MoonYawAtSunrise` 180, `MoonPitchAtSunrise` 25).
    pub fn moon_dir(&self) -> Vec3 {
        let (s, c) = 25f32.to_radians().sin_cos();
        Quat::from_axis_angle(SKY_AXIS, self.sky_angle()) * Vec3::new(0.0, s, c)
    }

    /// 0 at night, 1 by day, easing through dusk and dawn.
    pub fn daylight(&self) -> f32 {
        smoothstep(-0.1, 0.15, self.sun_dir().y)
    }

    /// Camera exposure (EV100) for the time of day: 15 at noon, 6 under the moon.
    pub fn ev100(&self) -> f32 {
        // By day, exposure partly follows the sun's light (as the eye
        // adapts), so a low afternoon sun doesn't leave the land too dark.
        let y = self.sun_dir().y;
        let lux = 90_000.0 * y.clamp(0.0, 1.0).powf(0.6) + 600.0;
        let day = NOON_EV100 + 0.7 * (lux / NOON_LUX).log2();
        NIGHT_EV100 + (day - NIGHT_EV100) * smoothstep(-0.1, 0.2, y)
    }
}

/// Exposure scale for an EV100 value (as Bevy computes it).
pub fn exposure_scale(ev100: f32) -> f32 {
    1.0 / (1.2 * 2f32.powf(ev100))
}

pub(crate) fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Light colour of the moon.
const MOONLIGHT: Color = Color::srgb(0.62, 0.72, 1.0);

#[derive(Component)]
pub struct Sun;

fn setup_sky(mut commands: Commands, mut media: ResMut<Assets<ScatteringMedium>>) {
    commands.spawn((
        Sun,
        DirectionalLight { illuminance: 90_000.0, shadow_maps_enabled: true, ..default() },
        // The world, plus the shadow proxies only the sun sees.
        bevy::camera::visibility::RenderLayers::from_layers(&[0, crate::shadow_lod::SHADOW_LAYER]),
        CascadeShadowConfigBuilder { num_cascades: 4, first_cascade_far_bound: 12.0, maximum_distance: 400.0, ..default() }.build(),
        SunDisk::EARTH,
        Transform::default(),
        Name::new("sun"),
    ));
    commands.spawn((Atmosphere::earth(media.add(ScatteringMedium::earth(256, 256))), Name::new("atmosphere")));
}

/// Settings for any 3D camera that should see the sky. The atmosphere also
/// provides aerial perspective, so no separate distance fog is used.
pub fn camera_sky_bundle() -> impl Bundle {
    (
        AtmosphereSettings { aerial_view_lut_max_distance: 1.2e4, ..default() },
        // Sky-driven ambient and reflections come from `sky_probe`.
    )
}

/// Moves the sun, or after dusk the moon (the same light, so shadows carry
/// on), and sets exposure and ambient light for the time of day.
pub(crate) fn advance_time(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut tod: ResMut<TimeOfDay>,
    env: Res<GameEnvironment>,
    grade: Option<Res<crate::grade::GameGrade>>,
    mut scene: ResMut<SceneExposure>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight, &mut SunDisk), With<Sun>>,
    mut cameras: Query<&mut Exposure, With<crate::player::PlayerCamera>>,
    post: Option<Res<crate::post_effects::PostLight>>,
) {
    if keys.pressed(KeyCode::KeyT) {
        tod.hours += time.delta_secs() * 2.0;
    }
    tod.hours = (tod.hours + tod.speed * time.delta_secs()).rem_euclid(24.0);
    let Ok((mut t, mut light, mut disk)) = sun.single_mut() else { return };
    let sun_dir = tod.sun_dir();
    let elevation = sun_dir.y;
    let t_day = tod.hours / 24.0;
    let game = env.0.as_ref().filter(|e| !e.light_hdr.0.is_empty() && !e.exposure_min.0.is_empty() && crate::grade::enabled(grade.as_deref()));
    let (dir, illuminance, color, disk_intensity) = if let Some(e) = game {
        // The presets' light (see `NOON_SUN_LUX`), from the sun while its
        // colour outshines the moon's.
        let (sun_c, moon_c, hdr) = game_light(e, t_day);
        let unit = lux_per_unit(e, elevation);
        let exposure = e.exposure_min.at(t_day).max(0.05);
        let sky = if e.sky_hdr.0.is_empty() { 1.0 } else { e.sky_hdr.at(t_day) };
        let curve_or = |c: &fc3_formats::environment::Curve, v: f32| if c.0.is_empty() { v } else { c.at(t_day) };
        let probe = curve_or(&e.probe_scale, 1.0) * curve_or(&e.probe_hdr, 1.0);
        *scene = SceneExposure { ev100: (unit / (1.2 * exposure)).log2(), game: Some((exposure, sky)), probe: Some(probe), unit: Some(unit) };
        let by_sun = sun_c.dot(LUMA) >= moon_c.dot(LUMA);
        let (dir, c) = if by_sun { (Vec3::new(sun_dir.x, sun_dir.y.max(0.03), sun_dir.z).normalize(), sun_c) } else { (tod.moon_dir(), moon_c) };
        let m = c.max_element();
        let color = if m > 1e-4 { Color::linear_rgb(c.x / m, c.y / m, c.z / m) } else { MOONLIGHT };
        (dir, unit * (sun_c + moon_c).dot(LUMA) * hdr, color, if elevation > -0.03 { 1.0 } else { 0.0 })
    } else if elevation > -0.12 {
        // The sun, then twilight glowing from the horizon after sunset.
        let lux = 90_000.0 * elevation.clamp(0.0, 1.0).powf(0.6) + 600.0 * smoothstep(-0.12, 0.0, elevation);
        let dir = Vec3::new(sun_dir.x, sun_dir.y.max(0.03), sun_dir.z).normalize();
        // The sun's colour over the day from the world's presets (warm by
        // day, reddening toward dusk); in the twilight they leave black, a
        // low red glow.
        let twilight = Color::srgb(1.0, 0.75, 0.6).mix(&Color::WHITE, smoothstep(-0.05, 0.15, elevation));
        let color = env.0.as_ref().and_then(|e| hue(e.sun_color.at(tod.hours / 24.0))).unwrap_or(twilight);
        (dir, lux, color, if elevation > -0.03 { 1.0 } else { 0.0 })
    } else {
        // Moonlight rising as the sun sinks (the dome draws the moon itself).
        let color = env.0.as_ref().and_then(|e| hue(e.moon_color.at(tod.hours / 24.0))).unwrap_or(MOONLIGHT);
        (tod.moon_dir(), MOON_LUX * smoothstep(-0.12, -0.25, elevation), color, 0.0)
    };
    *t = Transform::from_translation(dir).looking_at(Vec3::ZERO, Vec3::Y);
    light.illuminance = illuminance;
    light.color = color;
    if disk.intensity != disk_intensity {
        disk.intensity = disk_intensity;
    }
    // Exposure follows the light.
    if game.is_none() {
        *scene = SceneExposure { ev100: tod.ev100(), game: None, probe: None, unit: None };
    }
    // (A screen effect's bloom clip opens the exposure further: a blast's flash.)
    let ev100 = scene.ev100 - post.map_or(0.0, |p| p.exposure.max(0.01).log2());
    for mut e in &mut cameras {
        e.ev100 = ev100;
    }
    // Fill light for night; by day the sky's environment light dominates.
    ambient.brightness = 3.0 + 147.0 * smoothstep(0.1, 0.4, elevation);
    // The sky's light is less blue by day (the world's sky light saturation).
    let saturation = env.0.as_ref().map_or(1.0, |e| e.sky_saturation.at(tod.hours / 24.0));
    ambient.color = Color::WHITE.mix(&Color::srgb(0.75, 0.85, 1.0), saturation);
}
