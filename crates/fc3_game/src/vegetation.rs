//! Streams baked vegetation (trees, grass, ground clutter) from the cache.
//!
//! Each sector's `.fcv` loads as one asset. Instances are split into a near
//! layer (grass, small radius) and a far layer (trees and clutter), spawned
//! and despawned independently as the player moves. Same mesh + material
//! entities are batched by Bevy's automatic instancing.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::render::render_resource::ShaderType;
use bevy::render::storage::ShaderBuffer;
use fc3_formats::cache::{self, veg_kind, CachedVegetation, VegInstance};
use fc3_formats::terrain::SectorCoord;

use crate::cache_assets::{cache_path, Fc3Model};
use crate::data::{to_dunia, CacheInfo};
use crate::objects::WorldObject;
use crate::terrain::StreamingAnchor;

/// Trees and clutter load within this many sectors; grass within `GRASS_RADIUS`.
const FAR_RADIUS: i32 = 5;

/// How many sectors round the anchor have their trees loaded as models
/// (beyond, `vista` stands in their billboards).
pub fn far_radius(settings: &crate::settings::Settings) -> i32 {
    (FAR_RADIUS as f32 * settings.distance_scale()).round() as i32
}
const GRASS_RADIUS: i32 = 2;
const TREE_DRAW: Range<f32> = 0.0..360.0;

/// How far grass and small clutter show at a vegetation quality (High: 75
/// and 180 m).
fn reach(q: crate::settings::Quality) -> (f32, f32) {
    use crate::settings::Quality;
    match q {
        Quality::Low => (40.0, 110.0),
        Quality::Medium => (55.0, 145.0),
        Quality::High => (75.0, 180.0),
        _ => (100.0, 240.0),
    }
}
/// Vegetation instances spawned per frame (bounded to avoid hitches: 1,200
/// made about 500 entities in a frame, which held the main thread up for
/// 15 to 20 ms); a layer that doesn't fit continues on the next frame.
const INSTANCE_BUDGET: usize = 500;

pub struct VegetationPlugin;

impl Plugin for VegetationPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "foliage_wind.wgsl");
        bevy::asset::embedded_asset!(app, "foliage_sway.wgsl");
        bevy::asset::embedded_asset!(app, "foliage_prepass.wgsl");
        let pushers = app.world_mut().resource_mut::<Assets<ShaderBuffer>>().add(ShaderBuffer::from(Pushers::default()));
        let _ = PUSHERS.set(pushers);
        // The sway module stays loaded for the foliage shaders to import.
        let sway: Handle<Shader> = app
            .world()
            .resource::<AssetServer>()
            .load(bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("foliage_sway.wgsl")).with_source("embedded"));
        app.insert_resource(SwayShader(sway));
        app.init_asset::<Fc3Vegetation>()
            .register_asset_loader(VegetationLoader)
            .add_plugins(MaterialPlugin::<FoliageMaterial>::default())
            .init_resource::<VegStreamer>()
            .init_resource::<FoliageCache>()
            .init_resource::<Concealment>()
            .add_systems(Update, (conceal, concealment_test).chain().run_if(resource_exists::<CacheInfo>))
            .add_systems(Update, (request_vegetation, spawn_vegetation).chain().in_set(VegStreaming).run_if(resource_exists::<CacheInfo>))
            .add_systems(Update, follow_wind)
            .add_systems(PostUpdate, push_plants.after(TransformSystems::Propagate));
    }
}

/// What grass and bushes part around: the feet (xyz) and reach (w, 0 for
/// none) of the player and the nearest people, animals and vehicles.
#[derive(Clone, Copy, Default, ShaderType)]
struct Pushers {
    items: [Vec4; MAX_PUSHERS],
}

const MAX_PUSHERS: usize = 8;
/// Only those this near the camera part the plants.
const PUSH_RANGE: f32 = 50.0;

/// Shared by every foliage material, rewritten each frame.
static PUSHERS: std::sync::OnceLock<Handle<ShaderBuffer>> = std::sync::OnceLock::new();

