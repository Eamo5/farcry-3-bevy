//! Particles and decals drawn with the game's effect textures.
//!
//! Effects push [`Particle`]s (simulated here: motion, growth, fades and
//! flipbook frames) or one-frame sprites, and bullet holes push [`Decal`]s.
//! Each flipbook is drawn as a single mesh rebuilt every frame from its live
//! particles, camera-facing quads relative to the camera, so a whole effect
//! type costs one draw. Decals are lit quads laid on the hit surface; the
//! oldest give way once there are too many.

use std::collections::VecDeque;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;
use fc3_formats::cache::fx_rel;

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;
use crate::player::PlayerCamera;
use crate::sky::smoothstep;

pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "particles.wgsl");
        app.add_plugins(MaterialPlugin::<ParticleMaterial>::default())
            .init_resource::<Particles>()
            .init_resource::<Decals>()
            .add_systems(Update, (setup_batches.run_if(resource_exists::<CacheInfo>), simulate, fx_test))
            .add_systems(PostUpdate, build_batches.before(TransformSystems::Propagate));
    }
}

/// How a sheet's texels become colour.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Blend {
    /// Adds light (fire, flashes, sparks).
    Add,
    /// Blends by the texture's alpha (smoke).
    Alpha,
    /// Grey sheets without alpha: brightness is coverage (dust).
    Luma,
}

/// A flipbook texture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sheet {
    Fire,
    Fireball,
    ExploFire,
    Dust,
    DustBurst,
    Burst,
    MuzzleSide,
    MuzzleFront,
    Smoke,
    ExploSmoke,
    Sparks,
    Splash,
    Glow,
    Drop,
}

struct SheetDef {
    stem: &'static str,
    cols: u32,
    rows: u32,
    blend: Blend,
    /// Frame width over height.
    aspect: f32,
}

const SHEETS: [(Sheet, SheetDef); 14] = [
    (Sheet::Fire, SheetDef { stem: "fc3_gfx_fire2_16x4", cols: 16, rows: 4, blend: Blend::Add, aspect: 0.5 }),
    (Sheet::Fireball, SheetDef { stem: "fc3_gfx_fireball_5x5", cols: 5, rows: 5, blend: Blend::Add, aspect: 1.0 }),
    (Sheet::ExploFire, SheetDef { stem: "fc3_gfx_explofire_8x2", cols: 8, rows: 2, blend: Blend::Add, aspect: 1.0 }),
    (Sheet::Dust, SheetDef { stem: "fc3_gfx_impactsmoke01_4x4", cols: 4, rows: 4, blend: Blend::Luma, aspect: 0.5 }),
    (Sheet::DustBurst, SheetDef { stem: "fc3_gfx_impactsmoke06_3x4", cols: 3, rows: 4, blend: Blend::Luma, aspect: 1.33 }),
    (Sheet::Burst, SheetDef { stem: "fc3_gfx_impactburst_3x2", cols: 3, rows: 2, blend: Blend::Alpha, aspect: 0.67 }),
    (Sheet::MuzzleSide, SheetDef { stem: "fc3_muzzleflash_4x1", cols: 4, rows: 1, blend: Blend::Add, aspect: 1.0 }),
    (Sheet::MuzzleFront, SheetDef { stem: "fc3_gfx_muzzle01_2x2", cols: 2, rows: 2, blend: Blend::Add, aspect: 1.0 }),
    (Sheet::Smoke, SheetDef { stem: "fc3_gfx_smokeloop_6x5", cols: 6, rows: 5, blend: Blend::Alpha, aspect: 0.83 }),
    (Sheet::ExploSmoke, SheetDef { stem: "gfx_smokeexplo_4x3", cols: 4, rows: 3, blend: Blend::Alpha, aspect: 0.75 }),
    (Sheet::Sparks, SheetDef { stem: "fc3_gfx_sparks_2x2", cols: 2, rows: 2, blend: Blend::Add, aspect: 2.0 }),
    (Sheet::Splash, SheetDef { stem: "fc3_gfx_splash_01", cols: 1, rows: 1, blend: Blend::Luma, aspect: 1.0 }),
    (Sheet::Glow, SheetDef { stem: "fc3_gfx_lightglow01", cols: 1, rows: 1, blend: Blend::Add, aspect: 1.0 }),
    (Sheet::Drop, SheetDef { stem: "fc3_gfx_drop_01", cols: 1, rows: 1, blend: Blend::Luma, aspect: 0.15 }),
];

fn def(sheet: Sheet) -> &'static SheetDef {
    &SHEETS.iter().find(|(s, _)| *s == sheet).unwrap().1
}

/// Which flipbook frame a particle shows.
#[derive(Clone, Copy)]
pub enum Frame {
    Fixed(u32),
    /// Plays through once over the particle's life.
    Life,
    /// Loops at a rate (frames per second) from an offset.
    Loop(f32, u32),
}

