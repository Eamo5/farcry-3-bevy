//! Decks the navigation mesh crosses water on that the world has nothing
//! under. F4's trial starts its technical on a bridge over a river whose
//! model isn't in the game's data (no sector, prefab or story entity puts
//! one there; see `docs/story.md`), but the sector's navmesh keeps its deck,
//! 8 m wide and level with the road on either bank. Each nav sector near the
//! player has its polygons that lie over water (the ground under every
//! corner below the water's surface) and well above it looked under: those
//! no collider holds up get a static collider and a plank floor (the game's
//! `woodplank_06`, weathered darker), edged down to a beam's depth. Where a
//! placed model's bounds take in the polygon (a bridge drawn without a
//! collider of its own, as a compound truss bridge is) the deck is only its
//! collider. It is looked at as soon as
//! its ground is in, so a vehicle set down there has its deck (a collider
//! alone at first), and again as the objects around come in with their
//! colliders (a real bridge, a pier): what they hold up is taken out of the
//! deck, and what is left is drawn.

use std::collections::{HashMap, HashSet};

use avian3d::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use fc3_formats::terrain::SectorCoord;

pub struct NavDecksPlugin;

impl Plugin for NavDecksPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Decks>().add_systems(Update, build_decks.run_if(in_state(crate::menu::GameState::Playing)));
    }
}

/// How far above the water a deck lies at least; when a sector is looked
/// at, from when it came in (again as its objects and their colliders come
/// in); how far under a polygon a collider counts as holding it up; the
/// depth of a deck's edge.
const ABOVE_WATER: f32 = 1.5;
const LOOKS: [f32; 3] = [0.0, 5.0, 15.0];
const HELD: f32 = 1.2;
const EDGE: f32 = 0.35;
/// The planks' texture, and the size (m) one repeat of it covers.
const PLANKS: &str = r"graphics\__fc3_graphics\_common\_textures\generic\wood\woodplank_06_d.xbt";
const PLANK_REPEAT: f32 = 2.0;
/// The planks' tint, weathered (as the game's own wooden bridges are).
const PLANK_TINT: Color = Color::srgb(0.55, 0.47, 0.38);
/// How far outside a model's bounds a polygon still counts as its.
const COVER_MARGIN: f32 = 0.3;

#[derive(Resource, Default)]
struct Decks {
    seen: HashMap<SectorCoord, Seen>,
    material: Option<Handle<StandardMaterial>>,
}

/// A nav sector loaded: since when, how many times looked at, and its deck
/// (with its polygons' count, and whether it is drawn) if it needs one.
struct Seen {
    since: f32,
    looks: usize,
    deck: Option<(Entity, usize, bool)>,
}

