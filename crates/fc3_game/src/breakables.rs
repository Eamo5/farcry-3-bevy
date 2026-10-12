//! Breakable objects. A placed object the game breaks (a script's
//! `DestroyCompoundObject`, a blast close by, an explosive barrel going up,
//! a cage door shot open) shows its model's broken state: what was only
//! there whole goes, the remains stay (with the game's own collision for
//! them), and its loose pieces fall as bodies of their own, settling for a
//! while before they're cleared away. A broken object stays broken when its
//! sector streams in again (without its pieces falling twice).

use std::collections::HashSet;

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::cache_assets::Fc3Model;
use crate::objects::{PlacedId, SlotMaterials, WorldObject};

pub struct BreakablesPlugin;

impl Plugin for BreakablesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Breaks>().add_systems(Update, (break_test, blasts, break_objects, clear_pieces).chain());
    }
}

/// The placed entities broken (by id), and those broken just now (their
/// pieces fall).
#[derive(Resource, Default)]
pub struct Breaks {
    pub broken: HashSet<u64>,
    fresh: HashSet<u64>,
    /// Objects with no placed id broken now (an encounter's cage door).
    objects: Vec<Entity>,
}

impl Breaks {
    /// Breaks a placed entity now.
    pub fn break_now(&mut self, id: u64) {
        if self.broken.insert(id) {
            self.fresh.insert(id);
        }
    }

    /// Breaks an object now (one with no placed id: it stays broken only
    /// while it's there).
    pub fn break_object(&mut self, e: Entity) {
        self.objects.push(e);
    }

    pub fn reset(&mut self) {
        self.broken.clear();
        self.fresh.clear();
        self.objects.clear();
    }
}

/// Whether a model has a broken state to show.
pub fn has_broken_state(model: &Fc3Model) -> bool {
    model.lods.first().is_some_and(|l| !l.broken.is_empty())
}

/// Breakables close to a blast break: those whose middle is within half its
/// reach and half their own size.
fn blasts(
    mut explosions: MessageReader<crate::weapons::Explosion>,
    objects: Query<(Entity, Option<&PlacedId>, &WorldObject, &GlobalTransform), Without<Broken>>,
    models: Res<Assets<Fc3Model>>,
    mut breaks: ResMut<Breaks>,
) {
    for x in explosions.read().filter(|x| x.damage > 0.0) {
        for (e, id, obj, at) in &objects {
            let Some(model) = models.get(&obj.model).filter(|m| has_broken_state(m)) else { continue };
            let middle = at.transform_point(model.center());
            if middle.distance(x.at) < x.radius * 0.5 + model.radius() * 0.5 {
                match id {
                    Some(id) => breaks.break_now(id.0),
                    None => breaks.break_object(e),
                }
            }
        }
    }
}

/// A placed object shown broken.
#[derive(Component)]
pub struct Broken;

/// A loose piece of a broken object, and how long it has lain.
#[derive(Component)]
struct Piece(f32);

/// Pieces are cleared away after this long (s).
const PIECE_LIFE: f32 = 30.0;