#[allow(clippy::type_complexity)]
fn push_plants(
    cams: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    players: Query<(&Transform, &crate::player::Player)>,
    npcs: Query<&Transform, (With<crate::npc::Npc>, Without<crate::player::Player>)>,
    animals: Query<&Transform, (With<crate::wildlife::WildAnimal>, Without<crate::player::Player>)>,
    vehicles: Query<&Transform, (With<crate::vehicles::Vehicle>, Without<crate::player::Player>)>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut list: Local<Vec<(f32, Vec4)>>,
) {
    let (Ok(cam), Some(handle)) = (cams.single(), PUSHERS.get()) else { return };
    let eye = cam.translation();
    list.clear();
    let mut add = |feet: Vec3, reach: f32| {
        let d = feet.distance(eye);
        if d < PUSH_RANGE {
            list.push((d, feet.extend(reach)));
        }
    };
    let half = crate::player::HALF_HEIGHT;
    for (t, p) in &players {
        if !p.driving {
            add(t.translation - Vec3::Y * half, 0.8);
        }
    }
    for t in &npcs {
        add(t.translation - Vec3::Y * half, 0.7);
    }
    for t in &animals {
        add(t.translation - Vec3::Y * 0.6, 0.9);
    }
    for t in &vehicles {
        add(t.translation, 1.8);
    }
    list.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut p = Pushers::default();
    for (slot, (_, v)) in p.items.iter_mut().zip(list.iter()) {
        *slot = *v;
    }
    if let Some(mut buffer) = buffers.get_mut(handle) {
        buffer.set_data(p);
    }
}

/// Points every foliage material's sway along the weather's wind, stronger in
/// storms (only when it has shifted, as each change re-uploads the materials).
fn follow_wind(weather: Res<crate::clouds::Weather>, mut last: Local<Vec4>, mut materials: ResMut<Assets<FoliageMaterial>>) {
    let wind = weather.wind();
    let speed = wind.length();
    let dir = Vec2::new(wind.x, wind.z).normalize_or(Vec2::new(0.8, 0.6));
    let air = Vec4::new(dir.x, dir.y, (0.4 + speed / 5.0).min(3.0), 0.0);
    if (air - *last).abs().max_element() < 0.05 {
        return;
    }
    *last = air;
    for (_, m) in materials.iter_mut() {
        m.extension.air = air;
    }
}

/// Vegetation streaming systems.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct VegStreaming;

/// Grass and tree materials with wind sway (`foliage_wind.wgsl`).
pub type FoliageMaterial = bevy::pbr::ExtendedMaterial<StandardMaterial, Wind>;

/// Keeps the sway module (`foliage_sway.wgsl`) loaded for the shaders that import it.
#[derive(Resource)]
#[allow(dead_code)]
struct SwayShader(Handle<Shader>);

/// Wind response: x sway per metre of height, y height where the bend
/// saturates, z leaf flutter, w speed. Plants whose bend saturates low (grass
/// and bushes) also part around passers-by.
#[derive(Asset, bevy::render::render_resource::AsBindGroup, TypePath, Debug, Clone, Default)]
pub struct Wind {
    #[uniform(100)]
    pub params: Vec4,
    /// Direction (xy, Bevy x/z) and strength (z) of the weather's wind.
    #[uniform(100)]
    pub air: Vec4,
    /// Sunlight through the leaves (the world's vegetation SSS): rgb its
    /// colour times its backlight strength, w 1 when on.
    #[uniform(100)]
    pub sss: Vec4,
    /// The passers-by (see `push_plants`).
    #[storage(101, read_only)]
    pub pushers: Handle<ShaderBuffer>,
    /// The world's horizon atlas, for shadows of distant land (`horizon`).
    #[texture(102)]
    #[sampler(103)]
    pub horizon: Handle<Image>,
}

