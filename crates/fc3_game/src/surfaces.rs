//! What the ground and walls are made of, for footstep and impact sounds.
//!
//! The game's sound events switch on a surface (`audio::SURFACE_GROUP`)
//! whose value comes from a logic material. Terrain gets it from the painted
//! layer that dominates the atlas mask at the spot (each layer names its
//! surface type), placed models from their collision files (what most of the
//! walkable area is, the odd patch that differs, and what the walls are),
//! characters are flesh, vehicles metal. The converter gathers it all into
//! `world/surfaces.bin`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use fc3_formats::cache::{self, ModelSurfaces, SurfaceTable};
use fc3_formats::terrain::{SectorCoord, NO_LAYER};

use crate::data::to_dunia;
use crate::objects::WorldObject;
use crate::terrain::Heightfield;
use crate::CacheInfo;

pub use fc3_formats::audio::SURFACE_GROUP;

pub struct SurfacesPlugin;

impl Plugin for SurfacesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Surfaces>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_surfaces)
            .add_systems(Update, surface_test.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// Surface switch values the game picks without looking anything up.
pub mod value {
    /// `Base/Default`, for ground nothing is known about.
    pub const DEFAULT: u32 = 0x0007_4C91;
    pub const FLESH: u32 = 0x0007_4C97;
    pub const VEHICLE_METAL: u32 = 0x0007_4C9E;
    pub const SHALLOW_WATER: u32 = 0x0007_4CB4;
    pub const DEEP_WATER: u32 = 0x0007_4CB3;
}

#[derive(Resource, Default)]
pub struct Surfaces {
    switch_values: Vec<u32>,
    layers: Vec<u8>,
    models: HashMap<u64, ModelSurfaces>,
    /// Terrain layer names, in the layer table's order.
    layer_names: Vec<String>,
    root: std::path::PathBuf,
    /// Terrain masks read so far, by atlas id (`None` when missing).
    masks: Mutex<HashMap<u32, Option<Arc<Vec<u8>>>>>,
}

/// Masks kept before the oldest are dropped (each is ~11 KB).
const MASKS_KEPT: usize = 96;

impl Surfaces {
    fn switch(&self, logic: u8) -> u32 {
        self.switch_values.get(logic as usize).copied().unwrap_or(0)
    }

    fn mask(&self, atlas: u32) -> Option<Arc<Vec<u8>>> {
        let mut masks = self.masks.lock().unwrap();
        if let Some(m) = masks.get(&atlas) {
            return m.clone();
        }
        if masks.len() >= MASKS_KEPT {
            masks.clear();
        }
        let m = std::fs::read(self.root.join(cache::atlas_rel(atlas, "mask"))).ok().map(Arc::new);
        masks.insert(atlas, m.clone());
        m
    }

    /// The painted layer that dominates the terrain at a Bevy position.
    fn terrain(&self, heights: &Heightfield, p: Vec3) -> u32 {
        self.terrain_logic(heights, p).map_or(0, |l| self.switch(l))
    }

    /// The logic material of the terrain's dominant layer at a position.
    fn terrain_logic(&self, heights: &Heightfield, p: Vec3) -> Option<u8> {
        self.layer_at(heights, p).and_then(|i| self.layers.get(i as usize)).copied()
    }

    /// Index (into the layer table) of the painted layer that dominates the
    /// terrain at a Bevy position.
    fn layer_at(&self, heights: &Heightfield, p: Vec3) -> Option<u8> {
        let d = to_dunia(p);
        let coord = SectorCoord::containing(d[0], d[1]);
        let sector = heights.sectors.get(&coord)?;
        let origin = coord.origin();
        let weights = self
            .mask(coord.atlas_coord().id())
            .and_then(|dds| {
                let size = fc3_formats::texture::dds_info(&dds)?.width;
                let (row, col) = fc3_formats::vegetation::atlas_pixel(coord, d[0] - origin[0], d[1] - origin[1], size);
                fc3_formats::texture::bc1_texel(&dds, col, row)
            })
            .map(|[r, g, b]| {
                let [r, g, b] = [r, g, b].map(|c| c as f32 / 255.0);
                [r, g, b, (1.0 - r - g - b).max(0.0)]
            })
            .unwrap_or([1.0, 0.0, 0.0, 0.0]);
        let best = (0..4).filter(|&i| sector.layers[i] != NO_LAYER).max_by(|&a, &b| weights[a].total_cmp(&weights[b]))?;
        Some(sector.layers[best])
    }

    /// A model's surface at a point on it (model space), facing `up`.
    fn model(&self, m: &ModelSurfaces, local: Vec3, up: bool) -> u32 {
        self.switch(self.model_logic(m, local, up))
    }

    /// A model's logic material at a point on it (model space), facing `up`.
    fn model_logic(&self, m: &ModelSurfaces, local: Vec3, up: bool) -> u8 {
        if !up {
            return m.wall;
        }
        let mut best: Option<(f32, u8)> = None;
        for (tri, logic) in &m.patches {
            let [a, b, c] = tri.map(Vec3::from_array);
            let Some(y) = height_in(a, b, c, local) else { continue };
            let off = (y - local.y).abs();
            if off < 0.4 && best.is_none_or(|(o, _)| off < o) {
                best = Some((off, *logic));
            }
        }
        best.map_or(m.walk, |b| b.1)
    }
}

/// Height of triangle `abc` above `p`'s horizontal position, if `p` is over it.
fn height_in(a: Vec3, b: Vec3, c: Vec3, p: Vec3) -> Option<f32> {
    let (v0, v1, v2) = (b.xz() - a.xz(), c.xz() - a.xz(), p.xz() - a.xz());
    let den = v0.perp_dot(v1);
    if den.abs() < 1e-8 {
        return None;
    }
    let s = v2.perp_dot(v1) / den;
    let t = v0.perp_dot(v2) / den;
    const EDGE: f32 = -0.02;
    (s >= EDGE && t >= EDGE && s + t <= 1.0 - EDGE).then(|| a.y + s * (b.y - a.y) + t * (c.y - a.y))
}

fn load_surfaces(mut surfaces: ResMut<Surfaces>, info: Res<CacheInfo>) {
    surfaces.root = info.root.clone();
    let table = match std::fs::read(info.root.join(cache::SURFACES_REL)).map_err(anyhow::Error::from).and_then(|b| SurfaceTable::from_bytes(&b)) {
        Ok(t) => t,
        Err(e) => {
            warn!("no surface table ({e:#}); footsteps and impacts use the default surface");
            return;
        }
    };
    info!("surfaces: {} logic materials, {} terrain layers, {} models", table.switch_values.len(), table.layers.len(), table.models.len());
    if let Ok(text) = std::fs::read_to_string(info.root.join(cache::TERRAIN_LAYERS_REL)) {
        surfaces.layer_names = cache::TerrainLayerTable::from_text(&text).layers.into_iter().map(|l| l.name).collect();
    }
    surfaces.switch_values = table.switch_values;
    surfaces.layers = table.layers;
    surfaces.models = table.models.into_iter().map(|m| (m.model, m)).collect();
}

/// What something hit is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    /// A logic material's id (`logicmaterials.lib`'s `disId`).
    Logic(u8),
    Flesh,
    Vehicle,
}

