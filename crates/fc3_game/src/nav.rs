//! The game's navigation meshes around the player, and paths across them.
//!
//! Sector meshes (converted from the game's `.rnv` files) are read in the
//! background for the 5 x 5 sectors around the player. Whenever that set
//! changes, they are joined into one graph of walkable polygons, with the
//! portals between neighbouring sectors matched along their shared border.
//! Paths are A* over polygons, straightened through the portals with the
//! funnel algorithm.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::tasks::{block_on, futures_lite::future, AsyncComputeTaskPool, Task};
use fc3_formats::cache::{self, CachedNav};
use fc3_formats::terrain::SectorCoord;

use crate::player::Player;

pub struct NavPlugin;

impl Plugin for NavPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NavMesh>().add_systems(Update, stream_nav).add_plugins(crate::nav_decks::NavDecksPlugin);
    }
}

/// Sectors loaded around the player, and how far away they are dropped.
const LOAD_RADIUS: i32 = 2;
const DROP_RADIUS: i32 = 3;
/// Polygons A* may open before giving up.
const SEARCH_LIMIT: usize = 6000;

#[derive(Resource, Default)]
pub struct NavMesh {
    sectors: HashMap<SectorCoord, Arc<CachedNav>>,
    loading: HashMap<SectorCoord, Task<Option<CachedNav>>>,
    /// Sectors without a mesh (open sea).
    missing: std::collections::HashSet<SectorCoord>,
    graph: NavGraph,
    dirty: bool,
    /// The graph being rebuilt in the background (it links thousands of
    /// polygons, too slow for a frame).
    building: Option<Task<NavGraph>>,
}

#[derive(Default)]
struct NavGraph {
    polys: Vec<Poly>,
    /// Each sector's polygons (start, count).
    by_sector: HashMap<SectorCoord, (usize, usize)>,
    /// Spots along the mesh's walls (edges with nothing walkable beyond),
    /// with the direction back into the walkable side, by 8 m cell.
    walls: HashMap<(i32, i32), Vec<(Vec3, Vec3)>>,
}

const WALL_CELL: f32 = 8.0;

fn wall_cell(p: Vec3) -> (i32, i32) {
    ((p.x / WALL_CELL).floor() as i32, (p.z / WALL_CELL).floor() as i32)
}

struct Poly {
    verts: Vec<Vec3>,
    centre: Vec3,
    min: Vec2,
    max: Vec2,
    /// Neighbour and the shared span.
    links: Vec<(u32, Vec3, Vec3)>,
}

fn sector_of(p: Vec3) -> SectorCoord {
    // Bevy z is the negated Dunia y.
    SectorCoord::containing(p.x, -p.z)
}

