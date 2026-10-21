//! The world's outdoor environment presets: how the light, fog and exposure
//! change over the day. They live in the world's managers file
//! (`fc3_main.managers.fcb`) as named templates in the item database
//! (`Preview_Lighting:Preview_Lighting`, `..._Fog`, `..._Exposure`,
//! `..._Sky`), which the open-air zones (jungle, savannah and the rest) use.
//!
//! Curves and gradients are keyed by the time of day as a fraction (0 at
//! midnight, 0.25 at 6:00). Curve knots store (time, value, then Bezier
//! handles, which are flat in these presets), so they are read as straight
//! segments between knots.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::fcb::{self, Object};
use crate::hash::name_hash;

/// The managers file holding the presets.
pub const MANAGERS_PATH: &str = r"worlds\fc3_main\generated\fc3_main.managers.fcb";
/// Prefix of the outdoor presets' names.
pub const OUTDOOR: &str = "Preview_Lighting:Preview_";
/// Prefix of the presets the weather blends to in a storm.
pub const STORM: &str = "Default:Storm.";
/// The single-player world's weather (`CEnvironmentWeather`).
pub const WEATHER: &str = "Default:StormIntensity.Default";

/// The weather's rhythm (`CEnvironmentWeather`): how long clear spells and
/// storms last (s: least, most), how long a storm takes to come on (s), and
/// its lightning: how bright a flash is (a multiplier on the light), how
/// fast it fades (per s) and the wait between flashes (s: least, most).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WeatherTimes {
    pub clear: (f32, f32),
    pub storm: (f32, f32),
    pub transition: f32,
    pub lightning: f32,
    pub lightning_speed: f32,
    pub lightning_gap: (f32, f32),
}
/// Field holding a database template's name.
const TEMPLATE_NAME: u32 = 0xD97A_7AA9;
/// Field holding a database template's GUID (`{...}`), by which scripts
/// name the presets they put over the world's (`SetFogOverride`...).
const TEMPLATE_GUID: u32 = 0xD854_F1C7;
/// Field holding a template body's class (`CEnvironmentFog`...).
const TEMPLATE_CLASS: u32 = 0xCF68_E402;

/// A value over the day, as (time of day 0..1, value) knots.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Curve(pub Vec<(f32, f32)>);

/// A colour over the day, as (time of day 0..1, linear RGB) knots.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gradient(pub Vec<(f32, [f32; 3])>);

fn segment<T: Copy>(knots: &[(f32, T)], t: f32, lerp: impl Fn(T, T, f32) -> T, fallback: T) -> T {
    let Some(first) = knots.first() else { return fallback };
    if t <= first.0 {
        return first.1;
    }
    for w in knots.windows(2) {
        let ((t0, a), (t1, b)) = (w[0], w[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            return lerp(a, b, f);
        }
    }
    knots.last().map_or(fallback, |k| k.1)
}

impl Curve {
    /// The value at a time of day (0..1).
    pub fn at(&self, t: f32) -> f32 {
        segment(&self.0, t.rem_euclid(1.0), |a, b, f| a + (b - a) * f, 0.0)
    }

    /// The value at `x` for a curve over something other than the day (no
    /// wrapping; the end values hold beyond the knots).
    pub fn eval(&self, x: f32) -> f32 {
        segment(&self.0, x, |a, b, f| a + (b - a) * f, 0.0)
    }

    /// `k` of the way from this curve to another: knots at both's, so
    /// the mix of the two straight-line pieces is exact (one not given,
    /// the other once it's halfway).
    pub fn mix(&self, other: &Curve, k: f32) -> Curve {
        if self == other || self.0.is_empty() || other.0.is_empty() {
            return if k < 0.5 { self.clone() } else { other.clone() };
        }
        Curve(knots_of(&self.0, &other.0).into_iter().map(|t| (t, self.eval(t) + (other.eval(t) - self.eval(t)) * k)).collect())
    }
}

/// Both lists' knot positions, in order, once each.
fn knots_of<A, B>(a: &[(f32, A)], b: &[(f32, B)]) -> Vec<f32> {
    let mut at: Vec<f32> = a.iter().map(|k| k.0).chain(b.iter().map(|k| k.0)).collect();
    at.sort_by(f32::total_cmp);
    at.dedup();
    at
}

fn mix3(a: [f32; 3], b: [f32; 3], k: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * k)
}

