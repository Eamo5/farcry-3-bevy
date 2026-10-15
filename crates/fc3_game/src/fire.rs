//! Grass fires. Molotovs, the flamethrower and explosions set the ground
//! alight; fire spreads cell by cell through grass and bushes, faster
//! downwind, burns them away, scorches the ground and burns anything standing
//! in it. Rain damps it down.
//!
//! A burnable region (`CBurnableRegion`: a field of crops a mission wants
//! burnt) catches when fire reaches it and burns across its whole shape;
//! it tells the story it caught (`FireStarted`), that most of it has burnt
//! (`AlmostBurnedOut`, past [`REGION_BURNT`]) and that its fire went out
//! (`FireStopped`).

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::combat::{DamageEvent, DamageKind, Health};
use crate::particles::{Particle, Particles, Sheet};
use crate::player::PlayerCamera;
use crate::weapons::Rng;

pub struct FirePlugin;

impl Plugin for FirePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Ignite>()
            .init_resource::<Fires>()
            .init_resource::<Regions>()
            .add_systems(Update, (ignite, spread, burn_regions, burn_effects, burn_damage, fire_lights).chain())
            .add_systems(
                PreUpdate,
                autopilot_burn
                    .after(crate::controls::update_actions)
                    .before(crate::controls::apply_action_maps)
                    .run_if(in_state(crate::menu::GameState::Playing)),
            );
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

/// The share of a burnable region burnt when it reports it has almost
/// burnt out.
const REGION_BURNT: f32 = 0.8;
/// Chance a burning cell of a region lights each unburnt neighbour of the
/// region, each spread step (crops catch faster than grass).
const REGION_SPREAD: f32 = 0.35;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RegionState {
    Unlit,
    Burning,
    AlmostOut,
    Out,
}

/// A burnable region: its fire cells, those burnt, and how far it has got.
struct Region {
    id: u64,
    cells: Vec<Cell>,
    burnt: HashSet<Cell>,
    state: RegionState,
    /// Its centre (Bevy space, at its own height).
    centre: Vec3,
}

/// The story's burnable regions, read once its data is in.
#[derive(Resource, Default)]
pub struct Regions {
    list: Option<Vec<Region>>,
    next: f32,
}

impl Regions {
    /// Where to set a region alight while it has yet to catch: its cell
    /// nearest its centre (the centre of a bent field may lie outside it).
    pub fn unlit(&self, id: u64) -> Option<Vec3> {
        let r = self.list.as_ref()?.iter().find(|r| r.id == id && r.state == RegionState::Unlit)?;
        r.cells.iter().map(|&c| cell_centre(c, r.centre.y)).min_by(|a, b| a.distance_squared(r.centre).total_cmp(&b.distance_squared(r.centre)))
    }
}

