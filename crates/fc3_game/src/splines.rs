//! Roads and rivers from the world's splines (`fc3_formats::splines`).
//!
//! Roads are ribbons over the terrain with their road materials (dirt, sand,
//! asphalt, jungle paths): the road's map, dyed, laid through a coverage map
//! that frays the edges (`road.wgsl`). Each follows the ground across and
//! along it, a little above.
//!
//! Rivers are ribbons at their water's level, over the water planes (see
//! `water`), that carry the current: their material's ripples run downstream
//! at its flow speed, with foam in the rapids (`river.wgsl`). The splines'
//! distances run downstream.
//!
//! Both are built once the terrain under them has streamed in near the player.
//!
//! The roads also make the GPS's road network (`RoadGraph`): the points along
//! each road, joined where roads meet, for routes to the map's waypoint.

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use fc3_formats::cache::{self, CachedMaterial};
use fc3_formats::splines::SplinePrimitive;
use fc3_formats::terrain::SectorCoord;
use std::collections::HashMap;

use crate::cache_assets::cache_path;
use crate::data::CacheInfo;
use crate::player::PlayerCamera;
use crate::terrain::Heightfield;

pub struct SplinesPlugin;

impl Plugin for SplinesPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "road.wgsl");
        bevy::asset::embedded_asset!(app, "river.wgsl");
        app.add_plugins((MaterialPlugin::<RoadMaterial>::default(), MaterialPlugin::<RiverMaterial>::default()))
            .init_resource::<Roads>()
            .add_systems(Update, (load_roads.run_if(resource_exists::<CacheInfo>), stream_roads));
    }
}

pub type RoadMaterial = ExtendedMaterial<StandardMaterial, RoadExt>;

#[derive(ShaderType, Clone, Copy, Debug, Default)]
pub struct RoadParams {
    /// rgb: the dye (linear), w: the map's repeats across the road.
    pub tint: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct RoadExt {
    #[uniform(100)]
    pub params: RoadParams,
    #[texture(101)]
    #[sampler(102)]
    pub map: Handle<Image>,
    #[texture(103)]
    pub coverage: Handle<Image>,
}

impl MaterialExtension for RoadExt {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("road.wgsl")).with_source("embedded").into()
    }
}

/// Road material parameters (name hashes): the road's map and its tiling,
/// the coverage map, and the dye.
mod param {
    pub const MAP: u32 = 0x6051_D015;
    pub const MAP_TILING: u32 = 0x7C2C_A022;
    pub const COVERAGE: u32 = 0xF958_81AF;
    pub const TINT: u32 = 0x974D_A6CD;
}

pub type RiverMaterial = ExtendedMaterial<StandardMaterial, RiverExt>;