/// The world's render settings (`Render/WorldSettings` in the generic
/// database; see `WORLD_SETTINGS_PATH`): the bloom's response curve, which
/// takes an exposed brightness to how much of it blooms.
pub fn bloom_response(world_settings: &[u8]) -> Option<Curve> {
    let start = world_settings.windows(4).position(|w| w == b"nbCF")?;
    let root = fcb::parse(&world_settings[start..]).ok()?;
    let mut found = None;
    root.walk(&mut |o, _| {
        if found.is_none() && o.child("curveBloomResponseCurve").is_some() {
            found = Some(curve(o, "curveBloomResponseCurve"));
        }
    });
    found.filter(|c| !c.0.is_empty())
}

/// The vegetation's light through leaves from the world's render settings
/// (`VegetationSSS`): colour (rgb), highlight, strength, shadow strength and
/// backlight strength.
pub fn vegetation_sss(world_settings: &[u8]) -> Option<[f32; 7]> {
    let start = world_settings.windows(4).position(|w| w == b"nbCF")?;
    let root = fcb::parse(&world_settings[start..]).ok()?;
    let mut found = None;
    root.walk(&mut |o, _| {
        if found.is_none() {
            if let Some(s) = o.child("VegetationSSS") {
                let c = s.vec3("clrColor").unwrap_or([1.0; 3]);
                let f = |n: &str| s.f32(n).unwrap_or(0.0);
                found = Some([c[0], c[1], c[2], f("Highlight"), f("Strength"), f("ShadowStrength"), f("BacklightStrength")]);
            }
        }
    });
    found
}

/// The world's render settings object.
pub const WORLD_SETTINGS_PATH: &str = r"generated\databases\generic\renderworldsettings_render\worldsettings.obj";

impl Gradient {
    /// The colour at a time of day (0..1).
    pub fn at(&self, t: f32) -> [f32; 3] {
        self.sample(t.rem_euclid(1.0))
    }

    /// The colour at a position along the gradient (no wrapping).
    pub fn sample(&self, p: f32) -> [f32; 3] {
        segment(&self.0, p, |a, b, f| std::array::from_fn(|i| a[i] + (b[i] - a[i]) * f), [0.0; 3])
    }

    /// `k` of the way from this gradient to another (as `Curve::mix`).
    pub fn mix(&self, other: &Gradient, k: f32) -> Gradient {
        if self == other || self.0.is_empty() || other.0.is_empty() {
            return if k < 0.5 { self.clone() } else { other.clone() };
        }
        Gradient(knots_of(&self.0, &other.0).into_iter().map(|t| (t, mix3(self.sample(t), other.sample(t), k))).collect())
    }
}

/// A gradient that changes over the day: (time of day 0..1, gradient) keys.
/// The sky dome's colours are kept this way, each gradient running from the
/// zenith (0) to the horizon (1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyedGradient(pub Vec<(f32, Gradient)>);

impl KeyedGradient {
    /// The colour at a time of day (0..1) and a position along the gradient.
    pub fn at(&self, t: f32, p: f32) -> [f32; 3] {
        let t = t.rem_euclid(1.0);
        let keys = &self.0;
        let Some(first) = keys.first() else { return [0.0; 3] };
        if t <= first.0 {
            return first.1.sample(p);
        }
        for w in keys.windows(2) {
            let ((t0, a), (t1, b)) = (&w[0], &w[1]);
            if t <= *t1 {
                let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                let (ca, cb) = (a.sample(p), b.sample(p));
                return std::array::from_fn(|i| ca[i] + (cb[i] - ca[i]) * f);
            }
        }
        keys.last().map_or([0.0; 3], |k| k.1.sample(p))
    }

    /// `k` of the way from this to another: keys at both's, each a
    /// gradient with knots at all of both's (exact at them, near between).
    pub fn mix(&self, other: &KeyedGradient, k: f32) -> KeyedGradient {
        if self == other || self.0.is_empty() || other.0.is_empty() {
            return if k < 0.5 { self.clone() } else { other.clone() };
        }
        let mut places: Vec<f32> = self.0.iter().chain(&other.0).flat_map(|(_, g)| g.0.iter().map(|p| p.0)).collect();
        places.sort_by(f32::total_cmp);
        places.dedup();
        KeyedGradient(
            knots_of(&self.0, &other.0)
                .into_iter()
                .map(|t| (t, Gradient(places.iter().map(|&p| (p, mix3(self.at(t, p), other.at(t, p), k))).collect())))
                .collect(),
        )
    }
}

