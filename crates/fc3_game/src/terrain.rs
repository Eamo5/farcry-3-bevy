//! Streams terrain sectors from the converted cache around the viewer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use fc3_formats::cache;
use fc3_formats::terrain::{SectorCoord, TerrainSector};

use crate::cache_assets::{cache_path, terrain_mesh, Fc3Terrain, TERRAIN_LODS};
use crate::data::{to_dunia, CacheInfo};

/// Radius (in sectors) of terrain kept loaded around the viewer.
pub const TERRAIN_RADIUS: i32 = 16;

/// The radius (sectors) of streamed terrain at the current view distance
/// (beyond, `far_terrain` stands in).
pub fn terrain_radius(settings: &crate::settings::Settings) -> i32 {
    (TERRAIN_RADIUS as f32 * settings.distance_scale()).round() as i32
}

pub struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainStreamer>()
            .init_resource::<Heightfield>()
            .add_systems(Update, (request_terrain, spawn_terrain).chain().in_set(TerrainStreaming).run_if(resource_exists::<CacheInfo>));
    }
}

/// Terrain streaming systems; anything attaching to terrain chunks runs after.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TerrainStreaming;

/// Marks the entity whose position drives world streaming.
#[derive(Component)]
pub struct StreamingAnchor;

#[derive(Component)]
pub struct TerrainChunk {
    pub coord: SectorCoord,
}

/// CPU heights of every loaded sector, for gameplay queries (cheap to
/// clone for background work: the sectors are shared).
#[derive(Resource, Default, Clone)]
pub struct Heightfield {
    pub sectors: HashMap<SectorCoord, Arc<TerrainSector>>,
}

impl Heightfield {
    /// Terrain height (Bevy Y) at a Bevy-space position, if that sector is loaded.
    pub fn height_at(&self, pos: Vec3) -> Option<f32> {
        let d = to_dunia(pos);
        let c = SectorCoord::containing(d[0], d[1]);
        let s = self.sectors.get(&c)?;
        let o = c.origin();
        // No ground where it opens into a cave.
        if s.hole_at(d[0] - o[0], d[1] - o[1]) {
            return None;
        }
        Some(s.sample(d[0] - o[0], d[1] - o[1]))
    }

    /// Whether a loaded sector is cut away entirely (no ground at all, so no
    /// terrain collider: what lies under it, a wreck, a cave, is the floor).
    pub fn cut_away(&self, c: SectorCoord) -> bool {
        self.sectors.get(&c).is_some_and(|s| !s.holes.is_empty() && s.holes.iter().all(|h| *h))
    }
}

struct Slot {
    handle: Handle<Fc3Terrain>,
    entity: Option<Entity>,
    lod: usize,
    /// When the asset came in (see `cache_assets::ready`).
    since: Option<f32>,
    /// The finer meshes built so far (by LOD; the coarsest comes with the
    /// asset), and the one being built.
    fine: [Option<Handle<Mesh>>; TERRAIN_LODS - 1],
    building: Option<(usize, Task<Mesh>)>,
}

/// Finer terrain meshes started building, and handed to the GPU, per frame
/// (within the upload budget, see `upload_budget`): crossing into a new
/// sector brings a row of them, and all at once they held the render thread
/// up by several milliseconds.
const FINE_STARTS_PER_FRAME: usize = 8;
const FINE_ADDS_PER_FRAME: usize = 6;

#[derive(Resource, Default)]
pub struct TerrainStreamer {
    slots: HashMap<SectorCoord, Slot>,
    pub center: Option<SectorCoord>,
}

impl TerrainStreamer {
    /// Sectors requested but not yet displayed.
    pub fn pending_count(&self) -> usize {
        self.slots.values().filter(|s| s.entity.is_none()).count()
    }

    pub fn loaded_count(&self) -> usize {
        self.slots.values().filter(|s| s.entity.is_some()).count()
    }
}

/// A sector's level of detail at a distance (sectors), by terrain quality:
/// High keeps the finest within 3 and the middle within 8.
fn lod_for(dist: i32, quality: crate::settings::Quality) -> usize {
    use crate::settings::Quality;
    let (fine, middle) = match quality {
        Quality::Low => (1, 5),
        Quality::Medium => (2, 6),
        _ => (3, 8),
    };
    if dist <= fine {
        0
    } else if dist <= middle {
        1
    } else {
        2
    }
}

