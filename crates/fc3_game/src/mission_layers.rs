//! Mission layers in the streamed world: the props and buildings a mission
//! switches on (see `story`), spawned into the loaded sectors while their
//! layer is present and removed when it goes. Models only layers use are
//! loaded the first time a layer needs them.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::cache_assets::{cache_path, Fc3Model, Fc3Sector};
use crate::objects::{spawn_placement, ObjectStreaming, SectorObjects};

pub struct MissionLayersPlugin;

impl Plugin for MissionLayersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MissionLayers>().add_systems(Update, stream_layers.after(ObjectStreaming));
    }
}

/// The mission layers whose entities are in the world now.
#[derive(Resource, Default)]
pub struct MissionLayers {
    pub present: HashSet<u64>,
}

/// A loaded sector's spawned layer contents (layer -> root).
#[derive(Component, Default)]
struct SpawnedLayers(HashMap<u64, Entity>);

/// A layer's share of a sector, spawned.
#[derive(Component)]
pub struct LayerObjects {
    pub layer: u64,
}

fn stream_layers(
    layers: Res<MissionLayers>,
    mut cache: Local<HashMap<String, Handle<Fc3Model>>>,
    server: Res<AssetServer>,
    sectors: Res<Assets<Fc3Sector>>,
    models: Res<Assets<Fc3Model>>,
    mut roots: Query<(Entity, &SectorObjects, Option<&mut SpawnedLayers>)>,
    mut commands: Commands,
    dressed: Res<crate::story_actors::DressedEntities>,
    world: Res<crate::pickups::WorldState>,
    host: Option<NonSend<crate::story::StoryHost>>,
) {
    let data = host.as_ref().map(|h| h.story.data());
    let mut budget = 2;
    for (root, so, spawned) in &mut roots {
        let Some(sector) = sectors.get(&so.sector) else { continue };
        if sector.layers.is_empty() {
            continue;
        }
        let Some(mut spawned) = spawned else {
            commands.entity(root).insert(SpawnedLayers::default());
            continue;
        };
        let have = &mut spawned.0;
        // Layers that went.
        have.retain(|layer, e| {
            let keep = layers.present.contains(layer);
            if !keep {
                crate::retire::retire(&mut commands, *e);
            }
            keep
        });
        for (layer, content) in &sector.layers {
            if budget == 0 || !layers.present.contains(layer) || have.contains_key(layer) || content.entities.is_empty() {
                continue;
            }
            // Its models, loading them the first time.
            let handles: Vec<Handle<Fc3Model>> = content
                .placements
                .iter()
                .map(|p| {
                    let path = sector.model_paths.get(p.model as usize).cloned().unwrap_or_default();
                    match sector.models.get(p.model as usize) {
                        Some(h) if *h != Handle::default() => h.clone(),
                        _ => cache.entry(path.clone()).or_insert_with(|| server.load(cache_path(&path))).clone(),
                    }
                })
                .collect();
            if !handles.iter().all(|h| crate::cache_assets::done(&server, h)) {
                continue;
            }
            budget -= 1;
            let lroot = commands
                .spawn((Transform::IDENTITY, Visibility::Visible, LayerObjects { layer: *layer }, ChildOf(root), Name::new(format!("layer {layer:#x}"))))
                .id();
            let mut placed = HashMap::new();
            for (p, handle) in content.placements.iter().zip(&handles) {
                // Characters are spawned as characters (`story_actors`).
                let entity = content.entities.get(p.entity as usize);
                if entity.is_some_and(|e| world.removed.contains(&e.id)) { continue; }
                if entity.is_some_and(|e| crate::objects::is_character(&e.class) || dressed.0.contains(&e.id)) {
                    continue;
                }
                let Some(model) = models.get(handle) else { continue };
                if model.lods.is_empty() {
                    continue;
                }
                let obj = spawn_placement(&mut commands, lroot, sector, p, handle, model);
                placed.insert(p.entity, obj);
                if let Some(e) = entity {
                    commands.entity(obj).insert(crate::objects::PlacedId(e.id));
                }
            }
            for (i, entity) in content.entities.iter().enumerate() {
                if let Some(kind) = layer_pickup(entity, &world.removed, data.as_deref()) {
                    commands.spawn((crate::pickups::Pickup { kind, id: entity.id, visual: placed.get(&(i as u32)).copied() },
                        Transform::from_translation(Vec3::from_array(entity.translation)), Visibility::Visible, ChildOf(lroot)));
                }
            }
            have.insert(*layer, lroot);
        }
    }
}

fn layer_pickup(entity: &fc3_formats::cache::CachedEntity, removed: &HashSet<u64>, data: Option<&fc3_story::data::StoryData>) -> Option<crate::pickups::PickupKind> {
    if removed.contains(&entity.id) { return None }
    // Some plants have a colocated _02 minimap marker. Only the original
    // collectible gets a transaction, not its marker or a similarly named prop.
    if data.is_some_and(|d| !d.entities.get(&entity.id).is_some_and(|e|
        e.component("CPickupCollectible").is_some() || e.component("CPickupAmmo").is_some())) { return None }
    match crate::pickups::classify(&entity.name)? {
        crate::pickups::Classified::Pickup(kind) => Some(kind),
        crate::pickups::Classified::Explosive => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_hunting_layer_plants_are_collectible_but_their_markers_and_removed_items_are_not() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let data = fc3_story::data::StoryData::load(&cache).unwrap();
        for (id, plant) in [(2077562518085057487, crate::syringes::Plant::ScrewPine),
            (2077840889587192723, crate::syringes::Plant::TorchGinger)] {
            let original = data.entities.get(&id).unwrap();
            let entity = fc3_formats::cache::CachedEntity { id, name: original.name.clone(), ..default() };
            assert!(matches!(layer_pickup(&entity, &HashSet::new(), Some(&data)), Some(crate::pickups::PickupKind::Plant(p)) if p == plant));
            assert!(layer_pickup(&entity, &HashSet::from([id]), Some(&data)).is_none());
            let marker = data.entities.get(&(id + 1)).unwrap();
            let entity = fc3_formats::cache::CachedEntity { id: id + 1, name: marker.name.clone(), ..default() };
            assert!(layer_pickup(&entity, &HashSet::new(), Some(&data)).is_none());
        }
    }
}
