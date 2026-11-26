//! The world's light probes lighting the shade, as the game's "Deferred
//! Radiance Transfer Volumes" do (`fc3_formats::light_probes`; GDC 2012,
//! Gilabert and Stefanov).
//!
//! The probes of the sectors about the camera are relit as the hour, the
//! weather and the light change: each one's four basis directions get the
//! sun's and the sky's light bounced to it (its transfer matrices dotted with
//! their spherical harmonics) and the sky's visibility. A grid of voxels that
//! follows the camera (`RES` of `CELL` m, the game's own is 96 × 96 × 16)
//! takes from the probes about each voxel, nearest the most, what reaches
//! each of its six sides: the sky's light from above as the sky lights the
//! open, times how much of it the probes see in that direction, and the
//! bounce, the basis directions weighed by the game's own shading weights
//! (squared and clamped for the three upward, wrapped for the down one).
//! Bevy's irradiance volume (an ambient cube per voxel) shades with it, in
//! place of the sky's environment map; past the volume and across its edge
//! the environment map lights as before (`vendor/bevy_pbr`).
//!
//! Where no probe stands near a voxel (high in the air) it takes the sky's
//! light as the environment map gives it. `FC3_PROBES=0` leaves the probes
//! out.
//!
//! Past that volume the game darkens the sky's light on distant things with
//! a top-down occlusion map made from the probes, less for things high above
//! them, so the distant jungle's shade isn't sky-blue. A coarser volume
//! stands in for it here (`FAR_RES` of `FAR_CELL` m about the camera, from
//! the probes of `FAR_LOAD` sectors round): each voxel takes the probes in
//! its column of ground, those about its height the most, and comes out
//! into the open sky over `FAR_ABOVE` m above their tops. It lights only
//! what the near volume leaves (`vendor/bevy_pbr`: it's marked as affecting
//! lightmapped meshes, of which there are none).

use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::light::{IrradianceVolume, LightProbe};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use fc3_formats::light_probes::{Probe, BASIS, ONE, OPEN_SKY};
use fc3_formats::terrain::SectorCoord;

use crate::player::PlayerCamera;

pub struct ProbesPlugin;

impl Plugin for ProbesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProbeField>().init_resource::<ProbesLive>().add_systems(PostUpdate, (stream_probes, relight, rebuild).chain());
    }
}

/// Whether the probes' volume lights the shade about the camera: then the
/// shelter overhead's stand-ins (`sky::Indoors`: the sky's light dimmed and
/// warmed, the exposure opened) are left to the probes and the eye.
#[derive(Resource, Default)]
pub struct ProbesLive(pub bool);

/// The voxels along x, y (up) and z.
const RES: [usize; 3] = [48, 12, 48];
/// A voxel's size (m).
const CELL: f32 = 3.0;
/// The far volume's voxels, their size (m), and how far (m) the camera goes
/// from its centre before it's built afresh about it.
const FAR_RES: [usize; 3] = [64, 20, 64];
const FAR_CELL: f32 = 12.0;
const FAR_STEP: f32 = 48.0;
/// The sectors about the camera's whose probes are kept (Chebyshev): the
/// far volume's reach.
const FAR_LOAD: i32 = 6;
/// How far (m, on the ground plane) a far voxel's probes reach, and the
/// column hash's cell.
const FAR_REACH: f32 = 18.0;
/// Over how far (m) above its probes' tops a far voxel comes out into the
/// open sky.
const FAR_ABOVE: f32 = 15.0;
const FAR_FALLOFF: f32 = 0.1;
/// How far (m) from a voxel a probe still lights it, and the spatial hash's
/// cell.
const REACH: f32 = 10.0;
/// From how far (m) a voxel's nearest probe begins to give way to the open
/// sky's light.
const SURE: f32 = 5.0;
/// The share of the volume's half-size over which it gives way to the
/// environment map at its edges.
const FALLOFF: f32 = 0.25;

const Y00: f32 = 0.282095;
const Y1: f32 = 0.488603;

/// A probe relit: where, how much sky each upward basis direction sees, and
/// the light bounced to it from each basis direction (cd/m², E/π).
#[derive(Clone, Copy)]
struct LitProbe {
    pos: Vec3,
    sky: [f32; 3],
    bounce: [Vec3; 4],
}

/// Everything a volume is built from.
struct Lit {
    probes: Vec<LitProbe>,
    /// Probe indices by spatial hash cell.
    cells: HashMap<IVec3, Vec<u32>>,
    /// And by column of ground (`FAR_REACH` m), for the far volume.
    columns: HashMap<IVec2, Vec<u32>>,
    /// The sky's light (E/π, cd/m²) on each of the six sides: from above
    /// only, and with the ground's stand-in below as the environment map has.
    sky_above: [Vec3; 6],
    sky_all: [Vec3; 6],
    /// The large interior volumes about, and their ambient on surfaces facing
    /// up, down and sideways (`interiors::ambient_light`).
    shapes: Vec<(Arc<ShapeGrid>, [Vec3; 3])>,
}

