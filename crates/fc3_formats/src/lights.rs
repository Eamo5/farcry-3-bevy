//! Placed lights: lamps, lanterns, street lights and fires.
//!
//! Light entities carry a light component with a colour, an attenuation
//! radius, an intensity, a flicker and the distance at which the engine fades
//! the light out, plus a component saying whether the time of day switches it
//! (lamps that come on at dusk). Fires light their surroundings through their
//! particle effects: an effect's emitters can give off a light for as long as
//! it plays, as the game's particle library sets out. The converter collects
//! them all into one list for the game.

use std::collections::HashMap;

use crate::fcb::Object;

/// Field hashes (names from the game's own identifiers where known).
mod h {
    /// `CDynamicLightComponent`.
    pub const LIGHT_COMPONENT: u32 = 0x2D603C0D;
    /// `fRadius`, `fInnerRadius`.
    pub const RADIUS: u32 = 0xF8186C6E;
    pub const INNER_RADIUS: u32 = 0x24DEE30A;
    /// `clrColor`, `fIntensity`, `fTurnOffDistance`.
    pub const COLOUR: u32 = 0x500F629A;
    pub const INTENSITY: u32 = 0xB23932DE;
    pub const FADE_DISTANCE: u32 = 0xC17FC318;
    /// `fFlickeringFrequency`, `fFlickeringAmplitude`, `fFlickeringNoise`.
    pub const FLICKER_FREQUENCY: u32 = 0x05375BA4;
    pub const FLICKER_AMPLITUDE: u32 = 0x48E57BA6;
    pub const FLICKER_NOISE: u32 = 0x7F10B812;
    pub const SWITCH_COMPONENT: u32 = 0x8DAB48CC;
    pub const SWITCH_MODE: u32 = 0x49BF3666;
}

/// How a light behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightKind {
    /// Shines in all directions.
    Omni,
    /// A cone along the entity's forward axis.
    Spot,
    /// A camp fire's own light, where its flames burn.
    Fire,
    /// The light another particle effect gives off.
    Effect,
}

/// A light's own settings, from its components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightParams {
    pub colour: [f32; 3],
    /// Attenuation radius (m): the light is out by here.
    pub radius: f32,
    /// Full strength within this distance (m); in between, the game's light
    /// passes fade it as `a / d² + b`, from 1 here to 0 at the radius.
    pub inner_radius: f32,
    pub intensity: f32,
    /// Distance from the camera at which the light fades out (m).
    pub fade_distance: f32,
    /// Switched on only at night.
    pub night_only: bool,
    /// Flickering: frequency (per second), amplitude (a share of the
    /// intensity) and noise; all 0 for a steady light.
    pub flicker: [f32; 3],
    /// An effect's lights, one per particle its emitter puts out: a new one
    /// every so often (s), each lasting so long (s), fading in and out over
    /// these (s); all 0 for a light that is simply on.
    pub cycle: [f32; 4],
}

/// Reads the light settings from an entity's `Components`, if it has a light.
pub fn parse_light(components: &Object) -> Option<LightParams> {
    let light = components.children.iter().find(|c| c.name_hash == h::LIGHT_COMPONENT)?;
    let f = |hash| light.field_h(hash).filter(|b| b.len() >= 4).map(|b| f32::from_le_bytes(b[..4].try_into().unwrap()));
    let colour = light.field_h(h::COLOUR).filter(|b| b.len() >= 12).map(|b| {
        let c = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        [c(0), c(1), c(2)]
    })?;
    let night_only = components
        .children
        .iter()
        .find(|c| c.name_hash == h::SWITCH_COMPONENT)
        .and_then(|c| c.field_h(h::SWITCH_MODE))
        .is_some_and(|b| b.first() == Some(&2));
    Some(LightParams {
        colour,
        radius: f(h::RADIUS).unwrap_or(5.0),
        inner_radius: f(h::INNER_RADIUS).unwrap_or(0.0),
        intensity: f(h::INTENSITY).unwrap_or(1.0),
        fade_distance: f(h::FADE_DISTANCE).unwrap_or(40.0),
        night_only,
        flicker: [f(h::FLICKER_FREQUENCY).unwrap_or(0.0), f(h::FLICKER_AMPLITUDE).unwrap_or(0.0), f(h::FLICKER_NOISE).unwrap_or(0.0)],
        cycle: [0.0; 4],
    })
}

/// A light placed in the world (Bevy space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldLight {
    pub kind: LightKind,
    pub position: [f32; 3],
    /// Unit direction a spot shines in (unused for other kinds).
    pub direction: [f32; 3],
    pub params: LightParams,
}