#[derive(ShaderType, Clone, Copy, Debug, Default)]
pub struct RiverParams {
    /// rgb: the water's colour (linear), a: foam (rapids).
    pub colour: Vec4,
    /// x: metres per tile of the maps, y: flow speed (m/s).
    pub flow: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub struct RiverExt {
    #[uniform(100)]
    pub params: RiverParams,
    #[texture(101)]
    #[sampler(102)]
    pub ripples: Handle<Image>,
    #[texture(103)]
    pub foam: Handle<Image>,
}

impl MaterialExtension for RiverExt {
    fn fragment_shader() -> ShaderRef {
        bevy::asset::AssetPath::from_path_buf(bevy::asset::embedded_path!("river.wgsl")).with_source("embedded").into()
    }
}

/// River material parameters (name hashes): its ripples' normal map, their
/// tile size (m) and flow speed (m/s), the foam map (rapids only) and the
/// water's colour.
mod river_param {
    pub const RIPPLES: u32 = 0x8C65_8791;
    pub const TILE: u32 = 0x4D26_EAD9;
    pub const FLOW: u32 = 0x9444_8C99;
    pub const FOAM: u32 = 0xEE34_B08B;
    pub const COLOUR: u32 = 0x2591_A0B2;
}

/// A river material from a converted `WaterRiver` one.
fn river_material(cm: &CachedMaterial, server: &AssetServer, materials: &mut Assets<RiverMaterial>) -> Option<Handle<RiverMaterial>> {
    // (An unset map is an empty path: no map, not a missing one, which would
    // load as plain white and turn the whole river to foam.)
    let tex = |h: u32| cm.textures.iter().find(|(k, p)| *k == h && !p.is_empty()).map(|(_, p)| p.clone());
    let floats = |h: u32| cm.params.iter().find(|(k, _)| *k == h).map(|(_, raw)| fc3_formats::original::param_floats(raw)).unwrap_or_default();
    let ripples = tex(river_param::RIPPLES)?;
    let foam = tex(river_param::FOAM);
    let c = floats(river_param::COLOUR);
    let colour = if c.len() >= 3 { Color::srgb(c[0], c[1], c[2]).to_linear() } else { LinearRgba::rgb(0.01, 0.04, 0.03) };
    let tile = floats(river_param::TILE).first().copied().filter(|v| *v > 0.0).unwrap_or(6.0);
    let speed = floats(river_param::FLOW).first().copied().unwrap_or(1.0);
    Some(materials.add(RiverMaterial {
        base: StandardMaterial { perceptual_roughness: 0.06, reflectance: 0.35, alpha_mode: AlphaMode::Blend, depth_bias: RIVER_DEPTH_BIAS, ..default() },
        extension: RiverExt {
            params: RiverParams {
                colour: Vec4::new(colour.red, colour.green, colour.blue, if foam.is_some() { 1.0 } else { 0.0 }),
                flow: Vec4::new(tile, speed, 0.0, 0.0),
            },
            ripples: load_tiled(server, &ripples, false),
            foam: load_tiled(server, foam.as_ref().unwrap_or(&ripples), true),
        },
    }))
}

fn load_tiled(server: &AssetServer, rel: &str, srgb: bool) -> Handle<Image> {
    server
        .load_builder()
        .with_settings(move |s: &mut ImageLoaderSettings| {
            s.is_srgb = srgb;
            s.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                mipmap_filter: ImageFilterMode::Linear,
                anisotropy_clamp: 8,
                ..default()
            });
            s.asset_usage = RenderAssetUsages::RENDER_WORLD;
        })
        .load(cache_path(rel))
}

/// A road material from a converted `Road` one, or None if it lacks a map.
fn road_material(cm: &CachedMaterial, server: &AssetServer, materials: &mut Assets<RoadMaterial>) -> Option<Handle<RoadMaterial>> {
    let tex = |h: u32| cm.textures.iter().find(|(k, _)| *k == h).map(|(_, p)| p.clone());
    let floats = |h: u32| cm.params.iter().find(|(k, _)| *k == h).map(|(_, raw)| fc3_formats::original::param_floats(raw)).unwrap_or_default();
    let map = tex(param::MAP).or_else(|| Some(cm.diffuse.clone()).filter(|d| !d.is_empty()))?;
    let coverage = tex(param::COVERAGE).unwrap_or_else(|| map.clone());
    let tint = floats(param::TINT);
    let dye = if tint.len() >= 3 { Color::srgb(tint[0], tint[1], tint[2]).to_linear() } else { LinearRgba::WHITE };
    let across = floats(param::MAP_TILING).first().copied().filter(|v| *v > 0.0).unwrap_or(1.0);
    Some(materials.add(RoadMaterial {
        base: StandardMaterial { perceptual_roughness: 0.85, reflectance: 0.2, alpha_mode: AlphaMode::Blend, depth_bias: ROAD_DEPTH_BIAS, ..default() },
        extension: RoadExt {
            params: RoadParams { tint: Vec4::new(dye.red, dye.green, dye.blue, across) },
            map: load_tiled(server, &map, true),
            coverage: load_tiled(server, &coverage, false),
        },
    }))
}

/// Roads are drawn out to this distance at most (their own is often more).
const ROAD_RANGE: f32 = 450.0;
/// Height above the ground, and how many columns across a ribbon has.
const LIFT: f32 = 0.06;
const COLUMNS: usize = 5;
/// Pull toward the camera (Depth32 units) so the road wins over the ground.
const ROAD_DEPTH_BIAS: f32 = 16000.0;
/// Rivers sit this little above their water plane, and are drawn this far.
const RIVER_LIFT: f32 = 0.03;
const RIVER_DEPTH_BIAS: f32 = 8000.0;
const RIVER_RANGE: f32 = 500.0;

enum Look {
    Road(Handle<RoadMaterial>),
    River(Handle<RiverMaterial>),
}

struct Road {
    spline: SplinePrimitive,
    look: Look,
    /// Middle (Bevy space) and reach from it.
    centre: Vec3,
    radius: f32,
    /// Sectors the ribbon crosses (all must have terrain before it's built).
    sectors: Vec<SectorCoord>,
}