/// A large interior volume's shape (a cave's), its triangles binned on the
/// ground plane, to tell what's inside it: a ray straight up from a point
/// inside crosses its surface an odd number of times.
pub(crate) struct ShapeGrid {
    interior: fc3_formats::interiors::Interior,
    min: Vec3,
    max: Vec3,
    tris: Vec<[Vec3; 3]>,
    /// Triangle indices by `BIN` m cell (x, z) from `min`.
    bins: Vec<Vec<u32>>,
    across: usize,
}

const BIN: f32 = 4.0;

impl ShapeGrid {
    fn new(s: &fc3_formats::interiors::InteriorShape) -> ShapeGrid {
        let v: Vec<Vec3> = s.vertices.iter().map(|v| Vec3::from(*v)).collect();
        let tris: Vec<[Vec3; 3]> = s.indices.chunks_exact(3).map(|t| [v[t[0] as usize], v[t[1] as usize], v[t[2] as usize]]).collect();
        let (min, max) = v.iter().fold((Vec3::MAX, Vec3::MIN), |(a, b), p| (a.min(*p), b.max(*p)));
        let across = (((max.x - min.x) / BIN).floor() as usize + 1).min(4096);
        let down = (((max.z - min.z) / BIN).floor() as usize + 1).min(4096);
        let mut bins = vec![Vec::new(); across * down];
        let bin = |p: f32, lo: f32, n: usize| (((p - lo) / BIN).floor().max(0.0) as usize).min(n - 1);
        for (i, t) in tris.iter().enumerate() {
            let (lo, hi) = (t[0].min(t[1]).min(t[2]), t[0].max(t[1]).max(t[2]));
            for z in bin(lo.z, min.z, down)..=bin(hi.z, min.z, down) {
                for x in bin(lo.x, min.x, across)..=bin(hi.x, min.x, across) {
                    bins[z * across + x].push(i as u32);
                }
            }
        }
        ShapeGrid { interior: s.interior, min, max, tris, bins, across }
    }

    fn contains(&self, p: Vec3) -> bool {
        if p.cmplt(self.min).any() || p.cmpgt(self.max).any() {
            return false;
        }
        // (A hair off the grid's round numbers, so as not to run down an
        // edge two triangles share and count both.)
        let p = p + Vec3::new(1.37e-3, 0.0, 2.71e-3);
        let (x, z) = (((p.x - self.min.x) / BIN) as usize, ((p.z - self.min.z) / BIN) as usize);
        let Some(ids) = self.bins.get(z * self.across + x.min(self.across - 1)) else { return false };
        let mut crossings = 0;
        for &i in ids {
            let [a, b, c] = self.tris[i as usize];
            // Where the vertical through p meets the triangle, if it does.
            let d = (b.z - c.z) * (a.x - c.x) + (c.x - b.x) * (a.z - c.z);
            if d.abs() < 1e-9 {
                continue;
            }
            let u = ((b.z - c.z) * (p.x - c.x) + (c.x - b.x) * (p.z - c.z)) / d;
            let v = ((c.z - a.z) * (p.x - c.x) + (a.x - c.x) * (p.z - c.z)) / d;
            let w = 1.0 - u - v;
            if u < 0.0 || v < 0.0 || w < 0.0 {
                continue;
            }
            if u * a.y + v * b.y + w * c.y > p.y {
                crossings += 1;
            }
        }
        crossings % 2 == 1
    }

    /// Whether it reaches into a box.
    fn meets(&self, lo: Vec3, hi: Vec3) -> bool {
        self.min.cmple(hi).all() && self.max.cmpge(lo).all()
    }
}

/// The large interior volumes' shapes, from the cache.
fn load_shapes(root: &std::path::Path) -> Vec<Arc<ShapeGrid>> {
    let bytes = std::fs::read(root.join(fc3_formats::cache::INTERIOR_SHAPES_REL)).unwrap_or_default();
    fc3_formats::interiors::shapes_from_bytes(&bytes).unwrap_or_default().iter().map(|s| Arc::new(ShapeGrid::new(s))).collect()
}