impl bevy::pbr::MaterialExtension for Wind {
    fn vertex_shader() -> bevy::shader::ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("foliage_wind.wgsl")).with_source("embedded").into()
    }

    fn fragment_shader() -> bevy::shader::ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("foliage_wind.wgsl")).with_source("embedded").into()
    }

    // The depth, normal and shadow passes sway the same way.
    fn prepass_vertex_shader() -> bevy::shader::ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("foliage_prepass.wgsl")).with_source("embedded").into()
    }
}

const GRASS_WIND: Vec4 = Vec4::new(0.12, 1.0, 0.015, 1.0);
const TREE_WIND: Vec4 = Vec4::new(0.012, 15.0, 0.03, 0.6);
/// Bushes and ferns (larger plant meshes sway like trees).
const BUSH_WIND: Vec4 = Vec4::new(0.05, 1.5, 0.02, 0.9);
const BUSH_RADIUS: f32 = 2.5;

/// One wind material per source material and wind response.
#[derive(Resource, Default)]
pub struct FoliageCache(HashMap<(AssetId<StandardMaterial>, [u32; 4]), Handle<FoliageMaterial>>);

#[derive(Asset, TypePath)]
pub struct Fc3Vegetation {
    pub models: Vec<Handle<Fc3Model>>,
    pub instances: Vec<VegInstance>,
}

#[derive(Default, TypePath)]
pub struct VegetationLoader;

impl AssetLoader for VegetationLoader {
    type Asset = Fc3Vegetation;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<Fc3Vegetation, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let v = CachedVegetation::from_bytes(&bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
        let models = v.models.iter().map(|m| ctx.load(cache_path(m))).collect();
        Ok(Fc3Vegetation { models, instances: v.instances })
    }

    fn extensions(&self) -> &[&str] {
        &["fcv"]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Layer {
    Far,
    Grass,
}

struct Slot {
    handle: Handle<Fc3Vegetation>,
    roots: HashMap<Layer, Entity>,
    wanted: HashSet<Layer>,
    /// Next instance index to spawn per layer (absent once complete).
    cursor: HashMap<Layer, usize>,
    /// When the asset came in (see `cache_assets::ready`).
    since: Option<f32>,
}

/// How hidden the player is in grass and bushes: 0 in the open, 1 deep in
/// cover as tall as they are (crouched or standing).
#[derive(Resource, Default)]
pub struct Concealment {
    pub level: f32,
    next: f32,
    debug: (usize, usize, usize, f32),
}

/// Plants within this distance of the player hide them; each patch as tall
/// as their eyes hides this much of what's still showing.
const CONCEAL_RADIUS: f32 = 1.4;
const CONCEAL_PER_PATCH: f32 = 0.55;

/// Measures the cover round the player a few times a second: grass and
/// bushes at least as tall as their eyes (crouched or standing) count fully,
/// lower ones in part.
fn conceal(
    time: Res<Time>,
    mut cover: ResMut<Concealment>,
    streamer: Res<VegStreamer>,
    vegs: Res<Assets<Fc3Vegetation>>,
    models: Res<Assets<Fc3Model>>,
    players: Query<(&Transform, &crate::player::Player)>,
) {
    if time.elapsed_secs() < cover.next {
        return;
    }
    cover.next = time.elapsed_secs() + 0.2;
    let Ok((pt, p)) = players.single() else { return };
    let feet = pt.translation - Vec3::Y * crate::player::HALF_HEIGHT;
    let d = crate::data::to_dunia(pt.translation);
    let c = SectorCoord::containing(d[0], d[1]);
    // The eye's height above the feet decides how tall cover must be.
    let eye = p.eye.max(0.5);
    let mut open_view = 1.0f32;
    let mut tallest = 0.0f32;
    let (mut seen, mut near, mut with_model) = (0, 0, 0);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let Some(veg) = streamer.slots.get(&SectorCoord::new(c.x + dx, c.y + dy)).and_then(|s| vegs.get(&s.handle)) else { continue };
            seen += veg.instances.len();
            for inst in &veg.instances {
                if inst.kind == veg_kind::TREE {
                    continue;
                }
                let at = Vec3::from_array(inst.translation);
                let flat = Vec2::new(at.x - feet.x, at.z - feet.z).length();
                // Cheap reject before the model's footprint is looked up.
                if flat > CONCEAL_RADIUS + 6.0 || (at.y - feet.y).abs() > 2.5 {
                    continue;
                }
                let Some(model) = veg.models.get(inst.model as usize).and_then(|h| models.get(h)) else { continue };
                // A clump covers its footprint, not just its centre.
                let reach = ((model.aabb_max.x - model.aabb_min.x).max(model.aabb_max.z - model.aabb_min.z) * 0.5 * inst.scale).min(6.0);
                if flat > CONCEAL_RADIUS.min(reach + 0.4) + reach * 0.5 {
                    continue;
                }
                near += 1;
                with_model += 1;
                if inst.kind == veg_kind::MESH && !model.is_vegetation {
                    continue;
                }
                let height = (model.aabb_max.y - model.aabb_min.y) * inst.scale;
                tallest = tallest.max(height);
                // Each patch as tall as the eye hides most of what's left.
                open_view *= 1.0 - CONCEAL_PER_PATCH * (height / (eye * 0.85)).clamp(0.0, 1.0);
            }
        }
    }
    cover.level = 1.0 - open_view;
    cover.debug = (seen, near, with_model, (tallest * 100.0).round() / 100.0);
}

