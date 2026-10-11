//! Streams world-sector placements (props, buildings, rocks) from the cache.
//!
//! Each placement spawns one entity per LOD part with a `VisibilityRange`, so
//! LOD switching and distance culling happen on the GPU side with no
//! per-frame CPU work.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use fc3_formats::cache::{self, CachedEntity};
use fc3_formats::terrain::SectorCoord;

use crate::cache_assets::{cache_path, Fc3Model, Fc3Sector};
use crate::data::{to_dunia, CacheInfo};
use crate::terrain::StreamingAnchor;

/// Radius (in sectors) of placements kept loaded.
pub const OBJECT_RADIUS: i32 = 6;
/// Multiplier on the game's authored LOD distances.
pub const LOD_SCALE: f32 = 2.0;
/// Objects are never drawn beyond this, regardless of authored LODs.
pub const MAX_DRAW_DISTANCE: f32 = 420.0;

pub struct ObjectsPlugin;

impl Plugin for ObjectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ObjectStreamer>()
            .add_systems(Update, (request_objects, spawn_objects).chain().in_set(ObjectStreaming).run_if(resource_exists::<CacheInfo>))
            .add_systems(Update, near_debug)
            .add_message::<PlaceProp>()
            .add_systems(Update, place_props.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// `FC3_NEAR_DEBUG[=metres]`: logs the placed objects within 15 m (or that far) of the player once,
/// with their parts' materials (what's that grey thing?).
#[allow(clippy::type_complexity)]
fn near_debug(
    time: Res<Time>,
    mut done: Local<bool>,
    server: Res<AssetServer>,
    sectors: Res<Assets<Fc3Sector>>,
    players: Query<&GlobalTransform, With<crate::player::Player>>,
    objects: Query<(&GlobalTransform, &WorldObject, &ChildOf, &Children)>,
    roots: Query<&SectorObjects>,
    parts: Query<&MeshMaterial3d<StandardMaterial>>,
) {
    let Ok(radius) = std::env::var("FC3_NEAR_DEBUG") else { return };
    if *done || time.elapsed_secs() < 25.0 {
        return;
    }
    let radius: f32 = radius.parse().unwrap_or(15.0);
    *done = true;
    let Ok(pt) = players.single() else { return };
    for (gt, o, parent, children) in &objects {
        if gt.translation().distance(pt.translation()) > radius {
            continue;
        }
        let name = roots
            .get(parent.parent())
            .ok()
            .and_then(|r| sectors.get(&r.sector))
            .and_then(|s| s.entities.get(o.entity_index as usize))
            .map(|e| e.name.clone())
            .unwrap_or_default();
        let mats: Vec<String> = children.iter().filter_map(|c| parts.get(c).ok()).filter_map(|m| server.get_path(m.0.id()).map(|p| p.to_string())).collect();
        info!("near: {name} at {:.1?} model {:?} materials {mats:?}", gt.translation(), server.get_path(o.model.id()).map(|p| p.to_string()));
    }
}

/// Object streaming systems; anything attaching to placed objects runs after.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectStreaming;

/// A placed world entity.
#[derive(Component)]
pub struct WorldObject {
    pub model: Handle<Fc3Model>,
    pub entity_index: u32,
}

/// Root of everything spawned for one sector.
#[derive(Component)]
pub struct SectorObjects {
    pub coord: SectorCoord,
    pub sector: Handle<Fc3Sector>,
}

struct Slot {
    handle: Handle<Fc3Sector>,
    root: Option<Entity>,
    /// When the sector asset came in (see `cache_assets::ready`).
    since: Option<f32>,
}

#[derive(Resource, Default)]
pub struct ObjectStreamer {
    slots: HashMap<SectorCoord, Slot>,
    center: Option<SectorCoord>,
}

impl ObjectStreamer {
    pub fn pending_count(&self) -> usize {
        self.slots.values().filter(|s| s.root.is_none()).count()
    }

    /// The sector under the streaming centre, and whether it is in (or has
    /// no objects).
    pub fn centre(&self) -> Option<(SectorCoord, bool)> {
        self.center.map(|c| (c, self.slots.get(&c).is_none_or(|s| s.root.is_some())))
    }
}

fn request_objects(
    settings: Res<crate::settings::Settings>,
    mut streamer: ResMut<ObjectStreamer>,
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
    let mut keep = HashSet::new();
    let radius = (OBJECT_RADIUS as f32 * settings.distance_scale()).round() as i32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let c = SectorCoord::new(center.x + dx, center.y + dy);
            if !c.valid() || !info.objects.contains(&c.id()) {
                continue;
            }
            keep.insert(c);
            streamer.slots.entry(c).or_insert_with(|| Slot { handle: server.load(cache_path(&cache::sector_rel(c.id()))), root: None, since: None });
        }
    }
    streamer.slots.retain(|c, slot| {
        if keep.contains(c) {
            return true;
        }
        if let Some(e) = slot.root {
            // No longer a loaded sector to the systems that look for them.
            commands.entity(e).remove::<SectorObjects>();
            crate::retire::retire(&mut commands, e);
        }
        false
    });
}

