//! Despawning big streamed groups a slice at a time. A sector's vegetation
//! layer or placed objects can be thousands of entities, and despawning them
//! all at once held up a frame by 20 ms or more as the player moved on. So a
//! retired group (at the edge of the streamed area) is taken apart over the
//! next frames instead.

use bevy::prelude::*;

pub struct RetirePlugin;

impl Plugin for RetirePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Last, take_apart);
    }
}

/// On a group root being taken apart.
#[derive(Component)]
struct Retiring;

/// Children (with their own descendants) despawned per frame, over all
/// retiring groups.
const BUDGET: usize = 300;

/// Has a group despawned over the next frames.
pub fn retire(commands: &mut Commands, root: Entity) {
    if let Ok(mut e) = commands.get_entity(root) {
        e.insert(Retiring);
    }
}

fn take_apart(groups: Query<(Entity, Option<&Children>), With<Retiring>>, mut commands: Commands) {
    let mut budget = BUDGET;
    for (root, children) in &groups {
        let children = children.map(|c| &c[..]).unwrap_or(&[]);
        let n = children.len().min(budget);
        for &child in &children[..n] {
            commands.entity(child).despawn();
        }
        budget -= n;
        if n == children.len() {
            commands.entity(root).despawn();
        }
        if budget == 0 {
            break;
        }
    }
}
