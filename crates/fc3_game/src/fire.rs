//! Grass fires. Molotovs, the flamethrower and explosions set the ground
//! alight; fire spreads cell by cell through grass and bushes, faster
//! downwind, burns them away, scorches the ground and burns anything standing
//! in it. Rain damps it down.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::combat::{DamageEvent, DamageKind, Health};
use crate::particles::{Particle, Particles, Sheet};
use crate::player::PlayerCamera;
use crate::weapons::Rng;

pub struct FirePlugin;

impl Plugin for FirePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Ignite>().init_resource::<Fires>().add_systems(Update, (ignite, spread, burn_effects, burn_damage, fire_lights).chain());
    }
}

/// Grass and bushes that fire spreads through and burns away.
#[derive(Component)]
pub struct Flammable;

/// Sets fire to the ground around a point.
#[derive(Message, Clone, Copy)]
pub struct Ignite {
    pub at: Vec3,
    pub radius: f32,
    pub instigator: Option<Entity>,
}

/// Fire grid cell size (m).
const CELL: f32 = 2.0;
/// Burning cells at once, and cells one fire may burn in total.
const MAX_BURNING: usize = 260;
const MAX_BURNT: usize = 1600;

type Cell = (i32, i32);

fn cell_of(p: Vec3) -> Cell {
    ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

fn cell_centre(c: Cell, y: f32) -> Vec3 {
    Vec3::new((c.0 as f32 + 0.5) * CELL, y, (c.1 as f32 + 0.5) * CELL)
}

struct Burning {
    age: f32,
    life: f32,
    /// Ground height.
    y: f32,
}

#[derive(Resource, Default)]
pub struct Fires {
    burning: HashMap<Cell, Burning>,
    burnt: HashSet<Cell>,
    /// Whoever started the current fire (credited with its kills).
    instigator: Option<Entity>,
    next_spread: f32,
    next_damage: f32,
    lights: Vec<Entity>,
}

impl Fires {
    pub fn is_burning_at(&self, p: Vec3) -> bool {
        self.burning.contains_key(&cell_of(p))
    }

    fn light(&mut self, c: Cell, y: f32, rng: &mut Rng) {
        if self.burning.len() < MAX_BURNING && self.burnt.len() < MAX_BURNT && !self.burnt.contains(&c) {
            self.burning.entry(c).or_insert(Burning { age: 0.0, life: rng.range(7.0, 11.0), y });
        }
    }
}

/// Plants per fire cell near a point (only what's spawned near the camera).
fn fuel_near(plants: &Query<(Entity, &GlobalTransform), With<Flammable>>, centre: Vec3, radius: f32) -> HashMap<Cell, Vec<(Entity, f32)>> {
    let mut fuel: HashMap<Cell, Vec<(Entity, f32)>> = HashMap::new();
    let r2 = radius * radius;
    for (e, gt) in plants {
        let p = gt.translation();
        let d = p - centre;
        if d.x * d.x + d.z * d.z < r2 {
            fuel.entry(cell_of(p)).or_default().push((e, p.y));
        }
    }
    fuel
}

fn ignite(
    mut events: MessageReader<Ignite>,
    mut fires: ResMut<Fires>,
    mut rng: ResMut<Rng>,
    heights: Res<crate::terrain::Heightfield>,
    plants: Query<(Entity, &GlobalTransform), With<Flammable>>,
) {
    for ev in events.read() {
        if ev.at.y < crate::water::surface_at(ev.at, &heights) + 0.2 {
            continue;
        }
        if fires.burning.is_empty() {
            fires.burnt.clear();
        }
        fires.instigator = ev.instigator.or(fires.instigator);
        let fuel = fuel_near(&plants, ev.at, ev.radius + CELL);
        let ground = heights.height_at(ev.at).unwrap_or(ev.at.y);
        let r = (ev.radius / CELL).ceil() as i32;
        let c0 = cell_of(ev.at);
        for dx in -r..=r {
            for dz in -r..=r {
                let c = (c0.0 + dx, c0.1 + dz);
                if cell_centre(c, 0.0).with_y(ev.at.y).distance(ev.at) > ev.radius + CELL * 0.5 {
                    continue;
                }
                // The splash itself burns briefly even on bare ground.
                let y = fuel.get(&c).map(|v| v[0].1).unwrap_or(ground);
                fires.light(c, y, &mut rng);
                if !fuel.contains_key(&c) {
                    if let Some(b) = fires.burning.get_mut(&c) {
                        b.life = b.life.min(rng.range(3.0, 5.0));
                    }
                }
            }
        }
    }
}

/// Fire creeps into neighbouring cells that have plants, faster downwind;
/// burnt-out cells lose their plants.
#[allow(clippy::too_many_arguments)]
fn spread(
    time: Res<Time>,
    weather: Res<crate::clouds::Weather>,
    mut fires: ResMut<Fires>,
    mut rng: ResMut<Rng>,
    plants: Query<(Entity, &GlobalTransform), With<Flammable>>,
    mut decals: ResMut<crate::particles::Decals>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 || fires.burning.is_empty() {
        return;
    }
    let rain = weather.storm();
    for b in fires.burning.values_mut() {
        b.age += dt * (1.0 + rain * 1.5);
    }
    fires.next_spread -= dt;
    if fires.next_spread > 0.0 {
        return;
    }
    fires.next_spread = 0.4;
    // Fuel around the fire's extent.
    let (sum, n) = fires.burning.iter().fold((Vec3::ZERO, 0.0), |(s, n), (c, b)| (s + cell_centre(*c, b.y), n + 1.0));
    let centre = sum / n;
    let extent = fires.burning.keys().map(|c| cell_centre(*c, centre.y).distance(centre)).fold(0.0f32, f32::max) + 8.0;
    let fuel = fuel_near(&plants, centre, extent);
    let wind = weather.wind();
    let wind_dir = wind.normalize_or_zero();
    let mut new = Vec::new();
    let mut done = Vec::new();
    for (c, b) in &fires.burning {
        if b.age >= b.life {
            done.push(*c);
            continue;
        }
        if b.age < 1.2 {
            continue;
        }
        for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
            let n = (c.0 + dx, c.1 + dz);
            let Some(f) = fuel.get(&n) else { continue };
            let dir = Vec3::new(dx as f32, 0.0, dz as f32).normalize();
            let downwind = (1.0 + dir.dot(wind_dir) * (wind.length() / 6.0).min(1.5)).max(0.1);
            let p = 0.16 * downwind * (1.0 - 0.9 * rain);
            if rng.f32() < p {
                new.push((n, f[0].1));
            }
        }
    }
    for (c, y) in new {
        fires.light(c, y, &mut rng);
    }
    for c in done {
        let Some(b) = fires.burning.remove(&c) else { continue };
        fires.burnt.insert(c);
        if let Some(f) = fuel.get(&c) {
            for (e, _) in f {
                commands.entity(*e).despawn();
            }
            decals.add_tinted(crate::particles::DecalSheet::Sand, cell_centre(c, b.y), Vec3::Y, CELL * 1.4, rng.f32(), 0.12);
        }
    }
}