#[derive(Resource, Default)]
struct Roads {
    loaded: bool,
    list: Vec<Road>,
    shown: HashMap<usize, Entity>,
}

fn bevy_of(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], -p[1])
}

/// Reads the road and river splines.
fn load_roads(
    info: Res<CacheInfo>,
    server: Res<AssetServer>,
    mut materials: ResMut<Assets<RoadMaterial>>,
    mut rivers: ResMut<Assets<RiverMaterial>>,
    mut roads: ResMut<Roads>,
    mut commands: Commands,
) {
    if roads.loaded {
        return;
    }
    roads.loaded = true;
    let splines = std::fs::read(info.root.join(cache::SPLINES_REL)).ok().and_then(|b| cache::splines_from_bytes(&b).ok()).unwrap_or_default();
    // One material per spline material, by its shader.
    let mut made: HashMap<String, Option<(bool, Handle<RoadMaterial>, Handle<RiverMaterial>)>> = HashMap::new();
    for (_, s) in splines {
        let material = made
            .entry(s.material.clone())
            .or_insert_with(|| {
                let cm = std::fs::read(info.root.join(cache::material_rel(&s.material))).ok().and_then(|b| CachedMaterial::from_bytes(&b).ok())?;
                match cm.shader.as_str() {
                    "Road" => road_material(&cm, &server, &mut materials).map(|m| (true, m, Handle::default())),
                    "WaterRiver" => river_material(&cm, &server, &mut rivers).map(|m| (false, Handle::default(), m)),
                    _ => None,
                }
            })
            .clone();
        let Some((is_road, road, river)) = material.filter(|_| s.sections.len() >= 2) else { continue };
        let look = if is_road { Look::Road(road) } else { Look::River(river) };
        let pts: Vec<Vec3> = s.sections.iter().map(|sec| bevy_of(sec.points[0])).collect();
        let centre = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
        let radius = pts.iter().map(|p| p.distance(centre)).fold(0.0, f32::max) + s.half_width;
        let mut sectors: Vec<SectorCoord> = s.sections.iter().map(|sec| SectorCoord::containing(sec.points[0][0], sec.points[0][1])).collect();
        sectors.dedup();
        roads.list.push(Road { spline: s, look, centre, radius, sectors });
    }
    let rivers = roads.list.iter().filter(|r| matches!(r.look, Look::River(_))).count();
    // The road network: every road, and the vehicle patrols' paths (which
    // carry on over the bridges, where there's no road spline).
    let mut lines: Vec<Vec<Vec2>> = roads
        .list
        .iter()
        .filter(|r| matches!(r.look, Look::Road(_)))
        .map(|r| r.spline.sections.iter().map(|sec| Vec2::new(sec.points[0][0], -sec.points[0][1])).collect())
        .collect();
    let patrols = std::fs::read_to_string(info.root.join(cache::PATROLS_REL)).unwrap_or_default();
    for route in fc3_formats::patrols::from_text(&patrols) {
        let mut line: Vec<Vec2> = route.points.iter().map(|p| Vec2::new(p[0], p[2])).collect();
        if route.looped {
            line.extend(line.first().copied());
        }
        lines.push(line);
    }
    let graph = RoadGraph::build(lines);
    info!("splines: {} roads, {rivers} rivers; road network {} points", roads.list.len() - rivers, graph.points.len());
    commands.insert_resource(graph);
}

/// The road network for the GPS: points along the roads (Bevy x, z) and the
/// links between them.
#[derive(Resource, Default)]
pub struct RoadGraph {
    pub points: Vec<Vec2>,
    links: Vec<Vec<u32>>,
    /// Points by grid cell, for finding the nearest.
    cells: HashMap<(i32, i32), Vec<u32>>,
}

/// Grid cell size, how close a road's end must come to another road to join
/// it, how close two roads' points must be where they cross, and the spacing
/// kept between points along a road.
const GRAPH_CELL: f32 = 32.0;
const JOIN: f32 = 12.0;
/// How far ahead of a road's end (within the cone) it joins another road.
const JOIN_AHEAD: f32 = 30.0;
const JOIN_CONE: f32 = 0.5;
/// From a road's dead end, the route may cut across to a road this near, at
/// this many times the distance.
const GAP: f32 = 80.0;
const GAP_COST: f32 = 2.5;
const CROSS: f32 = 3.0;
const SPACING: f32 = 4.0;