/// Sine or cosine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trig {
    Sin,
    Cos,
}

impl Trig {
    fn of(self, x: f32) -> f32 {
        match self {
            Trig::Sin => x.sin(),
            Trig::Cos => x.cos(),
        }
    }
}

/// The engine's flicker wave, as read out of its code (`engine::flicker_wave`):
/// at `x = time_scale × frequency × seconds`, `main(x) + noise_scale × noise ×
/// Σ term(k x)`, every argument shifted by a phase; a light's flicker is
/// `amplitude_scale × amplitude` times that.
#[derive(Debug, Clone, PartialEq)]
pub struct FlickerWave {
    pub time_scale: f32,
    pub main: Trig,
    pub terms: Vec<(Trig, f32)>,
    pub noise_scale: f32,
    pub amplitude_scale: f32,
    /// A particle effect's light is off below this intensity.
    pub effect_cutoff: f32,
}

impl FlickerWave {
    pub fn eval(&self, x: f32, phase: f32, noise: f32) -> f32 {
        let rough: f32 = self.terms.iter().map(|(t, k)| t.of(k * x + phase)).sum();
        self.main.of(x + phase) + self.noise_scale * noise * rough
    }

    pub fn to_text(&self) -> String {
        let trig = |t: Trig| if t == Trig::Sin { "sin" } else { "cos" };
        let mut s = String::from("# the engine's flicker wave (from its code)\n");
        s += &format!(
            "time_scale {}\nmain {}\nnoise_scale {}\namplitude_scale {}\neffect_cutoff {}\n",
            self.time_scale,
            trig(self.main),
            self.noise_scale,
            self.amplitude_scale,
            self.effect_cutoff
        );
        for (t, k) in &self.terms {
            s += &format!("term {} {k}\n", trig(*t));
        }
        s
    }

    pub fn from_text(text: &str) -> Option<FlickerWave> {
        let trig = |s: &str| match s {
            "sin" => Some(Trig::Sin),
            "cos" => Some(Trig::Cos),
            _ => None,
        };
        let (mut time_scale, mut main, mut noise_scale, mut amplitude_scale, mut terms) = (None, None, None, None, Vec::new());
        let mut effect_cutoff = 0.0;
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let v: Vec<&str> = line.split_whitespace().collect();
            match v.as_slice() {
                ["time_scale", x] => time_scale = x.parse().ok(),
                ["main", t] => main = trig(t),
                ["noise_scale", x] => noise_scale = x.parse().ok(),
                ["amplitude_scale", x] => amplitude_scale = x.parse().ok(),
                ["effect_cutoff", x] => effect_cutoff = x.parse().unwrap_or(0.0),
                ["term", t, k] => terms.push((trig(t)?, k.parse().ok()?)),
                _ => {}
            }
        }
        Some(FlickerWave { time_scale: time_scale?, main: main?, terms, noise_scale: noise_scale?, amplitude_scale: amplitude_scale?, effect_cutoff })
    }
}

/// Where the game keeps its particle library as XML: every effect, with its
/// emitters' settings.
pub const PARTICLE_LIBRARY: &str = r"worlds\fc3_main\generated\fc3_main_deploadnewparticles.xml";

/// A light a particle effect gives off for as long as it plays (one of its
/// emitters with `LightActive`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectLight {
    /// From the effect's origin (game space, Z up).
    pub offset: [f32; 3],
    pub params: LightParams,
}

/// The lights of the particle library's looping effects, by effect name
/// (lower case). One-shot lights (muzzle flashes, explosions) are left out.
pub fn effect_lights(xml: &str) -> HashMap<String, Vec<EffectLight>> {
    let mut out: HashMap<String, Vec<EffectLight>> = HashMap::new();
    let mut effect: Option<String> = None;
    // A lit emitter waiting for its emission rate (the first value of its
    // `EmitRateKF` curve), and whether that curve has started.
    let mut pending: Option<(String, EffectLight)> = None;
    let mut in_rate = false;
    let mut finish = |pending: &mut Option<(String, EffectLight)>, rate: Option<f32>| {
        if let Some((name, mut light)) = pending.take() {
            let [_, duration, fade_in, fade_out] = light.params.cycle;
            if let Some(rate) = rate.filter(|r| *r > 0.0 && duration > 0.0) {
                light.params.cycle = [1.0 / rate, duration, fade_in, fade_out];
            } else {
                light.params.cycle = [0.0; 4];
            }
            out.entry(name).or_default().push(light);
        }
    };
    let mut at = 0;
    while let Some(i) = xml[at..].find('<') {
        let start = at + i + 1;
        let end = xml[start..].find('>').map_or(xml.len(), |e| start + e);
        let tag = &xml[start..end];
        // (Each tag's attributes keep their leading space for `attr`.)
        let named = |name: &str| tag.strip_prefix(name).filter(|t| t.starts_with(char::is_whitespace));
        if let Some(t) = named("PartSys") {
            finish(&mut pending, None);
            effect = attr(t, "Name").map(str::to_ascii_lowercase);
        } else if let Some(t) = named("PartEmit") {
            finish(&mut pending, None);
            pending = effect.clone().zip(emitter_light(t));
            in_rate = false;
        } else if pending.is_some() && tag.starts_with("EmitRateKF") {
            in_rate = true;
        } else if let Some(t) = named("ValueKF").filter(|_| in_rate) {
            let rate = attr(t, "Value").and_then(|v| v.trim().parse::<f32>().ok());
            finish(&mut pending, rate);
            in_rate = false;
        }
        at = end;
    }
    finish(&mut pending, None);
    out
}

