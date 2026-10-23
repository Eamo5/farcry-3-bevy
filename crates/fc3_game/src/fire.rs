//! Grass fires. Molotovs, the flamethrower and explosions set the ground
//! alight; fire spreads cell by cell through grass and bushes, faster
//! downwind, burns them away, scorches the ground and burns anything standing
//! in it. Rain damps it down. The flames and smoke are the game's fire
//! manager's own effects (`fire_settings`), where they're cached, and a
//! patch burns as long as the fire manager's region profile says, its smoke
//! lingering as long, and scares animals off from as far. The ground burns
//! with the profile of the logic zone painted on it (the savannah's, the
//! jungle's...); where its zone has none (roads, rivers, beaches) it doesn't
//! burn, and a splash there only flares up. A burnable region burns with
//! its own.
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
            .init_resource::<FireEffects>()
            .add_systems(Startup, (load_fire_effects, load_fire_profiles))
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

/// A bush (not grass), how tall it stands (m): it burns with its own
/// flames.
#[derive(Component)]
pub struct Bush {
    pub height: f32,
}

/// Sets fire to the ground around a point.
#[derive(Message, Clone, Copy)]
pub struct Ignite {
    pub at: Vec3,
    pub radius: f32,
    pub instigator: Option<Entity>,
    /// What the splash's own fire does each second where the ground doesn't
    /// burn (a sticky stream's `fFireDmg`); else the ground's profile's.
    pub damage: Option<f32>,
}

/// Fire grid cell size (m).
const CELL: f32 = 2.0;
/// What a patch does each second to whoever stands in it when no profile
/// says (theirs: `fFireDmg`).
const FIRE_DPS: f32 = 28.0;
/// How often (s) a patch hurts.
const DAMAGE_EVERY: f32 = 0.25;
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
    /// A splash on bare ground, burning out by itself.
    bare: bool,
    /// Its region profile (an index into [`Fires::profiles`]).
    profile: Option<usize>,
    /// What it does each second to whoever stands in it.
    damage: f32,
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
    /// The fire manager's region profiles (as cached), and the one the open
    /// world's grass burns with where no zones are painted.
    profiles: Vec<fc3_formats::fire_settings::RegionProfile>,
    grass: Option<usize>,
    /// The logic zones painted on the ground, and the profile the ground of
    /// each burns with (one not listed doesn't burn).
    zones: Option<fc3_formats::sound_regions::RegionMap>,
    zone_profiles: HashMap<u8, usize>,
}

impl Fires {
    pub fn is_burning_at(&self, p: Vec3) -> bool {
        self.burning.contains_key(&cell_of(p))
    }

    fn profile(&self, b: &Burning) -> Option<&fc3_formats::fire_settings::RegionProfile> {
        b.profile.and_then(|i| self.profiles.get(i))
    }

    /// Whether a cell's ground burns, and with which profile: its painted
    /// logic zone's (none: it doesn't), else, with no zones, the savannah's.
    fn ground(&self, c: Cell) -> Option<Option<usize>> {
        let Some(zones) = &self.zones else { return Some(self.grass) };
        let p = cell_centre(c, 0.0);
        let zone = zones.region_at(p.x, -p.z)?;
        self.zone_profiles.get(&zone).map(|&i| Some(i))
    }

    /// When a patch's flames go out: its profile's fire half died down (the
    /// effect's own flames burn down the rest), else near its end.
    fn flames_out(&self, b: &Burning) -> f32 {
        match self.profile(b) {
            Some(p) => (b.life - p.extinction * 0.5).max(b.life * 0.5),
            None => b.life * FLAMES_OUT,
        }
    }

    /// The nearest fire near enough to scare off an animal at a point (in
    /// its profile's `Fire_FearStim` radius).
    pub fn scaring(&self, p: Vec3) -> Option<Vec3> {
        self.burning
            .iter()
            .filter_map(|(c, b)| {
                let (at, r) = (cell_centre(*c, b.y), self.profile(b)?.fear);
                (at.distance_squared(p) < r * r).then_some(at)
            })
            .min_by(|a, b| a.distance_squared(p).total_cmp(&b.distance_squared(p)))
    }

