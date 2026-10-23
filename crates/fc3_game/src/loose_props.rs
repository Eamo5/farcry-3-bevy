//! Props a script sets loose (the `PhysicsModifier` box's StartPhysics:
//! `CAPI_Entity.StartPhysOnEntity`): BAM_040's falling rocks, the buoys and
//! crates on BAM_020's sinking ship, the wrecks the encounters leave. A
//! placed prop stands still until then (the game's rigid bodies sleep);
//! woken, it falls and tumbles as a body of its own shape (its collision's
//! convex hull, else its bounds), and bobs up where it lands in water.

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::cache_assets::Fc3Model;
use crate::objects::{PlacedId, WorldObject};
use crate::physics::ObjectCollisionReady;
use crate::story::EntityPlacements;

pub struct LoosePropsPlugin;

impl Plugin for LoosePropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (wake_props, float_props).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// A prop set loose, falling as a body of its own.
#[derive(Component)]
pub struct Loose;

/// The body a woken prop falls as: its collision's hull (in its own frame),
/// else a box of its bounds.
fn body_of(model: &Fc3Model) -> Option<Collider> {
    let hull = model.collision.as_ref().filter(|c| c.positions.len() >= 4).and_then(|c| Collider::convex_hull(c.positions.clone()));
    hull.or_else(|| {
        let size = (model.aabb_max - model.aabb_min).max(Vec3::splat(0.05));
        let centre = model.center();
        Some(Collider::compound(vec![(centre, Quat::IDENTITY, Collider::cuboid(size.x, size.y, size.z))]))
    })
}

#[allow(clippy::type_complexity)]
fn wake_props(
    placements: Res<EntityPlacements>,
    models: Res<Assets<Fc3Model>>,
    props: Query<(Entity, &PlacedId, &WorldObject), (With<ObjectCollisionReady>, Without<Loose>)>,
    mut commands: Commands,
) {
    if placements.loose.is_empty() {
        return;
    }
    for (e, id, obj) in &props {
        if !placements.loose.contains(&id.0) {
            continue;
        }
        let Some(body) = models.get(&obj.model).and_then(body_of) else { continue };
        debug!("loose prop {}: falls", id.0);
        commands.entity(e).insert((RigidBody::Dynamic, body, Loose)).remove::<ColliderDisabled>();
    }
}

/// How hard water holds up a woken prop, as a share of its weight per
/// metre under (wood, buoys, crates: they float).
const FLOAT: f32 = 2.5;

fn float_props(heights: Res<crate::terrain::Heightfield>, mut props: Query<(&Transform, Forces, &ComputedMass), With<Loose>>) {
    for (t, mut f, mass) in &mut props {
        let p = t.translation;
        let surface = crate::water::surface_at(p, &heights);
        let under = (surface - p.y + 0.2).clamp(0.0, 1.0);
        if under <= 0.0 {
            continue;
        }
        let m = mass.value();
        let v = f.linear_velocity();
        f.apply_force(Vec3::Y * FLOAT * 9.81 * m * under - v * m * 1.5 * under);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A woken prop with no collision of its own falls as a box of its bounds.
    #[test]
    fn a_prop_without_collision_falls_as_its_bounds() {
        let model = Fc3Model {
            lods: Vec::new(),
            aabb_min: Vec3::new(-1.0, 0.0, -0.5),
            aabb_max: Vec3::new(1.0, 2.0, 0.5),
            collision: None,
            broken_collision: None,
            bones: Vec::new(),
            is_vegetation: false,
            inverse_bindposes: None,
        };
        let body = body_of(&model).unwrap();
        let aabb = body.aabb(Vec3::ZERO, Quat::IDENTITY);
        assert!((aabb.min - Vec3::new(-1.0, 0.0, -0.5)).length() < 1e-3 && (aabb.max - Vec3::new(1.0, 2.0, 0.5)).length() < 1e-3);
    }
}
