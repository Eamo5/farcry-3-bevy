//! The ocean: a camera-centred radial grid displaced by Gerstner swell, shaded
//! by `water.wgsl` with the game's water normal maps and foam texture.
//!
//! Water depth (for colour, opacity, shoreline foam and calmer shallows) comes
//! from a sea-floor texture built from the loaded terrain around the player,
//! rebuilt as the player moves; it also holds the ground's height, over which
//! the shader marches reflected rays to mirror the shore. The water draws in
//! the transmissive pass, so it sees a copy of the scene behind it: bent by the
//! ripples beneath, and sampled where those rays land for the reflections.
//! Below the surface the camera gets underwater fog.
//!
//! Inland water: rivers and lakes above the sea are flat planes, one per
//! sector whose header carries a water level (`world/water_planes.txt`), drawn
//! with the same shader, calmer and murkier, over a depth texture of their own.
//! `surface` / `surface_at` give the water's height anywhere (sea or inland)
//! for swimming, boats, splashes and the underwater checks.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use bevy::tasks::{futures::check_ready, AsyncComputeTaskPool, Task};
use fc3_formats::cache;
use fc3_formats::terrain::SectorCoord;
use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock, RwLock};

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;
use crate::player::PlayerCamera;
use crate::sky::SEA_LEVEL;
use crate::terrain::Heightfield;

pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExt>;

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "water.wgsl");
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default()).add_systems(
            Update,
            (setup_water.run_if(resource_exists::<CacheInfo>), water_quality, follow_camera, update_floor, inland_water, update_fog, caustic_light),
        )
        .add_systems(Update, (story_levels, follow_levels).chain().after(crate::story::StorySet).before(inland_water));
    }
}

/// Sea-floor texture: 4 m per texel over 2 km around the player.
const FLOOR_TEXELS: u32 = 512;
const FLOOR_SIZE: f32 = 2048.0;

#[derive(ShaderType, Clone, Copy, Debug, Default)]
pub struct WaterParams {
    pub floor: Vec4,
    /// x: swell scale, y: 1 for inland water (murky, no crest foam), z: how
    /// strong the sun's caustics on the bed are (0 at night or under cloud).
    pub shape: Vec4,
}

/// Inland water levels by sector id, read once from the cache.
static PLANES: OnceLock<HashMap<u32, f32>> = OnceLock::new();

/// Water levels the story's scripts have set or are moving, by sector id
/// (`CDominoWaterLevelManager`: BAM_020's sinking ship, BAM_030's flooding
/// tunnel, BAM_055's sub), over the world's own.
static SCRIPTED: LazyLock<RwLock<HashMap<u32, f32>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// The sea's level, as a script has moved it (`SetWorldOceanLevel`: DEN_060
/// drains it 20 m through its vision of the island), else the world's.
static OCEAN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(SEA_LEVEL.to_bits());

