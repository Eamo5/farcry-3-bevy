//! What flashes for the player's eye: the highlight a placed object's
//! graphic component carries (`bHighLight`: mission pickups such as the
//! doctor's mushrooms), pulsing between two colours (`clrHighLightColor2`
//! and `clrHighLightColor`) so many times a second (`fHighLightSpeed`), and
//! what scripts switch on and off over that (`CAPI_Entity.SetShimmerOnEntity`,
//! the `ShimmerModifier` box, 141 uses: the side quests' givers, doors to
//! open, oxygen bottles, a treasure; a picked-up mushroom's taken off). A
//! script's colour and rate stand over the object's own (`-1`: the
//! object's). It is drawn as the object's meshes again over themselves,
//! added on in the colour of the moment.

use std::collections::HashMap;

use bevy::camera::visibility::{RenderLayers, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;

pub struct ShimmerPlugin;
impl Plugin for ShimmerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Shimmers>().add_systems(Update, (own_highlights, apply_shimmers, pulse_shimmers).chain());
    }
}

/// A highlight: its low and high colours and its flashes a second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Highlight {
    pub low: Vec3,
    pub high: Vec3,
    pub rate: f32,
}

/// The editor's own highlight (what a mission pickup's comes with).
const HIGHLIGHT: Highlight = Highlight { low: Vec3::ZERO, high: Vec3::new(0.4, 0.3686, 0.2745), rate: 1.0 };

impl Highlight {
    /// A placed object's own, if it carries one switched on.
    pub fn of(entry: &fc3_story::data::Entry) -> Option<Highlight> {
        let g = entry.component("CGraphicComponent")?;
        (g.field("bHighLight").and_then(|b| b.first().copied()).unwrap_or(0) != 0).then(|| Highlight {
            low: g.vec3("clrHighLightColor2").map_or(HIGHLIGHT.low, Vec3::from),
            high: g.vec3("clrHighLightColor").map_or(HIGHLIGHT.high, Vec3::from),
            rate: g.f32("fHighLightSpeed").filter(|r| *r > 0.0).unwrap_or(HIGHLIGHT.rate),
        })
    }

    /// With a script's colour and rate over it (below zero: kept).
    fn with(self, colour: [f32; 3], rate: f32) -> Highlight {
        Highlight {
            high: if colour.iter().any(|c| *c < 0.0) { self.high } else { Vec3::from(colour) },
            rate: if rate > 0.0 { rate } else { self.rate },
            ..self
        }
    }

    /// Its colour `t` seconds in.
    fn at(&self, t: f32) -> Vec3 {
        let k = 0.5 - 0.5 * (std::f32::consts::TAU * self.rate * t).cos();
        self.low.lerp(self.high, k)
    }
}

/// The highlights: placed objects' own, and the scripts' switches over
/// them (by entity: on with a colour and rate, or off).
#[derive(Resource, Default)]
pub struct Shimmers {
    pub script: HashMap<u64, Option<([f32; 3], f32)>>,
    /// Placed objects' own, with the object while it's in the world.
    own: HashMap<u64, (Entity, Highlight)>,
    drawn: HashMap<u64, Drawn>,
}

impl Shimmers {
    /// Whether an entity is showing a highlight now.
    pub fn lit(&self, id: u64) -> bool {
        self.drawn.contains_key(&id)
    }

    /// What an entity shows now, if anything.
    fn wanted(&self, id: u64) -> Option<Highlight> {
        let own = self.own.get(&id).map(|(_, h)| *h);
        match self.script.get(&id) {
            Some(Some((colour, rate))) => Some(own.unwrap_or(HIGHLIGHT).with(*colour, *rate)),
            Some(None) => None,
            None => own,
        }
    }
}

struct Drawn {
    material: Handle<StandardMaterial>,
    overlays: Vec<Entity>,
    look: Highlight,
}

/// A mesh drawn again over its own for a highlight.
#[derive(Component)]
struct ShimmerOverlay;