#[derive(Clone, Copy)]
pub struct Particle {
    pub sheet: Sheet,
    pub pos: Vec3,
    pub vel: Vec3,
    pub life: f32,
    pub age: f32,
    /// Height in metres at birth; `grow` multiplies it by the end of life.
    pub size: f32,
    pub grow: f32,
    pub rot: f32,
    pub spin: f32,
    /// Tint and peak opacity.
    pub colour: Vec4,
    pub frame: Frame,
    /// Downward acceleration (negative rises).
    pub gravity: f32,
    /// Fraction of velocity lost per second.
    pub drag: f32,
    /// Stretch along the velocity (seconds of travel), for sparks.
    pub stretch: f32,
    /// Fraction of life spent fading in, and where fading out starts.
    pub fade_in: f32,
    pub fade_out: f32,
    /// Lit by the scene (smoke and dust darken at night).
    pub lit: bool,
}

impl Particle {
    pub fn new(sheet: Sheet, pos: Vec3, life: f32, size: f32) -> Particle {
        let blend = def(sheet).blend;
        Particle {
            sheet,
            pos,
            vel: Vec3::ZERO,
            life,
            age: 0.0,
            size,
            grow: 1.0,
            rot: 0.0,
            spin: 0.0,
            colour: Vec4::ONE,
            frame: Frame::Life,
            gravity: 0.0,
            drag: 0.0,
            stretch: 0.0,
            fade_in: if blend == Blend::Add { 0.0 } else { 0.08 },
            fade_out: 0.55,
            lit: blend != Blend::Add,
        }
    }

    fn alpha(&self) -> f32 {
        let t = (self.age / self.life.max(1e-4)).clamp(0.0, 1.0);
        let fade_in = if self.fade_in > 0.0 { (t / self.fade_in).min(1.0) } else { 1.0 };
        fade_in * (1.0 - smoothstep(self.fade_out, 1.0, t))
    }

    fn frame_index(&self, frames: u32) -> u32 {
        match self.frame {
            Frame::Fixed(f) => f % frames,
            Frame::Life => (((self.age / self.life.max(1e-4)) * frames as f32) as u32).min(frames - 1),
            Frame::Loop(fps, offset) => ((self.age * fps) as u32 + offset) % frames,
        }
    }
}

/// Live particles, plus sprites drawn for one frame only (lamp glows).
#[derive(Resource, Default)]
pub struct Particles {
    list: Vec<Particle>,
    sprites: Vec<Particle>,
}

const MAX_PARTICLES: usize = 6000;

impl Particles {
    pub fn spawn(&mut self, p: Particle) {
        if self.list.len() < MAX_PARTICLES {
            self.list.push(p);
        }
    }

    /// A sprite shown this frame only.
    pub fn sprite(&mut self, p: Particle) {
        self.sprites.push(p);
    }
}

/// A bullet-hole or blood decal atlas.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecalSheet {
    Concrete,
    Wood,
    Metal,
    Sand,
    Blood,
}

const DECAL_SHEETS: [(DecalSheet, &str, u32); 5] = [
    (DecalSheet::Concrete, "fc3_gfx_impactdecal_concrete_4x1", 4),
    (DecalSheet::Wood, "fc3_gfx_impactdecal_wood_4x1", 4),
    (DecalSheet::Metal, "fc3_gfx_impactdecal_metal_2x1", 2),
    (DecalSheet::Sand, "fc3_gfx_impactdecal_sand_2x1", 2),
    (DecalSheet::Blood, "fc3_gfx_bloodsplatters_4x1", 4),
];

#[derive(Clone, Copy)]
struct Decal {
    sheet: DecalSheet,
    pos: Vec3,
    normal: Vec3,
    tangent: Vec3,
    size: f32,
    frame: u32,
    age: f32,
    /// Brightness (scorch marks are darkened sand).
    tint: f32,
}

#[derive(Resource, Default)]
pub struct Decals {
    list: VecDeque<Decal>,
}

const MAX_DECALS: usize = 400;
const DECAL_LIFE: f32 = 120.0;

impl Decals {
    /// Lays a decal on a surface; `spin` (0..1) picks its rotation and frame.
    pub fn add(&mut self, sheet: DecalSheet, pos: Vec3, normal: Vec3, size: f32, spin: f32) {
        self.add_tinted(sheet, pos, normal, size, spin, 1.0);
    }

    /// A decal darkened (or lightened) by `tint`.
    pub fn add_tinted(&mut self, sheet: DecalSheet, pos: Vec3, normal: Vec3, size: f32, spin: f32, tint: f32) {
        let normal = normal.normalize_or(Vec3::Y);
        let base = normal.any_orthonormal_vector();
        let tangent = Quat::from_axis_angle(normal, spin * std::f32::consts::TAU) * base;
        let frames = DECAL_SHEETS.iter().find(|d| d.0 == sheet).map(|d| d.2).unwrap_or(1);
        if self.list.len() >= MAX_DECALS {
            self.list.pop_front();
        }
        self.list.push_back(Decal { sheet, pos, normal, tangent, size, frame: ((spin * 997.0) as u32) % frames, age: 0.0, tint });
    }
}

// ------------------------------------------------------------- material ---

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct ParticleMaterial {
    #[uniform(0)]
    mode: Vec4,
    #[texture(1)]
    #[sampler(2)]
    sheet: Handle<Image>,
    /// x: the depth (m) it fades over nearing what's behind (0: hard).
    #[uniform(3)]
    soft: Vec4,
}