#[derive(Resource, Default)]
struct ProbeField {
    sectors: HashMap<u32, Arc<Vec<Probe>>>,
    centre: Option<SectorCoord>,
    /// Bumped as sectors come and go.
    loaded: u32,
    /// The large interior volumes' shapes (loaded with the first sectors).
    shapes: Option<Vec<Arc<ShapeGrid>>>,
    /// Those reaching into the probes' sectors and in the world now.
    shapes_on: Vec<usize>,
    lit: Option<Arc<Lit>>,
    /// What the probes were last relit for, and a count of relightings.
    lit_for: Option<(u32, f32, f32, Vec3, f32)>,
    lit_version: u32,
    /// The volume about the camera, and the far one about it.
    near: Volume,
    far: Volume,
}

/// A volume as it's built and placed.
#[derive(Default)]
struct Volume {
    built_for: Option<(IVec3, u32)>,
    task: Option<Task<(IVec3, u32, Vec<u8>)>>,
    volume: Option<(Entity, Handle<Image>)>,
}

/// What a volume is: its voxels, their size (m), how far (m) the camera
/// moves before it's built afresh, its falloff, and whether it's the far one.
#[derive(Clone, Copy)]
struct Kind {
    res: [usize; 3],
    cell: f32,
    step: f32,
    falloff: f32,
    far: bool,
}

const NEAR: Kind = Kind { res: RES, cell: CELL, step: CELL, falloff: FALLOFF, far: false };
const FAR: Kind = Kind { res: FAR_RES, cell: FAR_CELL, step: FAR_STEP, falloff: FAR_FALLOFF, far: true };

fn enabled() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var("FC3_PROBES").as_deref() != Ok("0"));
    *ON
}

/// Loads the probes of the sectors about the camera, letting go of those
/// further off.
fn stream_probes(info: Option<Res<crate::data::CacheInfo>>, cams: Query<&GlobalTransform, With<PlayerCamera>>, mut field: ResMut<ProbeField>) {
    let (Some(info), Ok(cam)) = (info, cams.single()) else { return };
    if !enabled() {
        return;
    }
    let p = crate::data::to_dunia(cam.translation());
    let centre = SectorCoord::containing(p[0], p[1]);
    if field.centre == Some(centre) {
        return;
    }
    field.centre = Some(centre);
    if field.shapes.is_none() {
        let shapes = if crate::interiors::enabled() { load_shapes(&info.root) } else { Vec::new() };
        info!("light probes: {} large interior volumes by their shape", shapes.len());
        field.shapes = Some(shapes);
    }
    let near = |s: SectorCoord| (s.x - centre.x).abs() <= FAR_LOAD && (s.y - centre.y).abs() <= FAR_LOAD;
    let before = field.sectors.len();
    field.sectors.retain(|id, _| near(SectorCoord::from_id(*id)));
    let mut changed = before != field.sectors.len();
    for dy in -FAR_LOAD..=FAR_LOAD {
        for dx in -FAR_LOAD..=FAR_LOAD {
            let s = SectorCoord::new(centre.x + dx, centre.y + dy);
            if !s.valid() || field.sectors.contains_key(&s.id()) {
                continue;
            }
            let path = info.root.join(fc3_formats::light_probes::cache_rel(s.id()));
            let probes = std::fs::read(&path).ok().and_then(|b| fc3_formats::light_probes::from_bytes(&b)).unwrap_or_default();
            field.sectors.insert(s.id(), Arc::new(probes));
            changed = true;
        }
    }
    if changed {
        field.loaded += 1;
    }
}

/// The basis directions in Bevy space.
fn basis() -> [Vec3; 4] {
    BASIS.map(|[x, y, z]| Vec3::new(x, z, -y))
}

/// The six sides' normals, in the irradiance volume's order of axes.
pub(crate) const SIDES: [Vec3; 6] = [Vec3::NEG_X, Vec3::X, Vec3::NEG_Y, Vec3::Y, Vec3::NEG_Z, Vec3::Z];

/// A second-order harmonic of light, per channel: L00 and L1 (Bevy axes).
#[derive(Clone, Copy, Default)]
struct Sh {
    l0: Vec3,
    l1: [Vec3; 3],
}

impl Sh {
    /// The light on a surface facing `n`, E/π.
    fn irradiance(&self, n: Vec3) -> Vec3 {
        let l1 = self.l1[0] * n.x + self.l1[1] * n.y + self.l1[2] * n.z;
        (self.l0 * Y00 + l1 * (2.0 / 3.0 * Y1)).max(Vec3::ZERO)
    }

    /// In Dunia's order of a probe's transfer (L00, y, z, x), one channel.
    fn dunia(&self, c: usize) -> [f32; 4] {
        // Dunia (x, y, z) is Bevy (x, -z, y).
        [self.l0[c], -self.l1[2][c], self.l1[1][c], self.l1[0][c]]
    }
}