fn read_regions(data: &fc3_story::data::StoryData) -> Vec<Region> {
    use fc3_story::triggers::{shape_of, Shape};
    let mut out = Vec::new();
    for e in data.entities.values().filter(|e| e.class == "CBurnableRegion" || e.has("CIgnitorComponent")) {
        let Some(shape @ Shape::Prism { .. }) = shape_of(e) else { continue };
        let Shape::Prism { points, .. } = &shape else { continue };
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in points {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        // Fire cells are on Bevy's x and z (Dunia's x and -y).
        let (c0, c1) = (cell_of(crate::data::to_bevy([lo[0], hi[1], 0.0])), cell_of(crate::data::to_bevy([hi[0], lo[1], 0.0])));
        let mut cells = Vec::new();
        for x in c0.0..=c1.0 {
            for z in c0.1..=c1.1 {
                let at = crate::data::to_dunia(cell_centre((x, z), 0.0));
                if shape.contains([at[0], at[1], e.pos[2]]) {
                    cells.push((x, z));
                }
            }
        }
        if cells.is_empty() {
            continue;
        }
        let centre = crate::data::to_bevy([(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, e.pos[2]]);
        out.push(Region { id: e.id, cells, burnt: HashSet::new(), state: RegionState::Unlit, centre });
    }
    out
}

/// Burnable regions catch from the fire around them, burn across their
/// shape and tell the story how far they have got.
fn burn_regions(
    time: Res<Time>,
    mut regions: ResMut<Regions>,
    mut fires: ResMut<Fires>,
    mut rng: ResMut<Rng>,
    heights: Res<crate::terrain::Heightfield>,
    mut decals: ResMut<crate::particles::Decals>,
    mut host: Option<NonSendMut<crate::story::StoryHost>>,
) {
    let Some(host) = host.as_mut() else { return };
    if regions.list.is_none() {
        let list = read_regions(&host.story.data());
        debug!("fire: {} burnable regions", list.len());
        regions.list = Some(list);
    }
    regions.next -= time.delta_secs();
    if regions.next > 0.0 {
        return;
    }
    regions.next = 0.4;
    let Some(list) = regions.list.as_mut() else { return };
    for r in list.iter_mut().filter(|r| r.state != RegionState::Out) {
        let burning: Vec<Cell> = r.cells.iter().copied().filter(|c| fires.burning.contains_key(c)).collect();
        // Cells whose fire has gone out are burnt (and scorched).
        let out: Vec<Cell> = r.cells.iter().copied().filter(|c| fires.burnt.contains(c) && !r.burnt.contains(c)).collect();
        for c in out {
            r.burnt.insert(c);
            let y = heights.height_at(cell_centre(c, 0.0)).unwrap_or(r.centre.y);
            decals.add_tinted(crate::particles::DecalSheet::Sand, cell_centre(c, y), Vec3::Y, CELL * 1.4, rng.f32(), 0.12);
        }
        if r.state == RegionState::Unlit {
            if burning.is_empty() {
                continue;
            }
            debug!("fire: region {} caught", r.id);
            r.state = RegionState::Burning;
            host.story.entity_event(r.id, "FireStarted", vec![]);
        }
        // The crops carry it across the region.
        for c in &burning {
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let n = (c.0 + dx, c.1 + dz);
                if !r.burnt.contains(&n) && !fires.burning.contains_key(&n) && r.cells.contains(&n) && rng.f32() < REGION_SPREAD {
                    let y = heights.height_at(cell_centre(n, 0.0)).unwrap_or(r.centre.y);
                    fires.light(n, y, &mut rng);
                }
            }
        }
        let share = r.burnt.len() as f32 / r.cells.len() as f32;
        if r.state == RegionState::Burning && share >= REGION_BURNT {
            r.state = RegionState::AlmostOut;
            host.story.entity_event(r.id, "AlmostBurnedOut", vec![]);
        }
        if burning.is_empty() {
            // Gone out: done if most of it burnt, else it can catch again.
            debug!("fire: region {} out, {:.0}% burnt", r.id, share * 100.0);
            host.story.entity_event(r.id, "FireStopped", vec![]);
            r.state = if r.state == RegionState::AlmostOut { RegionState::Out } else { RegionState::Unlit };
        }
    }
}

/// Tests: how long before the autopilot tries again a region that didn't catch.
const BURN_AGAIN: f32 = 120.0;

/// Tests (`FC3_STORY_AUTOPILOT`): a burnable region a script waits to catch
/// (a drug field), once the story has gone quiet: the player set down a few
/// metres off it, facing it, burns it with the flamethrower, or throws a
/// molotov into it. Not with nothing to burn it with, and one that didn't
/// catch is left a while (HOY_010's weed, burnt for a buzz, drew the player
/// away from Sam every few seconds).
#[allow(clippy::too_many_arguments)]
fn autopilot_burn(
    host: Option<NonSendMut<crate::story::StoryHost>>,
    time: Res<Time>,
    regions: Res<Regions>,
    heights: Res<crate::terrain::Heightfield>,
    mut actions: ResMut<crate::controls::ActionState>,
    mut players: Query<(&mut Transform, &mut crate::player::Player, &crate::weapons::Inventory)>,
    mut burning: Local<Option<(u64, f32)>>,
    mut tried: Local<std::collections::HashMap<u64, f32>>,
) {
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() {
        return;
    }
    let Some(mut host) = host else { return };
    let Ok((mut t, mut p, inventory)) = players.single_mut() else { return };
    let now = time.elapsed_secs();
    if let Some((id, left)) = burning.as_mut() {
        if regions.unlit(*id).is_none() || *left <= 0.0 {
            if regions.unlit(*id).is_some() {
                tried.insert(*id, now);
            }
            *burning = None;
            return;
        }
        if p.spawning {
            return;
        }
        *left -= time.delta_secs();
        let flamer = inventory.slots.iter().position(|s| inventory.defs[s.def].class == crate::weapons::WeaponClass::Flamethrower);
        if (*left * 2.0) as u32 != ((*left + time.delta_secs()) * 2.0) as u32 {
            debug!("story autopilot: burning {id}: flamethrower slot {flamer:?} (holding {}), molotovs {}", inventory.current, inventory.molotovs);
        }
        match flamer {
            Some(slot) if slot != inventory.current => actions.cycle = 1,
            Some(_) => actions.hold(crate::controls::Action::Fire),
            None if inventory.molotovs > 0 => actions.press(crate::controls::Action::Molotov),
            None => {}
        }
        return;
    }
    let can_burn = inventory.molotovs > 0 || inventory.slots.iter().any(|s| inventory.defs[s.def].class == crate::weapons::WeaponClass::Flamethrower);
    if host.quiet() < 6.0 || !can_burn {
        return;
    }
    let awaited = host.story.awaited_entity_events("FireStarted");
    let Some((id, centre)) = awaited.iter().filter(|id| tried.get(*id).is_none_or(|at| now - at > BURN_AGAIN)).find_map(|&id| Some((id, regions.unlit(id)?)))
    else {
        return;
    };
    // A few metres off it, on the player's side, looking down at it.
    let side = (t.translation - centre).with_y(0.0).normalize_or(Vec3::Z);
    let mut feet = centre + side * 3.0;
    feet.y = heights.height_at(feet).unwrap_or(centre.y);
    info!("story autopilot: burn region {id}");
    crate::story::teleport(&mut t, &mut p, crate::data::to_dunia(feet), 0.0);
    let ground = centre.with_y(heights.height_at(centre).unwrap_or(centre.y));
    let aim = ground - (feet + Vec3::Y * crate::player::EYE_HEIGHT);
    p.yaw = (-aim.x).atan2(-aim.z);
    p.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
    *burning = Some((id, 6.0));
    host.reset_quiet();
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

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::{fcb::Object, hash::name_hash};
    use std::sync::Arc;

    /// A burnable region's fire cells are the cells whose centres fall in
    /// its shape (Dunia's x and y onto Bevy's x and -z).
    #[test]
    fn a_burnable_region_covers_the_cells_in_its_shape() {
        let object = |name: &str, fields: Vec<(&str, Vec<u8>)>| {
            Arc::new(Object { name_hash: name_hash(name), fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(), children: vec![] })
        };
        // A 10 m square from (100, 200) in Dunia x and y.
        let mut shape = 4u32.to_le_bytes().to_vec();
        for (x, y) in [(100.0f32, 200.0f32), (110.0, 200.0), (110.0, 210.0), (100.0, 210.0)] {
            shape.extend([x, y, 0.0f32].iter().flat_map(|v| v.to_le_bytes()));
        }
        let mut data = fc3_story::data::StoryData::default();
        data.entities.insert(
            7,
            fc3_story::data::Entry {
                id: 7,
                name: "field".into(),
                class: "CBurnableRegion".into(),
                pos: [105.0, 205.0, 0.0],
                angles: [0.0; 3],
                layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
                origin: fc3_story::data::Origin::Global,
                components: Arc::new(Object { name_hash: name_hash("Components"), fields: vec![], children: vec![object("CIgnitorComponent", vec![])] }),
                fields: object("Entity", vec![("hidShapePoints", shape)]),
            },
        );
        let regions = read_regions(&data);
        assert_eq!(regions.len(), 1);
        let r = &regions[0];
        assert_eq!(r.id, 7);
        // 10 m across is five 2 m cells each way.
        assert_eq!(r.cells.len(), 25);
        assert!(r.cells.contains(&cell_of(crate::data::to_bevy([101.0, 201.0, 0.0]))));
        assert!(!r.cells.contains(&cell_of(crate::data::to_bevy([99.0, 201.0, 0.0]))));
        assert_eq!(r.state, RegionState::Unlit);
    }
}