impl ParticleMaterial {
    /// A plain sheet (its frames chosen by UV): 0 adds light, 1 blends by
    /// alpha, 2 takes brightness as coverage.
    pub(crate) fn plain(mode: f32, sheet: Handle<Image>) -> Self {
        Self { mode: Vec4::new(mode, 0.0, 0.0, 0.0), sheet, soft: Vec4::ZERO }
    }

    pub(crate) fn alpha_sheet(sheet: Handle<Image>, frames: [u32; 2], interpolate: bool) -> Self {
        Self { mode: Vec4::new(1.0, frames[0] as f32, frames[1] as f32, if interpolate { 1.0 } else { 0.0 }), sheet, soft: Vec4::ZERO }
    }

    /// Fading over `range` (m) as it nears what's behind it.
    pub(crate) fn soft(mut self, range: f32) -> Self {
        self.soft.x = range;
        self
    }
}

impl Material for ParticleMaterial {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("particles.wgsl")).with_source("embedded").into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }

    /// Draw particles after the sea and the sky (they never write depth, so
    /// anything transparent drawn later would cover them).
    fn depth_bias(&self) -> f32 {
        1.0e6
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// A batch mesh: particles of one sheet, or decals of one atlas.
#[derive(Component)]
enum Batch {
    Particles(Sheet),
    Decals(DecalSheet),
}

/// A batch mesh before its first particles: one invisible quad (an empty
/// mesh has no GPU allocation to update later).
pub(crate) fn empty_mesh() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    let mut quads = Quads::default();
    quads.push(Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, (Vec2::ZERO, Vec2::ZERO), Vec3::Y, Vec4::ZERO);
    quads.write(&mut mesh);
    mesh
}

fn load_fx(server: &AssetServer, stem: &str) -> Handle<Image> {
    server
        .load_builder()
        .with_settings(|s: &mut ImageLoaderSettings| {
            s.is_srgb = true;
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                mipmap_filter: ImageFilterMode::Linear,
                ..default()
            });
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(cache_path(&fx_rel(stem)))
}

/// How soft (m) our stand-in smoke and dust are.
const STAND_IN_SOFT: f32 = 0.5;

fn setup_batches(
    mut done: Local<bool>,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut particle_materials: ResMut<Assets<ParticleMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    if *done {
        return;
    }
    *done = true;
    let common = (Transform::default(), Visibility::Hidden, NotShadowCaster, NotShadowReceiver, bevy::camera::visibility::NoFrustumCulling);
    for (sheet, d) in &SHEETS {
        let mode = match d.blend {
            Blend::Add => 0.0,
            Blend::Alpha => 1.0,
            Blend::Luma => 2.0,
        };
        // (Smoke and dust soft where they meet the ground; sparks sharp.)
        let soft = if d.blend == Blend::Add { 0.0 } else { STAND_IN_SOFT };
        let material = particle_materials.add(ParticleMaterial::plain(mode, load_fx(&server, d.stem)).soft(soft));
        commands.spawn((Batch::Particles(*sheet), Mesh3d(meshes.add(empty_mesh())), MeshMaterial3d(material), common.clone(), Name::new("particles")));
    }
    for (sheet, stem, _) in &DECAL_SHEETS {
        let material = materials.add(StandardMaterial {
            base_color_texture: Some(load_fx(&server, stem)),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.9,
            ..default()
        });
        commands.spawn((Batch::Decals(*sheet), Mesh3d(meshes.add(empty_mesh())), MeshMaterial3d(material), common.clone(), Name::new("decals")));
    }
}

fn simulate(time: Res<Time>, mut particles: ResMut<Particles>, mut decals: ResMut<Decals>) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    particles.list.retain_mut(|p| {
        p.age += dt;
        if p.age >= p.life {
            return false;
        }
        p.vel.y -= p.gravity * dt;
        p.vel *= (1.0 - p.drag * dt).max(0.0);
        p.pos += p.vel * dt;
        p.rot += p.spin * dt;
        true
    });
    for d in decals.list.iter_mut() {
        d.age += dt;
    }
    while decals.list.front().is_some_and(|d| d.age > DECAL_LIFE) {
        decals.list.pop_front();
    }
}

/// Quad geometry gathered for one batch.
#[derive(Default)]
pub(crate) struct Quads {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
}