/// Reads sector meshes around the player in the background, drops far ones and
/// rebuilds the graph when the set changes.
fn stream_nav(mut nav: ResMut<NavMesh>, info: Res<crate::data::CacheInfo>, players: Query<&Transform, With<Player>>) {
    let Ok(pt) = players.single() else { return };
    let centre = sector_of(pt.translation);
    let nav = &mut *nav;
    // Finished reads.
    let finished: Vec<(SectorCoord, Option<CachedNav>)> = nav.loading.iter_mut().filter_map(|(c, t)| block_on(future::poll_once(t)).map(|r| (*c, r))).collect();
    let done: Vec<SectorCoord> = finished
        .into_iter()
        .map(|(c, r)| {
            match r {
                Some(mesh) => {
                    nav.sectors.insert(c, Arc::new(mesh));
                }
                None => {
                    nav.missing.insert(c);
                }
            }
            c
        })
        .collect();
    for c in &done {
        nav.loading.remove(c);
        nav.dirty = true;
    }
    // Far sectors go; near ones start loading.
    let far = |c: &SectorCoord| (c.x - centre.x).abs().max((c.y - centre.y).abs()) > DROP_RADIUS;
    let before = nav.sectors.len();
    nav.sectors.retain(|c, _| !far(c));
    if nav.sectors.len() != before {
        nav.dirty = true;
    }
    nav.loading.retain(|c, _| !far(c));
    nav.missing.retain(|c| !far(c));
    let pool = AsyncComputeTaskPool::get();
    for dy in -LOAD_RADIUS..=LOAD_RADIUS {
        for dx in -LOAD_RADIUS..=LOAD_RADIUS {
            let c = SectorCoord::new(centre.x + dx, centre.y + dy);
            if !c.valid() || nav.sectors.contains_key(&c) || nav.loading.contains_key(&c) || nav.missing.contains(&c) {
                continue;
            }
            let path = info.root.join(cache::nav_rel(c.id()));
            nav.loading.insert(c, pool.spawn(async move { std::fs::read(path).ok().and_then(|b| CachedNav::from_bytes(&b).ok()) }));
        }
    }
    if let Some(graph) = nav.building.as_mut().and_then(|t| block_on(future::poll_once(t))) {
        nav.building = None;
        nav.graph = graph;
        let links: usize = nav.graph.polys.iter().map(|p| p.links.len()).sum();
        debug!("nav: {} sectors, {} polygons, {} links", nav.sectors.len(), nav.graph.polys.len(), links / 2);
    }
    if nav.dirty && nav.loading.is_empty() && nav.building.is_none() {
        nav.dirty = false;
        let sectors = nav.sectors.clone();
        nav.building = Some(pool.spawn(async move { NavGraph::build(&sectors) }));
    }
}