#[allow(clippy::type_complexity)]
fn break_objects(
    mut breaks: ResMut<Breaks>,
    objects: Query<(Entity, Option<&PlacedId>, &WorldObject, &GlobalTransform, Option<&Children>, Option<&SlotMaterials>), Without<Broken>>,
    meshes: Query<&Mesh3d>,
    models: Res<Assets<Fc3Model>>,
    mut rng: ResMut<crate::weapons::Rng>,
    mut commands: Commands,
) {
    if breaks.broken.is_empty() && breaks.objects.is_empty() {
        return;
    }
    let targets = std::mem::take(&mut breaks.objects);
    for (e, id, obj, at, children, slots) in &objects {
        let by_id = id.is_some_and(|id| breaks.broken.contains(&id.0));
        if !by_id && !targets.contains(&e) {
            continue;
        }
        let Some(model) = models.get(&obj.model) else {
            if !by_id {
                breaks.objects.push(e);
            }
            continue;
        };
        let falling = !by_id || id.is_some_and(|id| breaks.fresh.remove(&id.0));
        let id = id.map_or(0, |id| id.0);
        commands.entity(e).insert(Broken);
        let Some(lod) = model.lods.first() else { continue };
        // A model with no broken state just goes.
        if lod.broken.is_empty() {
            commands.entity(e).insert((Visibility::Hidden, ColliderDisabled));
            continue;
        }
        // What was only there whole goes.
        let whole: Vec<AssetId<Mesh>> = model.lods.iter().flat_map(|l| &l.parts).filter(|p| p.whole_only).map(|p| p.mesh.id()).collect();
        for &c in children.into_iter().flatten() {
            if meshes.get(c).is_ok_and(|m| whole.contains(&m.0.id())) {
                commands.entity(c).despawn();
            }
        }
        // The remains stay; the pieces fall (or are long gone). They wear
        // the placement's own materials where it has a set.
        let material =
            |b: &crate::cache_assets::BrokenPart| slots.and_then(|s| s.0.get(b.part.slot as usize)).cloned().unwrap_or_else(|| b.part.material.clone());
        for b in &lod.broken {
            if !b.piece {
                commands.spawn((Mesh3d(b.part.mesh.clone()), MeshMaterial3d(material(b)), crate::original_shaders::PropPart, ChildOf(e)));
            } else if falling {
                let Some(hull) = Collider::convex_hull(b.points.clone()) else { continue };
                let out = (Vec3::from_array(b.part.pivot.as_ref().map_or([0.0; 3], |p| p.at)) - model.center()).normalize_or_zero();
                let push = Vec3::new(rng.range(-0.5, 0.5), rng.range(0.5, 1.5), rng.range(-0.5, 0.5)) + out;
                let spin = Vec3::new(rng.range(-2.0, 2.0), rng.range(-2.0, 2.0), rng.range(-2.0, 2.0));
                commands.spawn((
                    Transform::from_matrix(at.to_matrix()),
                    Visibility::Visible,
                    RigidBody::Dynamic,
                    hull,
                    LinearVelocity(at.affine().transform_vector3(push)),
                    AngularVelocity(spin),
                    Piece(0.0),
                    children![(Mesh3d(b.part.mesh.clone()), MeshMaterial3d(material(b)), crate::original_shaders::PropPart)],
                ));
            }
        }
        let pieces = lod.broken.iter().filter(|b| b.piece).count();
        info!(
            "broken: {id} ({} remains, {pieces} pieces{}, {} collision)",
            lod.broken.len() - pieces,
            if falling { " falling" } else { "" },
            if model.broken_collision.is_some() { "its own" } else { "no" }
        );
        // What's left to collide with.
        match broken_collider(model) {
            Some(c) => {
                commands.entity(e).insert((RigidBody::Static, c));
            }
            None => {
                commands.entity(e).remove::<Collider>();
            }
        }
    }
}

/// What a broken object still collides with: the game's own shape for its
/// remains, if it has one.
pub fn broken_collider(model: &Fc3Model) -> Option<Collider> {
    model.broken_collision.as_ref().map(|c| Collider::trimesh(c.positions.clone(), c.triangles.clone()))
}

/// `FC3_BREAK_TEST=<id>:<secs>` (tests): breaks that placed entity once,
/// that long into the game.
fn break_test(time: Res<Time>, mut breaks: ResMut<Breaks>, mut done: Local<bool>) {
    if *done {
        return;
    }
    let Some((id, at)) = std::env::var("FC3_BREAK_TEST").ok().and_then(|v| {
        let (id, at) = v.split_once(':')?;
        Some((id.parse::<u64>().ok()?, at.parse::<f32>().ok()?))
    }) else {
        *done = true;
        return;
    };
    if time.elapsed_secs() >= at {
        *done = true;
        breaks.break_now(id);
    }
}

