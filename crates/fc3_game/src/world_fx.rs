//! The world's placed effects, played near the player with the game's own
//! flipbooks: spray where waterfalls land and mist rolling off them, low mist
//! drifting through the valleys, fireflies after dark, candle and torch
//! flames, camp fires burning and dying down to embers. The converter keeps
//! the kinds it knows (`cache::world_fx_kind`).

use bevy::prelude::*;
use fc3_formats::cache;

use crate::particles::{Frame, Particle, Particles, Sheet};
use crate::player::PlayerCamera;
use crate::weapons::Rng;
use crate::CacheInfo;

pub struct WorldFxPlugin;

impl Plugin for WorldFxPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PlaceFire>()
            .add_systems(OnExit(crate::menu::GameState::Loading), (load_effects, spawn_drops))
            .add_systems(Update, (place_fires, play_effects, screen_drops).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    FallsSpray,
    FallsMist,
    Mist,
    Fireflies,
    Candle,
    Torch,
    CampFire,
    Embers,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "falls_spray" => Kind::FallsSpray,
            "falls_mist" => Kind::FallsMist,
            "mist" => Kind::Mist,
            "fireflies" => Kind::Fireflies,
            "candle" => Kind::Candle,
            "torch" => Kind::Torch,
            "camp_fire" => Kind::CampFire,
            "embers" => Kind::Embers,
            _ => return None,
        })
    }

    /// How far off (m) it's played.
    fn range(self) -> f32 {
        match self {
            Kind::FallsSpray => 80.0,
            Kind::FallsMist | Kind::Mist => 120.0,
            Kind::Fireflies => 45.0,
            Kind::Candle | Kind::Torch => 40.0,
            Kind::CampFire | Kind::Embers => 60.0,
        }
    }
}

#[derive(Resource, Default)]
struct WorldEffects {
    list: Vec<(Kind, Vec3)>,
}

/// A fire set down with something (an encounter's camp fire): its flames
/// (a kind `cache::world_fx_kind` names; none for one without) and the
/// light it gives (`lamps` takes that).
#[derive(Message, Clone)]
pub struct PlaceFire {
    pub kind: String,
    pub at: Vec3,
    pub light: Option<fc3_formats::lights::WorldLight>,
}

/// Adds set-down fires' flames to the placed effects (once per spot).
fn place_fires(mut fires: MessageReader<PlaceFire>, effects: Option<ResMut<WorldEffects>>) {
    let Some(mut effects) = effects else { return };
    for f in fires.read() {
        if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
            info!("fire set down: {} at {:.1} (light {})", f.kind, f.at, f.light.is_some());
        }
        let Some(kind) = Kind::parse(&f.kind) else { continue };
        if !effects.list.iter().any(|(k, at)| *k == kind && at.distance(f.at) < 0.5) {
            effects.list.push((kind, f.at));
        }
    }
}

fn load_effects(mut commands: Commands, info: Res<CacheInfo>) {
    let text = std::fs::read_to_string(info.root.join(cache::WORLD_FX_REL)).unwrap_or_default();
    let list: Vec<(Kind, Vec3)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let kind = Kind::parse(it.next()?)?;
            let mut v = [0.0f32; 3];
            for c in &mut v {
                *c = it.next()?.parse().ok()?;
            }
            Some((kind, Vec3::from_array(v)))
        })
        .collect();
    info!("{} placed effects", list.len());
    commands.insert_resource(WorldEffects { list });
}

fn jitter(rng: &mut Rng, r: f32) -> Vec3 {
    Vec3::new(rng.range(-r, r), rng.range(-r, r), rng.range(-r, r))
}

