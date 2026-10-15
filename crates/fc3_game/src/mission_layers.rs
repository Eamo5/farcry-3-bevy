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
    /// Back to a checkpoint: what the layers put in the world is put there
    /// afresh, their vehicles where they stand at the start (JAS_200's
    /// visitor's jeep had driven off along its way, and its crew, asked to
    /// board again, waited at its start for nothing).
    pub reset: bool,
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
    mut layers: ResMut<MissionLayers>,
    mut cache: Local<HashMap<String, Handle<Fc3Model>>>,
    server: Res<AssetServer>,
    sectors: Res<Assets<Fc3Sector>>,
    models: Res<Assets<Fc3Model>>,
    mut roots: Query<(Entity, &SectorObjects, Option<&mut SpawnedLayers>)>,
    mut commands: Commands,
    dressed: Res<crate::story_actors::DressedEntities>,
    world: Res<crate::pickups::WorldState>,
    host: Option<NonSend<crate::story::StoryHost>>,
    vehicles: Query<(Entity, &crate::vehicles::Vehicle, Has<crate::vehicles::MissionVehicle>)>,
) {
    let data = host.as_ref().map(|h| h.story.data());
    let reset = std::mem::take(&mut layers.reset);
    if reset {
        for (_, _, spawned) in &mut roots {
            if let Some(mut spawned) = spawned {
                for (_, e) in spawned.0.drain() {
                    crate::retire::retire(&mut commands, e);
                }
            }
        }
        for (e, _, mission) in &vehicles {
            if mission {
                commands.entity(e).despawn();
            }
        }
    }
    let mut live_vehicles: HashSet<u64> = vehicles.iter().filter(|(_, _, mission)| !(reset && *mission)).map(|(_, v, _)| v.story_id()).collect();
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
                if entity.is_some_and(|e| live_vehicles.contains(&e.id)) { continue; }
                if entity.is_some_and(|e| world.removed.contains(&e.id)) { continue; }
                if entity.is_some_and(|e| crate::objects::is_character(&e.class) || dressed.0.contains(&e.id)) {
                    continue;
                }
                let Some(model) = models.get(handle) else { continue };
                if model.lods.is_empty() {
                    continue;
                }
                let obj = if let Some((e, layout)) = entity.and_then(|e| {
                    data.as_ref()?
                        .entities
                        .get(&e.id)
                        .and_then(|entry| Some((fc3_story::vehicles::Layout::from_entry(entry)?, fc3_story::vehicles::max_health(entry))))
                        .map(|layout| (e, layout))
                }) {
                    let (layout, health) = layout;
                    if let Some(vehicle) = crate::vehicles::build_placed_vehicle(&mut commands, sector, p, model, e.id, &layout, health) {
                        live_vehicles.insert(e.id);
                        commands.entity(vehicle).insert(crate::vehicles::MissionVehicle(*layer));
                        vehicle
                    } else {
                        spawn_placement(&mut commands, lroot, sector, p, handle, model)
                    }
                } else { spawn_placement(&mut commands, lroot, sector, p, handle, model) };
                placed.insert(p.entity, obj);
                if let Some(e) = entity {
                    commands.entity(obj).insert(crate::objects::PlacedId(e.id));
                }
            }
            for (i, entity) in content.entities.iter().enumerate() {
                if let Some(kind) = layer_pickup(entity, &world.removed, data.as_deref()) {
                    let pickup = commands.spawn((crate::pickups::Pickup { kind, id: entity.id, visual: placed.get(&(i as u32)).copied() },
                        Transform::from_translation(Vec3::from_array(entity.translation)), Visibility::Visible, ChildOf(lroot))).id();
                    if kind == crate::pickups::PickupKind::Loot {
                        commands.entity(pickup).insert(crate::pickups::LootSource(entity.name.clone()));
                        // (Its own loot, when its entity names some.)
                        let own = data.as_deref().and_then(|d| d.entities.get(&entity.id)).and_then(|e| e.component("CFCXPawnLootComponent"));
                        if let Some(own) = own.and_then(|c| crate::pickups::OwnLoot::from_component(c)) {
                            commands.entity(pickup).insert(own);
                        }
                    }
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
    if data.is_some_and(|d| {
        !d.entities
            .get(&entity.id)
            .is_some_and(|e| e.component("CPickupCollectible").is_some() || e.component("CPickupAmmo").is_some() || e.component("CPickupWeapon").is_some())
    }) {
        return None;
    }
    let weapon = data.is_some_and(|d| d.entities.get(&entity.id).is_some_and(|e| e.component("CPickupWeapon").is_some()));
    match crate::pickups::classify(&entity.name) {
        Some(crate::pickups::Classified::Pickup(kind)) => Some(kind),
        Some(crate::pickups::Classified::Explosive) => None,
        // A mission's own pickup, named for the mission (a stash to hide
        // something in): picked up like loot, its own loot given.
        None if !entity.name.to_ascii_lowercase().starts_with("pickups/")
            && data.is_some_and(|d| d.entities.get(&entity.id).is_some_and(|e| e.component("CPickupCollectible").is_some())) =>
        {
            Some(crate::pickups::PickupKind::Loot)
        }
        // A weapon pickup we don't know as a weapon: the mission's own item.
        None if weapon => Some(crate::pickups::PickupKind::Mission),
        None => None,
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