/// The outdoor environment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Environment {
    /// The sun's light colour (black while it is down).
    pub sun_color: Gradient,
    /// The moon's light colour (black while the sun is up).
    pub moon_color: Gradient,
    /// The light's brightness multiplier (sun by day, moon by night).
    pub light_hdr: Curve,
    /// Fog: density per metre over the day, the distance it starts at and
    /// the distance beyond which it stops thickening.
    pub fog_density: Curve,
    pub fog_start: f32,
    pub fog_end: f32,
    /// The exposure's target middle grey over the day, and the range the
    /// eye's adaptation keeps the exposure in (a multiplier on the scene).
    pub middle_grey: Curve,
    pub exposure_min: Curve,
    pub exposure_max: Curve,
    /// The sky dome's brightness multiplier.
    pub sky_hdr: Curve,
    /// The light probes' (the shade's light) scale and brightness multiplier.
    pub probe_scale: Curve,
    pub probe_hdr: Curve,
    /// How saturated the sky's light is.
    pub sky_saturation: Curve,
    /// The sky dome's colour on the sun's side and on the far side, from
    /// the zenith to the horizon, over the day.
    pub sky_sun_side: KeyedGradient,
    pub sky_far_side: KeyedGradient,
    /// In a storm (`Default:Storm.*`): the sky dome's multiplier, the cloud
    /// colour over the day, and the fog (density per metre, the distance it
    /// stops thickening at, and how much of the sky's colour it takes).
    pub storm_sky_hdr: Curve,
    pub storm_cloud: Gradient,
    pub storm_fog_density: Curve,
    pub storm_fog_end: f32,
    pub storm_fog_sky: f32,
    /// How the storm's fog covers the sky toward the horizon: its strength
    /// and the power of (1 - elevation) it falls off by.
    pub storm_sky_fog: (f32, f32),
    /// The bloom's strength (`CEnvironmentAdaptiveBloom`'s `fIntensity`,
    /// what the final pass scales the summed bloom by).
    pub bloom_intensity: f32,
    /// Light through leaves (see `vegetation_sss`).
    pub veg_sss: Option<[f32; 7]>,
    /// How much of an exposed brightness blooms (see `bloom_response`), as
    /// (brightness, amount) knots.
    pub bloom_response: Curve,
    /// Depth of field (`CEnvironmentDepthOfField`): focus, near and far
    /// distances (m), blur scale, circle of confusion, and 1 when it blurs the
    /// sky too; `None` when the preset is off or missing.
    pub dof: Option<[f32; 6]>,
    /// God rays: full while the sun is within the first angle of the view's
    /// centre, fading out by the second (degrees). Their intensity, tint and
    /// cloud masking are constant over the day.
    pub shafts_fade: (f32, f32),
    /// The weather's rhythm and lightning ([`WEATHER`]).
    pub weather: Option<WeatherTimes>,
    /// The sun (and moon) switched off, its light and shadows with it: a
    /// shadow preset's `bEnableSun` 0, put over the world's under it (the
    /// mines, the ULCs' caves).
    pub sun_off: bool,
}

fn knots(o: &Object, name: &str) -> Vec<Arc<Object>> {
    o.child(name).and_then(|c| c.child("Knots")).map(|k| k.children_named("Knot").cloned().collect()).unwrap_or_default()
}

