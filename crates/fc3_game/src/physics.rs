//! Static collision for terrain and world objects.
//!
//! Terrain sectors near the player get heightfield colliders; placed objects
//! get trimesh colliders built once per model and shared between instances:
//! from the game's own collision where the model ships one (its `.hkx`,
//! converted to `collision/<hash>.col`, see `docs/physics.md`), else from the
//! render mesh's coarsest detail level without its see-through, undrawn,
//! water and foliage parts.

use std::collections::{HashMap, HashSet};

use avian3d::prelude::*;
use bevy::prelude::*;
use fc3_formats::terrain::{SectorCoord, TerrainSector, GRID, SECTOR_SIZE};

use crate::cache_assets::Fc3Model;
use crate::data::to_dunia;
use crate::objects::WorldObject;
use crate::terrain::{StreamingAnchor, TerrainChunk};

/// Terrain sectors within this Chebyshev radius of the player get colliders.
pub const TERRAIN_PHYSICS_RADIUS: i32 = 2;
/// Objects smaller than this (bounding radius, m) never block movement.
const MIN_COLLIDER_RADIUS: f32 = 0.35;

pub struct WorldPhysicsPlugin;

impl Plugin for WorldPhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugins::default())
            .insert_resource(Gravity(Vec3::NEG_Y * 9.81))
            .init_resource::<ModelColliders>()
            .add_systems(OnExit(crate::menu::GameState::Loading), load_intangible)
            .add_systems(
                Update,
                (terrain_colliders, object_colliders)
                    .after(crate::terrain::TerrainStreaming)
                    .after(crate::objects::ObjectStreaming)
                    .after(crate::vegetation::VegStreaming),
            );
    }
}

/// Marks the child entity holding a terrain sector's heightfield.
#[derive(Component)]
pub struct TerrainCollider;

/// The placed object's collision has been considered, including models that
/// intentionally have no blocking geometry.
#[derive(Component)]
pub struct ObjectCollisionReady;

/// Builds the collider for one sector's ground: a heightfield, or a triangle
/// mesh with the cave mouths left open when the sector has any. The collider
/// entity must be placed at the sector centre, i.e. local `(32, 0, -32)` from
/// the terrain chunk.
pub fn sector_heightfield(s: &TerrainSector) -> Collider {
    if !s.holes.is_empty() {
        return sector_trimesh(s);
    }
    // Rows run along X, columns along Z (centred). Chunk-local z = -row.
    let heights: Vec<Vec<f32>> = (0..GRID).map(|i| (0..GRID).map(|j| s.height(i, GRID - 1 - j)).collect()).collect();
    Collider::heightfield(heights, Vec3::new(SECTOR_SIZE, 1.0, SECTOR_SIZE))
}

/// The sector's ground as triangles (same split as the render mesh), minus holes.
fn sector_trimesh(s: &TerrainSector) -> Collider {
    let half = SECTOR_SIZE * 0.5;
    let vertices: Vec<Vec3> = (0..GRID * GRID)
        .map(|i| {
            let (col, row) = (i % GRID, i / GRID);
            Vec3::new(col as f32 - half, s.height(col, row), half - row as f32)
        })
        .collect();
    let cells = fc3_formats::terrain::CELLS;
    let idx = |c: usize, r: usize| (r * GRID + c) as u32;
    let mut triangles = Vec::with_capacity(cells * cells * 2);
    for row in 0..cells {
        for col in 0..cells {
            if s.holes[row * cells + col] {
                continue;
            }
            let (a, b, c, d) = (idx(col, row), idx(col + 1, row), idx(col + 1, row + 1), idx(col, row + 1));
            triangles.push([a, b, c]);
            triangles.push([a, c, d]);
        }
    }
    Collider::trimesh(vertices, triangles)
}

pub fn heightfield_offset() -> Vec3 {
    Vec3::new(SECTOR_SIZE * 0.5, 0.0, -SECTOR_SIZE * 0.5)
}