/// `FC3_CONCEAL_TEST`: logs the player's cover each second (try with `--pos`
/// in tall grass, crouched with `FC3_CROUCH`).
fn concealment_test(time: Res<Time>, cover: Res<Concealment>, mut last: Local<f32>) {
    if std::env::var("FC3_CONCEAL_TEST").is_err() || time.elapsed_secs() - *last < 1.0 {
        return;
    }
    *last = time.elapsed_secs();
    info!("conceal-test: cover {:.2} (instances in reach {:?})", cover.level, cover.debug);
}

#[derive(Resource, Default)]
pub struct VegStreamer {
    slots: HashMap<SectorCoord, Slot>,
    center: Option<SectorCoord>,
    /// The vegetation quality the placed plants have.
    quality: Option<crate::settings::Quality>,
}

impl VegStreamer {
    pub fn pending_count(&self) -> usize {
        self.slots.values().filter(|s| !s.cursor.is_empty() || s.wanted.iter().any(|l| !s.roots.contains_key(l))).count()
    }

    /// A sector's solid clutter (rocks, trunks: colliders through the
    /// world-object path): `Err` while it is still being placed, else the
    /// root it hangs under, if the sector has any.
    pub fn solid_root(&self, c: SectorCoord) -> Result<Option<Entity>, ()> {
        let Some(s) = self.slots.get(&c).filter(|s| s.wanted.contains(&Layer::Far)) else { return Ok(None) };
        match s.roots.get(&Layer::Far) {
            Some(&root) if !s.cursor.contains_key(&Layer::Far) => Ok(Some(root)),
            _ => Err(()),
        }
    }
}