/// The sky's light as a harmonic, from its colour by direction: above the
/// horizon only, or all round.
fn project(colour: &dyn Fn(Vec3) -> Vec3, above_only: bool) -> Sh {
    const ROWS: usize = 24;
    const COLS: usize = 48;
    let mut sh = Sh::default();
    for r in 0..ROWS {
        let theta = (r as f32 + 0.5) / ROWS as f32 * std::f32::consts::PI;
        let (st, ct) = theta.sin_cos();
        let d_omega = st * (std::f32::consts::PI / ROWS as f32) * (std::f32::consts::TAU / COLS as f32);
        for c in 0..COLS {
            let phi = (c as f32 + 0.5) / COLS as f32 * std::f32::consts::TAU;
            let dir = Vec3::new(st * phi.cos(), ct, st * phi.sin());
            if above_only && dir.y <= 0.0 {
                continue;
            }
            let l = colour(dir) * d_omega;
            sh.l0 += l * Y00;
            for a in 0..3 {
                sh.l1[a] += l * (Y1 * dir[a]);
            }
        }
    }
    sh
}

/// Relights the probes as the light changes.
#[allow(clippy::too_many_arguments)]
fn relight(
    env: Res<crate::sky::GameEnvironment>,
    tod: Res<crate::sky::TimeOfDay>,
    scene: Res<crate::sky::SceneExposure>,
    weather: Res<crate::clouds::Weather>,
    suns: Query<(&Transform, &DirectionalLight), With<crate::sky::Sun>>,
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    (daylight, host): (Res<crate::clouds::Daylight>, Option<NonSend<crate::story::StoryHost>>),
    mut field: ResMut<ProbeField>,
) {
    let (Some(e), Ok((sun_t, sun))) = (env.0.as_ref(), suns.single()) else { return };
    let cam = cams.single().ok().map(|c| c.translation());
    if field.sectors.is_empty() || e.sky_sun_side.0.is_empty() {
        return;
    }
    // The large interiors reaching into the probes' sectors, in the world
    // now (those of mission layers while their entity is).
    let shapes_on: Vec<usize> = match (field.centre, field.shapes.as_ref()) {
        (Some(c), Some(shapes)) => {
            let [x0, y0] = SectorCoord::new(c.x - FAR_LOAD, c.y - FAR_LOAD).origin();
            let [x1, y1] = SectorCoord::new(c.x + FAR_LOAD + 1, c.y + FAR_LOAD + 1).origin();
            let (lo, hi) = (Vec3::new(x0, -1e4, -y1), Vec3::new(x1, 1e4, -y0));
            shapes
                .iter()
                .enumerate()
                .filter(|(_, s)| s.meets(lo, hi))
                .filter(|(_, s)| {
                    s.interior.layer == fc3_formats::world::DEFAULT_MISSION_LAYER || host.as_ref().is_some_and(|h| h.story.is_loaded(s.interior.entity))
                })
                .map(|(i, _)| i)
                .collect()
        }
        _ => Vec::new(),
    };
    let to_luminance = crate::sky_probe::sky_to_luminance(&scene, &weather);
    let sun_dir = sun_t.translation.normalize_or(Vec3::Y);
    let sun_light = sun.color.to_linear().to_vec3() * sun.illuminance;
    let key = (field.loaded, tod.hours, weather.storm(), sun_light, to_luminance);
    if let Some((loaded, hours, storm, light, lum)) = field.lit_for {
        let hours_off = (key.1 - hours).abs().min(24.0 - (key.1 - hours).abs());
        let near = |a: f32, b: f32| (a - b).abs() <= 0.02 * a.abs().max(b.abs()).max(1e-3);
        if loaded == key.0
            && hours_off < 0.02
            && (key.2 - storm).abs() < 0.02
            && near(key.3.length(), light.length())
            && near(key.4, lum)
            && shapes_on == field.shapes_on
        {
            return;
        }
    }
    field.lit_for = Some(key);
    let shapes: Vec<(Arc<ShapeGrid>, [Vec3; 3])> = match (field.shapes.as_ref(), scene.unit) {
        (Some(all), Some(unit)) => shapes_on.iter().map(|&i| (all[i].clone(), crate::interiors::ambient_light(&all[i].interior, daylight.0, unit))).collect(),
        _ => Vec::new(),
    };
    field.shapes_on = shapes_on;
    let t = tod.hours / 24.0;
    // (The sky as it lights the open: the probes do the sheltering.)
    let colour = crate::sky_probe::sky_colour(e, t, tod.sun_dir(), weather.storm(), 0.0);
    let light = move |d: Vec3| colour(d) * to_luminance;
    let (above, all) = (project(&light, true), project(&light, false));
    // The probes' share of the sun's bounce: their scale and multiplier (the
    // sky's light has them already).
    let curve_or = |c: &fc3_formats::environment::Curve, v: f32| if c.0.is_empty() { v } else { c.at(t) };
    let sun_share = curve_or(&e.probe_scale, 1.0) * curve_or(&e.probe_hdr, 1.0);
    let d = [sun_dir.x, -sun_dir.z, sun_dir.y];
    let sun_sh = |c: usize| [sun_light[c] * Y00, sun_light[c] * Y1 * d[1], sun_light[c] * Y1 * d[2], sun_light[c] * Y1 * d[0]];
    let dot = |a: [i16; 4], b: [f32; 4]| (0..4).map(|i| a[i] as f32 / ONE * b[i]).sum::<f32>();
    let mut probes = Vec::new();
    let mut cells: HashMap<IVec3, Vec<u32>> = HashMap::new();
    let mut columns: HashMap<IVec2, Vec<u32>> = HashMap::new();
    for p in field.sectors.values().flat_map(|s| s.iter()) {
        let bounce: [Vec3; 4] = std::array::from_fn(|b| {
            Vec3::from_array(std::array::from_fn(|c| {
                let tr = p.transfer[c][b];
                ((dot(tr, sun_sh(c)) * sun_share + dot(tr, above.dunia(c))) / std::f32::consts::PI).max(0.0)
            }))
        });
        let pos = Vec3::from_array(p.pos);
        cells.entry((pos / REACH).floor().as_ivec3()).or_default().push(probes.len() as u32);
        columns.entry((Vec2::new(pos.x, pos.z) / FAR_REACH).floor().as_ivec2()).or_default().push(probes.len() as u32);
        probes.push(LitProbe { pos, sky: p.sky.map(|v| (v as f32 / OPEN_SKY).clamp(0.0, 1.0)), bounce });
    }
    let lit = Lit { probes, cells, columns, sky_above: SIDES.map(|n| above.irradiance(n)), sky_all: SIDES.map(|n| all.irradiance(n)), shapes };
    // `FC3_PROBE_DEBUG`: the probe nearest the camera, relit.
    if std::env::var_os("FC3_PROBE_DEBUG").is_some() {
        if let Some(c) = cam {
            for (s, light) in lit.shapes.iter().filter(|(s, _)| s.contains(c)) {
                info!("light probes: the camera is in {:016x}'s shape ({} triangles): floor {:?}, ceiling {:?}", s.interior.entity, s.tris.len(), light[0], light[1]);
            }
        }
        if let Some(p) = cam.and_then(|c| lit.probes.iter().min_by(|a, b| a.pos.distance_squared(c).total_cmp(&b.pos.distance_squared(c)))) {
            info!(
                "light probes: {} relit; nearest the camera at {:?}: sky seen {:?}, bounced {:?}; the sky's light up {:?}, side {:?}, down {:?} (all round: down {:?}); sun {:?}",
                lit.probes.len(),
                p.pos,
                p.sky,
                p.bounce,
                lit.sky_above[3],
                lit.sky_above[1],
                lit.sky_above[2],
                lit.sky_all[2],
                sun_light
            );
        }
    }
    field.lit = Some(Arc::new(lit));
    field.lit_version += 1;
}

