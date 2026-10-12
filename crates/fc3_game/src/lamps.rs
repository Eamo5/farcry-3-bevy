//! The world's placed lights: lamps, lanterns, street lights and camp fires.
//!
//! About two thousand lights come from the converted world. A small pool of
//! Bevy point and spot lights is handed to the ones nearest the camera, each
//! fading out toward its distance limit; a larger pool of soft additive glows
//! marks lit lamps much further out, so villages read at night from across a
//! valley. Lamps the game switches by time of day come on at dusk, and lights
//! flicker as the game sets them to. Camp fires light their logs with their
//! flame effect's own light (and some have a broader light placed beside
//! them as well).

use std::collections::HashMap;

use bevy::prelude::*;
use fc3_formats::lights::{FlickerWave, LightKind, WorldLight};

use crate::clouds::Daylight;
use crate::data::CacheInfo;
use crate::particles::{Particle, Particles, Sheet};
use crate::player::PlayerCamera;
use crate::sky::smoothstep;

pub struct LampsPlugin;

impl Plugin for LampsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Lamps>()
            .add_systems(Update, (load_lamps.run_if(resource_exists::<CacheInfo>), place_fire_lights, assign_lamps, drive_lights, drive_glows).chain());
    }
}

const POINT_SLOTS: usize = 40;
const SPOT_SLOTS: usize = 12;
const GLOW_SLOTS: usize = 160;
/// Glows show this far out.
const GLOW_REACH: f32 = 250.0;
const CELL: f32 = 64.0;
/// Lights illuminate at least this far out, whatever their own fade distance.
const MIN_REACH: f32 = 45.0;
/// Without the game's grade: luminous power (lm) for an intensity-1,
/// full-white light, set by eye.
const LUMENS: f32 = 12_000.0;

/// With the game's grade: the luminous power (lm) whose inverse-square fall
/// matches the game's for a light of this inner radius and radius, where the
/// game lights with its colour times its intensity times `a / d² + b` (1 at
/// the inner radius, 0 at the radius) in its units of light, `unit` lux each.
/// Bevy's range window stands in for the `b` that brings it to 0.
fn game_lumens(l: &WorldLight, unit: f32) -> Option<f32> {
    let (r, inner) = (l.params.radius, l.params.inner_radius);
    if inner <= 0.0 || inner >= r {
        return None;
    }
    let a = inner * inner * r * r / (r * r - inner * inner);
    Some(4.0 * std::f32::consts::PI * unit * a)
}

/// How much to dim a light so that it lights what is `d` metres from it as
/// the game's does, never brighter. The game's light passes saturate their
/// falloff (`a / d² + b`), so inside the inner radius a light stays at its
/// full strength; Bevy's goes on rising as 1/d² (times its range window),
/// several times over at the middle of a wide light. Matched at the eye's
/// distance, what is near the eye (the hands, a wall beside it) is lit as in
/// the game; 1 for lights without the game's radii.
fn inner_clamp(l: &WorldLight, d: f32) -> f32 {
    let (r, inner) = (l.params.radius, l.params.inner_radius);
    if inner <= 0.0 || inner >= r {
        return 1.0;
    }
    let a = inner * inner * r * r / (r * r - inner * inner);
    let b = -inner * inner / (r * r - inner * inner);
    let d2 = d.max(0.05).powi(2);
    let game = (a / d2 + b).clamp(0.0, 1.0);
    let window = (1.0 - (d2 / (r * r)).powi(2)).clamp(0.0, 1.0).powi(2);
    let ours = a / d2 * window;
    if ours <= 1e-6 || ours <= game {
        1.0
    } else {
        game / ours
    }
}

#[derive(Resource, Default)]
struct Lamps {
    loaded: bool,
    list: Vec<WorldLight>,
    /// The engine's flicker wave, when the converter found it.
    wave: Option<FlickerWave>,
    cells: HashMap<(i32, i32), Vec<u32>>,
    points: Vec<Slot>,
    spots: Vec<Slot>,
    glows: Vec<Slot>,
    next_assign: f32,
}

/// A pooled light (or glow, with no entity) and the light it currently shows
/// (index into `list`).
struct Slot {
    entity: Entity,
    current: Option<u32>,
}

/// How far a light illuminates before fading out.
fn reach(l: &WorldLight) -> f32 {
    l.params.fade_distance.max(MIN_REACH) + 15.0
}