/// Emits each nearby effect's particles for this frame.
fn play_effects(
    time: Res<Time>,
    effects: Option<Res<WorldEffects>>,
    daylight: Res<crate::clouds::Daylight>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut particles: ResMut<Particles>,
    mut rng: ResMut<Rng>,
) {
    let (Some(effects), Ok(cam)) = (effects, cameras.single()) else { return };
    let eye = cam.translation();
    let dt = time.delta_secs();
    let night = 1.0 - (daylight.0 / 0.3).clamp(0.0, 1.0);
    let chance = |rate: f32, rng: &mut Rng| rng.f32() < rate * dt;
    for &(kind, at) in &effects.list {
        let d = at.distance(eye);
        if d > kind.range() {
            continue;
        }
        match kind {
            // White water thrown up where it lands.
            Kind::FallsSpray => {
                if chance(5.0, &mut rng) {
                    let mut s = Particle::new(Sheet::Splash, at + jitter(&mut rng, 1.2), rng.range(0.7, 1.1), rng.range(1.6, 2.6));
                    s.grow = 1.6;
                    s.vel = Vec3::Y * rng.range(1.0, 2.2) + jitter(&mut rng, 0.8);
                    s.gravity = 5.0;
                    s.rot = rng.range(0.0, std::f32::consts::TAU);
                    s.colour = Vec4::new(0.88, 0.92, 0.95, 0.55);
                    particles.spawn(s);
                }
                if chance(1.2, &mut rng) {
                    falls_mist(&mut particles, &mut rng, at, 0.1);
                }
            }
            // Spray drifting off the falls.
            Kind::FallsMist => {
                if chance(1.5, &mut rng) {
                    falls_mist(&mut particles, &mut rng, at, 0.13);
                }
            }
            // Low mist hanging in the valleys, thicker at dawn and dusk.
            Kind::Mist => {
                if chance(0.35, &mut rng) {
                    let mut s = Particle::new(
                        Sheet::Smoke,
                        at + Vec3::new(rng.range(-6.0, 6.0), rng.range(-0.5, 1.0), rng.range(-6.0, 6.0)),
                        rng.range(10.0, 14.0),
                        rng.range(6.0, 9.0),
                    );
                    s.grow = 1.4;
                    s.vel = Vec3::new(rng.range(-0.3, 0.3), 0.02, rng.range(-0.3, 0.3));
                    s.rot = rng.range(0.0, std::f32::consts::TAU);
                    s.spin = rng.range(-0.03, 0.03);
                    s.frame = Frame::Loop(4.0, (rng.f32() * 30.0) as u32);
                    s.fade_in = 0.3;
                    s.fade_out = 0.6;
                    let thick = 0.035 + 0.03 * (1.0 - (daylight.0 * 2.0 - 1.0).abs()).clamp(0.0, 1.0);
                    s.colour = Vec4::new(0.85, 0.87, 0.88, thick);
                    particles.spawn(s);
                }
            }
            // After dark: drifting glints.
            Kind::Fireflies => {
                if night > 0.0 && chance(3.0 * night, &mut rng) {
                    let mut g =
                        Particle::new(Sheet::Glow, at + Vec3::new(rng.range(-4.0, 4.0), rng.range(0.2, 2.0), rng.range(-4.0, 4.0)), rng.range(1.5, 3.0), 0.12);
                    g.vel = jitter(&mut rng, 0.4);
                    g.fade_in = 0.3;
                    g.fade_out = 0.6;
                    g.colour = Vec4::new(0.75, 1.0, 0.35, 1.5);
                    particles.spawn(g);
                }
            }
            Kind::Candle => {
                if chance(9.0, &mut rng) {
                    let mut f = Particle::new(Sheet::Fire, at + Vec3::Y * 0.05, rng.range(0.3, 0.45), rng.range(0.1, 0.14));
                    f.vel = Vec3::Y * 0.15;
                    f.grow = 0.7;
                    f.frame = Frame::Loop(24.0, (rng.f32() * 64.0) as u32);
                    f.colour = Vec4::new(1.0, 0.55, 0.15, 0.9);
                    particles.spawn(f);
                }
            }
            Kind::CampFire => crate::particles::fx::camp_fire(&mut particles, &mut rng, at, dt, 1.0),
            // A dying fire: low flames over the embers, and its smoke.
            Kind::Embers => crate::particles::fx::camp_fire(&mut particles, &mut rng, at, dt, 0.3),
            Kind::Torch => {
                if chance(12.0, &mut rng) {
                    let mut f = Particle::new(Sheet::Fire, at + jitter(&mut rng, 0.05) + Vec3::Y * 0.1, rng.range(0.4, 0.6), rng.range(0.35, 0.5));
                    f.vel = Vec3::Y * rng.range(0.3, 0.5);
                    f.grow = 0.8;
                    f.frame = Frame::Loop(24.0, (rng.f32() * 64.0) as u32);
                    f.fade_in = 0.15;
                    f.colour = Vec4::new(1.0, 0.4, 0.1, 0.85);
                    particles.spawn(f);
                }
                if chance(1.0, &mut rng) {
                    let mut s = Particle::new(Sheet::Smoke, at + Vec3::Y * 0.6, rng.range(2.5, 3.5), 0.35);
                    s.vel = Vec3::Y * 0.5;
                    s.grow = 3.0;
                    s.frame = Frame::Loop(8.0, (rng.f32() * 30.0) as u32);
                    s.colour = Vec4::new(0.3, 0.3, 0.3, 0.15);
                    particles.spawn(s);
                }
            }
        }
    }
}