fn request_terrain(
    mut streamer: ResMut<TerrainStreamer>,
    settings: Res<crate::settings::Settings>,
    info: Res<CacheInfo>,
    server: Res<AssetServer>,
    anchor: Query<&GlobalTransform, With<StreamingAnchor>>,
    mut commands: Commands,
    mut heights: ResMut<Heightfield>,
) {
    let Ok(anchor) = anchor.single() else { return };
    let d = to_dunia(anchor.translation());
    let center = SectorCoord::containing(d[0], d[1]);
    if streamer.center == Some(center) && !streamer.slots.is_empty() && !settings.is_changed() {
        return;
    }
    streamer.center = Some(center);
    let mut keep = HashSet::new();
    let radius = terrain_radius(&settings);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let c = SectorCoord::new(center.x + dx, center.y + dy);
            if !c.valid() || !info.terrain.contains(&c.id()) {
                continue;
            }
            keep.insert(c);
            let lod = lod_for(dx.abs().max(dy.abs()), settings.terrain);
            let slot = streamer.slots.entry(c).or_insert_with(|| Slot {
                handle: server.load(cache_path(&cache::terrain_rel(c.id()))),
                entity: None,
                lod,
                since: None,
                fine: Default::default(),
                building: None,
            });
            slot.lod = lod;
        }
    }
    streamer.slots.retain(|c, slot| {
        if keep.contains(c) {
            return true;
        }
        if let Some(e) = slot.entity {
            commands.entity(e).despawn();
        }
        heights.sectors.remove(c);
        false
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_terrain(
    time: Res<Time>,
    mut streamer: ResMut<TerrainStreamer>,
    server: Res<AssetServer>,
    terrains: Res<Assets<Fc3Terrain>>,
    mut commands: Commands,
    mut heights: ResMut<Heightfield>,
    mut shown: Query<&mut Mesh3d, With<TerrainChunk>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (mut starts, mut adds) = (FINE_STARTS_PER_FRAME, FINE_ADDS_PER_FRAME);
    for (c, slot) in streamer.slots.iter_mut() {
        let Some(t) = terrains.get(&slot.handle) else { continue };
        // A finer mesh built: hand it over.
        if adds > 0 && crate::upload_budget::has_room() {
            if let Some((l, task)) = &mut slot.building {
                if let Some(mesh) = block_on(future::poll_once(task)) {
                    crate::upload_budget::spend(mesh.get_vertex_buffer_size() + mesh.get_index_buffer_bytes().map_or(0, |b| b.len()));
                    slot.fine[*l] = Some(meshes.add(mesh));
                    slot.building = None;
                    adds -= 1;
                }
            }
        }
        // The level of detail wanted, or meanwhile the nearest one built
        // (finer first); the others go once it shows.
        let (want, coarse) = (slot.lod, TERRAIN_LODS - 1);
        if want < coarse && slot.fine[want].is_none() && slot.building.is_none() && starts > 0 {
            let (sector, c, step) = (t.sector.clone(), *c, 1 << want);
            slot.building = Some((want, AsyncComputeTaskPool::get().spawn(async move { terrain_mesh(&sector, c, step) })));
            starts -= 1;
        }
        let mesh = if want == coarse {
            t.coarse.clone()
        } else {
            (want..coarse).chain((0..want).rev()).find_map(|l| slot.fine[l].clone()).unwrap_or_else(|| t.coarse.clone())
        };
        if want == coarse || slot.fine[want].is_some() {
            for (l, fine) in slot.fine.iter_mut().enumerate() {
                if l != want {
                    *fine = None;
                }
            }
        }
        match slot.entity {
            Some(e) => {
                if let Ok(mut m) = shown.get_mut(e) {
                    if m.0 != mesh {
                        m.0 = mesh;
                    }
                }
            }
            None => {
                if !crate::cache_assets::ready(&server, &slot.handle, &mut slot.since, time.elapsed_secs()) {
                    continue;
                }
                let o = c.origin();
                let mut e = commands.spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(t.material.clone()),
                    Transform::from_xyz(o[0], 0.0, -o[1]),
                    TerrainChunk { coord: *c },
                    Name::new(format!("terrain {}", c.id())),
                ));
                if let Some(original) = &t.original {
                    e.insert(crate::original_shaders::TerrainSwap { standard: t.material.clone(), original: original.clone() });
                }
                slot.entity = Some(e.id());
                heights.sectors.insert(*c, t.sector.clone());
            }
        }
    }
}