fn terrain_colliders(
    mut commands: Commands,
    heights: Res<crate::terrain::Heightfield>,
    anchor: Query<&GlobalTransform, With<StreamingAnchor>>,
    vehicles: Query<&GlobalTransform, With<crate::vehicles::Vehicle>>,
    chunks: Query<(Entity, &TerrainChunk, Option<&Children>)>,
    colliders: Query<(), With<TerrainCollider>>,
) {
    let Ok(anchor) = anchor.single() else { return };
    let sector = |p: Vec3| {
        let d = to_dunia(p);
        SectorCoord::containing(d[0], d[1])
    };
    let center = sector(anchor.translation());
    // Vehicles out past the player's colliders (patrols) need ground too.
    let carried: Vec<SectorCoord> = vehicles.iter().map(|v| sector(v.translation())).collect();
    let within = |c: SectorCoord, o: SectorCoord, r: i32| (c.x - o.x).abs().max((c.y - o.y).abs()) <= r;
    for (entity, chunk, children) in &chunks {
        let near = within(chunk.coord, center, TERRAIN_PHYSICS_RADIUS) || carried.iter().any(|v| within(chunk.coord, *v, 1));
        let existing = children.and_then(|c| c.iter().find(|e| colliders.contains(*e)));
        match (near, existing) {
            (true, None) => {
                let Some(sector) = heights.sectors.get(&chunk.coord) else { continue };
                // (A sector cut away entirely has no ground at all.)
                if heights.cut_away(chunk.coord) {
                    continue;
                }
                commands.spawn((
                    RigidBody::Static,
                    sector_heightfield(sector),
                    Transform::from_translation(heightfield_offset()),
                    TerrainCollider,
                    ChildOf(entity),
                ));
            }
            (false, Some(e)) => commands.entity(e).despawn(),
            _ => {}
        }
    }
}

/// Trimesh colliders shared by every instance of a model, and the world
/// objects still waiting for theirs.
#[derive(Resource, Default)]
pub struct ModelColliders {
    by_model: HashMap<AssetId<Fc3Model>, Option<Collider>>,
    waiting: std::collections::VecDeque<Entity>,
    /// Placed entities with no body in the game (a model and no physics
    /// component: plants, vines, wires, decals): nothing to bump into.
    intangible: HashSet<u64>,
}

fn load_intangible(info: Res<crate::data::CacheInfo>, mut cache: ResMut<ModelColliders>) {
    let text = std::fs::read_to_string(info.root.join(fc3_formats::cache::INTANGIBLE_REL)).unwrap_or_default();
    cache.intangible = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
}

/// World objects given colliders per frame: a sector's worth at once (some
/// hundreds, each added to the broad phase) held up a frame by 8 ms. The
/// nearest are done first.
const COLLIDERS_PER_FRAME: usize = 150;

impl ModelColliders {
    pub fn get(&mut self, id: AssetId<Fc3Model>, model: &Fc3Model) -> Option<Collider> {
        self.by_model
            .entry(id)
            .or_insert_with(|| {
                if model.is_vegetation || model.radius() < MIN_COLLIDER_RADIUS {
                    return None;
                }
                let c = model.collision.as_ref()?;
                if c.triangles.is_empty() {
                    return None;
                }
                debug!("collider for {id:?}: {} triangles from the {}", c.triangles.len(), if c.from_havok { "game's collision" } else { "render mesh" });
                Some(Collider::trimesh(c.positions.clone(), c.triangles.clone()))
            })
            .clone()
    }
}