/// The light on each of a voxel's six sides at `at`.
fn voxel(lit: &Lit, at: Vec3, basis: &[Vec3; 4]) -> [Vec3; 6] {
    if let Some(sides) = inside_shape(lit, at) {
        return sides;
    }
    let mut total = 0.0;
    let mut sky = [0.0f32; 3];
    let mut bounce = [Vec3::ZERO; 4];
    let mut nearest = f32::MAX;
    let cell = (at / REACH).floor().as_ivec3();
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let Some(ids) = lit.cells.get(&(cell + IVec3::new(dx, dy, dz))) else { continue };
                for &i in ids {
                    let p = &lit.probes[i as usize];
                    let d2 = p.pos.distance_squared(at);
                    if d2 > REACH * REACH {
                        continue;
                    }
                    nearest = nearest.min(d2);
                    // (The nearest the most, as the game's nearest-probe
                    // grid, softened.)
                    let w = 1.0 / (d2 + 1.0).powi(2);
                    total += w;
                    for b in 0..3 {
                        sky[b] += p.sky[b] * w;
                    }
                    for b in 0..4 {
                        bounce[b] += p.bounce[b] * w;
                    }
                }
            }
        }
    }
    if total <= 0.0 {
        return lit.sky_all;
    }
    let sky = sky.map(|s| s / total);
    let bounce = bounce.map(|b| b / total);
    let sure = 1.0 - crate::sky::smoothstep(SURE, REACH, nearest.sqrt());
    shade(lit, sky, bounce, sure, basis)
}

