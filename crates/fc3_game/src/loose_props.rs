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
        app.add_systems(Update, (wake_props, push_props, float_props).run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// The mass (kg) a script's push is reckoned against: the game's impulses
/// are in its bodies' units, and SAM_020's barrels, pushed with 3400, roll
/// off at some 11 m/s.
const PUSHED_MASS: f32 = 300.0;
/// The fastest a push sends a prop off (m/s).
const PUSH_MOST: f32 = 20.0;

/// Gives the props scripts pushed their push once they're loose bodies:
/// toward the entity the script named, a little up.
fn push_props(
    time: Res<Time>,
    mut placements: ResMut<EntityPlacements>,
    mut props: Query<(Entity, &PlacedId, &Transform, Option<&mut LinearVelocity>), With<Loose>>,
    mut waited: Local<f32>,
    mut commands: Commands,
) {
    if placements.impulses.is_empty() {
        *waited = 0.0;
        return;
    }
    *waited += time.delta_secs();
    let pending = std::mem::take(&mut placements.impulses);
    for (id, amp, toward) in pending {
        let Some((e, _, t, v)) = props.iter_mut().find(|(_, p, ..)| p.0 == id) else {
            // (Not loose yet, or not in the world: waited for a while.)
            if *waited < 5.0 {
                placements.impulses.push((id, amp, toward));
            }
            continue;
        };
        let dir = ((toward - t.translation).normalize_or(Vec3::X) + Vec3::Y * 0.1).normalize();
        let dv = dir * (amp / PUSHED_MASS).min(PUSH_MOST);
        debug!("loose prop {id}: pushed at {:.1} m/s", dv.length());
        match v {
            Some(mut v) => v.0 += dv,
            None => {
                commands.entity(e).insert(LinearVelocity(dv));
            }
        }
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