fn request_vegetation(
    mut streamer: ResMut<VegStreamer>,
    settings: Res<crate::settings::Settings>,
    info: Res<CacheInfo>,
    server: Res<AssetServer>,
    anchor: Query<&GlobalTransform, With<StreamingAnchor>>,
    mut commands: Commands,
) {
    let Ok(anchor) = anchor.single() else { return };
    let d = to_dunia(anchor.translation());
    let center = SectorCoord::containing(d[0], d[1]);
    if streamer.center == Some(center) && !settings.is_changed() {
        return;
    }
    streamer.center = Some(center);
    // A new vegetation quality places everything again, at its reach.
    if streamer.quality != Some(settings.vegetation) {
        streamer.quality = Some(settings.vegetation);
        for slot in streamer.slots.values_mut() {
            for (_, e) in slot.roots.drain() {
                crate::retire::retire(&mut commands, e);
            }
            slot.cursor.clear();
        }
    }
    let mut keep = HashSet::new();
    let far = far_radius(&settings);
    let grass = (GRASS_RADIUS as f32 * settings.distance_scale()).round().max(1.0) as i32;
    for dy in -far..=far {
        for dx in -far..=far {
            let c = SectorCoord::new(center.x + dx, center.y + dy);
            if !c.valid() || !info.vegetation.contains(&c.id()) {
                continue;
            }
            keep.insert(c);
            let dist = dx.abs().max(dy.abs());
            let slot = streamer.slots.entry(c).or_insert_with(|| Slot {
                handle: server.load(cache_path(&cache::vegetation_rel(c.id()))),
                roots: HashMap::new(),
                wanted: HashSet::new(),
                cursor: HashMap::new(),
                since: None,
            });
            slot.wanted.clear();
            slot.wanted.insert(Layer::Far);
            if dist <= grass {
                slot.wanted.insert(Layer::Grass);
            }
            // Drop layers that are no longer wanted (grass leaving its radius).
            let stale: Vec<Layer> = slot.roots.keys().copied().filter(|l| !slot.wanted.contains(l)).collect();
            for l in stale {
                if let Some(e) = slot.roots.remove(&l) {
                    crate::retire::retire(&mut commands, e);
                }
                slot.cursor.remove(&l);
            }
        }
    }
    streamer.slots.retain(|c, slot| {
        if keep.contains(c) {
            return true;
        }
        for (_, e) in slot.roots.drain() {
            crate::retire::retire(&mut commands, e);
        }
        false
    });
}

/// Grass textures are near-grey and take their colour from the ground, so
/// they are multiplied by the ground colour (brightened to offset the grey
/// texture). The colour rides on each plant's `MeshTag` (linear RGB over
/// `TINT_MAX` in the low bytes, a set top byte marking it), so every grass
/// shares its type's one material and draws in one batch.
fn grass_tint_tag(tint: [u8; 3]) -> MeshTag {
    // Take the ground's hue but not its darkness: normalise to unit luminance,
    // then blend part-way from white so already-coloured plants stay bright.
    let lin = Color::srgb_u8(tint[0], tint[1], tint[2]).to_linear();
    let lum = (0.2126 * lin.red + 0.7152 * lin.green + 0.0722 * lin.blue).max(1e-3);
    let hue = [lin.red / lum, lin.green / lum, lin.blue / lum];
    let c = hue.map(|h| ((1.0 + (h - 1.0) * 0.6) * 1.05).clamp(0.0, TINT_MAX));
    let byte = |v: f32| (v / TINT_MAX * 255.0).round() as u32;
    MeshTag(0xFF00_0000 | byte(c[2]) << 16 | byte(c[1]) << 8 | byte(c[0]))
}

/// The brightest a grass tint channel goes (see `foliage_wind.wgsl`).
const TINT_MAX: f32 = 1.6;

fn draw_range(kind: u8, model: &Fc3Model, quality: crate::settings::Quality) -> Range<f32> {
    let (grass, clutter) = reach(quality);
    match kind {
        veg_kind::GRASS => 0.0..grass,
        veg_kind::TREE => TREE_DRAW,
        // Big clutter (boulders) stays visible further than pebbles.
        _ if model.radius() > 3.0 => TREE_DRAW,
        _ => 0.0..clutter,
    }
}