impl RoadGraph {
    fn cell(p: Vec2) -> (i32, i32) {
        ((p.x / GRAPH_CELL).floor() as i32, (p.y / GRAPH_CELL).floor() as i32)
    }

    fn add(&mut self, p: Vec2) -> u32 {
        let i = self.points.len() as u32;
        self.points.push(p);
        self.links.push(Vec::new());
        self.cells.entry(Self::cell(p)).or_default().push(i);
        i
    }

    fn link(&mut self, a: u32, b: u32) {
        if a != b && !self.links[a as usize].contains(&b) {
            self.links[a as usize].push(b);
            self.links[b as usize].push(a);
        }
    }

    /// Points within `r` of `p`.
    fn near(&self, p: Vec2, r: f32) -> impl Iterator<Item = u32> + '_ {
        let (cx, cy) = Self::cell(p);
        let reach = (r / GRAPH_CELL).ceil() as i32;
        (-reach..=reach)
            .flat_map(move |dy| (-reach..=reach).map(move |dx| (cx + dx, cy + dy)))
            .filter_map(|c| self.cells.get(&c))
            .flatten()
            .copied()
            .filter(move |&i| self.points[i as usize].distance(p) <= r)
    }

    /// Lines (points along each road, in order) joined into a network: each
    /// line's ends to the nearest points of the roads they come to, and
    /// wherever two lines cross.
    fn build(lines: Vec<Vec<Vec2>>) -> RoadGraph {
        let mut g = RoadGraph::default();
        let mut line_of: Vec<u32> = Vec::new();
        let mut ends = Vec::new();
        for (li, line) in lines.iter().enumerate() {
            let mut prev: Option<u32> = None;
            let mut first = None;
            let n = line.len();
            for (k, &p) in line.iter().enumerate() {
                if let Some(q) = prev {
                    let from = g.points[q as usize];
                    // Keep the ends, and points every few metres between
                    // (filled in where the line is sparse).
                    if k + 1 < n && from.distance(p) < SPACING {
                        continue;
                    }
                    let steps = (from.distance(p) / SPACING).floor() as usize;
                    let mut last = q;
                    for j in 1..steps {
                        let m = g.add(from.lerp(p, j as f32 / steps as f32));
                        line_of.push(li as u32);
                        g.link(last, m);
                        last = m;
                    }
                    prev = Some(last);
                }
                let i = g.add(p);
                line_of.push(li as u32);
                if let Some(q) = prev {
                    g.link(q, i);
                }
                first.get_or_insert(i);
                prev = Some(i);
            }
            // Each end and the way the road runs out of it.
            if let (Some(a), Some(b)) = (first, prev) {
                let (pa, pb) = (g.points[a as usize], g.points[b as usize]);
                let second = g.links[a as usize].first().map_or(pb, |&j| g.points[j as usize]);
                let before_last = g.links[b as usize].first().map_or(pa, |&j| g.points[j as usize]);
                ends.push((a, (pa - second).normalize_or_zero()));
                ends.push((b, (pb - before_last).normalize_or_zero()));
            }
        }
        // The nearest point of each other line within `r` of point `i`.
        let others = |g: &RoadGraph, i: u32, r: f32| {
            let p = g.points[i as usize];
            let mut best: HashMap<u32, (u32, f32)> = HashMap::new();
            for j in g.near(p, r) {
                let lj = line_of[j as usize];
                if lj == line_of[i as usize] {
                    continue;
                }
                let d = g.points[j as usize].distance(p);
                if best.get(&lj).is_none_or(|b| d < b.1) {
                    best.insert(lj, (j, d));
                }
            }
            best.into_values().map(|(j, _)| j).collect::<Vec<u32>>()
        };
        // Roads join where one's end comes near another road (splines stop
        // short of the road they meet, often by 15-25 m: further when it
        // lies ahead of the end)...
        for (e, dir) in ends {
            let at = g.points[e as usize];
            for j in others(&g, e, JOIN_AHEAD) {
                let to = g.points[j as usize] - at;
                if to.length() <= JOIN || to.normalize_or_zero().dot(dir) > JOIN_CONE {
                    g.link(e, j);
                }
            }
        }
        // ...and where two cross.
        for i in 0..g.points.len() as u32 {
            for j in others(&g, i, CROSS) {
                g.link(i, j);
            }
        }
        if std::env::var("FC3_GPS_DEBUG").is_ok() {
            let mut comp = vec![u32::MAX; g.points.len()];
            let mut sizes = Vec::new();
            for s in 0..g.points.len() {
                if comp[s] != u32::MAX {
                    continue;
                }
                let id = sizes.len() as u32;
                let mut stack = vec![s as u32];
                let mut n = 0;
                comp[s] = id;
                while let Some(i) = stack.pop() {
                    n += 1;
                    for &j in &g.links[i as usize] {
                        if comp[j as usize] == u32::MAX {
                            comp[j as usize] = id;
                            stack.push(j);
                        }
                    }
                }
                sizes.push(n);
            }
            let mut sorted = sizes.clone();
            sorted.sort_unstable_by(|a, b| b.cmp(a));
            info!("gps: {} pieces, largest {:?}", sizes.len(), &sorted[..sorted.len().min(8)]);
            // Where each of the biggest pieces comes nearest another big one.
            let mut order: Vec<u32> = (0..sizes.len() as u32).collect();
            order.sort_by_key(|&c| std::cmp::Reverse(sizes[c as usize]));
            let big: Vec<u32> = order.iter().copied().filter(|&c| sizes[c as usize] > 300).collect();
            for &c in big.iter().take(10) {
                let mut best = (f32::MAX, Vec2::ZERO, u32::MAX);
                for i in (0..g.points.len()).filter(|&i| comp[i] == c) {
                    let p = g.points[i];
                    for j in g.near(p, 400.0) {
                        let cj = comp[j as usize];
                        if cj != c && big.contains(&cj) {
                            let d = g.points[j as usize].distance(p);
                            if d < best.0 {
                                best = (d, p, cj);
                            }
                        }
                    }
                }
                info!(
                    "gps: piece {c} ({} points) nearest big piece {} at {:.0} m, near Dunia ({:.0}, {:.0})",
                    sizes[c as usize], best.2, best.0, best.1.x, best.1.y
                );
            }
        }
        g
    }

    /// The nearest point within `r` of `p`.
    pub fn nearest(&self, p: Vec2, r: f32) -> Option<u32> {
        self.near(p, r).min_by(|&a, &b| self.points[a as usize].distance(p).total_cmp(&self.points[b as usize].distance(p)))
    }

    /// The shortest way along the roads between two points (A*).
    pub fn route(&self, from: u32, to: u32) -> Option<Vec<Vec2>> {
        use std::cmp::Ordering;
        use std::collections::BinaryHeap;
        #[derive(PartialEq)]
        struct Open(f32, u32);
        impl Eq for Open {}
        impl PartialOrd for Open {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Open {
            fn cmp(&self, o: &Self) -> Ordering {
                o.0.total_cmp(&self.0)
            }
        }
        let goal = self.points[to as usize];
        let mut cost: HashMap<u32, f32> = HashMap::from([(from, 0.0)]);
        let mut came: HashMap<u32, u32> = HashMap::new();
        let mut open = BinaryHeap::from([Open(self.points[from as usize].distance(goal), from)]);
        let mut steps = 0;
        while let Some(Open(_, i)) = open.pop() {
            if i == to {
                let mut path = vec![self.points[i as usize]];
                let mut c = i;
                while let Some(&p) = came.get(&c) {
                    path.push(self.points[p as usize]);
                    c = p;
                }
                path.reverse();
                return Some(path);
            }
            steps += 1;
            if steps > 400_000 {
                return None;
            }
            let g = cost[&i];
            let here = self.points[i as usize];
            // Along the road; and from a road's dead end (at a bridge, a
            // square, a village), across to the nearest roads round about, at
            // a price.
            let gaps: Vec<(u32, f32)> = if self.links[i as usize].len() <= 1 {
                self.near(here, GAP).filter(|j| *j != i && !self.links[i as usize].contains(j)).map(|j| (j, GAP_COST)).collect()
            } else {
                Vec::new()
            };
            for (j, factor) in self.links[i as usize].iter().map(|&j| (j, 1.0)).chain(gaps) {
                let ng = g + here.distance(self.points[j as usize]) * factor;
                if cost.get(&j).is_none_or(|&c| ng < c) {
                    cost.insert(j, ng);
                    came.insert(j, i);
                    open.push(Open(ng + self.points[j as usize].distance(goal), j));
                }
            }
        }
        None
    }
}