/// Visibility range for LOD `i` of a model, from the authored switch distances.
fn lod_range(model: &Fc3Model, i: usize) -> Range<f32> {
    let end = |k: usize| (model.lods[k].distance * LOD_SCALE).min(MAX_DRAW_DISTANCE).max(1.0);
    let start = if i == 0 { 0.0 } else { end(i - 1) };
    let mut stop = end(i);
    // Large objects (buildings, cliffs) stay visible to the draw limit on their last LOD.
    if i + 1 == model.lods.len() && model.radius() > 6.0 {
        stop = MAX_DRAW_DISTANCE;
    }
    start..stop.max(start + 0.5)
}

fn spawn_objects(
    time: Res<Time>,
    mut streamer: ResMut<ObjectStreamer>,
    server: Res<AssetServer>,
    sectors: Res<Assets<Fc3Sector>>,
    models: Res<Assets<Fc3Model>>,
    mut commands: Commands,
    world: Res<crate::pickups::WorldState>,
    dressed: Res<crate::story_actors::DressedEntities>,
) {
    use crate::pickups::{classify, Classified, Explosive, Pickup};
    // FC3_STREAM_DEBUG: the sector under the streaming centre, once a second.
    if std::env::var("FC3_STREAM_DEBUG").is_ok() && (time.elapsed_secs() as u32) != ((time.elapsed_secs() - time.delta_secs()) as u32) {
        if let Some(c) = streamer.center {
            match streamer.slots.get(&c) {
                Some(slot) => {
                    let placements = sectors.get(&slot.handle).map(|s| s.placements.len());
                    let missing = sectors
                        .get(&slot.handle)
                        .map(|s| s.placements.iter().filter(|p| s.models.get(p.model as usize).is_none_or(|h| models.get(h).is_none())).count());
                    info!(
                        "stream-debug: sector {} root {:?} placements {placements:?} without a model {missing:?} state {:?}",
                        c.id(),
                        slot.root,
                        server.recursive_dependency_load_state(&slot.handle)
                    );
                }
                None => info!("stream-debug: sector {} has no slot", c.id()),
            }
        }
    }
    // Spawning is cheap, but bound it so a burst of loads can't stall a frame
    // (a sector can hold several hundred parts).
    let mut budget = 1;
    for (c, slot) in streamer.slots.iter_mut() {
        if slot.root.is_some() || budget == 0 {
            continue;
        }
        if !crate::cache_assets::ready(&server, &slot.handle, &mut slot.since, time.elapsed_secs()) {
            continue;
        }
        let Some(sector) = sectors.get(&slot.handle) else { continue };
        // Every model done one way or the other (not their textures, which
        // fill in later), or those placements would be skipped for good.
        if !sector.models.iter().filter(|h| **h != Handle::default()).all(|h| crate::cache_assets::done(&server, h)) {
            continue;
        }
        budget -= 1;
        let root = commands
            .spawn((
                Transform::IDENTITY,
                Visibility::Visible,
                SectorObjects { coord: *c, sector: slot.handle.clone() },
                Name::new(format!("objects {}", c.id())),
            ))
            .id();
        let mut placed: std::collections::HashMap<u32, Entity> = std::collections::HashMap::new();
        for p in &sector.placements {
            let entity_id = sector.entities.get(p.entity as usize).map(|e| e.id).unwrap_or(0);
            if world.removed.contains(&entity_id) {
                continue;
            }
            // Placed characters are spawned as characters, not statues in
            // their bind pose.
            if sector.entities.get(p.entity as usize).is_some_and(|e| is_character(&e.class)) || dressed.0.contains(&entity_id) {
                continue;
            }
            let Some(handle) = sector.models.get(p.model as usize) else { continue };
            let Some(model) = models.get(handle) else { continue };
            if model.lods.is_empty() {
                continue;
            }
            let obj = spawn_placement(&mut commands, root, sector, p, handle, model);
            commands.entity(obj).insert(PlacedId(entity_id));
            placed.insert(p.entity, obj);
            if let Some(Classified::Explosive) = sector.entities.get(p.entity as usize).and_then(|e| classify(&e.name)) {
                commands.entity(obj).insert((Explosive { id: entity_id }, crate::combat::Health::new(25.0)));
            }
            // Animal cages' doors give way to gunfire or a blast.
            if sector.entities.get(p.entity as usize).is_some_and(|e| e.name.to_ascii_lowercase().starts_with("animalcagedoor")) {
                commands.entity(obj).insert((crate::wildlife::CageDoor { id: entity_id }, crate::combat::Health::new(crate::wildlife::CAGE_DOOR_HEALTH)));
            }
        }
        // Pickups: interactable markers (with their placed mesh, if any).
        for (i, e) in sector.entities.iter().enumerate() {
            if world.removed.contains(&e.id) {
                continue;
            }
            if let Some(Classified::Pickup(kind)) = classify(&e.name) {
                let pickup = commands
                    .spawn((
                        Pickup { kind, id: e.id, visual: placed.get(&(i as u32)).copied() },
                        Transform::from_translation(Vec3::from_array(e.translation)),
                        Visibility::Visible,
                        ChildOf(root),
                    ))
                    .id();
                // (A container's own loot, by its prototype.)
                if kind == crate::pickups::PickupKind::Loot {
                    commands.entity(pickup).insert(crate::pickups::LootSource(e.name.clone()));
                }
            }
        }
        slot.root = Some(root);
    }
}