fn spawn_vegetation(
    time: Res<Time>,
    mut streamer: ResMut<VegStreamer>,
    server: Res<AssetServer>,
    vegs: Res<Assets<Fc3Vegetation>>,
    models: Res<Assets<Fc3Model>>,
    mut commands: Commands,
    materials: Res<Assets<StandardMaterial>>,
    mut foliage_materials: ResMut<Assets<FoliageMaterial>>,
    mut foliage_cache: ResMut<FoliageCache>,
    env: Res<crate::sky::GameEnvironment>,
) {
    // Light through leaves, from the world's render settings (FC3_NO_SSS turns it off).
    let sss = env
        .0
        .as_ref()
        .and_then(|e| e.veg_sss)
        .filter(|_| std::env::var("FC3_NO_SSS").is_err())
        .map_or(Vec4::ZERO, |v| Vec4::new(v[0] * v[6], v[1] * v[6], v[2] * v[6], 1.0));
    let mut budget = INSTANCE_BUDGET;
    let quality = streamer.quality.unwrap_or(crate::settings::Quality::High);
    let skip = std::env::var("FC3_NO_VEG").ok();
    if skip.as_deref() == Some("all") {
        return;
    }
    for (c, slot) in streamer.slots.iter_mut() {
        if budget == 0 {
            break;
        }
        // Layers not started yet, plus layers part-way through spawning.
        let mut work: Vec<Layer> = slot.wanted.iter().copied().filter(|l| !slot.roots.contains_key(l)).collect();
        work.extend(slot.cursor.keys().copied().filter(|l| slot.roots.contains_key(l)));
        if work.is_empty() || !crate::cache_assets::ready(&server, &slot.handle, &mut slot.since, time.elapsed_secs()) {
            continue;
        }
        let Some(veg) = vegs.get(&slot.handle) else { continue };
        // As with objects: every model done (not their textures).
        if !veg.models.iter().all(|h| crate::cache_assets::done(&server, h)) {
            continue;
        }
        for layer in work {
            if budget == 0 {
                break;
            }
            let root = *slot
                .roots
                .entry(layer)
                .or_insert_with(|| commands.spawn((Transform::IDENTITY, Visibility::Visible, Name::new(format!("vegetation {} {layer:?}", c.id())))).id());
            let start = slot.cursor.get(&layer).copied().unwrap_or(0);
            let mut next = veg.instances.len();
            for (idx, inst) in veg.instances.iter().enumerate().skip(start) {
                if budget == 0 {
                    next = idx;
                    break;
                }
                budget -= 1;
                let is_grass = inst.kind == veg_kind::GRASS;
                if is_grass != (layer == Layer::Grass) {
                    continue;
                }
                if skip.as_deref() == Some(["grass", "mesh", "tree"][(inst.kind == veg_kind::MESH) as usize + 2 * (inst.kind == veg_kind::TREE) as usize]) {
                    continue;
                }
                let Some(handle) = veg.models.get(inst.model as usize) else { continue };
                let Some(model) = models.get(handle) else { continue };
                let Some(lod) = model.lods.first() else { continue };
                let range = draw_range(inst.kind, model, quality);
                let fade = (range.end * 0.08).max(2.0);
                let vis = VisibilityRange { start_margin: 0.0..0.0, end_margin: (range.end - fade)..range.end, use_aabb: false };
                let mut obj = commands.spawn((
                    Transform { translation: Vec3::from_array(inst.translation), rotation: Quat::from_array(inst.rotation), scale: Vec3::splat(inst.scale) },
                    Visibility::Visible,
                    ChildOf(root),
                ));
                // Rocks and other solid clutter get colliders through the world-object
                // path; so do tree trunks.
                let solid = inst.kind == veg_kind::MESH && !model.is_vegetation;
                let trunk = inst.kind == veg_kind::TREE && model.collision.is_some();
                if solid || trunk {
                    obj.insert(WorldObject { model: handle.clone(), entity_index: u32::MAX });
                    if std::env::var_os("FC3_COLLISION_DEBUG").is_some() {
                        obj.insert(Name::new(format!("vegetation instance {idx} model {:?} at {:?}", handle.path(), inst.translation)));
                    }
                }
                // Grass and bushes burn (see `fire`); a bush with its own flames.
                if is_grass || (inst.kind == veg_kind::MESH && model.is_vegetation) {
                    obj.insert(crate::fire::Flammable);
                }
                if !is_grass && inst.kind == veg_kind::MESH && model.is_vegetation {
                    obj.insert(crate::fire::Bush { height: (model.aabb_max.y - model.aabb_min.y) * inst.scale });
                }
                let obj = obj.id();
                // RealTree models carry several detail levels; everything else draws LOD 0.
                let lods: Vec<(&crate::cache_assets::ModelLod, VisibilityRange)> = if inst.kind == veg_kind::TREE && model.lods.len() > 1 {
                    model
                        .lods
                        .iter()
                        .enumerate()
                        .filter_map(|(li, l)| {
                            let start = if li == 0 { 0.0 } else { l.distance };
                            let end = model.lods.get(li + 1).map_or(range.end, |n| n.distance).min(range.end);
                            if start >= end {
                                return None;
                            }
                            let f = (end * 0.08).clamp(1.0, fade);
                            let start_margin = if li == 0 { 0.0..0.0 } else { (start - f * 0.5)..(start + f * 0.5) };
                            let end_margin = if end >= range.end { vis.end_margin.clone() } else { (end - f * 0.5)..(end + f * 0.5) };
                            Some((l, VisibilityRange { start_margin, end_margin, use_aabb: false }))
                        })
                        .collect()
                } else {
                    vec![(lod, vis.clone())]
                };
                // Grass, bushes and trees sway in the wind; rocks and other clutter don't.
                let wind = if is_grass {
                    Some(GRASS_WIND)
                } else if inst.kind == veg_kind::TREE {
                    Some(TREE_WIND)
                } else if inst.kind == veg_kind::MESH && model.is_vegetation {
                    Some(if model.radius() < BUSH_RADIUS { BUSH_WIND } else { TREE_WIND })
                } else {
                    None
                };
                for (lod, vis) in lods {
                    for part in &lod.parts {
                        crate::camera::SPAWNED_VEGETATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let material = part.material.clone();
                        let mut e = match wind {
                            Some(w) => {
                                let foliage = foliage_cache
                                    .0
                                    .entry((material.id(), w.to_array().map(f32::to_bits)))
                                    .or_insert_with(|| {
                                        let mut base = materials.get(&material).cloned().unwrap_or_default();
                                        // Grass takes its whole colour from its tint (`grass_tint_tag`).
                                        if is_grass {
                                            base.base_color = Color::WHITE.with_alpha(base.base_color.alpha());
                                        }
                                        // How much light passes through this plant's leaves (the
                                        // converter's, carried in `diffuse_transmission`; drawn
                                        // here as the game's vegetation SSS, not Bevy's).
                                        let through = std::mem::take(&mut base.diffuse_transmission);
                                        let sss = if through > 0.0 { (sss.truncate() * through).extend(sss.w) } else { Vec4::ZERO };
                                        let pushers = PUSHERS.get().cloned().unwrap_or_default();
                                        let horizon = crate::horizon::atlas();
                                        foliage_materials.add(FoliageMaterial {
                                            base,
                                            extension: Wind { params: w, air: Vec4::new(0.8, 0.6, 1.0, 0.0), sss, pushers, horizon },
                                        })
                                    })
                                    .clone();
                                commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(foliage), vis.clone(), ChildOf(obj)))
                            }
                            None => {
                                let mut e = commands.spawn((Mesh3d(part.mesh.clone()), MeshMaterial3d(material), vis.clone(), ChildOf(obj)));
                                // Rocks and other solid clutter draw with the
                                // game's own shaders near the eye, as placed
                                // props do (their detail maps: plain, a
                                // boulder up close was a smear).
                                if solid && std::env::var_os("FC3_NO_CLUTTER_ORIGINAL").is_none() {
                                    e.insert(crate::original_shaders::PropPart);
                                }
                                e
                            }
                        };
                        if is_grass {
                            e.insert(grass_tint_tag(inst.tint));
                        }
                        if is_grass || model.radius() < 1.0 {
                            e.insert(NotShadowCaster);
                        } else {
                            e.insert(crate::shadow_lod::ShadowSize(model.radius() * inst.scale));
                        }
                    }
                }
            }
            if next >= veg.instances.len() {
                slot.cursor.remove(&layer);
            } else {
                slot.cursor.insert(layer, next);
            }
        }
    }
}