impl NavGraph {
    fn build(sectors: &HashMap<SectorCoord, Arc<CachedNav>>) -> NavGraph {
        let mut g = NavGraph::default();
        for (c, s) in sectors {
            let start = g.polys.len();
            for p in &s.polys {
                let verts: Vec<Vec3> = p.iter().map(|&i| Vec3::from_array(s.verts[i as usize])).collect();
                let centre = verts.iter().copied().sum::<Vec3>() / verts.len().max(1) as f32;
                let (min, max) = verts.iter().fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(lo, hi), v| (lo.min(v.xz()), hi.max(v.xz())));
                g.polys.push(Poly { verts, centre, min, max, links: Vec::new() });
            }
            for l in &s.links {
                let (a, b) = (start + l.a as usize, start + l.b as usize);
                let (p, q) = (Vec3::from_array(l.left), Vec3::from_array(l.right));
                if a < g.polys.len() && b < g.polys.len() {
                    g.polys[a].links.push((b as u32, p, q));
                    g.polys[b].links.push((a as u32, p, q));
                }
            }
            g.by_sector.insert(*c, (start, s.polys.len()));
        }
        // Neighbouring sectors meet along their borders (east and north of each, once).
        for (c, s) in sectors {
            for n in [SectorCoord::new(c.x + 1, c.y), SectorCoord::new(c.x, c.y + 1)] {
                let Some(other) = sectors.get(&n) else { continue };
                let (sa, sb) = (g.by_sector[c].0, g.by_sector[&n].0);
                for (a, b, p, q) in fc3_formats::nav::join_borders(&s.border, &other.border) {
                    let (a, b) = (sa + a as usize, sb + b as usize);
                    if a < g.polys.len() && b < g.polys.len() {
                        let (p, q) = (Vec3::from_array(p), Vec3::from_array(q));
                        g.polys[a].links.push((b as u32, p, q));
                        g.polys[b].links.push((a as u32, p, q));
                    }
                }
            }
        }
        // Walls: polygon edges that no portal runs along.
        for poly in &g.polys {
            let n = poly.verts.len();
            for k in 0..n {
                let (a, b) = (poly.verts[k], poly.verts[(k + 1) % n]);
                let along = |p: Vec3| {
                    let ab = (b - a).xz();
                    let t = (p.xz() - a.xz()).dot(ab) / ab.length_squared().max(1e-6);
                    (-0.01..=1.01).contains(&t) && (a.xz() + ab * t).distance(p.xz()) < 0.05
                };
                if poly.links.iter().any(|(_, p, q)| along(*p) && along(*q)) {
                    continue;
                }
                let edge = b - a;
                let len = edge.xz().length();
                if len < 0.6 {
                    continue;
                }
                let mut inward = Vec3::new(-edge.z, 0.0, edge.x).normalize_or_zero();
                if inward.dot(poly.centre - a) < 0.0 {
                    inward = -inward;
                }
                let steps = (len / 2.0).ceil().max(1.0) as usize;
                for s in 0..steps {
                    let at = a + edge * ((s as f32 + 0.5) / steps as f32) + inward * 0.45;
                    g.walls.entry(wall_cell(at)).or_default().push((at, inward));
                }
            }
        }
        g
    }

    /// The highest walkable surface right under a point, no more than
    /// `depth` below it (nor above it by more than a step).
    fn floor_under(&self, p: Vec3, depth: f32) -> Option<f32> {
        let sector = sector_of(p);
        let mut best: Option<f32> = None;
        for dz in -1..=1 {
            for dx in -1..=1 {
                let Some(&(start, count)) = self.by_sector.get(&SectorCoord::new(sector.x + dx, sector.y + dz)) else { continue };
                for poly in &self.polys[start..start + count] {
                    if p.xz().clamp(poly.min, poly.max) != p.xz() || !contains(&poly.verts, p) {
                        continue;
                    }
                    let h = plane_height(&poly.verts, p);
                    if h <= p.y + 0.5 && h >= p.y - depth && best.is_none_or(|b| h > b) {
                        best = Some(h);
                    }
                }
            }
        }
        best
    }

    /// The polygon under (or nearest to, within a few metres) a point.
    fn locate(&self, p: Vec3) -> Option<usize> {
        let sector = sector_of(p);
        let mut best: Option<(f32, usize)> = None;
        // Close to a sector border the nearest surface may be next door.
        for dz in -1..=1 {
            for dx in -1..=1 {
                let Some(&(start, count)) = self.by_sector.get(&SectorCoord::new(sector.x + dx, sector.y + dz)) else { continue };
                for i in start..start + count {
                    let poly = &self.polys[i];
                    if p.xz().distance_squared(p.xz().clamp(poly.min, poly.max)) > 16.0 {
                        continue;
                    }
                    let nearest = closest_point(&poly.verts, p);
                    if (nearest.y - p.y).abs() >= 3.0 {
                        continue;
                    }
                    let score = nearest.distance_squared(p);
                    if score < 16.0 && best.is_none_or(|(s, _)| score < s) {
                        best = Some((score, i));
                    }
                }
            }
        }
        best.map(|(_, i)| i)
    }
}

/// Whether a point lies inside a convex polygon (in the ground plane).
fn contains(verts: &[Vec3], p: Vec3) -> bool {
    let n = verts.len();
    let mut sign = 0.0f32;
    for k in 0..n {
        let (a, b) = (verts[k], verts[(k + 1) % n]);
        let c = (b.x - a.x) * (p.z - a.z) - (b.z - a.z) * (p.x - a.x);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// Height of a polygon at a point, from the triangle of its fan that holds it.
fn plane_height(verts: &[Vec3], p: Vec3) -> f32 {
    for k in 1..verts.len() - 1 {
        let (a, b, c) = (verts[0], verts[k], verts[k + 1]);
        let d = (b.z - c.z) * (a.x - c.x) + (c.x - b.x) * (a.z - c.z);
        if d.abs() < 1e-8 {
            continue;
        }
        let u = ((b.z - c.z) * (p.x - c.x) + (c.x - b.x) * (p.z - c.z)) / d;
        let v = ((c.z - a.z) * (p.x - c.x) + (a.x - c.x) * (p.z - c.z)) / d;
        if u >= -1e-3 && v >= -1e-3 && u + v <= 1.0 + 1e-3 {
            return a.y * u + b.y * v + c.y * (1.0 - u - v);
        }
    }
    verts.iter().map(|v| v.y).sum::<f32>() / verts.len() as f32
}

/// Projects onto a polygon's actual surface, not its centre. A noise just
/// outside a wall can be close to a large polygon whose centre is far away.
fn closest_point(verts: &[Vec3], p: Vec3) -> Vec3 {
    if contains(verts, p) {
        return Vec3::new(p.x, plane_height(verts, p), p.z);
    }
    (0..verts.len()).map(|i| {
        let (a, b) = (verts[i], verts[(i + 1) % verts.len()]);
        let edge = (b - a).xz();
        let t = ((p.xz() - a.xz()).dot(edge) / edge.length_squared().max(1e-8)).clamp(0.0, 1.0);
        a.lerp(b, t)
    }).min_by(|a, b| a.distance_squared(p).total_cmp(&b.distance_squared(p))).unwrap_or(p)
}

#[derive(PartialEq)]
struct Open(f32, u32);

impl Eq for Open {}

impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.total_cmp(&self.0)
    }
}

