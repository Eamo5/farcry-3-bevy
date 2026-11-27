//! Placed props that move by themselves: one whose `CSimpleAnimationComponent`
//! names a skeleton and an animation plays that animation over and over, as
//! the engine's simple animation does (an outpost's flag waving, a windmill's
//! sails, a ceiling fan, a weathervane). The prop's own model, loaded
//! skinned, plays it on a rig in place of its static meshes.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use fc3_formats::cache;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Model, ModelSettings};
use crate::characters::{spawn_dressed_rig, AnimKind, ClipSet, Dressed};
use crate::objects::{PlacedId, SlotMaterials, WorldObject};

pub struct PropAnimsPlugin;

impl Plugin for PropAnimsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (find_animated, rig_animated).chain());
    }
}

/// The simple animation's skeleton (`fileSkeleton`'s path) and animation
/// (`Animation`'s path).
const SKELETON: u32 = 0xF9F2_D5F4;
const ANIMATION: u32 = 0xFCAC_35BB;

/// How long (s) a prop waits for its skinned model and clip before it stays
/// as it is.
const GIVE_UP: f32 = 30.0;

/// A placed prop's rig, loading.
#[derive(Component)]
struct Pending {
    model: Handle<Fc3Model>,
    set: Arc<ClipSet>,
    waited: f32,
}

/// A placed prop playing its own animation, on this rig.
#[derive(Component)]
pub struct PropRig(pub Entity);

/// The skeleton and animation a placed entity's simple animation names, if
/// it names both.
pub fn simple_animation(entry: &fc3_story::data::Entry) -> Option<(String, String)> {
    simple_animation_in(&entry.components)
}

fn simple_animation_in(components: &fc3_formats::fcb::Object) -> Option<(String, String)> {
    let c = components.children.iter().find(|c| c.is("CSimpleAnimationComponent"))?;
    let path = |h| c.str_h(h).filter(|p| !p.is_empty());
    Some((path(SKELETON)?, path(ANIMATION)?))
}

/// The animated props of the story's entity table, by id, read for
/// themselves where no story runs (a free-roam game), in the background:
/// the outposts' flags waved only with the story on, and without it hung
/// as their skinned models' unskinned meshes, which don't show.
#[derive(Default)]
struct FreeTable {
    task: Option<Task<HashMap<u64, (String, String)>>>,
    table: Option<HashMap<u64, (String, String)>>,
}

fn find_animated(
    host: Option<NonSend<crate::story::StoryHost>>,
    no_story: Option<Res<crate::story::NoStory>>,
    info: Option<Res<crate::data::CacheInfo>>,
    server: Res<AssetServer>,
    added: Query<(Entity, &PlacedId, &WorldObject), Added<PlacedId>>,
    placed: Query<(Entity, &PlacedId, &WorldObject)>,
    mut free: Local<FreeTable>,
    mut commands: Commands,
) {
    let host_data = host.as_ref().map(|h| h.story.data());
    let mut all = false;
    if host_data.is_none() && no_story.is_some() && free.table.is_none() {
        if let (None, Some(info)) = (&free.task, &info) {
            let root = info.root.clone();
            free.task = Some(AsyncComputeTaskPool::get().spawn(async move {
                let bytes = std::fs::read(root.join(fc3_formats::story::ENTITIES_REL)).unwrap_or_default();
                let entities = fc3_formats::story::from_bytes(&bytes).unwrap_or_default();
                entities.iter().filter_map(|e| Some((e.id, simple_animation_in(&e.components)?))).collect()
            }));
        }
        let Some(done) = free.task.as_mut().and_then(|t| block_on(future::poll_once(t))) else { return };
        info!("prop anims: {} animated props, without the story", done.len());
        free.task = None;
        free.table = Some(done);
        // (Those already in the world, too.)
        all = true;
    }
    if added.is_empty() && !all {
        return;
    }
    let found: Vec<(Entity, u64, Handle<Fc3Model>)> = if all {
        placed.iter().map(|(e, id, o)| (e, id.0, o.model.clone())).collect()
    } else {
        added.iter().map(|(e, id, o)| (e, id.0, o.model.clone())).collect()
    };
    for (e, id, model) in found {
        let named = match &host_data {
            Some(data) => data.entities.get(&id).and_then(simple_animation),
            None => free.table.as_ref().and_then(|t| t.get(&id).cloned()),
        };
        let Some((skeleton, animation)) = named else { continue };
        let Some(path) = server.get_path(&model) else { continue };
        debug!("prop anims: {id} plays {animation}");
        let model = server.load_builder().with_settings(|m: &mut ModelSettings| m.skinned = true).load(skinned_cache_path(&path.path().to_string_lossy()));
        let set = Arc::new(ClipSet {
            skeleton: server.load(cache_path(&cache::skeleton_rel(&skeleton))),
            clips: [(AnimKind::Idle, server.load(cache_path(&cache::anim_rel(&animation))))].into(),
        });
        commands.entity(e).insert(Pending { model, set, waited: 0.0 });
    }
}

fn rig_animated(
    time: Res<Time>,
    server: Res<AssetServer>,
    models: Res<Assets<Fc3Model>>,
    mut pending: Query<(Entity, &mut Pending, Option<&Children>, Option<&SlotMaterials>)>,
    mut shown: Query<&mut Visibility>,
    mut commands: Commands,
) {
    for (e, mut p, children, slots) in &mut pending {
        p.waited += time.delta_secs();
        if !(p.set.ready(&server) && crate::cache_assets::usable(&server, p.model.id())) {
            if p.waited > GIVE_UP {
                commands.entity(e).remove::<Pending>();
            }
            continue;
        }
        commands.entity(e).remove::<Pending>();
        let Some(model) = models.get(&p.model) else { continue };
        // (Its static meshes hidden: the rig draws it now. One whose model
        // has no bones stays as it was.)
        let statics: Vec<Entity> = children.map(|c| c.iter().collect()).unwrap_or_default();
        // (Wearing its placement's materials: an outpost's flag its colours,
        // not the model's plain cloth.)
        let swaps = slots
            .map(|s| model.lods.iter().flat_map(|l| &l.parts).filter_map(|part| Some((part.material.id(), s.0.get(part.slot as usize)?.clone()))).collect())
            .unwrap_or_default();
        let Some(rig) = spawn_dressed_rig(&mut commands, e, &[Dressed { model, swaps }], None, p.set.clone(), Vec3::ZERO, None) else { continue };
        for c in statics {
            if let Ok(mut v) = shown.get_mut(c) {
                *v = Visibility::Hidden;
            }
        }
        commands.entity(e).insert(PropRig(rig));
        debug!("prop anims: {e:?} plays its own animation");
    }
}
