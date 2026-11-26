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
/// The sectors about the camera's whose probes are kept (Chebyshev).
const LOAD: i32 = 2;
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
    /// The sky's light (E/π, cd/m²) on each of the six sides: from above
    /// only, and with the ground's stand-in below as the environment map has.
    sky_above: [Vec3; 6],
    sky_all: [Vec3; 6],
}

#[derive(Resource, Default)]
struct ProbeField {
    sectors: HashMap<u32, Arc<Vec<Probe>>>,
    centre: Option<SectorCoord>,
    /// Bumped as sectors come and go.
    loaded: u32,
    lit: Option<Arc<Lit>>,
    /// What the probes were last relit for, and a count of relightings.
    lit_for: Option<(u32, f32, f32, Vec3, f32)>,
    lit_version: u32,
    built_for: Option<(IVec3, u32)>,
    task: Option<Task<(IVec3, u32, Vec<u8>)>>,
    volume: Option<(Entity, Handle<Image>)>,
}

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
    let near = |s: SectorCoord| (s.x - centre.x).abs() <= LOAD && (s.y - centre.y).abs() <= LOAD;
    let before = field.sectors.len();
    field.sectors.retain(|id, _| near(SectorCoord::from_id(*id)));
    let mut changed = before != field.sectors.len();
    for dy in -LOAD..=LOAD {
        for dx in -LOAD..=LOAD {
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
const SIDES: [Vec3; 6] = [Vec3::NEG_X, Vec3::X, Vec3::NEG_Y, Vec3::Y, Vec3::NEG_Z, Vec3::Z];

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
    mut field: ResMut<ProbeField>,
) {
    let (Some(e), Ok((sun_t, sun))) = (env.0.as_ref(), suns.single()) else { return };
    let cam = cams.single().ok().map(|c| c.translation());
    if field.sectors.is_empty() || e.sky_sun_side.0.is_empty() {
        return;
    }
    let to_luminance = crate::sky_probe::sky_to_luminance(&scene, &weather);
    let sun_dir = sun_t.translation.normalize_or(Vec3::Y);
    let sun_light = sun.color.to_linear().to_vec3() * sun.illuminance;
    let key = (field.loaded, tod.hours, weather.storm(), sun_light, to_luminance);
    if let Some((loaded, hours, storm, light, lum)) = field.lit_for {
        let hours_off = (key.1 - hours).abs().min(24.0 - (key.1 - hours).abs());
        let near = |a: f32, b: f32| (a - b).abs() <= 0.02 * a.abs().max(b.abs()).max(1e-3);
        if loaded == key.0 && hours_off < 0.02 && (key.2 - storm).abs() < 0.02 && near(key.3.length(), light.length()) && near(key.4, lum) {
            return;
        }
    }
    field.lit_for = Some(key);
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
    for p in field.sectors.values().flat_map(|s| s.iter()) {
        let bounce: [Vec3; 4] = std::array::from_fn(|b| {
            Vec3::from_array(std::array::from_fn(|c| {
                let tr = p.transfer[c][b];
                ((dot(tr, sun_sh(c)) * sun_share + dot(tr, above.dunia(c))) / std::f32::consts::PI).max(0.0)
            }))
        });
        let pos = Vec3::from_array(p.pos);
        cells.entry((pos / REACH).floor().as_ivec3()).or_default().push(probes.len() as u32);
        probes.push(LitProbe { pos, sky: p.sky.map(|v| (v as f32 / OPEN_SKY).clamp(0.0, 1.0)), bounce });
    }
    let lit = Lit { probes, cells, sky_above: SIDES.map(|n| above.irradiance(n)), sky_all: SIDES.map(|n| all.irradiance(n)) };
    // `FC3_PROBE_DEBUG`: the probe nearest the camera, relit.
    if std::env::var_os("FC3_PROBE_DEBUG").is_some() {
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

/// The volume's texels (`RES` voxels about `centre`, the ambient cubes laid
/// out as `bevy_pbr::irradiance_volume` has them), half floats.
fn build(lit: &Lit, centre: Vec3) -> Vec<u8> {
    let [rx, ry, rz] = RES;
    let basis = basis();
    let mut out = vec![0u8; rx * 2 * ry * 3 * rz * 8];
    for k in 0..rz {
        for j in 0..ry {
            for i in 0..rx {
                let at = centre + (Vec3::new(i as f32, j as f32, k as f32) + 0.5 - Vec3::new(rx as f32, ry as f32, rz as f32) * 0.5) * CELL;
                let sides = voxel(lit, at, &basis);
                for (f, c) in sides.iter().enumerate() {
                    let axis = f / 2;
                    let t = j + if f % 2 == 1 { ry } else { 0 };
                    let p = k + axis * rz;
                    let at = ((p * 2 * ry + t) * rx + i) * 8;
                    let c = c.min(Vec3::splat(60_000.0));
                    for (n, v) in [c.x, c.y, c.z, 1.0].into_iter().enumerate() {
                        out[at + n * 2..at + n * 2 + 2].copy_from_slice(&half::f16::from_f32(v).to_le_bytes());
                    }
                }
            }
        }
    }
    out
}

/// Builds the volume about the camera afresh (off the main thread) as it
/// moves a voxel or the probes are relit, and puts each one built in place.
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
    if live.0 != field.volume.is_some() {
        live.0 = field.volume.is_some();
    }
    let Ok((cam, env_light)) = cams.single() else { return };
    // The finished build, put in place.
    if let Some(mut task) = field.task.take() {
        match block_on(future::poll_once(&mut task)) {
            Some((snap, version, data)) => {
                let centre = snap.as_vec3() * CELL;
                let [rx, ry, rz] = RES;
                let scale = Vec3::new(rx as f32, ry as f32, rz as f32) * CELL;
                match field.volume.clone() {
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
                                LightProbe { falloff: Vec3::splat(FALLOFF), ..default() },
                                IrradianceVolume { voxels: image.clone(), intensity: 1.0, affects_lightmapped_meshes: false },
                                Transform::from_translation(centre).with_scale(scale),
                            ))
                            .id();
                        info!("light probes: a {rx} × {ry} × {rz} volume of {CELL} m voxels follows the camera");
                        field.volume = Some((e, image));
                    }
                }
                field.built_for = Some((snap, version));
            }
            None => field.task = Some(task),
        }
    }
    // (As bright as the sky's environment map, less its dimming under a
    // roof: the probes see to that.)
    if let (Some((e, _)), Some(env)) = (field.volume.as_ref(), env_light) {
        if let Ok((_, mut v)) = volumes.get_mut(*e) {
            let intensity = env.intensity / (1.0 - 0.65 * indoors.0).max(0.05);
            if (v.intensity - intensity).abs() > 0.005 {
                v.intensity = intensity;
            }
        }
    }
    let Some(lit) = field.lit.clone() else { return };
    let snap = (cam.translation() / CELL).round().as_ivec3();
    let version = field.lit_version;
    if field.task.is_some() || field.built_for == Some((snap, version)) {
        return;
    }
    let centre = snap.as_vec3() * CELL;
    field.task = Some(AsyncComputeTaskPool::get().spawn(async move { (snap, version, build(&lit, centre)) }));
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
            Lit { probes, cells, sky_above: SIDES.map(|n| above.irradiance(n)), sky_all: SIDES.map(|n| above.irradiance(n) + Vec3::splat(0.3)) }
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
}