/// A prop an encounter sets out (its model, by path), under an id of its own.
#[derive(Message, Clone)]
pub struct PlaceProp {
    pub model: String,
    pub at: Vec3,
    pub rotation: Quat,
    pub id: u64,
    /// For a cage's door: the animal kept behind it (by archetype).
    pub caged: Option<String>,
}

/// A prop set out by an encounter (its id).
#[derive(Component)]
pub struct EncounterProp(pub u64);

/// Encounter props are taken away beyond this (m): the encounter sets them out
/// anew when it next spawns.
const PROP_KEEP: f32 = 360.0;

/// Sets out the props encounters ask for once their models are in (each id
/// once; given up after half a minute), as world objects (they take their
/// models' collision like any placed object), and takes far ones away.
fn place_props(
    time: Res<Time>,
    mut requests: MessageReader<PlaceProp>,
    mut pending: Local<Vec<(PlaceProp, Handle<Fc3Model>, f32)>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    existing: Query<(Entity, &EncounterProp, &Transform)>,
    players: Query<&Transform, With<crate::player::Player>>,
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    let live: std::collections::HashSet<u64> = existing.iter().map(|(_, p, _)| p.0).collect();
    for r in requests.read() {
        if !live.contains(&r.id) && !pending.iter().any(|(p, _, _)| p.id == r.id) {
            pending.push((r.clone(), server.load(cache_path(&cache::model_rel(&r.model))), now));
        }
    }
    pending.retain(|(r, handle, since)| {
        if !crate::cache_assets::done(&server, handle.id()) {
            return now - since < 30.0;
        }
        let Some(model) = models.get(handle).filter(|m| !m.lods.is_empty()) else { return false };
        let obj = spawn_model(&mut commands, handle, model, Transform::from_translation(r.at).with_rotation(r.rotation));
        commands.entity(obj).insert((EncounterProp(r.id), Name::new("encounter prop")));
        if std::env::var_os("FC3_ENCOUNTER_DEBUG").is_some() {
            info!("encounter: prop {} at {:.1?}", r.model, r.at);
        }
        // (An animal cage's door gives way like any other.)
        if r.model.to_ascii_lowercase().contains("cagedoor") {
            commands.entity(obj).insert((crate::wildlife::CageDoor { id: r.id }, crate::combat::Health::new(crate::wildlife::CAGE_DOOR_HEALTH)));
            if let Some(a) = &r.caged {
                commands.entity(obj).insert(crate::wildlife::Caged(a.clone()));
            }
        }
        false
    });
    if let Ok(pt) = players.single() {
        for (e, _, t) in &existing {
            if t.translation.distance(pt.translation) > PROP_KEEP {
                commands.entity(e).despawn();
            }
        }
    }
}