fn cell_of(p: Vec3) -> (i32, i32) {
    ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

/// Night switching: 0 by day, 1 after dusk.
fn switched_on(l: &WorldLight, daylight: f32) -> f32 {
    if l.params.night_only {
        1.0 - smoothstep(0.15, 0.6, daylight)
    } else {
        1.0
    }
}

/// The light's intensity at time `t`, flickering as the engine has it (its
/// wave read out of its code at conversion). A light entity's flicker adds
/// `amplitude × wave` (scaled) to its intensity, in step for every light, or
/// without the wave the light burns steady. An effect's light is the sum of
/// its particles' lights, as the engine's particle code runs them: one per
/// particle its emitter puts out, each lasting its time and fading in and out,
/// each its intensity times `amplitude × |wave|` (scaled) with a phase of its
/// own, so an effect's intensity runs high (a camp fire's 50 averages about
/// 1), and each off below the engine's cutoff (without the wave, a flickering
/// effect's light can't be worked out and is left off).
fn light_level(l: &WorldLight, i: u32, t: f32, wave: Option<&FlickerWave>) -> f32 {
    let [frequency, amplitude, noise] = l.params.flicker;
    let intensity = l.params.intensity;
    if !matches!(l.kind, LightKind::Fire | LightKind::Effect) {
        let Some(wave) = wave.filter(|_| frequency != 0.0) else { return intensity };
        let x = wave.time_scale * frequency * t;
        return (intensity + wave.amplitude_scale * amplitude * wave.eval(x, 0.0, noise)).max(0.0);
    }
    // One particle's light (`instance` numbers them), fades aside.
    let one = |instance: f32| -> f32 {
        if frequency == 0.0 {
            return intensity;
        }
        let Some(wave) = wave else { return 0.0 };
        let phase = (i as f32 * 0.618_034 + instance * 0.754_878).fract() * std::f32::consts::TAU;
        intensity * wave.amplitude_scale * amplitude * wave.eval(wave.time_scale * frequency * t, phase, noise).abs()
    };
    let cutoff = wave.map_or(0.0, |w| w.effect_cutoff);
    let on = |level: f32| if level < cutoff { 0.0 } else { level };
    let [interval, duration, fade_in, fade_out] = l.params.cycle;
    if interval <= 0.0 || duration <= 0.0 {
        return on(one(0.0));
    }
    let newest = (t / interval).floor();
    let alive = (duration / interval).ceil() as i32;
    (0..=alive)
        .map(|j| newest - j as f32)
        .map(|k| {
            let age = t - k * interval;
            if !(0.0..=duration).contains(&age) {
                return 0.0;
            }
            let mut fade = 1.0;
            if fade_in > 0.0 && age < fade_in {
                fade = age / fade_in;
            }
            if fade_out > 0.0 && age > duration - fade_out {
                fade *= (duration - age) / fade_out;
            }
            on(one(k) * fade)
        })
        .sum()
}

/// Light colour normalised to its brightest channel, and that channel.
fn tint(l: &WorldLight) -> (Vec3, f32) {
    let c = Vec3::from(l.params.colour);
    let peak = c.max_element().max(1e-3);
    (c / peak, peak)
}

fn load_lamps(info: Res<CacheInfo>, mut lamps: ResMut<Lamps>, mut commands: Commands) {
    if lamps.loaded {
        return;
    }
    lamps.loaded = true;
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::LIGHTS_REL)).unwrap_or_default();
    let list = fc3_formats::lights::from_text(&text);
    lamps.wave = std::fs::read_to_string(info.root.join(fc3_formats::cache::FLICKER_REL)).ok().and_then(|t| FlickerWave::from_text(&t));
    if lamps.wave.is_none() {
        warn!("no flicker wave in the cache: lights burn steady");
    }
    for (i, l) in list.iter().enumerate() {
        lamps.cells.entry(cell_of(Vec3::from(l.position))).or_default().push(i as u32);
    }
    info!("{} placed lights", list.len());
    lamps.list = list;

    let hidden = || (Transform::default(), Visibility::Hidden);
    lamps.points = (0..POINT_SLOTS)
        .map(|_| Slot { entity: commands.spawn((PointLight { intensity: 0.0, radius: 0.05, ..default() }, hidden())).id(), current: None })
        .collect();
    lamps.spots = (0..SPOT_SLOTS)
        .map(|_| Slot {
            entity: commands.spawn((SpotLight { intensity: 0.0, outer_angle: 0.75, inner_angle: 0.45, ..default() }, hidden())).id(),
            current: None,
        })
        .collect();
    // Glows are particle sprites; their slots need no entity.
    lamps.glows = (0..GLOW_SLOTS).map(|_| Slot { entity: Entity::PLACEHOLDER, current: None }).collect();
}