impl Quads {
    /// Corners: centre ± `x` ± `y`, with the frame's UV rectangle.
    pub(crate) fn push(&mut self, centre: Vec3, x: Vec3, y: Vec3, uv: (Vec2, Vec2), normal: Vec3, colour: Vec4) {
        let (lo, hi) = uv;
        for (p, t) in [
            (centre - x - y, Vec2::new(lo.x, hi.y)),
            (centre + x - y, Vec2::new(hi.x, hi.y)),
            (centre + x + y, Vec2::new(hi.x, lo.y)),
            (centre - x + y, Vec2::new(lo.x, lo.y)),
        ] {
            self.positions.push(p.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(t.to_array());
            self.colors.push(colour.to_array());
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub(crate) fn write(self, mesh: &mut Mesh) {
        let quads = self.positions.len() / 4;
        let indices: Vec<u32> = (0..quads as u32).flat_map(|q| [0, 1, 2, 0, 2, 3].map(|i| q * 4 + i)).collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(Indices::U32(indices));
    }
}

pub(crate) fn frame_uv(index: u32, cols: u32, rows: u32) -> (Vec2, Vec2) {
    let (c, r) = (index % cols, index / cols);
    let size = Vec2::new(1.0 / cols as f32, 1.0 / rows as f32);
    let lo = Vec2::new(c as f32, r as f32) * size;
    // Half a texel in, so neighbouring frames don't bleed.
    let inset = size * 0.004;
    (lo + inset, lo + size - inset)
}

/// Rebuilds every batch mesh around the camera.
fn build_batches(
    mut particles: ResMut<Particles>,
    decals: Res<Decals>,
    daylight: Res<crate::clouds::Daylight>,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    mut batches: Query<(&Batch, &Mesh3d, &mut Transform, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok(cam) = cameras.single() else { return };
    let origin = cam.translation();
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    let view = cam.forward().as_vec3();
    // Smoke and dust follow the light level; fire doesn't.
    let ambient = 0.12 + 0.88 * daylight.0;
    let sprites = std::mem::take(&mut particles.sprites);
    for (batch, mesh, mut tr, mut vis) in &mut batches {
        tr.translation = origin;
        let mut quads = Quads::default();
        match batch {
            Batch::Particles(sheet) => {
                let d = def(*sheet);
                let mut live: Vec<&Particle> = particles.list.iter().chain(sprites.iter()).filter(|p| p.sheet == *sheet).collect();
                if d.blend != Blend::Add {
                    // Far to near.
                    live.sort_by(|a, b| b.pos.distance_squared(origin).total_cmp(&a.pos.distance_squared(origin)));
                }
                let frames = d.cols * d.rows;
                for p in live {
                    let t = (p.age / p.life.max(1e-4)).clamp(0.0, 1.0);
                    let h = p.size * (1.0 + (p.grow - 1.0) * t) * 0.5;
                    let w = h * d.aspect;
                    let (x, y) = if p.stretch > 0.0 && p.vel.length_squared() > 1e-4 {
                        // Along the motion as seen on screen.
                        let along = (p.vel - view * p.vel.dot(view)).normalize_or(up);
                        let side = view.cross(along).normalize_or(right);
                        (side * w, along * (h + p.vel.length() * p.stretch * 0.5))
                    } else {
                        let (s, c) = p.rot.sin_cos();
                        (right * (c * w) + up * (s * w), up * (c * h) - right * (s * h))
                    };
                    let light = if p.lit { ambient } else { 1.0 };
                    let colour = Vec4::new(p.colour.x * light, p.colour.y * light, p.colour.z * light, p.colour.w * p.alpha());
                    quads.push(p.pos - origin, x, y, frame_uv(p.frame_index(frames), d.cols, d.rows), -view, colour);
                }
            }
            Batch::Decals(sheet) => {
                let frames = DECAL_SHEETS.iter().find(|d| d.0 == *sheet).map(|d| d.2).unwrap_or(1);
                for dc in decals.list.iter().filter(|dc| dc.sheet == *sheet && dc.pos.distance_squared(origin) < 150.0 * 150.0) {
                    let half = dc.size * 0.5;
                    let x = dc.tangent * half;
                    let y = dc.normal.cross(dc.tangent) * half;
                    let fade = 1.0 - smoothstep(DECAL_LIFE - 10.0, DECAL_LIFE, dc.age);
                    quads.push(dc.pos + dc.normal * 0.012 - origin, x, y, frame_uv(dc.frame, frames, 1), dc.normal, Vec4::new(dc.tint, dc.tint, dc.tint, fade));
                }
            }
        }
        let empty = quads.positions.is_empty();
        let wanted = if empty { Visibility::Hidden } else { Visibility::Inherited };
        if *vis != wanted {
            *vis = wanted;
        }
        if empty {
            continue;
        }
        if let Some(mut m) = meshes.get_mut(&mesh.0) {
            quads.write(&mut m);
        }
    }
}

/// What shot effects need: the particle and decal lists, and enough of the
/// world to tell what a bullet hit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ShotFx<'w, 's> {
    /// Rounds that hit something other than what they were fired at.
    pub stray: MessageWriter<'w, crate::vehicles::StrayHit>,
    pub particles: ResMut<'w, Particles>,
    pub decals: ResMut<'w, Decals>,
    pub ignite: MessageWriter<'w, crate::fire::Ignite>,
    terrain: Query<'w, 's, (), With<crate::physics::TerrainCollider>>,
    vehicles: Query<'w, 's, (), With<crate::vehicles::Vehicle>>,
    parents: Query<'w, 's, &'static ChildOf>,
    objects: Query<'w, 's, &'static crate::objects::WorldObject>,
    sector_roots: Query<'w, 's, &'static crate::objects::SectorObjects>,
    sectors: Res<'w, Assets<crate::cache_assets::Fc3Sector>>,
    /// What was hit is made of, and the game's impact for it.
    probe: crate::surfaces::SurfaceProbe<'w, 's>,
    impacts: Res<'w, crate::authored_fx::BulletImpacts>,
    pub commands: Commands<'w, 's>,
}

/// Guesses a prop's material from its entity name.
fn surface_for_name(name: &str) -> fx::Surface {
    const WOOD: &[&str] = &[
        "wood", "shack", "hut", "plank", "crate", "pallet", "fence", "bamboo", "log", "table", "chair", "door", "dock", "pier", "palm", "tree", "stilt",
        "shed", "barn", "box", "shelf", "bench",
    ];
    const METAL: &[&str] = &[
        "metal",
        "container",
        "tank",
        "barrel",
        "car_",
        "truck",
        "jeep",
        "tin",
        "corrugated",
        "pipe",
        "generator",
        "antenna",
        "tower",
        "rail",
        "sign",
        "drum",
        "steel",
        "iron",
        "fuel",
        "vehicle",
        "wreck",
        "gate",
    ];
    let n = name.to_ascii_lowercase();
    if METAL.iter().any(|k| n.contains(k)) {
        fx::Surface::Metal
    } else if WOOD.iter().any(|k| n.contains(k)) {
        fx::Surface::Wood
    } else {
        fx::Surface::Hard
    }
}

impl ShotFx<'_, '_> {
    /// The kind of surface a collider belongs to.
    pub fn surface(&self, e: Entity) -> fx::Surface {
        if self.terrain.contains(e) {
            return fx::Surface::Ground;
        }
        if let Ok(obj) = self.objects.get(e) {
            let name = self
                .parents
                .get(e)
                .ok()
                .and_then(|p| self.sector_roots.get(p.parent()).ok())
                .and_then(|root| self.sectors.get(&root.sector))
                .and_then(|s| s.entities.get(obj.entity_index as usize))
                .map(|ent| ent.name.as_str());
            if let Some(name) = name {
                return surface_for_name(name);
            }
        }
        let mut cur = e;
        for _ in 0..4 {
            if self.vehicles.contains(cur) {
                return fx::Surface::Metal;
            }
            match self.parents.get(cur) {
                Ok(p) => cur = p.parent(),
                Err(_) => break,
            }
        }
        fx::Surface::Hard
    }

    /// Effects where a bullet ends: a splash if it reached water first,
    /// otherwise an impact on what it hit (`hit`: collider, distance, normal).
    pub fn bullet_end(&mut self, rng: &mut crate::weapons::Rng, from: Vec3, dir: Vec3, range: f32, hit: Option<(Entity, f32, Vec3)>) {
        let end = from + dir * hit.map(|h| h.1).unwrap_or(range);
        let sea = crate::water::plane_level(end);
        // The game's own impact for what was hit (its material's), else
        // ours.
        if from.y > sea && end.y < sea {
            let t = (from.y - sea) / (from.y - end.y);
            let at = from.lerp(end, t);
            if !self.impacts.named(WATER).is_some_and(|fx| crate::authored_fx::play_along(&mut self.commands, fx, at, Vec3::Y)) {
                fx::splash(&mut self.particles, rng, at, 0.6);
            }
            return;
        }
        if let Some((e, _, normal)) = hit {
            let surface = self.surface(e);
            let effect = self.impacts.of(self.probe.material_at(e, end, normal));
            if effect.is_some_and(|fx| crate::authored_fx::play_along(&mut self.commands, fx, end, normal)) {
                fx::impact_decal(&mut self.decals, rng, end, normal, surface);
            } else {
                fx::impact(&mut self.particles, &mut self.decals, rng, end, normal, surface);
            }
        }
    }

    /// A bullet in a body: the game's flesh impact (the head's for a
    /// headshot), facing back along the shot, else our blood.
    pub fn blood(&mut self, rng: &mut crate::weapons::Rng, at: Vec3, dir: Vec3, head: bool) {
        let flesh = self.impacts.named(if head { FLESH_HEAD } else { FLESH });
        if !flesh.is_some_and(|fx| crate::authored_fx::play_along(&mut self.commands, fx, at, -dir)) {
            fx::blood(&mut self.particles, rng, at, dir);
        }
    }
}

/// The logic materials whose bullet impacts water and bodies make.
const WATER: &str = "Base/Water_P_Deep";
const FLESH: &str = "Base/Flesh_P";
const FLESH_HEAD: &str = "Base/Flesh_P_Head";

impl ShotFx<'_, '_> {}

/// `FC3_FX_TEST=explosion|impacts|blood|splash`: repeats an effect where the
/// camera looks, from just before the screenshot is due (for checking looks).
fn fx_test(
    args: Res<crate::Args>,
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    mut rng: ResMut<crate::weapons::Rng>,
    spatial: avian3d::prelude::SpatialQuery,
    cameras: Query<&GlobalTransform, With<PlayerCamera>>,
    players: Query<Entity, With<crate::player::Player>>,
    mut shot: ShotFx,
) {
    use avian3d::prelude::SpatialQueryFilter;
    let Ok(kind) = std::env::var("FC3_FX_TEST") else { return };
    if std::env::var("FC3_FX_DEBUG").is_ok() && (time.elapsed_secs() * 2.0) as u32 != ((time.elapsed_secs() - time.delta_secs()) * 2.0) as u32 {
        let mut counts = std::collections::BTreeMap::new();
        for p in &shot.particles.list {
            *counts.entry(format!("{:?}", p.sheet)).or_insert(0) += 1;
        }
        info!("particles {:?}, decals {}", counts, shot.decals.list.len());
    }
    let t = time.elapsed_secs();
    // A fire needs time to spread before the screenshot.
    let lead = if kind == "fire" { 14.0 } else { 1.0 };
    if t < args.settle - lead || t < *next {
        return;
    }
    let Ok(cam) = cameras.single() else { return };
    let from = cam.translation();
    let forward = cam.forward();
    let filter = SpatialQueryFilter::from_excluded_entities(players.iter());
    let hit = spatial.cast_ray(from, forward, 300.0, true, &filter);
    let at = hit.map(|h| from + forward * h.distance).unwrap_or(from + forward * 15.0);
    match kind.as_str() {
        "explosion" => {
            *next = t + 1.0;
            fx::explosion(&mut shot.particles, &mut rng, at, 8.0);
        }
        "explosion-air" => {
            *next = t + 0.5;
            fx::explosion(&mut shot.particles, &mut rng, from + forward * 14.0, 8.0);
        }
        "impacts" => {
            *next = t + 0.25;
            for _ in 0..6 {
                let dir = (forward.as_vec3() + Vec3::new(rng.range(-0.08, 0.08), rng.range(-0.05, 0.05), rng.range(-0.08, 0.08))).normalize();
                let Ok(d3) = Dir3::new(dir) else { continue };
                let h = spatial.cast_ray(from, d3, 300.0, true, &filter).map(|h| (h.entity, h.distance, h.normal));
                shot.bullet_end(&mut rng, from, dir, 300.0, h);
            }
        }
        "blood" => {
            *next = t + 0.3;
            shot.blood(&mut rng, from + forward * 4.0, forward.as_vec3(), false);
        }
        "fire" => {
            *next = f32::INFINITY;
            shot.ignite.write(crate::fire::Ignite { at, radius: 3.0, instigator: None });
        }
        "splash" => {
            *next = t + 0.4;
            fx::splash(&mut shot.particles, &mut rng, Vec3::new(at.x, crate::water::plane_level(at), at.z), 1.5);
        }
        _ => {}
    }
}

// --------------------------------------------------------------- effects ---

/// Ready-made effects built from particles.
pub mod fx {
    use super::*;
    use crate::weapons::Rng;

    fn jitter(rng: &mut Rng, r: f32) -> Vec3 {
        Vec3::new(rng.range(-r, r), rng.range(-r, r), rng.range(-r, r))
    }

    /// Gun flash at the muzzle, pointing along `dir`.
    pub fn muzzle_flash(ps: &mut Particles, rng: &mut Rng, at: Vec3, dir: Vec3, scale: f32) {
        let mut front = Particle::new(Sheet::MuzzleFront, at, 0.05, 0.32 * scale);
        front.frame = Frame::Fixed((rng.f32() * 4.0) as u32);
        front.rot = rng.range(0.0, std::f32::consts::TAU);
        front.colour = Vec4::new(1.0, 0.62, 0.28, 1.0);
        ps.spawn(front);
        let mut side = Particle::new(Sheet::MuzzleSide, at + dir * 0.12 * scale, 0.05, 0.22 * scale);
        side.frame = Frame::Fixed((rng.f32() * 4.0) as u32);
        side.vel = dir * 0.01;
        side.stretch = 25.0 * scale;
        side.colour = Vec4::new(1.0, 0.58, 0.25, 0.9);
        ps.spawn(side);
    }

    /// Smoke wisp from a barrel after firing.
    pub fn barrel_smoke(ps: &mut Particles, rng: &mut Rng, at: Vec3) {
        let mut s = Particle::new(Sheet::Smoke, at, 0.9, 0.12);
        s.grow = 4.0;
        s.vel = Vec3::Y * 0.4 + jitter(rng, 0.1);
        s.frame = Frame::Loop(20.0, (rng.f32() * 30.0) as u32);
        s.colour = Vec4::new(0.8, 0.8, 0.8, 0.18);
        ps.spawn(s);
    }

    /// What a bullet hit.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Surface {
        Ground,
        Hard,
        Metal,
        Wood,
    }

    /// Bullet impact: dust or chips, sparks off metal, and a hole.
    pub fn impact(ps: &mut Particles, decals: &mut Decals, rng: &mut Rng, at: Vec3, normal: Vec3, surface: Surface) {
        let n = normal.normalize_or(Vec3::Y);
        let (sheet, tint, size) = match surface {
            Surface::Ground => (Sheet::DustBurst, Vec4::new(0.62, 0.55, 0.45, 0.85), 0.7),
            _ => (Sheet::Burst, Vec4::new(0.8, 0.78, 0.74, 0.9), 0.45),
        };
        let mut puff = Particle::new(sheet, at + n * 0.05, 0.6, size);
        puff.grow = 1.6;
        puff.vel = n * 0.6;
        puff.colour = tint;
        puff.rot = if sheet == Sheet::DustBurst { 0.0 } else { rng.range(-0.4, 0.4) };
        ps.spawn(puff);
        let mut dust = Particle::new(Sheet::Dust, at + n * 0.1, 1.4, 0.35);
        dust.grow = 2.5;
        dust.vel = n * 0.35 + Vec3::Y * 0.15;
        dust.drag = 0.8;
        dust.colour = Vec4::new(tint.x, tint.y, tint.z, 0.35);
        dust.rot = rng.range(-0.3, 0.3);
        ps.spawn(dust);
        if surface == Surface::Metal || (surface == Surface::Hard && rng.f32() < 0.3) {
            for _ in 0..3 {
                let mut s = Particle::new(Sheet::Sparks, at, rng.range(0.12, 0.25), 0.06);
                s.vel = (n + jitter(rng, 0.8)).normalize_or(n) * rng.range(4.0, 9.0);
                s.gravity = 9.8;
                s.stretch = 0.03;
                s.frame = Frame::Fixed((rng.f32() * 4.0) as u32);
                s.colour = Vec4::new(1.0, 0.55, 0.2, 1.2);
                ps.spawn(s);
            }
        }
        impact_decal(decals, rng, at, n, surface);
    }

    /// A bullet hole where a bullet struck.
    pub fn impact_decal(decals: &mut Decals, rng: &mut Rng, at: Vec3, normal: Vec3, surface: Surface) {
        let n = normal.normalize_or(Vec3::Y);
        let sheet = match surface {
            Surface::Ground => DecalSheet::Sand,
            Surface::Hard => DecalSheet::Concrete,
            Surface::Metal => DecalSheet::Metal,
            Surface::Wood => DecalSheet::Wood,
        };
        decals.add(sheet, at, n, if surface == Surface::Ground { 0.22 } else { 0.14 }, rng.f32());
    }

    /// A hit on a body: a puff of blood, and a splatter on whatever is behind.
    pub fn blood(ps: &mut Particles, rng: &mut Rng, at: Vec3, dir: Vec3) {
        let mut mist = Particle::new(Sheet::Burst, at, 0.35, 0.5);
        mist.grow = 1.8;
        mist.vel = dir * 1.5;
        mist.colour = Vec4::new(0.45, 0.03, 0.02, 0.9);
        ps.spawn(mist);
        for _ in 0..4 {
            let mut d = Particle::new(Sheet::Dust, at, rng.range(0.3, 0.5), 0.12);
            d.vel = (dir + jitter(rng, 0.6)) * rng.range(1.5, 3.0);
            d.gravity = 9.8;
            d.colour = Vec4::new(0.35, 0.02, 0.02, 0.9);
            ps.spawn(d);
        }
    }

    /// Fireball, smoke column, sparks and debris dust.
    pub fn explosion(ps: &mut Particles, rng: &mut Rng, at: Vec3, radius: f32) {
        let s = radius / 8.0;
        for i in 0..4 {
            let mut f = Particle::new(Sheet::Fireball, at + jitter(rng, 0.8 * s) + Vec3::Y * 0.6 * s, rng.range(0.55, 0.8), rng.range(3.0, 4.5) * s);
            f.grow = 1.5;
            f.rot = rng.range(0.0, std::f32::consts::TAU);
            f.vel = jitter(rng, 2.0) + Vec3::Y * 2.0;
            f.colour = Vec4::new(1.0, 0.5, 0.18, if i == 0 { 1.2 } else { 0.9 });
            ps.spawn(f);
        }
        let mut core = Particle::new(Sheet::ExploFire, at + Vec3::Y * 0.5 * s, 0.5, 5.0 * s);
        core.colour = Vec4::new(1.0, 0.6, 0.25, 1.1);
        ps.spawn(core);
        for _ in 0..6 {
            let mut sm = Particle::new(Sheet::ExploSmoke, at + jitter(rng, 1.0 * s) + Vec3::Y * s, rng.range(3.5, 5.5), rng.range(2.2, 3.0) * s);
            sm.grow = 2.2;
            sm.vel = Vec3::Y * rng.range(1.0, 2.5) + jitter(rng, 1.2);
            sm.drag = 0.3;
            sm.rot = rng.range(0.0, std::f32::consts::TAU);
            sm.spin = rng.range(-0.2, 0.2);
            sm.fade_in = 0.15;
            sm.colour = Vec4::new(0.35, 0.33, 0.31, 0.75);
            ps.spawn(sm);
        }
        for _ in 0..14 {
            let mut sp = Particle::new(Sheet::Sparks, at + Vec3::Y * 0.3, rng.range(0.4, 0.9), 0.12 * s.max(0.6));
            sp.vel = (Vec3::Y * 0.8 + jitter(rng, 1.0)).normalize_or(Vec3::Y) * rng.range(8.0, 18.0);
            sp.gravity = 9.8;
            sp.drag = 0.4;
            sp.stretch = 0.03;
            sp.frame = Frame::Fixed((rng.f32() * 4.0) as u32);
            sp.colour = Vec4::new(1.0, 0.55, 0.2, 1.3);
            ps.spawn(sp);
        }
        for _ in 0..3 {
            let mut d = Particle::new(Sheet::DustBurst, at + jitter(rng, 0.5 * s), 1.6, 3.0 * s);
            d.grow = 2.0;
            d.vel = Vec3::Y * 2.0;
            d.drag = 0.6;
            d.colour = Vec4::new(0.5, 0.45, 0.38, 0.7);
            ps.spawn(d);
        }
    }

    /// One pulse of a flamethrower's stream, carried along with the shooter;
    /// the flames die out where the stream strikes `reach` metres away.
    pub fn flame_stream(ps: &mut Particles, rng: &mut Rng, from: Vec3, dir: Vec3, carry: Vec3, reach: f32) {
        const DRAG: f32 = 1.3;
        for _ in 0..3 {
            let speed = rng.range(15.0, 19.0);
            // Time to travel `reach` against the drag (unreachable: full life).
            let k = reach * DRAG / speed;
            let arrive = if k < 0.98 { -(1.0 - k).ln() / DRAG } else { f32::MAX };
            let mut f = Particle::new(Sheet::Fireball, from + dir * rng.range(0.2, 0.8), rng.range(0.55, 0.75).min(arrive + 0.08), rng.range(0.25, 0.4));
            f.vel = (dir + jitter(rng, 0.07)).normalize() * speed + carry;
            f.drag = DRAG;
            f.gravity = -1.5;
            f.grow = 7.0;
            f.rot = rng.range(0.0, std::f32::consts::TAU);
            f.spin = rng.range(-2.0, 2.0);
            f.colour = Vec4::new(1.0, 0.45, 0.12, 0.9);
            ps.spawn(f);
        }
        if rng.f32() < 0.3 {
            let mut s = Particle::new(Sheet::Smoke, from + dir * reach.min(6.0), 2.5, 1.0);
            s.vel = dir * 4.0 + Vec3::Y * 1.2;
            s.drag = 0.5;
            s.grow = 3.0;
            s.colour = Vec4::new(0.25, 0.24, 0.23, 0.3);
            ps.spawn(s);
        }
    }

    /// A bottle of fuel bursting into flame.
    pub fn fire_burst(ps: &mut Particles, rng: &mut Rng, at: Vec3) {
        for _ in 0..8 {
            let mut f = Particle::new(Sheet::Fireball, at + jitter(rng, 1.2).with_y(0.3), rng.range(0.5, 0.9), rng.range(1.2, 2.2));
            f.vel = Vec3::Y * rng.range(1.0, 2.5) + jitter(rng, 1.5).with_y(0.0);
            f.grow = 1.4;
            f.rot = rng.range(0.0, std::f32::consts::TAU);
            f.colour = Vec4::new(1.0, 0.42, 0.1, 0.9);
            ps.spawn(f);
        }
    }

    /// Water spray where something hits the surface.
    pub fn splash(ps: &mut Particles, rng: &mut Rng, at: Vec3, size: f32) {
        let mut s = Particle::new(Sheet::Splash, at + Vec3::Y * size * 0.4, 0.5, size);
        s.grow = 1.5;
        s.vel = Vec3::Y * 1.2;
        s.gravity = 4.0;
        s.colour = Vec4::new(0.85, 0.9, 0.95, 0.8);
        ps.spawn(s);
        for _ in 0..3 {
            let mut d = Particle::new(Sheet::Dust, at, 0.7, size * 0.5);
            d.vel = (Vec3::Y * 2.0 + jitter(rng, 0.8)) * size * 2.0;
            d.gravity = 9.8;
            d.colour = Vec4::new(0.9, 0.95, 1.0, 0.5);
            ps.spawn(d);
        }
    }

    /// One frame's worth of a camp fire: flames, embers and smoke.
    pub fn camp_fire(ps: &mut Particles, rng: &mut Rng, at: Vec3, dt: f32, strength: f32) {
        if rng.f32() < dt * 14.0 {
            let mut f = Particle::new(Sheet::Fire, at + jitter(rng, 0.2).with_y(0.25), rng.range(0.7, 1.0), rng.range(1.0, 1.4));
            f.vel = Vec3::Y * rng.range(0.3, 0.6);
            f.grow = 0.8;
            f.frame = Frame::Loop(24.0, (rng.f32() * 64.0) as u32);
            f.fade_in = 0.15;
            f.colour = Vec4::new(1.0, 0.36, 0.07, 0.8 * strength);
            ps.spawn(f);
        }
        if rng.f32() < dt * 2.0 {
            let mut s = Particle::new(Sheet::Smoke, at + Vec3::Y * 0.9, rng.range(4.0, 6.0), 0.7);
            s.vel = Vec3::Y * rng.range(0.6, 0.9) + Vec3::new(0.15, 0.0, 0.1);
            s.grow = 3.5;
            s.rot = rng.range(0.0, std::f32::consts::TAU);
            s.spin = rng.range(-0.15, 0.15);
            s.frame = Frame::Loop(8.0, (rng.f32() * 30.0) as u32);
            s.fade_in = 0.2;
            s.colour = Vec4::new(0.5, 0.48, 0.46, 0.22);
            ps.spawn(s);
        }
        if rng.f32() < dt * 3.0 {
            let mut e = Particle::new(Sheet::Sparks, at + Vec3::Y * 0.3, rng.range(0.8, 1.5), 0.03);
            e.vel = Vec3::Y * rng.range(1.0, 2.2) + jitter(rng, 0.3);
            e.stretch = 0.02;
            e.frame = Frame::Fixed((rng.f32() * 4.0) as u32);
            e.colour = Vec4::new(1.0, 0.5, 0.15, 1.2);
            ps.spawn(e);
        }
    }
}