/// An XML attribute's value within a tag.
fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let at = tag.find(&format!(" {key}=\""))? + key.len() + 3;
    let len = tag[at..].find('"')?;
    Some(&tag[at..at + len])
}

/// The light an emitter gives off, if it is lit, active and looping.
fn emitter_light(tag: &str) -> Option<EffectLight> {
    if attr(tag, "LightActive") != Some("1") || attr(tag, "Active") != Some("1") || attr(tag, "EmitLoop") != Some("1") {
        return None;
    }
    let f = |k: &str| attr(tag, k).and_then(|v| v.trim().parse::<f32>().ok());
    let v3 = |k: &str| -> Option<[f32; 3]> {
        let v: Vec<f32> = attr(tag, k)?.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (v.len() == 3).then(|| [v[0], v[1], v[2]])
    };
    Some(EffectLight {
        offset: v3("PosOffset").unwrap_or([0.0; 3]),
        params: LightParams {
            colour: v3("LightColor")?,
            radius: f("LightOuterRadius")?,
            inner_radius: f("LightInnerRadius").unwrap_or(0.0),
            intensity: f("LightIntensity").unwrap_or(1.0),
            // The emitter stops drawing this far from the camera.
            fade_distance: f("FarClippingEnd").filter(|d| *d > 0.0).unwrap_or(40.0),
            night_only: false,
            flicker: [f("LightFlickeringFrequency").unwrap_or(0.0), f("LightFlickeringAmplitude").unwrap_or(0.0), f("LightFlickeringNoise").unwrap_or(0.0)],
            // The interval comes from the emission rate (`effect_lights`).
            cycle: [0.0, f("LightDuration").unwrap_or(0.0), f("LightFadeIn").unwrap_or(0.0), f("LightFadeOut").unwrap_or(0.0)],
        },
    })
}

/// Text form: one light per line.
pub fn to_text(lights: &[WorldLight]) -> String {
    let mut s = String::from(
        "# kind x y z dx dy dz r g b radius intensity fade night inner_radius flicker_frequency flicker_amplitude flicker_noise cycle_interval cycle_duration cycle_fade_in cycle_fade_out\n",
    );
    for l in lights {
        let kind = match l.kind {
            LightKind::Omni => "omni",
            LightKind::Spot => "spot",
            LightKind::Fire => "fire",
            LightKind::Effect => "effect",
        };
        let [x, y, z] = l.position;
        let [dx, dy, dz] = l.direction;
        let p = &l.params;
        let [r, g, b] = p.colour;
        let [ff, fa, fnoise] = p.flicker;
        let [ci, cd, cin, cout] = p.cycle;
        s += &format!(
            "{kind} {x:.2} {y:.2} {z:.2} {dx:.3} {dy:.3} {dz:.3} {r:.3} {g:.3} {b:.3} {:.2} {:.2} {:.1} {} {:.2} {ff:.2} {fa:.3} {fnoise:.2} {ci:.3} {cd:.3} {cin:.3} {cout:.3}\n",
            p.radius,
            p.intensity,
            p.fade_distance,
            p.night_only as u8,
            p.inner_radius
        );
    }
    s
}