impl NavMesh {
    /// Spots along walls within `r` of a point: where to stand, and the
    /// direction away from the wall.
    pub fn walls_near(&self, p: Vec3, r: f32) -> Vec<(Vec3, Vec3)> {
        let (c0, c1) = (wall_cell(p - Vec3::splat(r)), wall_cell(p + Vec3::splat(r)));
        let mut out = Vec::new();
        for x in c0.0..=c1.0 {
            for z in c0.1..=c1.1 {
                if let Some(list) = self.graph.walls.get(&(x, z)) {
                    out.extend(list.iter().filter(|(w, _)| w.distance(p) <= r).copied());
                }
            }
        }
        out
    }

    /// The sector meshes loaded around the player.
    pub fn sector_meshes(&self) -> impl Iterator<Item = (SectorCoord, &Arc<CachedNav>)> {
        self.sectors.iter().map(|(c, m)| (*c, m))
    }

    /// Whether any navigation mesh is loaded around the player.
    pub fn ready(&self) -> bool {
        !self.graph.polys.is_empty()
    }

    /// The walkable surface's height at a point, if it is over the mesh near
    /// that height.
    pub fn height_at(&self, p: Vec3) -> Option<f32> {
        let i = self.graph.locate(p)?;
        let poly = &self.graph.polys[i];
        contains(&poly.verts, p).then(|| plane_height(&poly.verts, p))
    }

    /// The walkable surface right under a point, no more than `depth` below
    /// it: a lower floor a path drops down to off a ledge.
    pub fn floor_under(&self, p: Vec3, depth: f32) -> Option<f32> {
        self.graph.floor_under(p, depth)
    }

    /// The height of the mesh's nearest point to `p`, if that is within
    /// `reach` (m) of it across the ground: for one just off the mesh.
    pub fn nearest_floor(&self, p: Vec3, reach: f32) -> Option<f32> {
        let i = self.graph.locate(p)?;
        let near = closest_point(&self.graph.polys[i].verts, p);
        (near.xz().distance(p.xz()) < reach).then_some(near.y)
    }