/// Inside a large interior (a cave): its own ambient, the floor colour on
/// what faces up, the ceiling's on what faces down.
fn inside_shape(lit: &Lit, at: Vec3) -> Option<[Vec3; 6]> {
    let (_, light) = lit.shapes.iter().find(|(s, _)| s.contains(at))?;
    Some(SIDES.map(|n| if n.y > 0.5 { light[0] } else if n.y < -0.5 { light[1] } else { light[2] }))
}

/// A far voxel's sides: from the probes in its column of ground (within
/// `FAR_REACH`), those about its height the most, out into the open sky over
/// `FAR_ABOVE` m above their tops, as the game's top-down occlusion map.
fn far_voxel(lit: &Lit, at: Vec3, basis: &[Vec3; 4]) -> [Vec3; 6] {
    if let Some(sides) = inside_shape(lit, at) {
        return sides;
    }
    let (mut total, mut sky, mut bounce) = (0.0, [0.0f32; 3], [Vec3::ZERO; 4]);
    let (mut nearest, mut top) = (f32::MAX, f32::MIN);
    let column = (Vec2::new(at.x, at.z) / FAR_REACH).floor().as_ivec2();
    for dz in -1..=1 {
        for dx in -1..=1 {
            let Some(ids) = lit.columns.get(&(column + IVec2::new(dx, dz))) else { continue };
            for &i in ids {
                let p = &lit.probes[i as usize];
                let h2 = (p.pos.x - at.x).powi(2) + (p.pos.z - at.z).powi(2);
                if h2 > FAR_REACH * FAR_REACH {
                    continue;
                }
                nearest = nearest.min(h2);
                top = top.max(p.pos.y);
                // (A canopy's tops for a voxel in the leaves, the ground's
                // for one under them.)
                let dy = (p.pos.y - at.y) * 0.5;
                let w = 1.0 / (h2 + dy * dy + 4.0);
                total += w;
                for b in 0..3 {
                    sky[b] += p.sky[b] * w;
                }
                for b in 0..4 {
                    bounce[b] += p.bounce[b] * w;
                }
            }
        }
    }
    if total <= 0.0 {
        return lit.sky_all;
    }
    let open = crate::sky::smoothstep(0.0, FAR_ABOVE, at.y - top);
    let sure = (1.0 - crate::sky::smoothstep(FAR_REACH * 0.5, FAR_REACH, nearest.sqrt())) * (1.0 - open);
    shade(lit, sky.map(|s| s / total), bounce.map(|b| b / total), sure, basis)
}

/// A voxel's sides from what its probes see of the sky and bounce, as sure
/// of them as `sure` (the open sky's light the rest).
fn shade(lit: &Lit, sky: [f32; 3], bounce: [Vec3; 4], sure: f32, basis: &[Vec3; 4]) -> [Vec3; 6] {
    std::array::from_fn(|f| {
        let n = SIDES[f];
        let up: [f32; 3] = std::array::from_fn(|b| n.dot(basis[b]).max(0.0).powi(2));
        let weight = up[0] + up[1] + up[2];
        let seen = if weight > 1e-3 { (up[0] * sky[0] + up[1] * sky[1] + up[2] * sky[2]) / weight } else { (sky[0] + sky[1] + sky[2]) / 3.0 };
        let down = n.dot(basis[3]) * 0.5 + 0.5;
        let bounced = bounce[0] * up[0] + bounce[1] * up[1] + bounce[2] * up[2] + bounce[3] * down;
        lit.sky_all[f].lerp(lit.sky_above[f] * seen + bounced, sure)
    })
}

/// A volume's texels (its voxels about `centre`, the ambient cubes laid out
/// as `bevy_pbr::irradiance_volume` has them), half floats.
fn build(lit: &Lit, centre: Vec3, kind: Kind) -> Vec<u8> {
    let [rx, ry, rz] = kind.res;
    let basis = basis();
    let mut out = vec![0u8; rx * 2 * ry * 3 * rz * 8];
    for k in 0..rz {
        for j in 0..ry {
            for i in 0..rx {
                let at = centre + (Vec3::new(i as f32, j as f32, k as f32) + 0.5 - Vec3::new(rx as f32, ry as f32, rz as f32) * 0.5) * kind.cell;
                let sides = if kind.far { far_voxel(lit, at, &basis) } else { voxel(lit, at, &basis) };
                for (f, c) in sides.iter().enumerate() {
                    put_side(&mut out, kind.res, [i, j, k], f, *c);
                }
            }
        }
    }
    out
}