/// Looks up what was hit or stepped on.
#[derive(SystemParam)]
pub struct SurfaceProbe<'w, 's> {
    surfaces: Res<'w, Surfaces>,
    heights: Res<'w, Heightfield>,
    server: Res<'w, AssetServer>,
    objects: Query<'w, 's, (&'static WorldObject, &'static GlobalTransform)>,
    vehicles: Query<'w, 's, (), With<crate::vehicles::Vehicle>>,
    flesh: Query<'w, 's, (), Or<(With<crate::npc::Npc>, With<crate::wildlife::WildAnimal>, With<crate::player::Player>)>>,
}

impl SurfaceProbe<'_, '_> {
    /// The water's surface height over `p` (sea, river or lake).
    pub fn water(&self, p: Vec3) -> f32 {
        crate::water::surface_at(p, &self.heights)
    }

    /// Surface switch value of `entity` at `point` (world space), hit with
    /// surface normal `normal`; 0 when unknown (the sounds' default).
    pub fn at(&self, entity: Entity, point: Vec3, normal: Vec3) -> u32 {
        if self.flesh.contains(entity) {
            return value::FLESH;
        }
        if self.vehicles.contains(entity) {
            return value::VEHICLE_METAL;
        }
        if let Ok((obj, gt)) = self.objects.get(entity) {
            let model = self
                .server
                .get_path(obj.model.id())
                .and_then(|p| p.path().file_stem()?.to_str().and_then(|s| u64::from_str_radix(s, 16).ok()))
                .and_then(|h| self.surfaces.models.get(&h));
            return match model {
                Some(m) => self.surfaces.model(m, gt.affine().inverse().transform_point3(point), normal.y > 0.6),
                None => 0,
            };
        }
        self.surfaces.terrain(&self.heights, point)
    }

