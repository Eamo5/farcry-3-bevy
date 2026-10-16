//! Props scripts spawn from library archetypes (`SpawnEntityFromArchetype`:
//! a mission bomb's "ghost" spot, the charge, its remote): drawn with their
//! archetype's model where the script put them, under the story's id, so
//! the story's placements, hiding and removal reach them as they do any
//! placed object.

use bevy::prelude::*;

use crate::cache_assets::Fc3Model;
use crate::data::to_bevy;
use crate::story::StoryHost;

pub struct StoryPropsPlugin;

impl Plugin for StoryPropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, spawn_props.after(crate::story::StorySet).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A spawned entity the story placed: (id, archetype name, where, turned).
pub type SpawnedProp = (u64, String, [f32; 3], [f32; 3]);

/// The model an archetype draws (its graphic component's), if any.
pub(crate) fn archetype_model(archetype: &fc3_story::data::Entry) -> Option<String> {
    let mut model = None;
    archetype.component("CGraphicComponent")?.walk(&mut |o, _| {
        if model.is_none() {
            model = o.str_h(0x3182_766C).filter(|m| m.to_ascii_lowercase().ends_with(".xbg"));
        }
    });
    model
}

fn spawn_props(
    host: Option<NonSendMut<StoryHost>>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    info: Res<crate::data::CacheInfo>,
    mut pending: Local<Vec<(u64, Handle<Fc3Model>, Transform, Vec<Handle<StandardMaterial>>)>>,
    mut wearing: Local<Option<std::collections::HashMap<u64, Vec<String>>>>,
    mut commands: Commands,
) {
    let Some(mut host) = host else { return };
    if host.spawned_props.is_empty() && pending.is_empty() {
        return;
    }
    // The materials archetypes put on their models' slots (the cache's table).
    let wearing = wearing.get_or_insert_with(|| {
        std::fs::read_to_string(info.root.join(fc3_formats::story::ARCHETYPE_MATERIALS_REL))
            .map(|t| fc3_formats::story::archetype_materials_from_text(&t))
            .unwrap_or_default()
    });
    let data = host.story.data();
    for (id, archetype, at, angles) in std::mem::take(&mut host.spawned_props) {
        let Some(entry) = data.archetype_named(&archetype) else {
            debug!("story prop {id}: no archetype {archetype}");
            continue;
        };
        let Some(path) = archetype_model(entry) else {
            debug!("story prop {id}: {archetype} draws nothing");
            continue;
        };
        let slots = wearing
            .get(&entry.id)
            .map(|rels| rels.iter().map(|m| server.load(crate::cache_assets::cache_path(&fc3_formats::cache::material_rel(m)))).collect())
            .unwrap_or_default();
        let transform = Transform { translation: to_bevy(at), rotation: Quat::from_array(fc3_formats::world::angles_to_bevy_quat(angles)), scale: Vec3::ONE };
        pending.push((id, crate::weapons::model_handle(&server, &path), transform, slots));
    }
    pending.retain(|(id, handle, transform, slots)| {
        let Some(model) = models.get(handle) else { return true };
        let e = crate::objects::spawn_model_wearing(&mut commands, handle, model, *transform, slots);
        commands.entity(e).insert(crate::objects::PlacedId(*id));
        false
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// An archetype draws the model its graphic component names.
    #[test]
    fn an_archetype_draws_its_graphic_components_model() {
        let obj = |name: &str, fields: Vec<(u32, Vec<u8>)>, children: Vec<Arc<fc3_formats::fcb::Object>>| {
            Arc::new(fc3_formats::fcb::Object { name_hash: fc3_formats::hash::name_hash(name), fields, children })
        };
        let model = obj("Meshes", vec![(0x3182_766C, b"graphics\\props\\charge.xbg\0".to_vec())], vec![]);
        let graphic = obj("CGraphicComponent", vec![], vec![model]);
        let entry = fc3_story::data::Entry {
            id: 1,
            name: "Props:Charge".into(),
            class: String::new(),
            pos: [0.0; 3],
            angles: [0.0; 3],
            layer: 0,
            origin: fc3_story::data::Origin::Global,
            components: obj("Components", vec![], vec![graphic]),
            fields: Arc::default(),
        };
        assert_eq!(archetype_model(&entry).as_deref(), Some("graphics\\props\\charge.xbg"));
        let bare = fc3_story::data::Entry { components: obj("Components", vec![], vec![]), ..entry };
        assert_eq!(archetype_model(&bare), None);
    }
}
