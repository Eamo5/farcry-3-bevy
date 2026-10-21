//! Placed props that move by themselves: one whose `CSimpleAnimationComponent`
//! names a skeleton and an animation plays that animation over and over, as
//! the engine's simple animation does (an outpost's flag waving, a windmill's
//! sails, a ceiling fan, a weathervane). The prop's own model, loaded
//! skinned, plays it on a rig in place of its static meshes.

use std::sync::Arc;

use bevy::prelude::*;
use fc3_formats::cache;

use crate::cache_assets::{cache_path, skinned_cache_path, Fc3Model, ModelSettings};
use crate::characters::{spawn_rig_with, AnimKind, ClipSet};
use crate::objects::{PlacedId, WorldObject};

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
    let c = entry.component("CSimpleAnimationComponent")?;
    let path = |h| c.str_h(h).filter(|p| !p.is_empty());
    Some((path(SKELETON)?, path(ANIMATION)?))
}

fn find_animated(
    host: Option<NonSend<crate::story::StoryHost>>,
    server: Res<AssetServer>,
    added: Query<(Entity, &PlacedId, &WorldObject), Added<PlacedId>>,
    mut commands: Commands,
) {
    if added.is_empty() {
        return;
    }
    let Some(host) = host else { return };
    let data = host.story.data();
    for (e, id, object) in &added {
        let Some((skeleton, animation)) = data.entities.get(&id.0).and_then(simple_animation) else { continue };
        let Some(path) = server.get_path(&object.model) else { continue };
        debug!("prop anims: {} plays {animation}", id.0);
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
    mut pending: Query<(Entity, &mut Pending, Option<&Children>)>,
    mut shown: Query<&mut Visibility>,
    mut commands: Commands,
) {
    for (e, mut p, children) in &mut pending {
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
        let Some(rig) = spawn_rig_with(&mut commands, e, &[model], None, p.set.clone(), Vec3::ZERO, None) else { continue };
        for c in statics {
            if let Ok(mut v) = shown.get_mut(c) {
                *v = Visibility::Hidden;
            }
        }
        commands.entity(e).insert(PropRig(rig));
        debug!("prop anims: {e:?} plays its own animation");
    }
}
