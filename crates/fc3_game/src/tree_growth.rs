//! Placed RealTrees the story grows and shrinks. `UpdateRealTreeRegen` sets a
//! tree's size (0 nothing, 1 grown), `SetRealTreeGrowthSpeed` moves it to a
//! size at a speed (its share of the full size a second), and the story
//! hears when it gets there (`Story::growth_finished`). The tree's placement
//! is drawn at that size from its foot. (The game grows a RealTree branch by
//! branch; here the whole tree scales.)

use std::collections::HashMap;

use bevy::prelude::*;

use crate::objects::PlacedId;

pub struct TreeGrowthPlugin;

impl Plugin for TreeGrowthPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TreeGrowth>().add_systems(Update, (grow, show).chain().run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The trees the story has sized, by entity id.
#[derive(Resource, Default)]
pub struct TreeGrowth {
    trees: HashMap<u64, Tree>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Tree {
    size: f32,
    target: f32,
    /// Share of the full size a second.
    speed: f32,
    /// The growth under way (the story's id for it).
    growth: Option<u64>,
}

impl TreeGrowth {
    /// A tree's size, at once.
    pub fn set(&mut self, tree: u64, size: f32) {
        let t = self.tree(tree);
        t.size = size.clamp(0.0, 1.0);
        if t.growth.is_none() {
            t.target = t.size;
        }
    }

    /// A tree grows (or shrinks) to a size at a speed.
    pub fn grow(&mut self, tree: u64, target: f32, speed: f32, growth: u64) {
        let t = self.tree(tree);
        t.target = target.clamp(0.0, 1.0);
        t.speed = speed.abs();
        t.growth = Some(growth);
    }

    /// A growth stopped where it is.
    pub fn stop(&mut self, tree: u64, growth: u64) {
        if let Some(t) = self.trees.get_mut(&tree).filter(|t| t.growth == Some(growth)) {
            t.growth = None;
            t.target = t.size;
        }
    }

    /// A tree's size (a tree the story hasn't touched is grown).
    pub fn size(&self, tree: u64) -> f32 {
        self.trees.get(&tree).map_or(1.0, |t| t.size)
    }

    pub fn reset(&mut self) {
        self.trees.clear();
    }

    fn tree(&mut self, tree: u64) -> &mut Tree {
        self.trees.entry(tree).or_insert(Tree { size: 1.0, target: 1.0, speed: 0.0, growth: None })
    }

    /// Moves the trees on; the growths that got there.
    fn advance(&mut self, dt: f32) -> Vec<u64> {
        let mut done = Vec::new();
        for t in self.trees.values_mut() {
            let Some(growth) = t.growth else { continue };
            let step = t.speed * dt;
            // (A growth with no speed is there at once.)
            t.size = if t.speed <= 0.0 || (t.target - t.size).abs() <= step { t.target } else { t.size + step * (t.target - t.size).signum() };
            if t.size == t.target {
                t.growth = None;
                done.push(growth);
            }
        }
        done
    }
}

fn grow(time: Res<Time>, mut growth: ResMut<TreeGrowth>, host: Option<NonSendMut<crate::story::StoryHost>>) {
    if growth.trees.values().all(|t| t.growth.is_none()) {
        return;
    }
    let done = growth.advance(time.delta_secs());
    let Some(mut host) = host else { return };
    for id in done {
        host.story.growth_finished(id);
    }
}

/// Each sized tree's placement drawn at its size (hidden at nothing).
fn show(growth: Res<TreeGrowth>, added: Query<(), Added<PlacedId>>, mut trees: Query<(&PlacedId, &mut Transform, &mut Visibility)>) {
    if growth.trees.is_empty() || (!growth.is_changed() && added.is_empty()) {
        return;
    }
    for (id, mut t, mut visibility) in &mut trees {
        let Some(tree) = growth.trees.get(&id.0) else { continue };
        let scale = Vec3::splat(tree.size.max(0.001));
        if t.scale != scale {
            t.scale = scale;
        }
        let shown = if tree.size > 0.001 { Visibility::Visible } else { Visibility::Hidden };
        if *visibility != shown {
            *visibility = shown;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trees_grow_and_shrink_at_their_speed_and_say_when_they_are_there() {
        let mut g = TreeGrowth::default();
        assert_eq!(g.size(1), 1.0);
        g.set(1, 0.0);
        g.set(2, 1.0);
        g.grow(1, 1.0, 0.25, 10);
        g.grow(2, 0.0, -0.5, 20);
        assert!(g.advance(1.0).is_empty());
        assert_eq!((g.size(1), g.size(2)), (0.25, 0.5));
        assert_eq!(g.advance(1.0), vec![20]);
        assert_eq!(g.size(2), 0.0);
        // Stopped, it stays where it was and never arrives.
        g.stop(1, 10);
        assert!(g.advance(10.0).is_empty());
        assert_eq!(g.size(1), 0.5);
        // Sizing a tree mid-growth moves it but keeps it going.
        g.grow(1, 1.0, 0.1, 11);
        g.set(1, 0.95);
        assert_eq!(g.advance(1.0), vec![11]);
        assert_eq!(g.size(1), 1.0);
    }

    #[test]
    fn a_placed_tree_is_drawn_at_its_size() {
        let mut app = App::new();
        app.init_resource::<TreeGrowth>().add_systems(Update, show);
        let tree = app.world_mut().spawn((PlacedId(5), Transform::default(), Visibility::Visible)).id();
        let other = app.world_mut().spawn((PlacedId(6), Transform::default(), Visibility::Visible)).id();
        app.world_mut().resource_mut::<TreeGrowth>().set(5, 0.0);
        app.update();
        assert_eq!(app.world().get::<Visibility>(tree), Some(&Visibility::Hidden));
        assert_eq!(app.world().get::<Transform>(other).unwrap().scale, Vec3::ONE);
        app.world_mut().resource_mut::<TreeGrowth>().set(5, 0.4);
        app.update();
        assert_eq!(app.world().get::<Transform>(tree).unwrap().scale, Vec3::splat(0.4));
        assert_eq!(app.world().get::<Visibility>(tree), Some(&Visibility::Visible));
    }
}