/// A polygon's corners, if it lies over water and well above it.
fn over_water(verts: &[Vec3], heights: &crate::terrain::Heightfield) -> Option<()> {
    for &v in verts {
        let ground = heights.height_at(v)?;
        let surface = crate::water::surface(v, Some(ground));
        if ground > surface - 0.5 || v.y < surface + ABOVE_WATER {
            return None;
        }
    }
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn build_decks(
    time: Res<Time>,
    mut decks: ResMut<Decks>,
    nav: Res<crate::nav::NavMesh>,
    heights: Res<crate::terrain::Heightfield>,
    spatial: SpatialQuery,
    server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    (objects, models): (Query<(&crate::objects::WorldObject, &GlobalTransform)>, Res<Assets<crate::cache_assets::Fc3Model>>),
    mut commands: Commands,
) {
    let now = time.elapsed_secs();
    let loaded: HashSet<SectorCoord> = nav.sector_meshes().map(|(c, _)| c).collect();
    // Sectors gone take their decks with them.
    decks.seen.retain(|c, seen| {
        let keep = loaded.contains(c);
        if let (false, Some((e, ..))) = (keep, seen.deck) {
            commands.entity(e).despawn();
        }
        keep
    });
    for c in &loaded {
        decks.seen.entry(*c).or_insert(Seen { since: now, looks: 0, deck: None });
    }
    // One sector a frame, due a look and with its ground in.
    let due = |s: &Seen| LOOKS.get(s.looks).is_some_and(|&at| now - s.since >= at);
    let Some(c) = decks.seen.iter().find(|(c, s)| due(s) && heights.sectors.contains_key(c)).map(|(c, _)| *c) else {
        return;
    };
    let Some((_, mesh)) = nav.sector_meshes().find(|(s, _)| *s == c) else { return };
    let seen = decks.seen.get_mut(&c).expect("seen");
    seen.looks += 1;
    // (Drawn once the objects around have had time to come in: until then
    // a real bridge's collider may yet be on its way.)
    let draw = seen.looks > 1;
    let old = seen.deck;
    // (Its own deck doesn't hold itself up.)
    let filter = SpatialQueryFilter::from_excluded_entities(old.map(|(e, ..)| e));
    let mut top: Vec<Vec<Vec3>> = Vec::new();
    for poly in &mesh.polys {
        let verts: Vec<Vec3> = poly.iter().filter_map(|&i| mesh.verts.get(i as usize)).map(|&v| Vec3::from_array(v)).collect();
        if verts.len() < 3 || over_water(&verts, &heights).is_none() {
            continue;
        }
        // (Held up already: a bridge, a pier, a stilt house's floor.)
        let centre = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
        if spatial.cast_ray(centre + Vec3::Y, Dir3::NEG_Y, 1.0 + HELD, true, &filter).is_some() {
            continue;
        }
        top.push(verts);
    }
    // Unchanged since the last look (and drawn if it is time): kept as it is.
    if old.map_or(0, |(_, n, _)| n) == top.len() && old.is_none_or(|(_, _, drawn)| drawn || !draw) {
        return;
    }
    if let Some((e, ..)) = old {
        commands.entity(e).despawn();
    }
    if top.is_empty() {
        decks.seen.get_mut(&c).expect("seen").deck = None;
        return;
    }
    let area: f32 = top.iter().map(|v| (1..v.len() - 1).map(|k| (v[k] - v[0]).cross(v[k + 1] - v[0]).length() / 2.0).sum::<f32>()).sum();
    let at = crate::data::to_dunia(top.iter().flatten().copied().sum::<Vec3>() / top.iter().map(Vec::len).sum::<usize>() as f32);
    info!("nav decks: sector {} ({}, {}): {} polygons over water with nothing under them, {area:.0} m2 about Dunia {at:.0?}", c.id(), c.x, c.y, top.len());
    let material = decks
        .material
        .get_or_insert_with(|| {
            // (Tiled: the sampler repeats.)
            let planks = server.load_with_settings(
                crate::cache_assets::cache_path(&fc3_formats::cache::texture_rel(PLANKS)),
                |s: &mut bevy::image::ImageLoaderSettings| {
                    s.is_srgb = true;
                    s.sampler = crate::cache_assets::repeat_sampler();
                },
            );
            materials.add(StandardMaterial { base_color: PLANK_TINT, base_color_texture: Some(planks), perceptual_roughness: 0.85, ..default() })
        })
        .clone();
    // (Drawn already by a model whose bounds take it in: only held up.)
    let covered = |verts: &Vec<Vec3>| {
        let centre = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
        objects.iter().any(|(o, t)| {
            let Some(m) = models.get(&o.model) else { return false };
            let local = t.affine().inverse().transform_point3(centre);
            (local.cmpge(m.aabb_min - COVER_MARGIN) & local.cmple(m.aabb_max + COVER_MARGIN)).all()
        })
    };
    let shown: Vec<Vec<Vec3>> = if draw { top.iter().filter(|v| !covered(v)).cloned().collect() } else { Vec::new() };
    let mut deck = commands.spawn((Transform::IDENTITY, Visibility::default(), RigidBody::Static, deck_collider(&top)));
    if !shown.is_empty() {
        deck.insert((Mesh3d(meshes.add(deck_mesh(&shown))), MeshMaterial3d(material)));
    }
    let e = deck.id();
    debug!("nav decks: {} of {} polygons drawn (the rest under models)", shown.len(), top.len());
    decks.seen.get_mut(&c).expect("seen").deck = Some((e, top.len(), draw));
}

/// The deck's collider: its floor (its polygons, fanned; at least one).
fn deck_collider(polys: &[Vec<Vec3>]) -> Collider {
    let (mut pos, mut idx) = (Vec::new(), Vec::new());
    for verts in polys {
        let base = pos.len() as u32;
        pos.extend(verts.iter().copied());
        idx.extend((1..verts.len() as u32 - 1).map(|k| [base, base + k, base + k + 1]));
    }
    Collider::trimesh(pos, idx)
}

/// The deck's floor (its polygons, fanned) and its edges (their outer
/// sides, down a beam's depth).
fn deck_mesh(polys: &[Vec<Vec3>]) -> Mesh {
    let (mut pos, mut normals, mut uvs, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    // (Edges: each side counted by its ends, the shared ones twice.)
    let key = |v: Vec3| ((v.x * 10.0).round() as i64, (v.z * 10.0).round() as i64);
    let mut sides: HashMap<((i64, i64), (i64, i64)), (Vec3, Vec3, Vec3, u32)> = HashMap::new();
    for verts in polys {
        let base = pos.len() as u32;
        let centre = verts.iter().copied().sum::<Vec3>() / verts.len() as f32;
        // (Wound to face up, whichever way the mesh lists them.)
        let up = (verts[1] - verts[0]).cross(verts[2] - verts[0]).y >= 0.0;
        for &v in verts {
            pos.push(v.to_array());
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([v.x / PLANK_REPEAT, v.z / PLANK_REPEAT]);
        }
        for k in 1..verts.len() as u32 - 1 {
            let tri = if up { [base, base + k, base + k + 1] } else { [base, base + k + 1, base + k] };
            idx.extend(tri);
        }
        for k in 0..verts.len() {
            let (a, b) = (verts[k], verts[(k + 1) % verts.len()]);
            let (ka, kb) = (key(a), key(b));
            let e = sides.entry(if ka < kb { (ka, kb) } else { (kb, ka) }).or_insert((a, b, centre, 0));
            e.3 += 1;
        }
    }
    for (a, b, centre, n) in sides.into_values() {
        if n != 1 {
            continue;
        }
        let along = (b - a).with_y(0.0);
        let mut out = Vec3::new(-along.z, 0.0, along.x).normalize_or_zero();
        if out.dot((a + b) / 2.0 - centre) < 0.0 {
            out = -out;
        }
        let base = pos.len() as u32;
        for v in [a, b, b - Vec3::Y * EDGE, a - Vec3::Y * EDGE] {
            pos.push(v.to_array());
            normals.push(out.to_array());
            uvs.push([(v.x + v.z) / PLANK_REPEAT, v.y / PLANK_REPEAT]);
        }
        // (Facing out.)
        let facing = (b - a).cross(Vec3::NEG_Y).dot(out) > 0.0;
        idx.extend(if facing { [base, base + 1, base + 2, base, base + 2, base + 3] } else { [base, base + 2, base + 1, base, base + 3, base + 2] });
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(idx));
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two squares side by side: their floor, and an edge down each outer
    /// side but not the one they share.
    #[test]
    fn a_decks_edges_run_round_its_outside() {
        let a = vec![Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 5.0, 0.0), Vec3::new(1.0, 5.0, 1.0), Vec3::new(0.0, 5.0, 1.0)];
        let b = vec![Vec3::new(1.0, 5.0, 0.0), Vec3::new(2.0, 5.0, 0.0), Vec3::new(2.0, 5.0, 1.0), Vec3::new(1.0, 5.0, 1.0)];
        let mesh = deck_mesh(&[a, b]);
        // 8 floor corners and 6 outer sides of 4.
        assert_eq!(mesh.count_vertices(), 8 + 6 * 4);
        let Some(Indices::U32(idx)) = mesh.indices() else { panic!("indices") };
        assert_eq!(idx.len(), 2 * 2 * 3 + 6 * 2 * 3);
    }
}