    /// A walkable path (waypoints after the start, ending at `to` or as close as
    /// the mesh gets), if both ends are on the loaded mesh.
    pub fn path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        let g = &self.graph;
        let (start, goal) = (g.locate(from)?, g.locate(to)?);
        let on_start = closest_point(&g.polys[start].verts, from);
        let on_goal = closest_point(&g.polys[goal].verts, to);
        if start == goal {
            return Some(vec![on_goal]);
        }
        // A* over polygon centres.
        let mut came: HashMap<u32, (u32, Vec3, Vec3)> = HashMap::new();
        let mut cost: HashMap<u32, f32> = HashMap::from([(start as u32, 0.0)]);
        let mut open = BinaryHeap::from([Open(0.0, start as u32)]);
        let target = g.polys[goal].centre;
        let mut best = (g.polys[start].centre.distance(target), start as u32);
        let mut opened = 0;
        while let Some(Open(_, cur)) = open.pop() {
            if cur as usize == goal {
                best = (0.0, cur);
                break;
            }
            opened += 1;
            if opened > SEARCH_LIMIT {
                break;
            }
            let here = g.polys[cur as usize].centre;
            let base = cost[&cur];
            for &(next, p, q) in &g.polys[cur as usize].links {
                let through = (p + q) * 0.5;
                let c = base + here.distance(through) + through.distance(g.polys[next as usize].centre);
                if cost.get(&next).is_none_or(|&old| c < old) {
                    cost.insert(next, c);
                    came.insert(next, (cur, p, q));
                    let h = g.polys[next as usize].centre.distance(target);
                    if h < best.0 {
                        best = (h, next);
                    }
                    open.push(Open(c + h, next));
                }
            }
        }
        // Portals back from the reached polygon (the goal, or the closest one).
        let end = best.1;
        let mut portals = Vec::new();
        let mut at = end;
        while at as usize != start {
            let &(prev, p, q) = came.get(&at)?;
            portals.push((prev, at, p, q));
            at = prev;
        }
        portals.reverse();
        let finish = if end as usize == goal { on_goal } else { closest_point(&g.polys[end as usize].verts, to) };
        // Left and right as seen travelling from one polygon into the next.
        let mut sides = vec![(on_start, on_start)];
        for &(a, b, p, q) in &portals {
            let dir = g.polys[b as usize].centre - g.polys[a as usize].centre;
            let mid = (p + q) * 0.5;
            let s = dir.x * (p.z - mid.z) - dir.z * (p.x - mid.x);
            sides.push(if s < 0.0 { (q, p) } else { (p, q) });
        }
        sides.push((finish, finish));
        let path = funnel(&sides);
        if std::env::var_os("FC3_NAV_TRACE").is_some() {
            debug!("nav path: from {from:.3?} (poly {start}, centre {:.3?}), to {to:.3?} (poly {goal}, centre {:.3?}), reached {end}; {} portals, waypoints {path:.3?}",
                g.polys[start].centre, g.polys[goal].centre, portals.len());
        }
        Some(path)
    }
}

/// Twice the signed area of a triangle in the ground plane.
fn area2(a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let (abx, abz) = (b.x - a.x, b.z - a.z);
    let (acx, acz) = (c.x - a.x, c.z - a.z);
    acx * abz - abx * acz
}