/// The lights of fires set down with something (an encounter's camp
/// fire), once per spot.
fn place_fire_lights(mut fires: MessageReader<crate::world_fx::PlaceFire>, mut lamps: ResMut<Lamps>) {
    for l in fires.read().filter_map(|f| f.light) {
        let at = Vec3::from(l.position);
        let cell = cell_of(at);
        if lamps.cells.get(&cell).is_some_and(|ids| ids.iter().any(|&i| Vec3::from(lamps.list[i as usize].position).distance(at) < 0.3)) {
            continue;
        }
        let i = lamps.list.len() as u32;
        lamps.list.push(l);
        lamps.cells.entry(cell).or_default().push(i);
    }
}

/// Gives `slots` to the lights in `wanted` (nearest first), keeping lights
/// that already have a slot where they are.
fn fill(slots: &mut [Slot], mut wanted: Vec<(f32, u32)>) {
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
    wanted.truncate(slots.len());
    let wanted: Vec<u32> = wanted.into_iter().map(|(_, i)| i).collect();
    for s in slots.iter_mut() {
        if s.current.is_some_and(|i| !wanted.contains(&i)) {
            s.current = None;
        }
    }
    for i in wanted {
        if slots.iter().any(|s| s.current == Some(i)) {
            continue;
        }
        if let Some(s) = slots.iter_mut().find(|s| s.current.is_none()) {
            s.current = Some(i);
        }
    }
}

/// Hands the pools to the nearest lit lights (a few times a second).
fn assign_lamps(time: Res<Time>, daylight: Res<Daylight>, cameras: Query<&GlobalTransform, With<PlayerCamera>>, mut lamps: ResMut<Lamps>) {
    let lamps = &mut *lamps;
    lamps.next_assign -= time.delta_secs();
    if lamps.next_assign > 0.0 || lamps.points.is_empty() {
        return;
    }
    lamps.next_assign = 0.2;
    let Ok(cam) = cameras.single() else { return };
    let c = cam.translation();
    let (cx, cz) = cell_of(c);
    let cells = (GLOW_REACH / CELL).ceil() as i32;
    let (mut points, mut spots, mut glows) = (Vec::new(), Vec::new(), Vec::new());
    for dx in -cells..=cells {
        for dz in -cells..=cells {
            let Some(ids) = lamps.cells.get(&(cx + dx, cz + dz)) else { continue };
            for &i in ids {
                let l = &lamps.list[i as usize];
                if switched_on(l, daylight.0) <= 0.0 {
                    continue;
                }
                let d = c.distance(Vec3::from(l.position));
                if d < reach(l) {
                    if l.kind == LightKind::Spot { &mut spots } else { &mut points }.push((d, i));
                }
                if d < GLOW_REACH {
                    glows.push((d, i));
                }
            }
        }
    }
    fill(&mut lamps.points, points);
    fill(&mut lamps.spots, spots);
    fill(&mut lamps.glows, glows);
}