pub fn from_text(text: &str) -> Vec<WorldLight> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let kind = match it.next()? {
                "omni" => LightKind::Omni,
                "spot" => LightKind::Spot,
                "fire" => LightKind::Fire,
                "effect" => LightKind::Effect,
                _ => return None,
            };
            let v: Vec<f32> = it.map(|s| s.parse().ok()).collect::<Option<_>>()?;
            if ![13, 14, 17, 21].contains(&v.len()) {
                return None;
            }
            let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
            Some(WorldLight {
                kind,
                position: [v[0], v[1], v[2]],
                direction: [v[3], v[4], v[5]],
                params: LightParams {
                    colour: [v[6], v[7], v[8]],
                    radius: v[9],
                    inner_radius: at(13),
                    intensity: v[10],
                    fade_distance: v[11],
                    night_only: v[12] != 0.0,
                    flicker: [at(14), at(15), at(16)],
                    cycle: [at(17), at(18), at(19), at(20)],
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip() {
        let lights = vec![
            WorldLight {
                kind: LightKind::Spot,
                position: [1.5, 102.25, -3.0],
                direction: [0.0, -1.0, 0.0],
                params: LightParams { colour: [0.63, 0.61, 0.55], radius: 8.0, inner_radius: 2.0, intensity: 1.5, fade_distance: 35.0, night_only: true, flicker: [0.0; 3], cycle: [0.0; 4] },
            },
            WorldLight {
                kind: LightKind::Fire,
                position: [10.0, 100.5, 20.0],
                direction: [0.0, -1.0, 0.0],
                params: LightParams { colour: [1.0, 0.325, 0.098], radius: 1.5, inner_radius: 0.8, intensity: 50.0, fade_distance: 30.0, night_only: false, flicker: [1.0, 0.05, 2.0], cycle: [0.5, 1.0, 0.1, 0.2] },
            },
        ];
        assert_eq!(from_text(&to_text(&lights)), lights);
    }

    #[test]
    fn flicker_wave_text_roundtrip() {
        let w = FlickerWave { time_scale: 2.5, main: Trig::Sin, terms: vec![(Trig::Cos, 1.5), (Trig::Sin, 4.25)], noise_scale: 0.3, amplitude_scale: 0.75, effect_cutoff: 0.02 };
        assert_eq!(FlickerWave::from_text(&w.to_text()), Some(w));
        assert_eq!(FlickerWave::from_text("main cos\n"), None);
    }

    /// Older light lists (without flicker) still read.
    #[test]
    fn reads_lists_without_flicker() {
        let l = from_text("omni 1 2 3 0 -1 0 1 1 1 4 1 20.0 1 1.00\n");
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].params.flicker, [0.0; 3]);
        assert_eq!(l[0].params.inner_radius, 1.0);
    }

    /// Looping emitters with a light give it to their effect; one-shot ones
    /// (a muzzle flash) and unlit ones don't.
    #[test]
    fn effect_lights_from_the_library() {
        let xml = r#"<NewPartLib>
	<PartSys Name="sample.fire.small_fire" Id="{1}">
		<PartEmit Name="sample.Fire.Small_Fire.Flames" Active="1" EmitLoop="1" LightActive="0" LightColor="1,1,1" LightOuterRadius="3">
			<EmitRateKF CurveVersion="1" />
		</PartEmit>
		<PartEmit Name="sample.Fire.Small_Fire.Light" Active="1" PosOffset="0,0,0.5" EmitLoop="1" FarClippingEnd="35" LightActive="1" LightInnerRadius="1" LightOuterRadius="2.5" LightIntensity="20" LightFlickeringFrequency="2" LightFlickeringAmplitude="0.1" LightFlickeringNoise="1" LightColor="1,0.5,0.25" LightDuration="1.5" LightFadeIn="0.1" LightFadeOut="0.25">
			<EmitRateKF CurveVersion="1">
				<ValueKF Value="2">
				</ValueKF>
			</EmitRateKF>
		</PartEmit>
	</PartSys>
	<PartSys Name="sample.muzzle" Id="{2}">
		<PartEmit Name="sample.Muzzle.Light" Active="1" EmitLoop="0" LightActive="1" LightOuterRadius="4" LightColor="1,1,1" />
	</PartSys>
</NewPartLib>"#;
        let lights = effect_lights(xml);
        assert_eq!(lights.len(), 1);
        let fire = &lights["sample.fire.small_fire"];
        assert_eq!(fire.len(), 1);
        assert_eq!(fire[0].offset, [0.0, 0.0, 0.5]);
        let p = fire[0].params;
        assert_eq!((p.colour, p.radius, p.inner_radius, p.intensity, p.fade_distance), ([1.0, 0.5, 0.25], 2.5, 1.0, 20.0, 35.0));
        assert_eq!(p.flicker, [2.0, 0.1, 1.0]);
        // Two particles a second, each with a light lasting 1.5 s.
        assert_eq!(p.cycle, [0.5, 1.5, 0.1, 0.25]);
    }
}