    /// Sets a cell alight, to burn as long as its profile's fire grows,
    /// burns and dies down (a little more or less), or 7 to 11 s.
    fn light(&mut self, c: Cell, y: f32, profile: Option<usize>, rng: &mut Rng) {
        if self.burning.len() < MAX_BURNING && self.burnt.len() < MAX_BURNT && !self.burnt.contains(&c) {
            let life = match profile.and_then(|i| self.profiles.get(i)) {
                Some(p) => (p.starting + p.alive + p.extinction) * rng.range(0.85, 1.15),
                None => rng.range(7.0, 11.0),
            };
            let damage = profile.and_then(|i| self.profiles.get(i)).map(|p| p.damage).filter(|d| *d > 0.0).unwrap_or(FIRE_DPS);
            self.burning.entry(c).or_insert(Burning { age: 0.0, life, y, bare: false, profile, damage });
        }
    }
}

fn load_fire_profiles(mut fires: ResMut<Fires>) {
    use fc3_formats::fire_settings as fs;
    let Ok(text) = std::fs::read_to_string(fc3_formats::cache::default_cache_root().join(fs::FIRE_REGIONS_REL)) else { return };
    fires.profiles = fs::regions_from_text(&text);
    fires.grass = fires.profiles.iter().position(|p| p.name == fs::SAVANNAH);
    // The ground by its painted zones.
    let root = fc3_formats::cache::default_cache_root();
    let zones = std::fs::read_to_string(root.join(fs::FIRE_ZONES_REL)).map(|t| fs::zones_from_text(&t)).unwrap_or_default();
    let by_zone: HashMap<u8, usize> = zones.into_iter().filter_map(|(z, k)| Some((z, fires.profiles.iter().position(|p| p.key == k)?))).collect();
    let map = std::fs::read(root.join(fc3_formats::sound_regions::ZONES_REL)).ok().and_then(|b| fc3_formats::sound_regions::RegionMap::from_bytes(&b).ok());
    fires.zones = map.filter(|_| !by_zone.is_empty());
    fires.zone_profiles = by_zone;
    info!(
        "fire: {} region profiles, {} zones burn ({}), else grass burns as {:?}",
        fires.profiles.len(),
        fires.zone_profiles.len(),
        if fires.zones.is_some() { "painted" } else { "no zone map" },
        fires.grass.map(|i| &fires.profiles[i].name)
    );
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
                // The splash itself burns briefly even on bare ground, or
                // ground that doesn't burn.
                let y = fuel.get(&c).map(|v| v[0].1).unwrap_or(ground);
                let burns = fires.ground(c);
                let profile = burns.unwrap_or(fires.grass);
                fires.light(c, y, profile, &mut rng);
                if !fuel.contains_key(&c) || burns.is_none() {
                    if let Some(b) = fires.burning.get_mut(&c) {
                        b.life = b.life.min(rng.range(3.0, 5.0));
                        b.bare = true;
                        if let Some(d) = ev.damage {
                            b.damage = d;
                        }
                    }
                }
            }
        }
    }
}