fn clear_pieces(time: Res<Time>, mut pieces: Query<(Entity, &mut Piece)>, mut commands: Commands) {
    for (e, mut p) in &mut pieces {
        p.0 += time.delta_secs();
        if p.0 > PIECE_LIFE {
            commands.entity(e).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache_assets::{BrokenPart, ModelLod, ModelPart};

    fn part(meshes: &mut Assets<Mesh>, whole_only: bool) -> ModelPart {
        ModelPart { mesh: meshes.add(Mesh::from(Cuboid::default())), material: Handle::default(), slot: 0, flags: 0, pivot: None, whole_only }
    }

    /// Broken: the whole-only part goes, the remains come and a piece falls;
    /// one without a broken state is just gone.
    #[test]
    fn a_broken_object_shows_its_remains_and_drops_its_pieces() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<Fc3Model>>()
            .insert_resource(crate::weapons::Rng::seeded(1))
            .init_resource::<Breaks>()
            .add_systems(Update, break_objects);
        let mut meshes = app.world_mut().resource_mut::<Assets<Mesh>>();
        let (always, whole, remains, piece) = (part(&mut meshes, false), part(&mut meshes, true), part(&mut meshes, false), part(&mut meshes, false));
        let corners = vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z, Vec3::ONE];
        let model = |broken: Vec<BrokenPart>, parts: Vec<ModelPart>| Fc3Model {
            lods: vec![ModelLod { distance: 0.0, parts, broken }],
            aabb_min: Vec3::ZERO,
            aabb_max: Vec3::ONE,
            collision: None,
            broken_collision: None,
            bones: Vec::new(),
            is_vegetation: false,
            inverse_bindposes: None,
        };
        let mut models = app.world_mut().resource_mut::<Assets<Fc3Model>>();
        let breakable = models.add(model(
            vec![
                BrokenPart { part: remains.clone(), state: 2, piece: false, points: Vec::new() },
                BrokenPart { part: piece.clone(), state: 2, piece: true, points: corners },
            ],
            vec![always.clone(), whole.clone()],
        ));
        let plain = models.add(model(Vec::new(), vec![always.clone()]));
        let bridge =
            app.world_mut().spawn((PlacedId(1), WorldObject { model: breakable, entity_index: 0 }, Transform::IDENTITY, GlobalTransform::IDENTITY)).id();
        let kept = app.world_mut().spawn((Mesh3d(always.mesh.clone()), ChildOf(bridge))).id();
        let gone = app.world_mut().spawn((Mesh3d(whole.mesh.clone()), ChildOf(bridge))).id();
        let crate_ = app.world_mut().spawn((PlacedId(2), WorldObject { model: plain, entity_index: 0 }, Transform::IDENTITY, GlobalTransform::IDENTITY)).id();
        app.world_mut().resource_mut::<Breaks>().break_now(1);
        app.world_mut().resource_mut::<Breaks>().break_now(2);
        app.update();
        assert!(app.world().get_entity(kept).is_ok() && app.world().get_entity(gone).is_err());
        let children: Vec<Entity> = app.world().get::<Children>(bridge).map(|c| c.to_vec()).unwrap_or_default();
        assert!(children.iter().any(|&c| app.world().get::<Mesh3d>(c).is_some_and(|m| m.0 == remains.mesh)));
        let mut pieces = app.world_mut().query_filtered::<&RigidBody, With<Piece>>();
        assert_eq!(pieces.iter(app.world()).collect::<Vec<_>>(), vec![&RigidBody::Dynamic]);
        assert_eq!(app.world().get::<Visibility>(crate_), Some(&Visibility::Hidden));
        // Streamed in again later: broken, but nothing falls twice.
        app.world_mut().entity_mut(bridge).remove::<Broken>();
        app.update();
        assert_eq!(app.world_mut().query_filtered::<(), With<Piece>>().iter(app.world()).count(), 1);
    }
}