/// Writes the light on a voxel's side (`SIDES[f]`, the way the surface
/// faces) into an irradiance volume's texels (`res` voxels, half floats).
/// (Bevy's shader reads the second half of a slice's rows for a normal
/// facing the negative way, whatever its docs say: the sides facing
/// negative go there. They had gone in the first, and up-facing ground took
/// the ground's dim bounce, ceilings and undersides the sky.)
pub(crate) fn put_side(out: &mut [u8], res: [usize; 3], [i, j, k]: [usize; 3], f: usize, c: Vec3) {
    let [rx, ry, rz] = res;
    let t = j + if f % 2 == 0 { ry } else { 0 };
    let p = k + (f / 2) * rz;
    let at = ((p * 2 * ry + t) * rx + i) * 8;
    let c = c.min(Vec3::splat(60_000.0));
    for (n, v) in [c.x, c.y, c.z, 1.0].into_iter().enumerate() {
        out[at + n * 2..at + n * 2 + 2].copy_from_slice(&half::f16::from_f32(v).to_le_bytes());
    }
}

/// Builds the volumes about the camera afresh (off the main thread) as it
/// moves on or the probes are relit, and puts each one built in place.
#[allow(clippy::too_many_arguments)]
fn rebuild(
    mut commands: Commands,
    mut field: ResMut<ProbeField>,
    mut images: ResMut<Assets<Image>>,
    cams: Query<(&GlobalTransform, Option<&bevy::light::GeneratedEnvironmentMapLight>), With<PlayerCamera>>,
    indoors: Res<crate::sky::Indoors>,
    mut volumes: Query<(&mut Transform, &mut IrradianceVolume)>,
    mut live: ResMut<ProbesLive>,
) {
    if live.0 != field.near.volume.is_some() {
        live.0 = field.near.volume.is_some();
    }
    let Ok((cam, env_light)) = cams.single() else { return };
    // (As bright as the sky's environment map, less its dimming under a
    // roof: the probes see to that.)
    let intensity = env_light.map(|env| env.intensity / (1.0 - 0.65 * indoors.0).max(0.05));
    let (lit, version) = (field.lit.clone(), field.lit_version);
    let field = &mut *field;
    static FAR_ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var("FC3_FAR_PROBES").as_deref() != Ok("0"));
    for (v, kind) in [(&mut field.near, NEAR), (&mut field.far, FAR)] {
        if kind.far && !*FAR_ON {
            continue;
        }
        place(v, kind, &mut commands, &mut images, &mut volumes, intensity);
        let Some(lit) = lit.clone() else { continue };
        let snap = (cam.translation() / kind.step).round().as_ivec3();
        if v.task.is_some() || v.built_for == Some((snap, version)) {
            continue;
        }
        let centre = snap.as_vec3() * kind.step;
        v.task = Some(AsyncComputeTaskPool::get().spawn(async move { (snap, version, build(&lit, centre, kind)) }));
    }
}