/// Fire creeps into neighbouring cells that have plants on ground that
/// burns, faster downwind; burnt-out cells lose their plants.
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
    let mut kept_off = 0;
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
            let Some(profile) = fires.ground(n) else {
                kept_off += 1;
                continue;
            };
            let dir = Vec3::new(dx as f32, 0.0, dz as f32).normalize();
            let downwind = (1.0 + dir.dot(wind_dir) * (wind.length() / 6.0).min(1.5)).max(0.1);
            let p = 0.16 * downwind * (1.0 - 0.9 * rain);
            if rng.f32() < p {
                new.push((n, f[0].1, profile));
            }
        }
    }
    debug!("fire: {} burning, {} burnt, {kept_off} plant cells by it on ground that doesn't burn", fires.burning.len(), fires.burnt.len());
    for (c, y, profile) in new {
        fires.light(c, y, profile, &mut rng);
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
    /// Its region profile (its ignitor's), if cached.
    profile: Option<usize>,
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

fn read_regions(data: &fc3_story::data::StoryData, profiles: &[fc3_formats::fire_settings::RegionProfile]) -> Vec<Region> {
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
        let key = e.component("CIgnitorComponent").and_then(|c| c.u64("fireregRegionProfile_SP"));
        let profile = key.and_then(|k| profiles.iter().position(|p| p.key == k));
        out.push(Region { id: e.id, cells, burnt: HashSet::new(), state: RegionState::Unlit, centre, profile });
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
        let list = read_regions(&host.story.data(), &fires.profiles);
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
                    let profile = r.profile.or(fires.grass);
                    fires.light(n, y, profile, &mut rng);
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
/// Tests: how far (m) from a field's middle the autopilot goes back to once
/// it has set it alight, out of the fire's way.
const BURN_RETREAT: f32 = 30.0;

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
    mut burning: Local<Option<(u64, f32, Vec3)>>,
    mut tried: Local<std::collections::HashMap<u64, f32>>,
) {
    let _slow = crate::story::Slow::new("autopilot_burn");
    if std::env::var("FC3_STORY_AUTOPILOT").is_err() {
        return;
    }
    let Some(mut host) = host else { return };
    let Ok((mut t, mut p, inventory)) = players.single_mut() else { return };
    let now = time.elapsed_secs();
    host.autopilot_burning = burning.is_some();
    if let Some((id, left, from)) = burning.as_mut() {
        if regions.unlit(*id).is_none() || *left <= 0.0 {
            if regions.unlit(*id).is_some() {
                tried.insert(*id, now);
            }
            // (Back out of the crops before the fire spreads to him: it
            // burned the autopilot to death in WIL_020's fields.)
            info!("story autopilot: back from burning {id}");
            let yaw = -p.yaw.to_degrees();
            crate::story::teleport(&mut t, &mut p, crate::data::to_dunia(*from), yaw);
            *burning = None;
            host.autopilot_burning = false;
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
    if host.quiet() < 6.0 || !can_burn || host.autopilot_wrecking {
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
    // (Where to go back to after: where he came from if that was near,
    // else well off on his side of it.)
    let came = t.translation - Vec3::Y * crate::player::HALF_HEIGHT;
    let from = if came.distance(centre) < BURN_RETREAT * 2.0 {
        came
    } else {
        let off = centre + side * BURN_RETREAT;
        off.with_y(heights.height_at(off).unwrap_or(centre.y))
    };
    crate::story::teleport(&mut t, &mut p, crate::data::to_dunia(feet), 0.0);
    let ground = centre.with_y(heights.height_at(centre).unwrap_or(centre.y));
    let aim = ground - (feet + Vec3::Y * crate::player::EYE_HEIGHT);
    p.yaw = (-aim.x).atan2(-aim.z);
    p.pitch = aim.y.atan2(Vec2::new(aim.x, aim.z).length().max(0.3));
    *burning = Some((id, 6.0, from));
    host.autopilot_burning = true;
    host.reset_quiet();
}

/// Flames, smoke and embers over burning cells near the camera.
/// The fire manager's own effects (`fire_settings`, as cached), each with
/// how many may play at once: a burning patch of ground's (those that burn
/// on, and the flare that burns out by itself, for a splash on bare ground)
/// and the smoke over them. With none cached, our flames and smoke stand in.
#[derive(Resource, Default)]
struct FireEffects {
    ground: Vec<(&'static str, usize)>,
    flare: Option<(&'static str, usize)>,
    smoke: Vec<(&'static str, usize)>,
    /// A burning plant's: the small one first (a bush), then the big.
    plants: Vec<(&'static str, usize)>,
    /// The bushes burning now, and their flames.
    bushes: HashMap<Entity, (Entity, &'static str)>,
    /// What plays now: each cell's fire, and the smoke over some (with when
    /// it clears, once the cell has burnt out, and how long it may linger).
    playing: HashMap<Cell, (Entity, &'static str)>,
    smoking: HashMap<Cell, (Entity, &'static str, Option<f32>, (f32, f32))>,
}

impl FireEffects {
    /// One of a list with room left in its pool, by a cell's lot.
    fn pick(list: &[(&'static str, usize)], using: &HashMap<&'static str, usize>, lot: usize) -> Option<&'static str> {
        (0..list.len()).map(|i| list[(lot + i) % list.len()]).find(|(n, cap)| using.get(n).copied().unwrap_or(0) < *cap).map(|(n, _)| n)
    }
}

fn load_fire_effects(mut effects: ResMut<FireEffects>) {
    use fc3_formats::fire_settings as fs;
    use fc3_formats::particle_effects as pe;
    let root = fc3_formats::cache::default_cache_root();
    let Ok(text) = std::fs::read_to_string(root.join(fs::FIRE_SETTINGS_REL)) else { return };
    for f in fs::from_text(&text).into_iter().filter(|f| crate::authored_fx::available(&f.effect)) {
        let name = crate::authored_fx::intern(&f.effect);
        let pool = f.pool as usize;
        match f.kind.as_str() {
            fs::REGION => {
                // (One whose flames don't loop burns out by itself.)
                let lasting = std::fs::read_to_string(root.join(pe::authored_rel(name)))
                    .ok()
                    .and_then(|xml| pe::Effect::parse(&xml).ok())
                    .is_some_and(|e| e.emitters.iter().filter(|e| e.active && !e.texture.is_empty()).all(|e| e.looping));
                if lasting {
                    effects.ground.push((name, pool));
                } else if effects.flare.is_none() {
                    effects.flare = Some((name, pool));
                }
            }
            fs::REGION_SMOKE => effects.smoke.push((name, pool)),
            fs::REALTREE => effects.plants.push((name, pool)),
            _ => {}
        }
    }
    info!("fire effects: {} burning, flare {:?}, {} smoke", effects.ground.len(), effects.flare.map(|f| f.0), effects.smoke.len());
}

/// How far off (m) a fire's and its smoke's effects play.
const FIRE_RANGE: f32 = 150.0;
/// A bush this tall (m) or more burns with the bigger plant fire.
const BIG_BUSH: f32 = 1.6;
const SMOKE_RANGE: f32 = 250.0;
/// Without a region profile, the share of its life a patch's flames go out
/// at (the last of them burning down as it ends), and how long (s) its smoke
/// hangs on after.
const FLAMES_OUT: f32 = 0.88;
const SMOKE_LINGER: (f32, f32) = (12.0, 20.0);

/// The fire manager's effects over the burning cells: the nearest first,
/// as many as the pools allow. A patch burns with one of the lasting fires
/// (one cell in four: each is a few metres across, bigger than our cells),
/// a splash on bare ground with the flare; one cell in nine smokes, on as
/// long as its profile's smoke lasts after it's out.
fn authored_fire(
    effects: &mut FireEffects,
    fires: &Fires,
    bushes: &Query<(Entity, &GlobalTransform, &Bush)>,
    cam: Vec3,
    now: f32,
    rng: &mut Rng,
    commands: &mut Commands,
) {
    let lot = |c: Cell| (c.0.wrapping_mul(73_856_093) ^ c.1.wrapping_mul(19_349_663)).unsigned_abs() as usize;
    let mut want: Vec<(f32, Cell, bool)> = fires
        .burning
        .iter()
        .filter(|(c, b)| (b.bare || (c.0.rem_euclid(2) == 0 && c.1.rem_euclid(2) == 0)) && b.age < fires.flames_out(b))
        .map(|(c, b)| (cell_centre(*c, b.y).distance(cam), *c, b.bare))
        .filter(|(d, ..)| *d < FIRE_RANGE)
        .collect();
    want.sort_by(|a, b| a.0.total_cmp(&b.0));
    let wanted: HashSet<Cell> = want.iter().map(|w| w.1).collect();
    effects.playing.retain(|c, (e, _)| {
        let keep = wanted.contains(c);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
    let mut using: HashMap<&'static str, usize> = HashMap::new();
    for (_, n) in effects.playing.values() {
        *using.entry(n).or_default() += 1;
    }
    for (_, c, bare) in want {
        if effects.playing.contains_key(&c) {
            continue;
        }
        let flare = effects.flare.filter(|_| bare).and_then(|f| FireEffects::pick(&[f], &using, 0));
        let Some(name) = flare.or_else(|| FireEffects::pick(&effects.ground, &using, lot(c))) else { continue };
        let at = cell_centre(c, fires.burning[&c].y);
        let e = commands.spawn((Transform::from_translation(at), crate::authored_fx::AuthoredFx(name), crate::authored_fx::NoLights)).id();
        *using.entry(name).or_default() += 1;
        effects.playing.insert(c, (e, name));
    }
    // Smoke over the patches burning (and a while after).
    let mut using: HashMap<&'static str, usize> = HashMap::new();
    for (_, n, ..) in effects.smoking.values() {
        *using.entry(n).or_default() += 1;
    }
    for (c, b) in &fires.burning {
        if b.bare || c.0.rem_euclid(3) != 1 || c.1.rem_euclid(3) != 1 || effects.smoking.contains_key(c) || cell_centre(*c, b.y).distance(cam) > SMOKE_RANGE {
            continue;
        }
        let Some(name) = FireEffects::pick(&effects.smoke, &using, lot(*c)) else { break };
        let e = commands.spawn((Transform::from_translation(cell_centre(*c, b.y)), crate::authored_fx::AuthoredFx(name))).id();
        *using.entry(name).or_default() += 1;
        let linger = fires.profile(b).map(|p| p.smoke).filter(|s| s.1 > 0.0).unwrap_or(SMOKE_LINGER);
        effects.smoking.insert(*c, (e, name, None, linger));
    }
    // The bushes in burning patches, alight with the plants' fires (the
    // nearest first, as many as their pools allow) until burnt away.
    if !effects.plants.is_empty() {
        let mut alight: Vec<(f32, Entity, Vec3, f32)> = bushes
            .iter()
            .map(|(e, t, b)| (t.translation().distance(cam), e, t.translation(), b.height))
            .filter(|(d, _, p, _)| *d < FIRE_RANGE && fires.burning.get(&cell_of(*p)).is_some_and(|b| b.age < fires.flames_out(b)))
            .collect();
        alight.sort_by(|a, b| a.0.total_cmp(&b.0));
        let keep: HashSet<Entity> = alight.iter().map(|a| a.1).collect();
        effects.bushes.retain(|bush, (e, _)| {
            let on = keep.contains(bush);
            if !on {
                commands.entity(*e).try_despawn();
            }
            on
        });
        let mut using: HashMap<&'static str, usize> = HashMap::new();
        for (_, n) in effects.bushes.values() {
            *using.entry(n).or_default() += 1;
        }
        for (_, bush, at, height) in alight {
            if effects.bushes.contains_key(&bush) {
                continue;
            }
            let big = usize::from(height >= BIG_BUSH).min(effects.plants.len() - 1);
            let Some(name) = FireEffects::pick(&effects.plants, &using, big) else { continue };
            let e = commands.spawn((Transform::from_translation(at), crate::authored_fx::AuthoredFx(name), crate::authored_fx::NoLights)).id();
            *using.entry(name).or_default() += 1;
            effects.bushes.insert(bush, (e, name));
        }
    }
    effects.smoking.retain(|c, (e, _, until, linger)| {
        if !fires.burning.contains_key(c) && until.is_none() {
            *until = Some(now + rng.range(linger.0, linger.1));
        }
        let keep = until.is_none_or(|t| now < t);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
}

#[allow(clippy::too_many_arguments)]
fn burn_effects(
    time: Res<Time>,
    fires: Res<Fires>,
    mut rng: ResMut<Rng>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut particles: ResMut<Particles>,
    mut effects: ResMut<FireEffects>,
    bushes: Query<(Entity, &GlobalTransform, &Bush)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let Ok(cam) = cameras.single() else { return };
    let c = cam.translation();
    if !effects.ground.is_empty() {
        authored_fire(&mut effects, &fires, &bushes, c, time.elapsed_secs(), &mut rng, &mut commands);
        return;
    }
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

/// Anything standing in a burning cell takes fire damage: its profile's a
/// second.
fn burn_damage(time: Res<Time>, mut fires: ResMut<Fires>, targets: Query<(Entity, &GlobalTransform, &Health)>, mut damage: MessageWriter<DamageEvent>) {
    if fires.burning.is_empty() {
        return;
    }
    fires.next_damage -= time.delta_secs();
    if fires.next_damage > 0.0 {
        return;
    }
    fires.next_damage = DAMAGE_EVERY;
    for (e, gt, h) in &targets {
        let p = gt.translation();
        if h.dead {
            continue;
        }
        if let Some(b) = fires.burning.get(&cell_of(p)) {
            if (p.y - b.y).abs() < 2.5 && b.age > 0.3 && b.age < b.life * 0.9 {
                damage.write(DamageEvent {
                    target: e,
                    amount: b.damage * DAMAGE_EVERY,
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
    /// its shape (Dunia's x and y onto Bevy's x and -z), and it burns with
    /// the profile its ignitor names.
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
                components: Arc::new(Object {
                    name_hash: name_hash("Components"),
                    fields: vec![],
                    children: vec![object("CIgnitorComponent", vec![("fireregRegionProfile_SP", 0x4556_7cc9u64.to_le_bytes().to_vec())])],
                }),
                fields: object("Entity", vec![("hidShapePoints", shape)]),
            },
        );
        let profile = |key: u64| fc3_formats::fire_settings::RegionProfile { key, ..Default::default() };
        let regions = read_regions(&data, &[profile(0x47fd_f013), profile(0x4556_7cc9)]);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].profile, Some(1));
        let r = &regions[0];
        assert_eq!(r.id, 7);
        // 10 m across is five 2 m cells each way.
        assert_eq!(r.cells.len(), 25);
        assert!(r.cells.contains(&cell_of(crate::data::to_bevy([101.0, 201.0, 0.0]))));
        assert!(!r.cells.contains(&cell_of(crate::data::to_bevy([99.0, 201.0, 0.0]))));
        assert_eq!(r.state, RegionState::Unlit);
    }

    /// The ground burns with its painted zone's profile; a zone with none,
    /// or none painted (a road), doesn't burn; with no zones, the
    /// savannah's.
    #[test]
    fn the_ground_burns_as_its_zone_says() {
        use fc3_formats::sound_regions::{RegionMap, NONE, ZONE_CELLS};
        // The sector at the origin: its west half zone 20, its east none.
        let mut cells = vec![NONE; ZONE_CELLS * ZONE_CELLS];
        for y in 0..ZONE_CELLS {
            cells[y * ZONE_CELLS..y * ZONE_CELLS + 32].fill(20);
        }
        cells[ZONE_CELLS + 1] = 9;
        let zones = RegionMap::from_sectors(ZONE_CELLS, [(fc3_formats::terrain::SectorCoord::new(80, 80), cells)]);
        let mut fires = Fires { grass: Some(0), zones: Some(zones), zone_profiles: HashMap::from([(20, 1)]), ..Default::default() };
        // (Bevy z is the negated Dunia y.)
        assert_eq!(fires.ground((1, -3)), Some(Some(1)));
        assert_eq!(fires.ground((20, -3)), None);
        assert_eq!(fires.ground((0, -1)), None);
        fires.zones = None;
        assert_eq!(fires.ground((20, -3)), Some(Some(0)));
    }

    /// A patch burns as long as its profile's fire grows, burns and dies
    /// down, and scares animals off within its fear radius.
    #[test]
    fn a_patch_burns_and_scares_as_its_profile_says() {
        let savannah = fc3_formats::fire_settings::RegionProfile { starting: 5.0, alive: 5.0, extinction: 5.0, fear: 20.0, ..Default::default() };
        let mut fires = Fires { profiles: vec![savannah], grass: Some(0), ..Default::default() };
        let mut rng = Rng::seeded(7);
        fires.light((0, 0), 0.0, Some(0), &mut rng);
        let b = &fires.burning[&(0, 0)];
        assert!((12.75..=17.25).contains(&b.life), "{}", b.life);
        assert!((fires.flames_out(b) - (b.life - 2.5)).abs() < 1e-4);
        assert_eq!(fires.scaring(Vec3::new(1.0, 0.0, 15.0)), Some(cell_centre((0, 0), 0.0)));
        assert_eq!(fires.scaring(Vec3::new(1.0, 0.0, 25.0)), None);
        // Without a profile: 7 to 11 s, and it scares nothing.
        fires.light((5, 5), 0.0, None, &mut rng);
        assert!((7.0..=11.0).contains(&fires.burning[&(5, 5)].life));
        assert_eq!(fires.scaring(cell_centre((5, 5), 0.0) + Vec3::X * 10.0), None);
    }
}