/// Flames, smoke and embers over burning cells near the camera.
fn burn_effects(
    time: Res<Time>,
    fires: Res<Fires>,
    mut rng: ResMut<Rng>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut particles: ResMut<Particles>,
) {
    let dt = time.delta_secs();
    let Ok(cam) = cameras.single() else { return };
    let c = cam.translation();
    for (cell, b) in &fires.burning {
        let at = cell_centre(*cell, b.y);
        if at.distance(c) > 150.0 {
            continue;
        }
        // Builds up, then dies down.
        let t = b.age / b.life;
        let strength = (t * 6.0).min(1.0) * (1.0 - crate::sky::smoothstep(0.7, 1.0, t));
        let jitter = Vec3::new(rng.range(-0.8, 0.8), 0.0, rng.range(-0.8, 0.8));
        if rng.f32() < dt * 5.0 * strength {
            let mut f = Particle::new(Sheet::Fire, at + jitter + Vec3::Y * 0.5, rng.range(0.6, 1.0), rng.range(1.2, 2.0) * strength.max(0.4));
            f.vel = Vec3::Y * rng.range(0.6, 1.2);
            f.frame = crate::particles::Frame::Loop(24.0, (rng.f32() * 64.0) as u32);
            f.fade_in = 0.15;
            f.colour = Vec4::new(1.0, 0.38, 0.08, 0.85);
            particles.spawn(f);
        }
        if rng.f32() < dt * 0.6 * strength {
            let mut s = Particle::new(Sheet::Smoke, at + jitter + Vec3::Y * 1.5, rng.range(5.0, 8.0), 1.5);
            s.vel = Vec3::Y * rng.range(1.0, 1.8) + Vec3::new(0.6, 0.0, 0.4);
            s.grow = 4.0;
            s.rot = rng.range(0.0, std::f32::consts::TAU);
            s.spin = rng.range(-0.1, 0.1);
            s.frame = crate::particles::Frame::Loop(6.0, (rng.f32() * 30.0) as u32);
            s.fade_in = 0.2;
            s.colour = Vec4::new(0.3, 0.29, 0.28, 0.4);
            particles.spawn(s);
        }
        if rng.f32() < dt * 2.0 * strength {
            let mut e = Particle::new(Sheet::Sparks, at + jitter + Vec3::Y * 0.5, rng.range(0.8, 1.6), 0.04);
            e.vel = Vec3::Y * rng.range(1.5, 3.0) + Vec3::new(rng.range(-0.4, 0.4), 0.0, rng.range(-0.4, 0.4));
            e.stretch = 0.02;
            e.frame = crate::particles::Frame::Fixed((rng.f32() * 4.0) as u32);
            e.colour = Vec4::new(1.0, 0.5, 0.15, 1.2);
            particles.spawn(e);
        }
    }
}