/// A ribbon across the sections, `COLUMNS` points from one edge through the
/// centre to the other: a road's on the ground (or the spline where there's
/// none), with UVs across 0..1 and along in road widths; a river's at the
/// spline's own height, with UVs in metres and its position across in the
/// vertex colour.
fn ribbon_mesh(s: &SplinePrimitive, heights: &Heightfield, river: bool) -> Mesh {
    let full = (s.half_width * 2.0).max(0.5);
    let mut positions = Vec::with_capacity(s.sections.len() * COLUMNS);
    let mut uvs = Vec::with_capacity(positions.capacity());
    let mut colors = Vec::with_capacity(positions.capacity());
    for sec in &s.sections {
        let (c, a, b) = (bevy_of(sec.points[0]), bevy_of(sec.points[1]), bevy_of(sec.points[2]));
        for k in 0..COLUMNS {
            let u = k as f32 / (COLUMNS - 1) as f32;
            let mut p = if u <= 0.5 { a.lerp(c, u * 2.0) } else { c.lerp(b, (u - 0.5) * 2.0) };
            if river {
                p.y += RIVER_LIFT;
                uvs.push([u * full, sec.distance]);
            } else {
                p.y = heights.height_at(p).unwrap_or(p.y) + LIFT;
                uvs.push([u, sec.distance / full]);
            }
            positions.push(p.to_array());
            // The section's weight fades the ends into the next piece.
            colors.push([u, 1.0, 1.0, sec.weight.clamp(0.0, 1.0)]);
        }
    }
    let mut indices = Vec::with_capacity((s.sections.len() - 1) * (COLUMNS - 1) * 6);
    // Wound to face up, whichever order the edges came in.
    let up = |t: [u32; 3]| {
        let [a, b, c] = t.map(|i| Vec3::from_array(positions[i as usize]));
        (b - a).cross(c - a).y >= 0.0
    };
    for i in 0..s.sections.len() - 1 {
        for k in 0..COLUMNS - 1 {
            let (a, b) = ((i * COLUMNS + k) as u32, ((i + 1) * COLUMNS + k) as u32);
            for t in [[a, b, a + 1], [a + 1, b, b + 1]] {
                indices.extend_from_slice(&if up(t) { t } else { [t[0], t[2], t[1]] });
            }
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh.compute_smooth_normals();
    mesh
}

/// Builds the roads near the camera once their ground is in, drops far ones.
fn stream_roads(
    time: Res<Time<Real>>,
    mut next: Local<f32>,
    mut roads: ResMut<Roads>,
    heights: Res<Heightfield>,
    cams: Query<&GlobalTransform, With<PlayerCamera>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    if time.elapsed_secs() < *next || roads.list.is_empty() {
        return;
    }
    *next = time.elapsed_secs() + 0.5;
    let Ok(cam) = cams.single() else { return };
    let eye = cam.translation();
    let mut built = 0;
    let Roads { list, shown, .. } = &mut *roads;
    for (i, r) in list.iter().enumerate() {
        let d = r.centre.distance(eye) - r.radius;
        let range = r.spline.draw_distance.min(if matches!(r.look, Look::River(_)) { RIVER_RANGE } else { ROAD_RANGE });
        match shown.get(&i).copied() {
            Some(e) if d > range + 80.0 => {
                if e != Entity::PLACEHOLDER {
                    commands.entity(e).despawn();
                }
                shown.remove(&i);
            }
            None if d < range && built < 24 && r.sectors.iter().all(|c| heights.sectors.contains_key(c)) => {
                let e = match &r.look {
                    Look::Road(m) => {
                        let mesh = meshes.add(ribbon_mesh(&r.spline, &heights, false));
                        commands.spawn((Mesh3d(mesh), MeshMaterial3d(m.clone()), Transform::IDENTITY, NotShadowCaster, Name::new("road"))).id()
                    }
                    // Only over an inland plane at its height (the sea's own
                    // shader covers river mouths at sea level).
                    Look::River(m) => {
                        let level = crate::water::plane_level(r.centre);
                        if level <= crate::sky::SEA_LEVEL + 0.5 || (level - r.centre.y).abs() > 1.5 {
                            shown.insert(i, Entity::PLACEHOLDER);
                            continue;
                        }
                        let mesh = meshes.add(ribbon_mesh(&r.spline, &heights, true));
                        commands.spawn((Mesh3d(mesh), MeshMaterial3d(m.clone()), Transform::IDENTITY, NotShadowCaster, Name::new("river"))).id()
                    }
                };
                shown.insert(i, e);
                built += 1;
            }
            _ => {}
        }
    }
}