/// The shortest way through a corridor of (left, right) portals, starting and
/// ending with degenerate portals at the two ends ("simple stupid funnel").
fn funnel(portals: &[(Vec3, Vec3)]) -> Vec<Vec3> {
    let mut out = Vec::new();
    let (mut apex, mut left, mut right) = (portals[0].0, portals[0].0, portals[0].1);
    let (mut left_i, mut right_i) = (0usize, 0usize);
    let mut i = 1;
    while i < portals.len() {
        let (l, r) = portals[i];
        // Tighten the right side.
        if area2(apex, right, r) <= 0.0 {
            if apex.xz().distance_squared(right.xz()) < 1e-8 || area2(apex, left, r) > 0.0 {
                right = r;
                right_i = i;
            } else {
                // Right crosses left: left becomes a corner.
                let k = left_i;
                apex = left;
                out.push(apex);
                right = apex;
                right_i = k;
                i = k + 1;
                continue;
            }
        }
        // Tighten the left side.
        if area2(apex, left, l) >= 0.0 {
            if apex.xz().distance_squared(left.xz()) < 1e-8 || area2(apex, right, l) < 0.0 {
                left = l;
                left_i = i;
            } else {
                let k = right_i;
                apex = right;
                out.push(apex);
                left = apex;
                left_i = k;
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }
    let end = portals[portals.len() - 1].0;
    if out.last().is_none_or(|p| p.xz().distance_squared(end.xz()) > 1e-6) {
        out.push(end);
    } else if let Some(last) = out.last_mut() {
        *last = end;
    }
    // The start itself isn't a waypoint.
    out.retain(|p| p.xz().distance_squared(portals[0].0.xz()) > 1e-6);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::cache::NavLink;

    /// An L-shaped corridor of 2 m squares: east along z = 0..2, then north
    /// (towards -z) from x = 8..10.
    fn corridor() -> NavGraph {
        let mut s = CachedNav::default();
        let mut square = |x: f32, z: f32| {
            let b = s.verts.len() as u32;
            s.verts.extend([[x, 0.0, z], [x + 2.0, 0.0, z], [x + 2.0, 0.0, z + 2.0], [x, 0.0, z + 2.0]]);
            s.polys.push(vec![b, b + 1, b + 2, b + 3]);
        };
        for k in 0..5 {
            square(k as f32 * 2.0, -40.0);
        }
        for k in 1..4 {
            square(8.0, -40.0 - k as f32 * 2.0);
        }
        for k in 0..4u32 {
            let x = (k + 1) as f32 * 2.0;
            s.links.push(NavLink { a: k, b: k + 1, left: [x, 0.0, -40.0], right: [x, 0.0, -38.0] });
        }
        for k in 0..3u32 {
            let z = -40.0 - k as f32 * 2.0;
            let a = if k == 0 { 4 } else { 4 + k };
            s.links.push(NavLink { a, b: 5 + k, left: [8.0, 0.0, z], right: [10.0, 0.0, z] });
        }
        NavGraph::build(&HashMap::from([(sector_of(Vec3::new(1.0, 0.0, -39.0)), Arc::new(s))]))
    }

    #[test]
    fn path_turns_the_corner_once() {
        let nav = NavMesh { graph: corridor(), ..default() };
        let from = Vec3::new(1.0, 0.0, -39.0);
        let to = Vec3::new(9.0, 0.0, -45.0);
        let path = nav.path(from, to).expect("path");
        assert_eq!(path.last().copied(), Some(to));
        // One corner, at the inside of the bend (x = 8, z = -40).
        assert_eq!(path.len(), 2, "{path:?}");
        assert!(path[0].distance(Vec3::new(8.0, 0.0, -40.0)) < 1e-3, "{path:?}");
    }

    #[test]
    fn straight_corridor_is_one_leg() {
        let nav = NavMesh { graph: corridor(), ..default() };
        let path = nav.path(Vec3::new(1.0, 0.0, -39.0), Vec3::new(9.0, 0.0, -39.0)).expect("path");
        assert_eq!(path, vec![Vec3::new(9.0, 0.0, -39.0)]);
    }

    #[test]
    fn height_follows_the_surface() {
        let p = vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 2.0, 0.0), Vec3::new(4.0, 2.0, 4.0), Vec3::new(0.0, 0.0, 4.0)];
        assert!((plane_height(&p, Vec3::new(2.0, 0.0, 1.0)) - 1.0).abs() < 1e-4);
        assert!(contains(&p, Vec3::new(2.0, 9.0, 2.0)) && !contains(&p, Vec3::new(5.0, 0.0, 2.0)));
    }

    #[test]
    fn nearby_polygon_is_selected_by_its_surface_not_its_centre() {
        let sector = CachedNav {
            verts: vec![[4.0, 0.0, -10.0], [24.0, 0.0, -10.0], [24.0, 0.0, -30.0], [4.0, 0.0, -30.0]],
            polys: vec![vec![0, 1, 2, 3]],
            ..Default::default()
        };
        let graph = NavGraph::build(&HashMap::from([(sector_of(Vec3::new(1.0, 0.0, -11.0)), Arc::new(sector))]));
        assert_eq!(graph.locate(Vec3::new(3.8, 0.0, -20.0)), Some(0));
    }

    #[test]
    fn nearest_surface_can_be_across_a_sector_border() {
        let graph = corridor();
        assert_eq!(graph.locate(Vec3::new(-0.2, 0.0, -39.0)), Some(0));
    }

    #[test]
    fn stacked_floors_select_the_nearest_height() {
        let mut sector = CachedNav::default();
        for height in [0.0, 4.0] {
            let base = sector.verts.len() as u32;
            sector.verts.extend([[1.0, height, -1.0], [5.0, height, -1.0], [5.0, height, -5.0], [1.0, height, -5.0]]);
            sector.polys.push(vec![base, base + 1, base + 2, base + 3]);
        }
        let graph = NavGraph::build(&HashMap::from([(sector_of(Vec3::new(2.0, 0.0, -2.0)), Arc::new(sector))]));
        assert_eq!(graph.locate(Vec3::new(2.0, 0.1, -2.0)), Some(0));
        assert_eq!(graph.locate(Vec3::new(2.0, 4.1, -2.0)), Some(1));
        assert_eq!(graph.locate(Vec3::new(2.0, 9.0, -2.0)), None);
    }

    #[test]
    fn a_nearby_floor_edge_wins_over_an_overlapping_roof() {
        let sector = CachedNav {
            verts: vec![[2.0, 0.0, -2.0], [6.0, 0.0, -2.0], [6.0, 0.0, -6.0], [2.0, 0.0, -6.0],
                [1.0, 1.5, -1.0], [7.0, 1.5, -1.0], [7.0, 1.5, -7.0], [1.0, 1.5, -7.0]],
            polys: vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
            ..Default::default()
        };
        let graph = NavGraph::build(&HashMap::from([(sector_of(Vec3::new(3.0, 0.0, -3.0)), Arc::new(sector))]));
        assert_eq!(graph.locate(Vec3::new(1.8, 0.1, -3.0)), Some(0));
    }

    #[test]
    fn path_stops_at_walkable_surface_for_an_off_mesh_goal() {
        let nav = NavMesh { graph: corridor(), ..default() };
        let path = nav.path(Vec3::new(1.0, 0.0, -39.0), Vec3::new(7.7, 0.0, -45.0)).expect("nearby surface");
        assert_eq!(path.last().copied(), Some(Vec3::new(8.0, 0.0, -45.0)));
    }

    #[test]
    fn sloped_triangulated_corridor_has_no_spurious_corners() {
        let mut sector = CachedNav::default();
        for k in 0..8 {
            let x = k as f32 * 2.0;
            let base = sector.verts.len() as u32;
            sector.verts.extend([[x, x * 0.1, -4.0], [x + 2.0, (x + 2.0) * 0.1, -4.0],
                [x + 2.0, (x + 2.0) * 0.1, -2.0], [x, x * 0.1, -2.0]]);
            sector.polys.extend([vec![base, base + 1, base + 2], vec![base, base + 2, base + 3]]);
            sector.links.push(NavLink { a: k * 2, b: k * 2 + 1, left: sector.verts[base as usize], right: sector.verts[base as usize + 2] });
            if k > 0 {
                sector.links.push(NavLink { a: k * 2 - 2, b: k * 2 + 1, left: sector.verts[base as usize], right: sector.verts[base as usize + 3] });
            }
        }
        let nav = NavMesh { graph: NavGraph::build(&HashMap::from([(sector_of(Vec3::new(1.0, 0.0, -3.0)), Arc::new(sector))])), ..default() };
        let to = Vec3::new(15.0, 1.5, -3.0);
        assert_eq!(nav.path(Vec3::new(1.0, 0.1, -3.0), to).unwrap(), vec![to]);
    }

    #[test]
    fn portal_height_seams_do_not_repeat_a_corner() {
        let from = Vec3::new(1.0, 0.0, 1.0);
        let to = Vec3::new(5.0, 0.4, -2.0);
        let path = funnel(&[(from, from), (Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
            (Vec3::new(3.0, 0.2, 0.0), Vec3::new(3.0, 0.2, 0.0)), (to, to)]);
        assert_eq!(path.len(), 2, "{path:?}");
        assert_eq!(path[0].xz(), Vec2::new(3.0, 0.0));
        assert_eq!(path[1], to);
    }
}