fn curve(o: &Object, name: &str) -> Curve {
    Curve(
        knots(o, name)
            .iter()
            .filter_map(|k| {
                let v = k.field("Value")?;
                let f = |i: usize| v.get(i * 4..i * 4 + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
                Some((f(0)?, f(1)?))
            })
            .collect(),
    )
}

fn gradient(o: &Object, name: &str) -> Gradient {
    Gradient(knots(o, name).iter().filter_map(|k| Some((k.f32("Position").unwrap_or(0.0), k.vec3("Color")?))).collect())
}

/// A depth of field preset's focus, near and far distances, blur scale,
/// circle of confusion and whether it blurs the sky; `None` when off.
fn dof(d: &Object) -> Option<[f32; 6]> {
    (d.field("bEnabled").and_then(|b| b.first().copied()).unwrap_or(0) != 0).then(|| {
        let f = |n: &str| d.f32(n).unwrap_or(0.0);
        let sky = d.field("bApplyDOFToSky").and_then(|b| b.first().copied()).unwrap_or(0) as f32;
        [f("fFocusDistance"), f("fNearDistance"), f("fFarDistance"), f("fBlurScale"), f("fCircleOfConfusion"), sky]
    })
}

fn keyed_gradient(o: &Object, name: &str) -> KeyedGradient {
    KeyedGradient(knots(o, name).iter().map(|k| (k.f32("Position").unwrap_or(0.0), gradient(k, "Gradient"))).filter(|(_, g)| !g.0.is_empty()).collect())
}

impl Environment {
    /// The world's colour cube (`fileColorRemapVolumeMapTexture` on the
    /// environment manager), the path of a volume texture.
    pub fn colour_cube_path(managers: &[u8]) -> Option<String> {
        let start = managers.windows(4).position(|w| w == b"nbCF")?;
        let root = fcb::parse(&managers[start..]).ok()?;
        let mut path = None;
        root.walk(&mut |o, _| {
            if path.is_none() {
                path = o.str("fileColorRemapVolumeMapTexture").filter(|p| !p.is_empty());
            }
        });
        path
    }

    /// Reads the outdoor presets from the managers file.
    pub fn parse(managers: &[u8]) -> Result<Environment> {
        let start = managers.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
        let root = fcb::parse(&managers[start..])?;
        // Every named template: its name and its body (the inner `Template`).
        let mut templates: HashMap<String, Arc<Object>> = HashMap::new();
        let inner = name_hash("Template");
        root.walk(&mut |o, _| {
            if let Some(name) = o.str_h(TEMPLATE_NAME) {
                if name.starts_with(OUTDOOR) || name.starts_with(STORM) || name == WEATHER {
                    if let Some(body) = o.children.iter().find(|c| c.name_hash == inner) {
                        templates.insert(name, body.clone());
                    }
                }
            }
        });
        let get = |kind: &str| templates.get(&format!("{OUTDOOR}{kind}")).with_context(|| format!("no {OUTDOOR}{kind} preset"));
        let (lighting, fog, exposure, sky) = (get("Lighting")?, get("Fog")?, get("Exposure")?, get("Sky")?);
        let rays = get("GodRays").ok();
        let bloom_intensity = get("AdaptiveBloom").ok().and_then(|b| b.f32("fIntensity")).unwrap_or(1.0);
        let dof = get("DoF").ok().and_then(|d| dof(d));
        let storm = |kind: &str| templates.get(&format!("{STORM}{kind}"));
        let (storm_sky, storm_cloud, storm_fog) = (storm("Sky"), storm("Cloud"), storm("Fog"));
        let angle = |name: &str, default: f32| rays.and_then(|r| r.f32(name)).unwrap_or(default);
        let weather = templates.get(WEATHER).map(|w| {
            // (A zero is stored as a single byte.)
            let f = |n: &str| w.f32(n).unwrap_or(0.0);
            WeatherTimes {
                clear: (f("fClearMinimumTime"), f("fClearMaximumTime")),
                storm: (f("fStormMinimumTime"), f("fStormMaximumTime")),
                transition: f("fStormTransitionTime"),
                lightning: f("fLightningIntensity"),
                lightning_speed: f("fLightningSpeed"),
                lightning_gap: (f("fLightningMinimumTime"), f("fLightningMaximumTime")),
            }
        });
        Ok(Environment {
            weather,
            sun_color: gradient(lighting, "gradSunLightColor"),
            moon_color: gradient(lighting, "gradMoonLightColor"),
            light_hdr: curve(lighting, "curveLightHDRMul"),
            fog_density: curve(fog, "curveFogDensity"),
            fog_start: fog.f32("fMinimumDistance").unwrap_or(0.0),
            fog_end: fog.f32("fMaximumDistance").unwrap_or(0.0),
            middle_grey: curve(exposure, "curveMiddleGrey"),
            exposure_min: curve(exposure, "curveExposureMin"),
            exposure_max: curve(exposure, "curveExposureMax"),
            sky_hdr: curve(sky, "curveSkyColorHDRMul"),
            probe_scale: curve(lighting, "curveProbeScale"),
            probe_hdr: curve(lighting, "curveProbeHDRMul"),
            sky_saturation: curve(sky, "curveSkyLightSaturation"),
            sky_sun_side: keyed_gradient(sky, "kfgradSkyDomeColorSunSide"),
            sky_far_side: keyed_gradient(sky, "kfgradSkyDomeColorSunOppositeSide"),
            shafts_fade: (angle("fAttenuationStartAngle", 40.0), angle("fAttenuationEndAngle", 90.0)),
            dof,
            bloom_response: Curve::default(),
            bloom_intensity,
            veg_sss: None,
            storm_sky_hdr: storm_sky.map(|s| curve(s, "curveSkyColorHDRMul")).unwrap_or_default(),
            storm_cloud: storm_cloud.and_then(|c| c.child("Lighting")).map(|l| gradient(l, "gradCloudColorSun")).unwrap_or_default(),
            storm_fog_density: storm_fog.map(|f| curve(f, "curveFogDensity")).unwrap_or_default(),
            storm_fog_end: storm_fog.and_then(|f| f.f32("fMaximumDistance")).unwrap_or(0.0),
            storm_fog_sky: storm_fog.and_then(|f| f.f32("fSkyColorFactor")).unwrap_or(0.0),
            storm_sky_fog: storm_fog.map_or((0.0, 1.0), |f| (f.f32("fSkyFogIntensity").unwrap_or(0.0), f.f32("fSkyFogPower").unwrap_or(1.0))),
            sun_off: false,
        })
    }

    /// A preset as scripts name it, by its GUID (braces and all) or its
    /// template's name (`SHA_Quest_Mines:Common.Mines_Fog`): its class
    /// (`CEnvironmentFog`, `CEnvironmentLighting`...) and its body.
    pub fn preset_by_guid(managers: &[u8], key: &str) -> Option<(String, Arc<Object>)> {
        let start = managers.windows(4).position(|w| w == b"nbCF")?;
        let root = fcb::parse(&managers[start..]).ok()?;
        let inner = name_hash("Template");
        // (Some scripts' names carry a space after.)
        let key = key.trim();
        let field = if key.starts_with('{') { TEMPLATE_GUID } else { TEMPLATE_NAME };
        let mut found = None;
        root.walk(&mut |o, _| {
            if found.is_none() && o.str_h(field).is_some_and(|g| g.trim().eq_ignore_ascii_case(key)) {
                found = o.children.iter().find(|c| c.name_hash == inner).and_then(|b| Some((b.str_h(TEMPLATE_CLASS)?, b.clone())));
            }
        });
        found
    }

    /// Puts a preset over the matching part of this environment (a scene's
    /// fog, lighting, exposure, sky, bloom, depth of field or its sun
    /// switched off); false for a kind it doesn't model.
    pub fn apply_preset(&mut self, class: &str, body: &Object) -> bool {
        match class {
            "CEnvironmentFog" => {
                self.fog_density = curve(body, "curveFogDensity");
                self.fog_start = body.f32("fMinimumDistance").unwrap_or(0.0);
                self.fog_end = body.f32("fMaximumDistance").unwrap_or(0.0);
            }
            "CEnvironmentLighting" => {
                self.sun_color = gradient(body, "gradSunLightColor");
                self.moon_color = gradient(body, "gradMoonLightColor");
                self.light_hdr = curve(body, "curveLightHDRMul");
                self.probe_scale = curve(body, "curveProbeScale");
                self.probe_hdr = curve(body, "curveProbeHDRMul");
            }
            "CEnvironmentExposure" => {
                self.middle_grey = curve(body, "curveMiddleGrey");
                self.exposure_min = curve(body, "curveExposureMin");
                self.exposure_max = curve(body, "curveExposureMax");
            }
            "CEnvironmentSky" => {
                self.sky_hdr = curve(body, "curveSkyColorHDRMul");
                self.sky_saturation = curve(body, "curveSkyLightSaturation");
                self.sky_sun_side = keyed_gradient(body, "kfgradSkyDomeColorSunSide");
                self.sky_far_side = keyed_gradient(body, "kfgradSkyDomeColorSunOppositeSide");
            }
            "CEnvironmentAdaptiveBloom" => self.bloom_intensity = body.f32("fIntensity").unwrap_or(self.bloom_intensity),
            "CEnvironmentDepthOfField" => self.dof = dof(body),
            "CEnvironmentShadow" => self.sun_off = body.field("bEnableSun").and_then(|b| b.first().copied()) == Some(0),
            _ => return false,
        }
        true
    }

    /// `k` of the way from this environment to another, as a preset comes
    /// on or goes over the seconds a script gives: its curves and colours
    /// mixed, its distances and bloom between; what is either on or off
    /// (the depth of field missing, the sun) switches halfway.
    pub fn mix(&self, other: &Environment, k: f32) -> Environment {
        let k = k.clamp(0.0, 1.0);
        let num = |a: f32, b: f32| a + (b - a) * k;
        Environment {
            sun_color: self.sun_color.mix(&other.sun_color, k),
            moon_color: self.moon_color.mix(&other.moon_color, k),
            light_hdr: self.light_hdr.mix(&other.light_hdr, k),
            fog_density: self.fog_density.mix(&other.fog_density, k),
            fog_start: num(self.fog_start, other.fog_start),
            fog_end: num(self.fog_end, other.fog_end),
            middle_grey: self.middle_grey.mix(&other.middle_grey, k),
            exposure_min: self.exposure_min.mix(&other.exposure_min, k),
            exposure_max: self.exposure_max.mix(&other.exposure_max, k),
            sky_hdr: self.sky_hdr.mix(&other.sky_hdr, k),
            probe_scale: self.probe_scale.mix(&other.probe_scale, k),
            probe_hdr: self.probe_hdr.mix(&other.probe_hdr, k),
            sky_saturation: self.sky_saturation.mix(&other.sky_saturation, k),
            sky_sun_side: self.sky_sun_side.mix(&other.sky_sun_side, k),
            sky_far_side: self.sky_far_side.mix(&other.sky_far_side, k),
            bloom_intensity: num(self.bloom_intensity, other.bloom_intensity),
            dof: match (self.dof, other.dof) {
                (Some(a), Some(b)) => Some(std::array::from_fn(|i| num(a[i], b[i]))),
                (a, b) => {
                    if k < 0.5 {
                        a
                    } else {
                        b
                    }
                }
            },
            sun_off: if k < 0.5 { self.sun_off } else { other.sun_off },
            // (What presets don't change: the other's.)
            ..other.clone()
        }
    }

    /// Plain-text form for the cache: one `name time values...` line per knot.
    pub fn to_text(&self) -> String {
        let mut out = String::from("# outdoor environment (time of day 0..1)\n");
        for (name, g) in [("sun_color", &self.sun_color), ("moon_color", &self.moon_color), ("storm_cloud", &self.storm_cloud)] {
            for (t, c) in &g.0 {
                out += &format!("{name} {t} {} {} {}\n", c[0], c[1], c[2]);
            }
        }
        for (name, c) in [
            ("light_hdr", &self.light_hdr),
            ("fog_density", &self.fog_density),
            ("middle_grey", &self.middle_grey),
            ("exposure_min", &self.exposure_min),
            ("exposure_max", &self.exposure_max),
            ("sky_hdr", &self.sky_hdr),
            ("probe_scale", &self.probe_scale),
            ("probe_hdr", &self.probe_hdr),
            ("storm_sky_hdr", &self.storm_sky_hdr),
            ("storm_fog_density", &self.storm_fog_density),
            ("bloom_response", &self.bloom_response),
            ("sky_saturation", &self.sky_saturation),
        ] {
            for (t, v) in &c.0 {
                out += &format!("{name} {t} {v}\n");
            }
        }
        for (name, k) in [("sky_sun", &self.sky_sun_side), ("sky_far", &self.sky_far_side)] {
            for (t, g) in &k.0 {
                for (p, c) in &g.0 {
                    out += &format!("{name} {t} {p} {} {} {}\n", c[0], c[1], c[2]);
                }
            }
        }
        out += &format!("fog_range {} {}\n", self.fog_start, self.fog_end);
        out += &format!("shafts_fade {} {}\n", self.shafts_fade.0, self.shafts_fade.1);
        out += &format!("storm_fog {} {}\n", self.storm_fog_end, self.storm_fog_sky);
        out += &format!("bloom_intensity {}\n", self.bloom_intensity);
        if let Some(v) = self.veg_sss {
            out += &format!("veg_sss {} {} {} {} {} {} {}\n", v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
        }
        if let Some(d) = self.dof {
            out += &format!("dof {} {} {} {} {} {}\n", d[0], d[1], d[2], d[3], d[4], d[5]);
        }
        out += &format!("storm_sky_fog {} {}\n", self.storm_sky_fog.0, self.storm_sky_fog.1);
        if let Some(w) = self.weather {
            out += &format!(
                "weather {} {} {} {} {} {} {} {} {}\n",
                w.clear.0, w.clear.1, w.storm.0, w.storm.1, w.transition, w.lightning, w.lightning_speed, w.lightning_gap.0, w.lightning_gap.1
            );
        }
        out
    }

    pub fn from_text(text: &str) -> Environment {
        let mut e = Environment::default();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut it = line.split_whitespace();
            let Some(name) = it.next() else { continue };
            let v: Vec<f32> = it.filter_map(|s| s.parse().ok()).collect();
            match (name, v.as_slice()) {
                ("sun_color", [t, r, g, b]) => e.sun_color.0.push((*t, [*r, *g, *b])),
                ("moon_color", [t, r, g, b]) => e.moon_color.0.push((*t, [*r, *g, *b])),
                ("storm_cloud", [t, r, g, b]) => e.storm_cloud.0.push((*t, [*r, *g, *b])),
                ("storm_sky_hdr", [t, x]) => e.storm_sky_hdr.0.push((*t, *x)),
                ("storm_fog_density", [t, x]) => e.storm_fog_density.0.push((*t, *x)),
                ("bloom_response", [t, x]) => e.bloom_response.0.push((*t, *x)),
                ("storm_fog", [a, b]) => (e.storm_fog_end, e.storm_fog_sky) = (*a, *b),
                ("dof", [a, b, c, d, f, g]) => e.dof = Some([*a, *b, *c, *d, *f, *g]),
                ("veg_sss", [a, b, c, d, f, g, k]) => e.veg_sss = Some([*a, *b, *c, *d, *f, *g, *k]),
                ("bloom_intensity", [x]) => e.bloom_intensity = *x,
                ("storm_sky_fog", [a, b]) => e.storm_sky_fog = (*a, *b),
                ("weather", [a, b, c, d, f, g, h, i, j]) => {
                    e.weather =
                        Some(WeatherTimes { clear: (*a, *b), storm: (*c, *d), transition: *f, lightning: *g, lightning_speed: *h, lightning_gap: (*i, *j) })
                }
                ("light_hdr", [t, x]) => e.light_hdr.0.push((*t, *x)),
                ("fog_density", [t, x]) => e.fog_density.0.push((*t, *x)),
                ("middle_grey", [t, x]) => e.middle_grey.0.push((*t, *x)),
                ("exposure_min", [t, x]) => e.exposure_min.0.push((*t, *x)),
                ("exposure_max", [t, x]) => e.exposure_max.0.push((*t, *x)),
                ("sky_hdr", [t, x]) => e.sky_hdr.0.push((*t, *x)),
                ("probe_scale", [t, x]) => e.probe_scale.0.push((*t, *x)),
                ("probe_hdr", [t, x]) => e.probe_hdr.0.push((*t, *x)),
                ("sky_saturation", [t, x]) => e.sky_saturation.0.push((*t, *x)),
                ("sky_sun" | "sky_far", [t, p, r, g, b]) => {
                    let k = if name == "sky_sun" { &mut e.sky_sun_side } else { &mut e.sky_far_side };
                    // Consecutive lines with the same time make up one gradient.
                    if k.0.last().is_none_or(|(last, _)| last != t) {
                        k.0.push((*t, Gradient::default()));
                    }
                    if let Some((_, grad)) = k.0.last_mut() {
                        grad.0.push((*p, [*r, *g, *b]));
                    }
                }
                ("fog_range", [a, b]) => (e.fog_start, e.fog_end) = (*a, *b),
                ("shafts_fade", [a, b]) => e.shafts_fade = (*a, *b),
                _ => {}
            }
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene's preset found by its GUID in the installed managers file
    /// (DOC_020's trip's bloom) and put over the world's.
    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_preset_by_guid_overrides_its_part() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let managers = std::fs::read(cache.join("story/managers.fcb")).unwrap();
        let (class, body) = Environment::preset_by_guid(&managers, "{4E7036EA-9C9B-4A5B-8E64-6504A6218F64}").unwrap();
        assert_eq!(class, "CEnvironmentAdaptiveBloom");
        let mut e = Environment { bloom_intensity: 1.0, ..Default::default() };
        assert!(e.apply_preset(&class, &body));
        assert_eq!(e.bloom_intensity, 30.0);
        assert!(Environment::preset_by_guid(&managers, "{00000000-0000-0000-0000-000000000000}").is_none());
        // By its name too (the mines' fog), a space after and all.
        let (class, _) = Environment::preset_by_guid(&managers, "SHA_Quest_Mines:Common.Mines_Fog ").unwrap();
        assert_eq!(class, "CEnvironmentFog");
        // The mines' shadow preset switches the sun off.
        let (class, body) = Environment::preset_by_guid(&managers, "SHA_Quest_Mines:Common.Mines_Shadow").unwrap();
        let mut under = Environment::default();
        assert!(under.apply_preset(&class, &body) && under.sun_off);
    }

    #[test]
    fn a_preset_comes_on_by_mixing_curves_and_colours() {
        // Halfway from a flat fog to a rising one: the mix of the two at
        // both's knots.
        let a = Curve(vec![(0.0, 0.0), (1.0, 0.0)]);
        let b = Curve(vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);
        let half = a.mix(&b, 0.5);
        assert_eq!(half.0, vec![(0.0, 0.0), (0.5, 0.5), (1.0, 0.0)]);
        assert_eq!(a.mix(&b, 1.0).eval(0.25), b.eval(0.25));
        let g = Gradient(vec![(0.0, [1.0, 0.0, 0.0])]).mix(&Gradient(vec![(0.0, [0.0, 0.0, 1.0])]), 0.25);
        assert_eq!(g.sample(0.0), [0.75, 0.0, 0.25]);
        // The rest: distances between, what's on or off at the halfway.
        let from = Environment { fog_end: 400.0, dof: None, ..Default::default() };
        let to = Environment { fog_end: 100.0, dof: Some([1.0; 6]), sun_off: true, ..Default::default() };
        let quarter = from.mix(&to, 0.25);
        assert_eq!((quarter.fog_end, quarter.dof, quarter.sun_off), (325.0, None, false));
        let most = from.mix(&to, 0.75);
        assert_eq!((most.fog_end, most.dof, most.sun_off), (175.0, Some([1.0; 6]), true));
        assert_eq!(from.mix(&to, 1.0), to);
    }

    #[test]
    fn curves_interpolate_and_round_trip() {
        let e = Environment {
            sun_color: Gradient(vec![(0.25, [0.0; 3]), (0.5, [1.0, 0.5, 0.0])]),
            light_hdr: Curve(vec![(0.0, 6.0), (0.25, 6.0), (0.25, 2.5), (0.75, 2.5), (0.75, 6.0)]),
            exposure_min: Curve(vec![(0.0, 1.0), (0.268, 1.45)]),
            sky_hdr: Curve(vec![(0.0, 1.25), (1.0, 1.25)]),
            probe_scale: Curve(vec![(0.0, 0.9), (0.25, 0.6)]),
            storm_sky_hdr: Curve(vec![(0.0, 0.03), (0.5, 0.3)]),
            storm_cloud: Gradient(vec![(0.25, [0.35; 3]), (0.5, [1.0; 3])]),
            storm_fog_end: 150.0,
            storm_fog_sky: 0.5,
            storm_sky_fog: (1.0, 12.0),
            dof: Some([0.001, 0.0, 1500.0, 0.25, 0.01, 0.0]),
            weather: Some(WeatherTimes {
                clear: (2000.0, 4840.0),
                storm: (120.0, 240.0),
                transition: 10.0,
                lightning: 24.0,
                lightning_speed: 12.0,
                lightning_gap: (4.0, 16.0),
            }),
            bloom_response: Curve(vec![(0.0, 0.0), (1.0, 0.03), (3.6, 0.43)]),
            veg_sss: Some([0.9, 1.0, 0.0, 4.0, 3.0, 1.5, 0.8]),
            bloom_intensity: 1.0,
            fog_start: 45.0,
            fog_end: 400.0,
            shafts_fade: (40.0, 90.0),
            sky_sun_side: KeyedGradient(vec![
                (0.0, Gradient(vec![(0.0, [0.1, 0.2, 0.3]), (1.0, [0.4, 0.5, 0.6])])),
                (0.5, Gradient(vec![(0.0, [0.5, 0.6, 0.8]), (1.0, [0.6, 0.7, 0.8])])),
            ]),
            ..Default::default()
        };
        assert_eq!(e.sun_color.at(0.375), [0.5, 0.25, 0.0]);
        assert_eq!(e.sun_color.at(0.1), [0.0; 3]);
        assert_eq!(e.light_hdr.at(0.5), 2.5);
        assert_eq!(e.light_hdr.at(0.9), 6.0);
        assert_eq!(Environment::from_text(&e.to_text()), e);
        // Sky dome: between keys in time, along each gradient in position.
        let mid = e.sky_sun_side.at(0.25, 1.0);
        assert!((mid[0] - 0.5).abs() < 1e-5 && (mid[2] - 0.7).abs() < 1e-5, "{mid:?}");
        assert_eq!(e.sky_sun_side.at(0.75, 0.0), [0.5, 0.6, 0.8]);
    }
}