pub fn ocean_level() -> f32 {
    f32::from_bits(OCEAN.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn set_ocean_level(level: f32) {
    OCEAN.store(level.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// Puts the scripts' water levels over the world's (all of them: one not
/// given is the world's again).
pub fn set_scripted(levels: &[(u32, f32)]) {
    let mut s = SCRIPTED.write().unwrap_or_else(|e| e.into_inner());
    if s.len() != levels.len() || levels.iter().any(|(id, l)| s.get(id) != Some(l)) {
        *s = levels.iter().copied().collect();
    }
}

fn scripted(id: u32) -> Option<f32> {
    SCRIPTED.read().unwrap_or_else(|e| e.into_inner()).get(&id).copied()
}

/// A sector's water level (a script's, else the world's), if it has one.
fn sector_level(id: u32) -> Option<f32> {
    scripted(id).or_else(|| PLANES.get().and_then(|m| m.get(&id)).copied())
}

/// The water plane over `p`'s sector: a river's or lake's level (or as a
/// script has it), or the sea's.
pub fn plane_level(p: Vec3) -> f32 {
    sector_level(sector_of(p)).unwrap_or_else(ocean_level)
}

fn sector_of(p: Vec3) -> u32 {
    let d = crate::data::to_dunia(p);
    SectorCoord::containing(d[0], d[1]).id()
}

/// The height of the water's surface over `p`, given the ground there: an
/// inland plane's level where the ground lies below it (not over a cave
/// mouth), otherwise the sea's. A level a script sets holds where there is
/// no ground either (the missions' rooms placed out past the islands).
pub fn surface(p: Vec3, ground: Option<f32>) -> f32 {
    let id = sector_of(p);
    let sea = ocean_level();
    let (level, script) = match scripted(id) {
        Some(l) => (l, true),
        None => (PLANES.get().and_then(|m| m.get(&id)).copied().unwrap_or(sea), false),
    };
    if level > sea && ground.map_or(script, |g| g < level) {
        level
    } else {
        sea
    }
}

pub fn surface_at(p: Vec3, heights: &Heightfield) -> f32 {
    surface(p, heights.height_at(p))
}

/// Surface water occupies exposed basins, not the space beneath dry terrain.
/// Keep the numerical surface API for buoyancy/geometry; occupancy checks
/// additionally reject the implicit ocean beneath inland caves.
pub fn submerged(p: Vec3, heights: &Heightfield, depth: f32) -> bool {
    let ground = heights.height_at(p);
    submerged_at_level(p.y, surface(p, ground), ground, depth)
}

fn submerged_at_level(y: f32, level: f32, ground: Option<f32>, depth: f32) -> bool {
    y < level - depth && ground.is_none_or(|g| g < level)
}

#[cfg(test)]
mod occupancy_tests {
    use super::*;
    #[test]
    fn dry_cave_does_not_inherit_the_ocean_under_the_terrain() {
        assert!(!submerged_at_level(94.0, 100.0, Some(146.0), 1.3));
        assert!(!submerged_at_level(95.6, 100.0, Some(146.0), 0.05));
        assert!(submerged_at_level(94.0, 100.0, Some(90.0), 1.3));
        assert!(submerged_at_level(122.0, 125.0, Some(120.0), 1.3));
        assert!(!submerged_at_level(99.0, 100.0, Some(90.0), 1.3));
        assert!(submerged_at_level(94.0, 100.0, None, 1.3));
    }
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct WaterExt {
    #[uniform(100)]
    pub params: WaterParams,
    #[texture(101)]
    #[sampler(102)]
    pub ripples: Handle<Image>,
    #[texture(103)]
    pub swell: Handle<Image>,
    #[texture(104)]
    #[sampler(105)]
    pub floor: Handle<Image>,
    #[texture(106)]
    pub foam: Handle<Image>,
}

impl MaterialExtension for WaterExt {
    fn vertex_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("water.wgsl")).with_source("embedded").into()
    }

    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("water.wgsl")).with_source("embedded").into()
    }
}

#[derive(Component)]
pub struct Ocean;

/// An inland water plane.
#[derive(Component)]
pub struct InlandWater;

/// Inland planes come into view within this distance of the camera and go
/// beyond the second; their depth texture covers `FLOOR_SIZE` at 2 m a texel.
const INLAND_SHOW: f32 = 700.0;
const INLAND_HIDE: f32 = 900.0;
const INLAND_TEXELS: u32 = 1024;
/// Sectors round each shown plane whose ground goes into its texture (for
/// reflecting the banks).
const INLAND_REFLECT: i32 = 1;
/// Inland water's swell (a ripple on the ocean's).
const INLAND_SWELL: f32 = 0.05;

#[derive(Resource)]
struct Inland {
    material: Handle<WaterMaterial>,
    floor: Handle<Image>,
    mesh: Handle<Mesh>,
    shown: HashMap<u32, Entity>,
    corner: Vec2,
    built_sectors: usize,
    /// The scripts' levels the depth texture was built for.
    built_levels: HashMap<u32, f32>,
    /// The depth texture being built in the background (corner, texels).
    task: Option<Task<(Vec2, Vec<u8>)>>,
}

#[derive(Resource)]
struct Water {
    material: Handle<WaterMaterial>,
    floor: Handle<Image>,
    /// World XZ of the floor texture's corner.
    corner: Vec2,
    built_sectors: usize,
    /// The sea's level its depths were taken at.
    built_level: f32,
    /// The floor texture being built in the background (corner, texels).
    task: Option<Task<(Vec2, Vec<u8>)>>,
}

/// Concentric rings, dense near the centre (a few cm) and reaching ~12 km.
fn radial_grid(rings: usize, segments: usize) -> Mesh {
    let mut positions = vec![[0.0, 0.0, 0.0]];
    for r in 1..=rings {
        let radius = 1.065f32.powi(r as i32) - 1.0;
        for s in 0..segments {
            let a = s as f32 / segments as f32 * std::f32::consts::TAU;
            positions.push([radius * a.cos(), 0.0, radius * a.sin()]);
        }
    }
    let at = |ring: usize, seg: usize| (1 + (ring - 1) * segments + seg % segments) as u32;
    let mut indices = Vec::new();
    for s in 0..segments {
        indices.extend_from_slice(&[0, at(1, s + 1), at(1, s)]);
    }
    for r in 1..rings {
        for s in 0..segments {
            let (a, b, c, d) = (at(r, s), at(r, s + 1), at(r + 1, s + 1), at(r + 1, s));
            indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    let uvs: Vec<[f32; 2]> = positions.iter().map(|p| [p[0], p[2]]).collect();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// One sector's plane: 64 m square from the origin, 8 m cells.
fn sector_grid() -> Mesh {
    const CELLS: usize = 8;
    let size = fc3_formats::terrain::SECTOR_SIZE;
    let mut positions = Vec::new();
    for z in 0..=CELLS {
        for x in 0..=CELLS {
            positions.push([x as f32 * size / CELLS as f32, 0.0, z as f32 * size / CELLS as f32]);
        }
    }
    let at = |x: usize, z: usize| (z * (CELLS + 1) + x) as u32;
    let mut indices = Vec::new();
    for z in 0..CELLS {
        for x in 0..CELLS {
            let (a, b, c, d) = (at(x, z), at(x + 1, z), at(x + 1, z + 1), at(x, z + 1));
            indices.extend_from_slice(&[a, c, b, a, d, c]);
        }
    }
    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    let uvs: Vec<[f32; 2]> = positions.iter().map(|p| [p[0], p[2]]).collect();
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Ground below the depth textures where it isn't known (nothing to reflect).
const NO_GROUND: f32 = -1000.0;

/// A texel of a depth texture: metres of water (r) and the ground's height (g).
fn texel(depth: f32, ground: f32) -> [u8; 4] {
    let (d, g) = (half::f16::from_f32(depth).to_le_bytes(), half::f16::from_f32(ground).to_le_bytes());
    [d[0], d[1], g[0], g[1]]
}

/// A depth texture, all `depth` deep over unknown ground until built.
fn depth_image(texels: u32, depth: f32) -> Image {
    let mut floor = Image::new_fill(
        Extent3d { width: texels, height: texels, depth_or_array_layers: 1 },
        TextureDimension::D2,
        &texel(depth, NO_GROUND),
        TextureFormat::Rg16Float,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    floor.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    floor
}

fn load_planes(info: &CacheInfo) {
    let text = std::fs::read_to_string(info.root.join(cache::WATER_PLANES_REL)).unwrap_or_default();
    let planes: HashMap<u32, f32> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect();
    info!("inland water: {} sectors", planes.len());
    let _ = PLANES.set(planes);
}

fn load(server: &AssetServer, rel: &str, srgb: bool) -> Handle<Image> {
    server
        .load_builder()
        .with_settings(move |s: &mut ImageLoaderSettings| {
            s.is_srgb = srgb;
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                mipmap_filter: ImageFilterMode::Linear,
                anisotropy_clamp: 8,
                ..default()
            });
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(cache_path(rel))
}

fn setup_water(
    mut done: Local<bool>,
    info: Res<CacheInfo>,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut commands: Commands,
) {
    if *done {
        return;
    }
    *done = true;
    load_planes(&info);
    let floor = images.add(depth_image(FLOOR_TEXELS, 64.0));
    let (ripples, swell, foam) =
        (load(&server, cache::WATER_RIPPLES_REL, false), load(&server, cache::WATER_SWELL_REL, false), load(&server, cache::WATER_FOAM_REL, false));
    // Opaque, but transmissive: drawn after the scene is copied, which the
    // shader blends in itself (see `water.wgsl`).
    let base = StandardMaterial {
        base_color: Color::srgba(0.02, 0.16, 0.22, 0.9),
        perceptual_roughness: 0.06,
        reflectance: 0.35,
        specular_transmission: 0.01,
        double_sided: true,
        cull_mode: None,
        ..default()
    };
    let material = materials.add(WaterMaterial {
        base: base.clone(),
        extension: WaterExt {
            params: WaterParams { floor: Vec4::new(0.0, 0.0, FLOOR_SIZE, SEA_LEVEL), shape: Vec4::new(1.0, 0.0, 0.0, 0.0) },
            ripples: ripples.clone(),
            swell: swell.clone(),
            floor: floor.clone(),
            foam: foam.clone(),
        },
    });
    let inland_floor = images.add(depth_image(INLAND_TEXELS, 0.0));
    let inland = materials.add(WaterMaterial {
        base,
        extension: WaterExt {
            params: WaterParams { floor: Vec4::new(0.0, 0.0, FLOOR_SIZE, SEA_LEVEL), shape: Vec4::new(INLAND_SWELL, 1.0, 0.0, 0.0) },
            ripples,
            swell,
            floor: inland_floor.clone(),
            foam,
        },
    });
    commands.insert_resource(Inland {
        material: inland,
        floor: inland_floor,
        mesh: meshes.add(sector_grid()),
        shown: HashMap::new(),
        corner: Vec2::splat(f32::MAX),
        built_sectors: 0,
        built_levels: HashMap::new(),
        task: None,
    });
    // (High's grid; `water_quality` puts the setting's on.)
    let (rings, segments) = ocean_grid(crate::settings::Quality::High);
    commands.spawn((
        Ocean,
        Mesh3d(meshes.add(radial_grid(rings, segments))),
        MeshMaterial3d(material.clone()),
        Transform::from_xyz(0.0, SEA_LEVEL, 0.0),
        bevy::light::NotShadowCaster,
        bevy::camera::visibility::NoFrustumCulling,
        Name::new("ocean"),
    ));
    commands.insert_resource(Water { material, floor, corner: Vec2::splat(f32::MAX), built_sectors: 0, built_level: SEA_LEVEL, task: None });
}

/// The ocean grid's rings and segments at a water quality (High: 150 by
/// 160).
fn ocean_grid(quality: crate::settings::Quality) -> (usize, usize) {
    use crate::settings::Quality;
    match quality {
        Quality::Low => (80, 96),
        Quality::Medium => (110, 128),
        Quality::High => (150, 160),
        _ => (200, 220),
    }
}

/// Puts the water quality's grid on the ocean.
fn water_quality(
    settings: Res<crate::settings::Settings>,
    mut shown: Local<Option<crate::settings::Quality>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut ocean: Query<&mut Mesh3d, With<Ocean>>,
) {
    let Ok(mut mesh) = ocean.single_mut() else { return };
    if *shown == Some(settings.water) {
        return;
    }
    if shown.is_some() || settings.water != crate::settings::Quality::High {
        let (rings, segments) = ocean_grid(settings.water);
        mesh.0 = meshes.add(radial_grid(rings, segments));
    }
    *shown = Some(settings.water);
}

fn follow_camera(cameras: Query<&GlobalTransform, With<PlayerCamera>>, mut ocean: Query<&mut Transform, With<Ocean>>) {
    let (Ok(c), Ok(mut o)) = (cameras.single(), ocean.single_mut()) else { return };
    let p = c.translation();
    // Snap so the grid doesn't slide over the swell.
    o.translation.x = (p.x / 2.0).round() * 2.0;
    o.translation.z = (p.z / 2.0).round() * 2.0;
    // (`FC3_OCEAN_TEST=<level>`, tests without the story: the sea there.)
    if let Some(level) = std::env::var("FC3_OCEAN_TEST").ok().and_then(|l| l.parse::<f32>().ok()) {
        set_ocean_level(level);
    }
    o.translation.y = ocean_level();
}

/// Puts a finished depth texture in place: its texels, and its corner on the
/// material.
fn put_floor(
    images: &mut Assets<Image>,
    materials: &mut Assets<WaterMaterial>,
    image: &Handle<Image>,
    material: &Handle<WaterMaterial>,
    corner: Vec2,
    data: Vec<u8>,
) {
    if let Some(mut image) = images.get_mut(image) {
        image.data = Some(data);
    }
    if let Some(mut m) = materials.get_mut(material) {
        m.extension.params.floor = Vec4::new(corner.x, corner.y, FLOOR_SIZE, ocean_level());
    }
}

/// Rebuilds the sea-floor depth texture when the player moves on or more
/// terrain loads (in the background: it is a quarter of a million height
/// lookups).
fn update_floor(
    time: Res<Time<Real>>,
    mut next_check: Local<f32>,
    water: Option<ResMut<Water>>,
    heights: Res<Heightfield>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
) {
    let Some(mut water) = water else { return };
    if let Some(task) = water.task.as_mut() {
        let Some((corner, data)) = check_ready(task) else { return };
        water.task = None;
        put_floor(&mut images, &mut materials, &water.floor, &water.material, corner, data);
        return;
    }
    if time.elapsed_secs() < *next_check {
        return;
    }
    *next_check = time.elapsed_secs() + 1.0;
    let Ok(cam) = cameras.single() else { return };
    let p = cam.translation();
    let corner = Vec2::new(((p.x - FLOOR_SIZE / 2.0) / 64.0).round() * 64.0, ((p.z - FLOOR_SIZE / 2.0) / 64.0).round() * 64.0);
    let moved = corner.distance(water.corner) > 256.0;
    let more = heights.sectors.len() != water.built_sectors;
    // (Again as a script moves the sea: its depths with it.)
    let sea = ocean_level();
    let risen = (sea - water.built_level).abs() > 0.5;
    if !moved && !more && !risen {
        return;
    }
    water.built_level = sea;
    if moved {
        water.corner = corner;
    } else {
        // Newly streamed terrain: refresh, but not every second.
        *next_check = time.elapsed_secs() + 4.0;
    }
    let corner = water.corner;
    water.built_sectors = heights.sectors.len();
    let heights = heights.clone();
    water.task = Some(AsyncComputeTaskPool::get().spawn(async move {
        let n = FLOOR_TEXELS as usize;
        let step = FLOOR_SIZE / FLOOR_TEXELS as f32;
        let mut data = Vec::with_capacity(n * n * 4);
        for y in 0..n {
            for x in 0..n {
                let at = Vec3::new(corner.x + (x as f32 + 0.5) * step, 0.0, corner.y + (y as f32 + 0.5) * step);
                data.extend_from_slice(&match heights.height_at(at) {
                    Some(h) => texel((sea - h).clamp(0.0, 64.0), h),
                    None => texel(64.0, NO_GROUND),
                });
            }
        }
        (corner, data)
    }));
}

/// Shows the inland planes near the camera (once their terrain has loaded)
/// and rebuilds their depth texture as the player moves or terrain streams in.
#[allow(clippy::too_many_arguments)]
fn inland_water(
    time: Res<Time<Real>>,
    mut next_check: Local<f32>,
    inland: Option<ResMut<Inland>>,
    heights: Res<Heightfield>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut commands: Commands,
) {
    let (Some(mut inland), Some(planes)) = (inland, PLANES.get()) else { return };
    if let Some(task) = inland.task.as_mut() {
        if let Some((corner, data)) = check_ready(task) {
            inland.task = None;
            put_floor(&mut images, &mut materials, &inland.floor, &inland.material, corner, data);
        }
    }
    if time.elapsed_secs() < *next_check {
        return;
    }
    *next_check = time.elapsed_secs() + 0.5;
    let Ok(cam) = cameras.single() else { return };
    let p = cam.translation();
    let size = fc3_formats::terrain::SECTOR_SIZE;
    // A sector's corner in Bevy space (Dunia y runs along -Z).
    let corner_of = |c: SectorCoord| {
        let o = c.origin();
        Vec2::new(o[0], -o[1] - size)
    };
    // The world's planes, and the scripts' over them.
    let scripts: HashMap<u32, f32> = SCRIPTED.read().unwrap_or_else(|e| e.into_inner()).clone();
    let mut levels = planes.clone();
    levels.extend(scripts.iter().map(|(id, l)| (*id, *l)));
    // (A plane whose level is gone: a script's, reloaded away.)
    inland.shown.retain(|id, e| {
        let keep = levels.contains_key(id);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });
    for (&id, &level) in &levels {
        let c = SectorCoord::from_id(id);
        let corner = corner_of(c);
        let d = (corner + Vec2::splat(size * 0.5)).distance(Vec2::new(p.x, p.z));
        // (A script's water stands where there's no terrain too.)
        let loaded = heights.sectors.contains_key(&c) || scripts.contains_key(&id);
        match inland.shown.get(&id).copied() {
            Some(e) if d > INLAND_HIDE || !loaded => {
                commands.entity(e).despawn();
                inland.shown.remove(&id);
            }
            None if d < INLAND_SHOW && loaded => {
                let e = commands
                    .spawn((
                        InlandWater,
                        Mesh3d(inland.mesh.clone()),
                        MeshMaterial3d(inland.material.clone()),
                        Transform::from_xyz(corner.x, level, corner.y),
                        bevy::light::NotShadowCaster,
                        Name::new("inland water"),
                    ))
                    .id();
                inland.shown.insert(id, e);
            }
            _ => {}
        }
    }
    // The depth texture (built in the background): over the shown planes,
    // metres below their level; and round them, the ground's height for the
    // reflections.
    let corner = Vec2::new(((p.x - FLOOR_SIZE / 2.0) / size).round() * size, ((p.z - FLOOR_SIZE / 2.0) / size).round() * size);
    let moved = corner.distance(inland.corner) > 256.0;
    // (Rising or falling water deepens or shallows its texture.)
    let risen = scripts.len() != inland.built_levels.len()
        || scripts.iter().any(|(id, l)| inland.built_levels.get(id).is_none_or(|b| (b - l).abs() > LEVEL_REBUILD));
    if inland.task.is_some() || (!moved && !risen && heights.sectors.len() == inland.built_sectors) {
        return;
    }
    inland.corner = corner;
    inland.built_sectors = heights.sectors.len();
    inland.built_levels = scripts;
    let around: std::collections::HashSet<SectorCoord> = inland
        .shown
        .keys()
        .flat_map(|&id| {
            let c = SectorCoord::from_id(id);
            (-INLAND_REFLECT..=INLAND_REFLECT).flat_map(move |dy| (-INLAND_REFLECT..=INLAND_REFLECT).map(move |dx| SectorCoord::new(c.x + dx, c.y + dy)))
        })
        .collect();
    let heights = heights.clone();
    inland.task = Some(AsyncComputeTaskPool::get().spawn(async move { (corner, inland_texels(&heights, &levels, &around, corner)) }));
}

/// How far (m) a script's water moves before its depth texture is built
/// again.
const LEVEL_REBUILD: f32 = 0.25;

/// Keeps the scripts' water planes at their levels as they rise and fall
/// (each frame; `inland_water` shows and hides them).
fn follow_levels(inland: Option<Res<Inland>>, mut planes: Query<&mut Transform, With<InlandWater>>) {
    let Some(inland) = inland else { return };
    let scripts = SCRIPTED.read().unwrap_or_else(|e| e.into_inner());
    if scripts.is_empty() {
        return;
    }
    for (id, e) in &inland.shown {
        if let (Some(level), Ok(mut t)) = (scripts.get(id), planes.get_mut(*e)) {
            if t.translation.y != *level {
                t.translation.y = *level;
            }
        }
    }
}

/// Puts the story's water levels over the world's.
fn story_levels(
    time: Res<Time>,
    mut tested: Local<bool>,
    host: Option<NonSend<crate::story::StoryHost>>,
    players: Query<&Transform, With<crate::player::Player>>,
) {
    let Some(host) = host else {
        set_scripted(&[]);
        return;
    };
    // FC3_WATER_TEST=<s>,<rise m>,<m/s> (tests): that far in, a script fills
    // the water of the sectors round the player that much higher.
    if let Some(v) = std::env::var("FC3_WATER_TEST").ok().filter(|_| !*tested) {
        let v: Vec<f32> = v.split(',').filter_map(|s| s.parse().ok()).collect();
        if let (Some(&at), Ok(p)) = (v.first(), players.single()) {
            if time.elapsed_secs() >= at {
                *tested = true;
                let c = SectorCoord::from_id(sector_of(p.translation));
                let (rise, speed) = (v.get(1).copied().unwrap_or(3.0), v.get(2).copied().unwrap_or(0.5));
                for (dx, dy) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))) {
                    let id = SectorCoord::new(c.x + dx, c.y + dy).id();
                    let Some(level) = sector_level(id) else { continue };
                    let to = level + rise;
                    info!("water test: filling sector {id} from {level:.1} to {to:.1} at {speed} m/s");
                    let lua = format!("CDominoWaterLevelManager_GetInstance():FillWater('fc3_main', {id}, {speed}, {to}, nil, nil)");
                    if let Err(e) = host.story.domino().lua().load(lua).exec() {
                        warn!("water test: {e}");
                    }
                }
            }
        }
    }
    set_scripted(&host.story.water_levels());
}

/// The inland depth texture's texels (see `inland_water`).
fn inland_texels(heights: &Heightfield, planes: &HashMap<u32, f32>, around: &std::collections::HashSet<SectorCoord>, corner: Vec2) -> Vec<u8> {
    let size = fc3_formats::terrain::SECTOR_SIZE;
    let corner_of = |c: SectorCoord| {
        let o = c.origin();
        Vec2::new(o[0], -o[1] - size)
    };
    let n = INLAND_TEXELS as i32;
    let step = FLOOR_SIZE / INLAND_TEXELS as f32;
    let blank = texel(0.0, NO_GROUND);
    let mut data: Vec<u8> = blank.iter().copied().cycle().take((n * n * 4) as usize).collect();
    for &c in around {
        let level = planes.get(&c.id()).copied();
        let sc = corner_of(c);
        let x0 = ((sc.x - corner.x) / step).floor() as i32;
        let y0 = ((sc.y - corner.y) / step).floor() as i32;
        let cells = (size / step) as i32;
        for y in y0.max(0)..(y0 + cells).min(n) {
            for x in x0.max(0)..(x0 + cells).min(n) {
                let at = Vec3::new(corner.x + (x as f32 + 0.5) * step, 0.0, corner.y + (y as f32 + 0.5) * step);
                // Over a cave mouth: no water.
                let h = heights.height_at(at);
                let depth = match (level, h) {
                    (Some(level), Some(h)) => (level - h).clamp(0.0, 64.0),
                    _ => 0.0,
                };
                let i = ((y * n + x) * 4) as usize;
                data[i..i + 4].copy_from_slice(&texel(depth, h.unwrap_or(NO_GROUND)));
            }
        }
    }
    data
}

/// Sets how bright the caustics on the bed are: with the sun up and the sky
/// clear (fainter in the murkier rivers), none at the lowest water quality.
fn caustic_light(
    settings: Res<crate::settings::Settings>,
    tod: Res<crate::sky::TimeOfDay>,
    weather: Res<crate::clouds::Weather>,
    water: Option<Res<Water>>,
    inland: Option<Res<Inland>>,
    mut last: Local<f32>,
    mut materials: ResMut<Assets<WaterMaterial>>,
) {
    let k = crate::sky::smoothstep(0.05, 0.4, tod.sun_dir().y) * (1.0 - weather.overcast());
    let k = if settings.water == crate::settings::Quality::Low { 0.0 } else { k };
    let (Some(water), Some(inland)) = (water, inland) else { return };
    if (k - *last).abs() < 0.02 {
        return;
    }
    *last = k;
    for (handle, scale) in [(Some(water.material.clone()), 1.0), (Some(inland.material.clone()), 0.5)] {
        if let Some(mut m) = handle.and_then(|h| materials.get_mut(&h)) {
            m.extension.params.shape.z = k * scale;
        }
    }
}

/// How much of the game's own fog is added over the atmosphere's haze.
const FOG_SHARE: f32 = 0.4;

/// Fog: thick and blue-green underwater (murkier in rivers), a grey haze in
/// storms, otherwise the world's own haze from its environment presets: from
/// `fog_start` it thickens linearly to `fog_end`, where it stops at the
/// amount its density gives over that stretch (about two thirds by day,
/// more at night), tinted like the sky and glowing toward the sun.
/// `FC3_NO_FOG=1` (debug) leaves clear weather to the atmosphere alone.
#[allow(clippy::too_many_arguments)]
fn update_fog(
    mut commands: Commands,
    heights: Res<Heightfield>,
    weather: Res<crate::clouds::Weather>,
    daylight: Res<crate::clouds::Daylight>,
    (tod, env): (Res<crate::sky::TimeOfDay>, Res<crate::sky::GameEnvironment>),
    (scene, indoors): (Res<crate::sky::SceneExposure>, Res<crate::sky::Indoors>),
    suns: Query<&DirectionalLight, With<crate::sky::Sun>>,
    mut cameras: Query<(Entity, &GlobalTransform, Option<&mut DistanceFog>), With<PlayerCamera>>,
) {
    let Ok((e, gt, fog)) = cameras.single_mut() else { return };
    let storm = weather.storm();
    let eye = gt.translation();
    let water = surface_at(eye, &heights);
    let wanted = if submerged(eye, &heights, 0.05) {
        if water > ocean_level() {
            Some((Color::srgb(0.05, 0.12, 0.08), FogFalloff::Exponential { density: 0.16 }))
        } else {
            Some((Color::srgb(0.03, 0.17, 0.2), FogFalloff::Exponential { density: 0.09 }))
        }
    } else if let Some((e, sky)) = env
        .0
        .as_ref()
        .filter(|e| storm > 0.02 && e.storm_fog_end > 0.0)
        .and_then(|e| crate::clouds::storm_sky(e, tod.hours / 24.0, &scene, weather.exposure_shift() + 0.8 * indoors.0).map(|s| (e, s)))
    {
        // The storm presets' fog: thick, taking part of the storm sky's
        // (white) colour, as much of it as of the clear fog given the
        // atmosphere's own haze.
        let density = e.storm_fog_density.at(tod.hours / 24.0) * FOG_SHARE * storm;
        let v = sky * e.storm_fog_sky;
        Some((Color::linear_rgb(v, v, v), FogFalloff::Exponential { density }))
    } else if storm > 0.02 {
        // Roughly the storm sky's brightness at the horizon, dimming at night.
        let v = 0.2 * daylight.0;
        Some((Color::srgb(v, v * 1.04, v * 1.1), FogFalloff::Exponential { density: 0.0025 * storm }))
    } else if let Some(e) = env.0.as_ref().filter(|e| e.fog_end > e.fog_start && std::env::var("FC3_NO_FOG").is_err()) {
        let density = e.fog_density.at(tod.hours / 24.0);
        // The game's fog is all its aerial perspective; here the atmosphere
        // already gives some, so only part of it is added.
        // Thinner from high up (the air above the haze).
        let height = 1.0 - 0.7 * crate::sky::smoothstep(120.0, 450.0, eye.y - SEA_LEVEL);
        let amount = (1.0 - (-density * (e.fog_end - e.fog_start)).exp()) * FOG_SHARE * height;
        // The sky's colour at the horizon (the game's fog takes the sky's
        // colour), or its brightness on screen, dimming at night.
        let factor = scene.sky_factor(weather.exposure_shift() + 0.8 * indoors.0);
        let colour = match crate::clouds::horizon_colour(e, tod.hours / 24.0, factor) {
            Some(h) => Color::linear_rgba(h.x, h.y, h.z, amount),
            None => {
                let v = 0.06 + 0.6 * daylight.0;
                Color::srgba(v * 0.92, v * 0.97, v * 1.05, amount)
            }
        };
        Some((colour, FogFalloff::Linear { start: e.fog_start, end: e.fog_end }))
    } else {
        None
    };
    let glow = suns.single().map(|l| l.color.with_alpha(0.35 * daylight.0)).unwrap_or(Color::NONE);
    match (wanted, fog) {
        (Some((color, falloff)), Some(mut fog)) => {
            fog.color = color;
            fog.falloff = falloff;
            fog.directional_light_color = glow;
        }
        (Some((color, falloff)), None) => {
            commands.entity(e).insert(DistanceFog { color, falloff, directional_light_color: glow, directional_light_exponent: 16.0 });
        }
        (None, Some(_)) => {
            commands.entity(e).remove::<DistanceFog>();
        }
        (None, None) => {}
    }
}