/// Puts a volume's finished build in place, and keeps its brightness.
fn place(
    v: &mut Volume,
    kind: Kind,
    commands: &mut Commands,
    images: &mut Assets<Image>,
    volumes: &mut Query<(&mut Transform, &mut IrradianceVolume)>,
    intensity: Option<f32>,
) {
    if let Some(mut task) = v.task.take() {
        match block_on(future::poll_once(&mut task)) {
            Some((snap, version, data)) => {
                let centre = snap.as_vec3() * kind.step;
                let [rx, ry, rz] = kind.res;
                let scale = Vec3::new(rx as f32, ry as f32, rz as f32) * kind.cell;
                match v.volume.clone() {
                    Some((e, image)) => {
                        if let Some(mut img) = images.get_mut(&image) {
                            img.data = Some(data);
                        }
                        if let Ok((mut t, _)) = volumes.get_mut(e) {
                            *t = Transform::from_translation(centre).with_scale(scale);
                        }
                    }
                    None => {
                        let image = images.add(Image::new(
                            Extent3d { width: rx as u32, height: (2 * ry) as u32, depth_or_array_layers: (3 * rz) as u32 },
                            TextureDimension::D3,
                            data,
                            TextureFormat::Rgba16Float,
                            RenderAssetUsages::all(),
                        ));
                        let e = commands
                            .spawn((
                                LightProbe { falloff: Vec3::splat(kind.falloff), ..default() },
                                // (The far one marked as lighting lightmapped
                                // meshes: `vendor/bevy_pbr` takes that for far.)
                                IrradianceVolume { voxels: image.clone(), intensity: 1.0, affects_lightmapped_meshes: kind.far },
                                Transform::from_translation(centre).with_scale(scale),
                            ))
                            .id();
                        info!("light probes: a {rx} × {ry} × {rz} volume of {} m voxels follows the camera", kind.cell);
                        v.volume = Some((e, image));
                    }
                }
                v.built_for = Some((snap, version));
            }
            None => v.task = Some(task),
        }
    }
    if let (Some((e, _)), Some(intensity)) = (v.volume.as_ref(), intensity) {
        if let Ok((_, mut iv)) = volumes.get_mut(*e) {
            if (iv.intensity - intensity).abs() > 0.005 {
                iv.intensity = intensity;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under open sky with nothing bounced, a voxel's sides take the sky's
    /// light from above; a probe seeing no sky leaves them only the bounce.
    #[test]
    fn a_voxel_takes_the_sky_its_probes_see() {
        let sky = |d: Vec3| if d.y > 0.0 { Vec3::ONE } else { Vec3::ZERO };
        let above = project(&sky, true);
        // Unit radiance over the upper hemisphere: E/π of 1 facing up, 0
        // facing down, a half on the side.
        assert!((above.irradiance(Vec3::Y).x - 1.0).abs() < 0.02);
        assert!(above.irradiance(Vec3::NEG_Y).x < 0.02);
        assert!((above.irradiance(Vec3::X).x - 0.5).abs() < 0.02);
        let lit = |seen: f32, bounce: Vec3| {
            let probes = vec![LitProbe { pos: Vec3::ZERO, sky: [seen; 3], bounce: [bounce; 4] }];
            let mut cells = HashMap::new();
            cells.insert(IVec3::ZERO, vec![0]);
            Lit { probes, cells, columns: HashMap::new(), sky_above: SIDES.map(|n| above.irradiance(n)), sky_all: SIDES.map(|n| above.irradiance(n) + Vec3::splat(0.3)), shapes: Vec::new() }
        };
        let open = voxel(&lit(1.0, Vec3::ZERO), Vec3::new(0.5, 0.5, 0.5), &basis());
        assert!((open[3].x - 1.0).abs() < 0.02, "{open:?}");
        let shut = voxel(&lit(0.0, Vec3::splat(0.2)), Vec3::new(0.5, 0.5, 0.5), &basis());
        // Facing up: the three upward bounces' weights, a third each.
        assert!((shut[3].x - 0.2).abs() < 0.02, "{shut:?}");
        // Facing down: the downward bounce alone.
        assert!((shut[2].x - 0.2).abs() < 0.02, "{shut:?}");
        // Far from any probe: the environment map's light.
        let far = voxel(&lit(0.0, Vec3::ZERO), Vec3::new(40.0, 0.0, 0.0), &basis());
        assert_eq!(far[3], lit(0.0, Vec3::ZERO).sky_all[3]);
    }

    /// A closed box's shape holds what's in it and not what's beside, above
    /// or below it; a voxel inside takes the interior's light.
    #[test]
    fn a_shape_holds_what_is_inside_it() {
        // A 10 m cube's twelve triangles, corners (0..10)³.
        let c = |i: u32| [(i & 1) as f32 * 10.0, ((i >> 1) & 1) as f32 * 10.0, ((i >> 2) & 1) as f32 * 10.0];
        let vertices: Vec<[f32; 3]> = (0..8).map(c).collect();
        let quads = [[0, 1, 3, 2], [4, 5, 7, 6], [0, 1, 5, 4], [2, 3, 7, 6], [0, 2, 6, 4], [1, 3, 7, 5]];
        let indices: Vec<u32> = quads.iter().flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]]).collect();
        let shape = fc3_formats::interiors::InteriorShape { vertices, indices, ..Default::default() };
        let grid = Arc::new(ShapeGrid::new(&shape));
        assert!(grid.contains(Vec3::new(5.0, 5.0, 5.0)) && grid.contains(Vec3::new(9.3, 0.4, 2.2)));
        assert!(!grid.contains(Vec3::new(5.0, 12.0, 5.0)) && !grid.contains(Vec3::new(5.0, -2.0, 5.0)) && !grid.contains(Vec3::new(13.0, 5.0, 5.0)));
        let light = [Vec3::splat(3.0), Vec3::splat(1.0), Vec3::splat(2.0)];
        let lit = Lit { probes: Vec::new(), cells: HashMap::new(), columns: HashMap::new(), sky_above: [Vec3::ZERO; 6], sky_all: [Vec3::splat(9.0); 6], shapes: vec![(grid, light)] };
        let inside = voxel(&lit, Vec3::new(5.0, 5.0, 5.0), &basis());
        assert_eq!((inside[3], inside[2], inside[0]), (light[0], light[1], light[2]));
        assert_eq!(voxel(&lit, Vec3::new(5.0, 15.0, 5.0), &basis())[3], Vec3::splat(9.0));
    }
}
