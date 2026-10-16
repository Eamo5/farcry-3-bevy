//! Things a script binds to others (`CAPI_Entity.Bind`): the bound one rides
//! on the other, on one of its bones when one is named (`Head`, `Spine2`),
//! its own origin there (the binding boxes take the offset away). The Ink
//! Monster's mask on the giant's head; a tracker on Willis's back.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::objects::PlacedId;
use crate::story_actors::StoryActor;

pub struct BindsPlugin;

impl Plugin for BindsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Binds>().add_systems(Update, follow.after(crate::story::StorySet));
    }
}

/// What is bound to what: the bound one's id, the one it rides on and the
/// bone (its name's hash), if any.
#[derive(Resource, Default)]
pub struct Binds {
    by_child: HashMap<u64, (u64, Option<u32>)>,
}

impl Binds {
    pub fn bind(&mut self, parent: u64, child: u64, bone: &str) {
        let bone = (!bone.is_empty()).then(|| fc3_formats::hash::name_hash(bone));
        self.by_child.insert(child, (parent, bone));
    }

    pub fn unbind(&mut self, child: u64) {
        self.by_child.remove(&child);
    }

    pub fn reset(&mut self) {
        self.by_child.clear();
    }
}

/// Each bound thing where what it rides on is now (its bone's place as last
/// posed, else its own).
fn follow(
    binds: Res<Binds>,
    actors: Query<(&StoryActor, &GlobalTransform)>,
    rigs: Query<&crate::characters::CharacterRig>,
    globals: Query<&GlobalTransform>,
    placed: Query<(&PlacedId, &GlobalTransform), Without<StoryActor>>,
    mut props: Query<(&PlacedId, &mut Transform, Option<&ChildOf>), Without<StoryActor>>,
) {
    if binds.by_child.is_empty() {
        return;
    }
    // Where each parent (and bone) is.
    let place = |parent: u64, bone: Option<u32>| -> Option<GlobalTransform> {
        if let Some((actor, gt)) = actors.iter().find(|(a, _)| a.id == parent) {
            let joint = bone.and_then(|b| actor.rig.and_then(|r| rigs.get(r).ok()).and_then(|r| r.joint(b))).and_then(|(_, j)| globals.get(j).ok());
            return Some(joint.copied().unwrap_or(*gt));
        }
        placed.iter().find(|(id, _)| id.0 == parent).map(|(_, gt)| *gt)
    };
    for (id, mut t, parent) in &mut props {
        let Some(&(on, bone)) = binds.by_child.get(&id.0) else { continue };
        let Some(world) = place(on, bone) else { continue };
        let mut local = world.compute_transform();
        if let Some(p) = parent.and_then(|p| globals.get(p.parent()).ok()) {
            local = Transform::from_matrix(p.to_matrix().inverse() * world.to_matrix());
        }
        local.scale = t.scale;
        if *t != local {
            *t = local;
        }
    }
}