/// Places and dims the pooled lights each frame (fades and flicker).
fn drive_lights(
    time: Res<Time>,
    daylight: Res<Daylight>,
    lamps: Res<Lamps>,
    scene: Res<crate::sky::SceneExposure>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut points: Query<(&mut PointLight, &mut Transform, &mut Visibility), Without<SpotLight>>,
    mut spots: Query<(&mut SpotLight, &mut Transform, &mut Visibility), Without<PointLight>>,
) {
    let Ok(cam) = cameras.single() else { return };
    let c = cam.translation();
    let t = time.elapsed_secs();
    let level = |i: u32| {
        let l = &lamps.list[i as usize];
        let r = reach(l);
        let fade = 1.0 - smoothstep(r - 15.0, r, c.distance(Vec3::from(l.position)));
        let (tint, peak) = tint(l);
        // The game's own falloff where the light has one, else set by eye.
        let per_unit = scene.unit.and_then(|u| game_lumens(l, u)).unwrap_or(LUMENS);
        let lumens = per_unit * light_level(l, i, t, lamps.wave.as_ref()) * peak * switched_on(l, daylight.0) * fade;
        // Out by the game's radius with its falloff (Bevy's range window
        // lands close to its `b`); further, and spots brighter, as set by eye.
        let game = scene.unit.is_some() && game_lumens(l, 1.0).is_some();
        // (Flat inside the inner radius, as the game's, where the eye is.)
        let lumens = if game { lumens * inner_clamp(l, c.distance(Vec3::from(l.position))) } else { lumens };
        (l, Color::linear_rgb(tint.x, tint.y, tint.z), lumens, game)
    };
    for s in &lamps.points {
        let Ok((mut light, mut tr, mut vis)) = points.get_mut(s.entity) else { continue };
        let Some(i) = s.current else {
            *vis = Visibility::Hidden;
            continue;
        };
        let (l, color, lumens, game) = level(i);
        light.intensity = lumens;
        light.color = color;
        light.range = match (game, l.kind) {
            (true, _) => l.params.radius,
            // Set by eye: a fire's glow carries well past its logs.
            (false, LightKind::Fire) => 9.0,
            (false, _) => l.params.radius * 1.6,
        };
        tr.translation = Vec3::from(l.position);
        *vis = Visibility::Inherited;
    }
    for s in &lamps.spots {
        let Ok((mut light, mut tr, mut vis)) = spots.get_mut(s.entity) else { continue };
        let Some(i) = s.current else {
            *vis = Visibility::Hidden;
            continue;
        };
        let (l, color, lumens, game) = level(i);
        light.intensity = lumens * if game { 1.0 } else { 1.5 };
        light.color = color;
        light.range = l.params.radius * if game { 1.0 } else { 2.4 };
        let dir = Vec3::from(l.direction);
        let up = if dir.cross(Vec3::Y).length_squared() > 1e-4 { Vec3::Y } else { Vec3::X };
        *tr = Transform::from_translation(Vec3::from(l.position)).looking_to(dir, up);
        *vis = Visibility::Inherited;
    }
}