/// Placed objects' own highlights, as they come into the world.
fn own_highlights(
    host: Option<NonSend<crate::story::StoryHost>>,
    added: Query<(Entity, &crate::objects::PlacedId), Added<crate::objects::PlacedId>>,
    mut shimmers: ResMut<Shimmers>,
) {
    // (`FC3_NO_HIGHLIGHTS` (tests): none of their own, to compare.)
    if added.is_empty() || std::env::var_os("FC3_NO_HIGHLIGHTS").is_some() {
        return;
    }
    let Some(host) = host else { return };
    let data = host.story.data();
    for (e, id) in &added {
        if let Some(h) = data.entities.get(&id.0).and_then(Highlight::of) {
            shimmers.own.insert(id.0, (e, h));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_shimmers(
    mut shimmers: ResMut<Shimmers>,
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    props: Query<(Entity, &crate::objects::PlacedId)>,
    actors: Query<(Entity, &crate::story_actors::StoryActor)>,
    children: Query<&Children>,
    meshes: Query<(&Mesh3d, Option<&SkinnedMesh>, Option<&VisibilityRange>, Option<&RenderLayers>), Without<ShimmerOverlay>>,
    overlays: Query<(), With<ShimmerOverlay>>,
    places: Query<&GlobalTransform>,
) {
    let shimmers = &mut *shimmers;
    // Gone out of the world (its overlays with it): drawn again when it's
    // back.
    shimmers.own.retain(|_, (e, _)| props.contains(*e));
    shimmers.drawn.retain(|_, d| d.overlays.first().is_some_and(|o| overlays.contains(*o)));
    // Off (or changed): the overlays go.
    let stale: Vec<u64> = shimmers.drawn.iter().filter(|(id, d)| shimmers.wanted(**id) != Some(d.look)).map(|(id, _)| *id).collect();
    for id in stale {
        if let Some(d) = shimmers.drawn.remove(&id) {
            for e in d.overlays {
                commands.entity(e).try_despawn();
            }
        }
    }
    // On: over every mesh under the entity, once it has its meshes in.
    let ids: Vec<(u64, Highlight)> = shimmers
        .own
        .keys()
        .chain(shimmers.script.keys())
        .filter(|id| !shimmers.drawn.contains_key(id))
        .filter_map(|id| Some((*id, shimmers.wanted(*id)?)))
        .collect();
    if ids.is_empty() {
        return;
    }
    // (Placed objects' own by their object; what only a script lights, by
    // looking for it.)
    let mut roots: Option<HashMap<u64, Entity>> = None;
    for (id, look) in ids {
        let root = shimmers.own.get(&id).map(|(e, _)| *e).or_else(|| {
            let roots = roots.get_or_insert_with(|| props.iter().map(|(e, p)| (p.0, e)).chain(actors.iter().map(|(e, a)| (a.id, e))).collect());
            roots.get(&id).copied()
        });
        let Some(root) = root else { continue };
        // (Each with its mesh's skin, and its level of detail's range and
        // layers: a far part's flash goes as the part does.)
        type Found = (Entity, Mesh3d, Option<SkinnedMesh>, Option<VisibilityRange>, Option<RenderLayers>);
        let found: Vec<Found> = std::iter::once(root)
            .chain(children.iter_descendants(root))
            .filter_map(|e| meshes.get(e).ok().map(|(m, s, r, l)| (e, m.clone(), s.cloned(), r.cloned(), l.cloned())))
            .collect();
        if found.is_empty() {
            continue;
        }
        let material = materials.add(StandardMaterial { base_color: Color::NONE, unlit: true, alpha_mode: AlphaMode::Add, depth_bias: 4.0, ..default() });
        let overlays = found
            .into_iter()
            .map(|(e, mesh, skin, range, layers)| {
                let mut overlay = commands.spawn((ShimmerOverlay, mesh, MeshMaterial3d(material.clone()), Transform::IDENTITY, NotShadowCaster, ChildOf(e)));
                if let Some(skin) = skin {
                    overlay.insert(skin);
                }
                if let Some(range) = range {
                    overlay.insert(range);
                }
                if let Some(layers) = layers {
                    overlay.insert(layers);
                }
                overlay.id()
            })
            .collect::<Vec<_>>();
        let at = places.get(root).map(|t| crate::data::to_dunia(t.translation())).unwrap_or_default();
        debug!("shimmer: {id} at {at:.1?} ({} meshes, {look:?})", overlays.len());
        shimmers.drawn.insert(id, Drawn { material, overlays, look });
    }
}

/// The flashes: each overlay's colour rising and falling at its rate.
fn pulse_shimmers(time: Res<Time>, shimmers: Res<Shimmers>, mut materials: ResMut<Assets<StandardMaterial>>) {
    let t = time.elapsed_secs();
    for d in shimmers.drawn.values() {
        if let Some(mut m) = materials.get_mut(&d.material) {
            let c = d.look.at(t);
            m.base_color = Color::linear_rgba(c.x, c.y, c.z, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripts_highlight_stands_over_the_objects_own() {
        let own = Highlight { low: Vec3::ZERO, high: Vec3::new(0.4, 0.3686, 0.2745), rate: 1.0 };
        let mut s = Shimmers::default();
        s.own.insert(7, (Entity::PLACEHOLDER, own));
        assert_eq!(s.wanted(7), Some(own));
        // (A picked-up mushroom's taken off; the object's colour kept.)
        s.script.insert(7, None);
        assert_eq!(s.wanted(7), None);
        s.script.insert(7, Some(([-1.0; 3], 0.5)));
        assert_eq!(s.wanted(7), Some(Highlight { rate: 0.5, ..own }));
        // One with none of its own: the editor's, in the script's colour.
        s.script.insert(8, Some(([1.0, 0.9, 0.6], -1.0)));
        assert_eq!(s.wanted(8), Some(Highlight { high: Vec3::new(1.0, 0.9, 0.6), ..HIGHLIGHT }));
        // A flash: the low colour, the high one half a beat on.
        assert_eq!(own.at(0.0), Vec3::ZERO);
        assert!((own.at(0.5) - own.high).length() < 1e-5);
    }
}