/// Water on the lens: the game's droplets over the view, from standing in a
/// waterfall's spray or coming up out of the sea, drying off in a few seconds.
#[derive(Component)]
struct ScreenDrops;

/// Spray this near the eye (m) wets the view; it takes this long to dry (s).
const SPRAY_WETS: f32 = 7.0;
const DRY_TIME: f32 = 3.0;

fn spawn_drops(mut commands: Commands, server: Res<AssetServer>) {
    let image: Handle<Image> = server.load(crate::cache_assets::cache_path(&cache::fx_rel("fc3_gfx_waterdrops_01")));
    // A few patches of the drop map about the view, turned different ways
    // (left, top, size in % of the width, flips).
    for (left, top, size, flip_x, flip_y) in
        [(-8.0, -12.0, 52.0, false, false), (48.0, -6.0, 46.0, true, false), (4.0, 38.0, 44.0, true, true), (56.0, 44.0, 50.0, false, true)]
    {
        commands.spawn((
            ScreenDrops,
            Node {
                left: Val::Percent(left),
                top: Val::Percent(top),
                width: Val::Percent(size),
                aspect_ratio: Some(1.0),
                position_type: PositionType::Absolute,
                ..default()
            },
            ImageNode { image: image.clone(), color: Color::srgba(1.0, 1.0, 1.0, 0.0), flip_x, flip_y, ..default() },
            GlobalZIndex(0),
            Pickable::IGNORE,
        ));
    }
}

fn screen_drops(
    time: Res<Time>,
    effects: Option<Res<WorldEffects>>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut drops: Query<&mut ImageNode, With<ScreenDrops>>,
    mut state: Local<(f32, bool)>,
    heights: Res<crate::terrain::Heightfield>,
) {
    let Ok(cam) = cameras.single() else { return };
    let eye = cam.translation();
    let (wet, was_under) = &mut *state;
    *wet = (*wet - time.delta_secs() / DRY_TIME).max(0.0);
    let under = crate::water::submerged(eye, &heights, 0.0);
    if *was_under && !under {
        *wet = 1.0;
    }
    *was_under = under;
    if !under && effects.as_ref().is_some_and(|fx| fx.list.iter().any(|(k, at)| *k == Kind::FallsSpray && at.distance(eye) < SPRAY_WETS)) {
        *wet = 1.0;
    }
    for mut image in &mut drops {
        image.color = Color::srgba(1.0, 1.0, 1.0, *wet * 0.7);
    }
}

/// A puff of spray off a waterfall.
fn falls_mist(particles: &mut Particles, rng: &mut Rng, at: Vec3, alpha: f32) {
    let mut s = Particle::new(Sheet::Smoke, at + jitter(rng, 1.5), rng.range(4.0, 6.0), rng.range(2.5, 4.0));
    s.grow = 2.4;
    s.vel = Vec3::Y * rng.range(0.4, 0.9) + Vec3::new(rng.range(-0.5, 0.5), 0.0, rng.range(-0.5, 0.5));
    s.rot = rng.range(0.0, std::f32::consts::TAU);
    s.spin = rng.range(-0.1, 0.1);
    s.frame = Frame::Loop(6.0, (rng.f32() * 30.0) as u32);
    s.fade_in = 0.15;
    s.colour = Vec4::new(0.9, 0.93, 0.96, alpha);
    particles.spawn(s);
}