/// Draws a glow sprite (the game's light glow) on each lit lamp in reach.
fn drive_glows(
    time: Res<Time>,
    daylight: Res<Daylight>,
    lamps: Res<Lamps>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut particles: ResMut<Particles>,
) {
    let Ok(cam) = cameras.single() else { return };
    let c = cam.translation();
    let t = time.elapsed_secs();
    let night = 1.0 - smoothstep(0.1, 0.5, daylight.0);
    if night <= 0.0 {
        return;
    }
    for i in lamps.glows.iter().filter_map(|s| s.current) {
        let l = &lamps.list[i as usize];
        let pos = Vec3::from(l.position);
        let d = c.distance(pos);
        let fade = 1.0 - smoothstep(GLOW_REACH - 40.0, GLOW_REACH, d);
        let g = (night * fade * switched_on(l, daylight.0) * light_level(l, i, t, lamps.wave.as_ref()).min(2.0) * 0.5).min(1.2);
        // A fire's own flames make its glow; keep that one small.
        let size = 0.7 * l.params.radius.sqrt() * if l.kind == LightKind::Fire { 0.6 } else { 1.0 };
        let mut p = Particle::new(Sheet::Glow, pos, 1.0, size * (d / 40.0).max(1.0).sqrt());
        let (tint, _) = tint(l);
        p.colour = tint.extend(g);
        particles.sprite(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::lights::{LightParams, Trig};

    fn light(kind: LightKind, intensity: f32, flicker: [f32; 3]) -> WorldLight {
        WorldLight {
            kind,
            position: [0.0; 3],
            direction: [0.0, -1.0, 0.0],
            params: LightParams {
                colour: [1.0; 3],
                radius: 4.0,
                inner_radius: 1.0,
                intensity,
                fade_distance: 20.0,
                night_only: false,
                flicker,
                cycle: [0.0; 4],
            },
        }
    }

    /// A made-up wave of the engine's shape.
    fn wave() -> FlickerWave {
        FlickerWave { time_scale: 2.0, main: Trig::Cos, terms: vec![(Trig::Sin, 3.0)], noise_scale: 0.5, amplitude_scale: 1.0, effect_cutoff: 0.05 }
    }

    #[test]
    fn without_a_frequency_or_a_wave() {
        let w = wave();
        assert_eq!(light_level(&light(LightKind::Omni, 2.0, [0.0, 0.5, 1.0]), 0, 1.3, Some(&w)), 2.0);
        assert_eq!(light_level(&light(LightKind::Omni, 2.0, [1.0, 0.5, 1.0]), 0, 1.3, None), 2.0);
        assert_eq!(light_level(&light(LightKind::Fire, 5.0, [0.0, 0.05, 2.0]), 0, 1.3, None), 5.0);
        // A flickering effect's light can't be worked out without the wave.
        assert_eq!(light_level(&light(LightKind::Fire, 50.0, [1.0, 0.05, 2.0]), 0, 1.3, None), 0.0);
    }

    /// Inside a light's inner radius the game's stays at full strength:
    /// ours is dimmed to match at the eye, and left alone further out.
    #[test]
    fn a_light_is_flat_inside_its_inner_radius() {
        let mut l = light(LightKind::Omni, 1.0, [0.0; 3]);
        (l.params.inner_radius, l.params.radius) = (10.0, 15.0);
        let a = 100.0 * 225.0 / 125.0;
        let window = |d: f32| (1.0 - (d * d / 225.0).powi(2)).powi(2);
        // At 5 m ours would be 7.2 times full strength (times the window).
        let k = inner_clamp(&l, 5.0);
        assert!((k * a / 25.0 * window(5.0) - 1.0).abs() < 1e-4, "{k}");
        // Just past the inner radius ours is a little over the game's, and
        // near the radius they agree to within the window's shape.
        assert!(inner_clamp(&l, 10.5) < 1.0 && inner_clamp(&l, 10.5) > 0.8);
        assert_eq!(inner_clamp(&l, 14.0), 1.0);
        // Lights without the game's radii are left as they are.
        l.params.inner_radius = 0.0;
        assert_eq!(inner_clamp(&l, 1.0), 1.0);
    }

    /// A lamp's flicker adds to its intensity and averages out.
    #[test]
    fn lamps_add_their_flicker() {
        let (w, l) = (wave(), light(LightKind::Omni, 2.0, [1.0, 0.2, 1.0]));
        let x: f32 = 2.0 * 0.7;
        let expect = 2.0 + 0.2 * (x.cos() + 0.5 * (3.0 * x).sin());
        assert!((light_level(&l, 5, 0.7, Some(&w)) - expect).abs() < 1e-5);
        let mean = (0..20_000).map(|k| light_level(&l, 5, k as f32 * 0.01, Some(&w))).sum::<f32>() / 20_000.0;
        assert!((mean - 2.0).abs() < 0.02, "{mean}");
    }

    /// An effect's lights come and go with its particles: each fades in and
    /// out over its life, and those alive at once add up.
    #[test]
    fn effect_lights_follow_their_particles() {
        let w = wave();
        let mut l = light(LightKind::Effect, 2.0, [0.0; 3]);
        l.params.cycle = [1.0, 1.0, 0.1, 0.1];
        let at = |t: f32, l: &WorldLight| light_level(l, 0, t, Some(&w));
        assert!((at(5.05, &l) - 1.0).abs() < 1e-3);
        assert!((at(5.5, &l) - 2.0).abs() < 1e-3);
        assert!((at(5.95, &l) - 1.0).abs() < 1e-3);
        // A new one every half second, each lasting a second: two at once.
        l.params.cycle = [0.5, 1.0, 0.1, 0.1];
        assert!((at(5.25, &l) - 4.0).abs() < 1e-3);
    }

    /// An effect's light is its intensity times its flicker's size, off below
    /// the cutoff.
    #[test]
    fn effects_scale_by_their_flicker() {
        let (w, l) = (wave(), light(LightKind::Effect, 10.0, [1.0, 0.5, 1.0]));
        let phase = (0.618_034f32 * 1.0).fract() * std::f32::consts::TAU;
        let x: f32 = 2.0 * 0.3;
        let size = ((x + phase).cos() + 0.5 * (3.0 * x + phase).sin()).abs();
        let expect = 10.0 * 0.5 * size;
        let got = light_level(&l, 1, 0.3, Some(&w));
        assert!(if expect < 0.05 { got == 0.0 } else { (got - expect).abs() < 1e-4 }, "{got} {expect}");
        let dim = light(LightKind::Effect, 0.01, [1.0, 0.5, 1.0]);
        assert_eq!(light_level(&dim, 1, 0.3, Some(&w)), 0.0);
    }
}