/// Spawns a model on its own (not from a sector), in its own materials,
/// as a world object at `transform`.
fn spawn_model(commands: &mut Commands, handle: &Handle<Fc3Model>, model: &Fc3Model, transform: Transform) -> Entity {
    let obj = commands.spawn((transform, Visibility::Visible, WorldObject { model: handle.clone(), entity_index: u32::MAX })).id();
    let small = model.radius() < 1.5;
    for (li, lod) in model.lods.iter().enumerate() {
        let range = lod_range(model, li);
        let fade = (range.end * 0.1).clamp(0.5, 8.0);
        let vis = VisibilityRange {
            start_margin: if li == 0 { 0.0..0.0 } else { (range.start - fade * 0.5)..(range.start + fade * 0.5) },
            end_margin: (range.end - fade * 0.5)..(range.end + fade * 0.5),
            use_aabb: false,
        };
        for part in &lod.parts {
            let mut e = commands.spawn((
                Mesh3d(part.mesh.clone()),
                MeshMaterial3d(part.material.clone()),
                vis.clone(),
                crate::original_shaders::PropPart,
                ChildOf(obj),
            ));
            if small || li > 1 {
                e.insert(NotShadowCaster);
            } else {
                e.insert(crate::shadow_lod::ShadowSize(model.radius()));
            }
        }
    }
    obj
}

/// Whether an entity class is a character (a pawn), whose placed model is
/// its bind pose.
/// A placed object's entity id (scenes move the props they hold).
#[derive(Component, Clone, Copy)]
pub struct PlacedId(pub u64);

pub fn is_character(class: &str) -> bool {
    class.contains("Pawn")
}

/// Spawns one placed object (its LOD parts with their visibility ranges)
/// under `root`.
pub fn spawn_placement(commands: &mut Commands, root: Entity, sector: &Fc3Sector, p: &cache::Placement, handle: &Handle<Fc3Model>, model: &Fc3Model) -> Entity {
    let transform = Transform { translation: Vec3::from_array(p.translation), rotation: Quat::from_array(p.rotation), scale: Vec3::splat(p.scale) };
    let obj = commands.spawn((transform, Visibility::Visible, WorldObject { model: handle.clone(), entity_index: p.entity }, ChildOf(root))).id();
    let small = model.radius() < 1.5;
    for (li, lod) in model.lods.iter().enumerate() {
        let range = lod_range(model, li);
        let fade = (range.end * 0.1).clamp(0.5, 8.0);
        let vis = VisibilityRange {
            start_margin: if li == 0 { 0.0..0.0 } else { (range.start - fade * 0.5)..(range.start + fade * 0.5) },
            end_margin: (range.end - fade * 0.5)..(range.end + fade * 0.5),
            use_aabb: false,
        };
        for part in &lod.parts {
            crate::camera::SPAWNED_OBJECTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut e = commands.spawn((
                Mesh3d(part.mesh.clone()),
                MeshMaterial3d(sector.part_material(p, part)),
                vis.clone(),
                crate::original_shaders::PropPart,
                ChildOf(obj),
            ));
            if small || li > 1 {
                e.insert(NotShadowCaster);
            } else {
                e.insert(crate::shadow_lod::ShadowSize(model.radius() * p.scale));
            }
        }
    }
    obj
}

/// Finds gameplay entities near a point (used by spawning systems).
pub fn entities_near<'a>(sectors: &'a Assets<Fc3Sector>, streamer: &'a ObjectStreamer, pos: Vec3, radius: f32) -> impl Iterator<Item = &'a CachedEntity> + 'a {
    streamer
        .slots
        .values()
        .filter_map(move |s| sectors.get(&s.handle))
        .flat_map(|s| s.entities.iter())
        .filter(move |e| Vec3::from_array(e.translation).distance(pos) <= radius)
}