/// Anything standing in a burning cell takes fire damage.
fn burn_damage(time: Res<Time>, mut fires: ResMut<Fires>, targets: Query<(Entity, &GlobalTransform, &Health)>, mut damage: MessageWriter<DamageEvent>) {
    if fires.burning.is_empty() {
        return;
    }
    fires.next_damage -= time.delta_secs();
    if fires.next_damage > 0.0 {
        return;
    }
    fires.next_damage = 0.25;
    for (e, gt, h) in &targets {
        let p = gt.translation();
        if h.dead {
            continue;
        }
        if let Some(b) = fires.burning.get(&cell_of(p)) {
            if (p.y - b.y).abs() < 2.5 && b.age > 0.3 && b.age < b.life * 0.9 {
                damage.write(DamageEvent {
                    target: e,
                    amount: 7.0,
                    kind: DamageKind::Fire,
                    source: Some(p),
                    instigator: fires.instigator,
                    headshot: false,
                    hit: crate::combat::Hit::Whole,
                });
            }
        }
    }
}

/// A few flickering lights over the biggest patches of fire.
fn fire_lights(time: Res<Time>, mut fires: ResMut<Fires>, mut lights: Query<(&mut PointLight, &mut Transform, &mut Visibility)>, mut commands: Commands) {
    const LIGHTS: usize = 4;
    if fires.lights.is_empty() {
        fires.lights = (0..LIGHTS)
            .map(|_| {
                commands
                    .spawn((
                        PointLight { color: Color::srgb(1.0, 0.55, 0.25), intensity: 0.0, range: 25.0, radius: 1.0, ..default() },
                        Transform::default(),
                        Visibility::Hidden,
                    ))
                    .id()
            })
            .collect();
        return;
    }
    // Cluster burning cells on a coarse grid and light the largest clusters.
    let mut clusters: HashMap<(i32, i32), (Vec3, f32)> = HashMap::new();
    for (c, b) in &fires.burning {
        let entry = clusters.entry((c.0.div_euclid(6), c.1.div_euclid(6))).or_default();
        entry.0 += cell_centre(*c, b.y);
        entry.1 += 1.0;
    }
    let mut list: Vec<(Vec3, f32)> = clusters.into_values().map(|(s, n)| (s / n, n)).collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1));
    let t = time.elapsed_secs();
    for (i, e) in fires.lights.iter().enumerate() {
        let Ok((mut light, mut tr, mut vis)) = lights.get_mut(*e) else { continue };
        match list.get(i) {
            Some((at, n)) => {
                let flicker = 0.8 + 0.2 * (t * 11.0 + i as f32).sin() * (t * 4.3 + i as f32 * 2.0).sin();
                light.intensity = 25_000.0 * n.min(12.0) * flicker;
                tr.translation = *at + Vec3::Y * 2.0;
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}