/// Gives newly spawned world objects their static colliders, a slice per
/// frame, nearest the player first.
fn object_colliders(
    mut commands: Commands,
    mut cache: ResMut<ModelColliders>,
    models: Res<Assets<Fc3Model>>,
    added: Query<Entity, Added<WorldObject>>,
    // Placed under roots at the origin, so their own transform is where they are.
    objects: Query<(&WorldObject, &Transform, Option<&crate::objects::PlacedId>, Has<crate::breakables::Broken>)>,
    players: Query<&Transform, With<crate::player::Player>>,
) {
    let fresh: Vec<Entity> = added.iter().collect();
    if !fresh.is_empty() {
        cache.waiting.extend(fresh);
        // Nearest first (they may be underfoot).
        if let Ok(p) = players.single() {
            let at = |e: &Entity| objects.get(*e).map_or(f32::MAX, |(_, t, _, _)| t.translation.distance_squared(p.translation));
            cache.waiting.make_contiguous().sort_by(|a, b| at(a).total_cmp(&at(b)));
        }
    }
    for _ in 0..COLLIDERS_PER_FRAME {
        let Some(entity) = cache.waiting.pop_front() else { break };
        let Ok((obj, _, placed, broken)) = objects.get(entity) else { continue };
        if placed.is_some_and(|p| cache.intangible.contains(&p.0)) {
            commands.entity(entity).try_insert(ObjectCollisionReady);
            continue;
        }
        let Some(model) = models.get(&obj.model) else {
            cache.waiting.push_back(entity);
            continue;
        };
        // (Broken already: what's left of it.)
        let collider = if broken { crate::breakables::broken_collider(model) } else { cache.get(obj.model.id(), model) };
        if let Some(collider) = collider {
            commands.entity(entity).try_insert((RigidBody::Static, collider));
        }
        commands.entity(entity).try_insert(ObjectCollisionReady);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::terrain::SectorCoord;

    #[test]
    fn object_collision_readiness_waits_for_assets_and_accepts_nonblocking_models() {
        let mut app = App::new();
        app.init_resource::<ModelColliders>()
            .init_resource::<Assets<Fc3Model>>()
            .add_systems(Update, object_colliders);
        let handle = app.world().resource::<Assets<Fc3Model>>().reserve_handle();
        let entity = app.world_mut().spawn((WorldObject { model: handle.clone(), entity_index: 0 }, Transform::IDENTITY)).id();
        app.update();
        assert!(app.world().get::<ObjectCollisionReady>(entity).is_none());
        assert!(app.world().resource::<ModelColliders>().waiting.contains(&entity));
        let model = |collision| Fc3Model {
            broken_collision: None,
            lods: Vec::new(), aabb_min: Vec3::splat(-2.0), aabb_max: Vec3::splat(2.0), collision,
            bones: Vec::new(), is_vegetation: false, inverse_bindposes: None,
        };
        app.world_mut().resource_mut::<Assets<Fc3Model>>().insert(handle.id(), model(Some(std::sync::Arc::new(crate::cache_assets::CollisionMesh {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Z], triangles: vec![[0, 2, 1]], from_havok: true,
        })))).unwrap();
        let empty = app.world_mut().resource_mut::<Assets<Fc3Model>>().add(model(None));
        let nonblocking = app.world_mut().spawn((WorldObject { model: empty, entity_index: 1 }, Transform::IDENTITY)).id();
        app.update();
        assert!(app.world().get::<ObjectCollisionReady>(entity).is_some());
        assert!(app.world().get::<Collider>(entity).is_some());
        assert!(app.world().get::<ObjectCollisionReady>(nonblocking).is_some());
        assert!(app.world().get::<Collider>(nonblocking).is_none());
        // A placed entity with no body in the game gets none, whatever its model.
        app.world_mut().resource_mut::<ModelColliders>().intangible.insert(7);
        let decoration = app.world_mut().spawn((WorldObject { model: handle.clone(), entity_index: 2 }, Transform::IDENTITY, crate::objects::PlacedId(7))).id();
        app.update();
        assert!(app.world().get::<ObjectCollisionReady>(decoration).is_some());
        assert!(app.world().get::<Collider>(decoration).is_none());
    }

    #[test]
    fn heightfield_matches_terrain_samples() {
        // A deliberately asymmetric surface so any axis flip is caught.
        let mut heights = Vec::with_capacity(GRID * GRID);
        for row in 0..GRID {
            for col in 0..GRID {
                heights.push(100.0 + col as f32 * 0.5 + row as f32 * 2.0);
            }
        }
        let s = TerrainSector {
            coord: SectorCoord::new(80, 80),
            min_height: 0.0,
            max_height: 300.0,
            material: String::new(),
            heights,
            normal_bytes: vec![],
            layers: [fc3_formats::terrain::NO_LAYER; 4],
            holes: vec![],
        };
        let collider = sector_heightfield(&s);
        let offset = heightfield_offset();
        for (col, row) in [(0.5f32, 0.5f32), (10.0, 50.0), (60.0, 3.0), (31.5, 31.5)] {
            // Chunk-local point for (col, row) is (col, h, -row).
            let origin = Vec3::new(col, 500.0, -row);
            let (dist, _) = collider.cast_ray(offset, Quat::IDENTITY, origin, Vec3::NEG_Y, 1000.0, true).expect("ray should hit heightfield");
            let hit_y = 500.0 - dist;
            let expected = s.sample(col, row);
            assert!((hit_y - expected).abs() < 0.05, "at ({col},{row}): collider {hit_y} vs terrain {expected}");
        }
    }
}