    /// What `entity` is made of at `point` (world space), hit with surface
    /// normal `normal`: a logic material (0, `Base/Default`, when unknown),
    /// or flesh or a vehicle's metal.
    pub fn material_at(&self, entity: Entity, point: Vec3, normal: Vec3) -> Material {
        if self.flesh.contains(entity) {
            return Material::Flesh;
        }
        if self.vehicles.contains(entity) {
            return Material::Vehicle;
        }
        if let Ok((obj, gt)) = self.objects.get(entity) {
            let model = self
                .server
                .get_path(obj.model.id())
                .and_then(|p| p.path().file_stem()?.to_str().and_then(|s| u64::from_str_radix(s, 16).ok()))
                .and_then(|h| self.surfaces.models.get(&h));
            return Material::Logic(model.map_or(0, |m| self.surfaces.model_logic(m, gt.affine().inverse().transform_point3(point), normal.y > 0.6)));
        }
        Material::Logic(self.surfaces.terrain_logic(&self.heights, point).unwrap_or(0))
    }

    /// The terrain's surface at a position (ignoring anything on it).
    pub fn terrain(&self, point: Vec3) -> u32 {
        self.surfaces.terrain(&self.heights, point)
    }

    /// The name of the painted terrain layer that dominates at a position
    /// (`North_Jungle_Grass`, `South_Coastal_Sand`...).
    pub fn terrain_layer(&self, point: Vec3) -> Option<&str> {
        let i = self.surfaces.layer_at(&self.heights, point)?;
        self.surfaces.layer_names.get(i as usize).map(String::as_str)
    }
}

/// `FC3_SURFACE_TEST`: 20 s in, probes the ground on a 2 m grid 60 m across
/// around the player and logs what was found, by kind of thing hit.
fn surface_test(
    time: Res<Time>,
    mut start: Local<Option<f32>>,
    mut done: Local<bool>,
    spatial: avian3d::prelude::SpatialQuery,
    probe: SurfaceProbe,
    players: Query<&GlobalTransform, With<crate::player::Player>>,
) {
    if *done || std::env::var("FC3_SURFACE_TEST").is_err() {
        return;
    }
    let now = time.elapsed_secs();
    if now - *start.get_or_insert(now) < 20.0 {
        return;
    }
    *done = true;
    let Ok(p) = players.single() else { return };
    let mut found: std::collections::BTreeMap<(String, u32), usize> = Default::default();
    let filter = avian3d::prelude::SpatialQueryFilter::default();
    for i in -15..=15 {
        for j in -15..=15 {
            let from = p.translation() + Vec3::new(i as f32 * 2.0, 40.0, j as f32 * 2.0);
            let Some(hit) = spatial.cast_ray(from, Dir3::NEG_Y, 120.0, true, &filter) else { continue };
            let point = from - Vec3::Y * hit.distance;
            let kind = match probe.objects.get(hit.entity) {
                Ok((obj, _)) => probe.server.get_path(obj.model.id()).map_or("object".to_string(), |p| p.path().display().to_string()),
                Err(_) => "terrain/other".to_string(),
            };
            *found.entry((kind, probe.at(hit.entity, point, hit.normal))).or_default() += 1;
        }
    }
    for ((kind, surface), n) in found {
        info!("surface-test: {kind} -> {surface:08x} x{n}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_over_triangle() {
        let (a, b, c) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(2.0, 1.0, 0.0), Vec3::new(0.0, 3.0, 2.0));
        assert_eq!(height_in(a, b, c, Vec3::new(0.5, 0.0, 0.5)), Some(1.5));
        assert_eq!(height_in(a, b, c, Vec3::new(1.5, 0.0, 1.5)), None);
    }
}
