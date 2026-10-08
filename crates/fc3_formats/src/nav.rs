//! Navigation meshes: `worlds\<world>\levels\<level>\generated\nv\sector<id>.rnv`.
//!
//! A sector file (`NdNS` header) holds the sector's 8 m x 8 m tiles, each a
//! Detour-style tile (`NdNT`, version 8). Some cells have several layers (a
//! floor under a roof). A tile header holds:
//!
//! * the magic and version;
//! * 13 `i32`s: `x, y, layer, polys, verts, ?, links, detail meshes,
//!   detail verts, detail tris, bv nodes, off-mesh links, off-mesh base`,
//!   where `x`/`y` are the tile's cell (`floor(coord / 8) + 640`);
//! * the agent's height, radius and climb;
//! * `bmin`, `bmax`, the bounding-volume quantisation factor and one more float;
//! * the vertices (`f32 x3`: x, height, and Dunia y);
//! * the polygons, 32 bytes each: `u32` first link, `u16` verts\[6\], `u16`
//!   neighbours\[6\], then `u8 ?, vertex count, flags, area`.
//!
//! A neighbour of 0 is a wall, `1..` is a polygon of the same tile (plus one), and
//! `0x8000 | side` leads into the next tile. Links, detail meshes and the
//! bounding-volume tree follow the polygons; they aren't needed here.
//!
//! [`build_sector`] turns a sector's tiles into one Bevy-space mesh. Edges into
//! another tile of the sector are joined where they overlap along the shared
//! border; edges on the sector's own border are kept for the game to join with
//! its neighbours.

use crate::cache::{CachedNav, NavBorder, NavLink};

pub const TILE_SIZE: f32 = 8.0;
const EXTERNAL: u16 = 0x8000;

#[derive(Debug, Clone, Default)]
pub struct NavTile {
    pub x: i32,
    pub y: i32,
    pub layer: i32,
    /// Detour space: x, height, Dunia y.
    pub verts: Vec<[f32; 3]>,
    pub polys: Vec<NavPoly>,
}

#[derive(Debug, Clone, Default)]
pub struct NavPoly {
    pub verts: Vec<u16>,
    pub neis: Vec<u16>,
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn f32_at(d: &[u8], o: usize) -> Option<f32> {
    u32_at(d, o).map(f32::from_bits)
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

/// The tiles of a sector file (malformed tiles are skipped).
pub fn parse_rnv(d: &[u8]) -> Vec<NavTile> {
    let mut tiles = Vec::new();
    let mut i = 0;
    while let Some(at) = d[i..].windows(4).position(|w| w == b"NdNT").map(|p| p + i) {
        i = at + 4;
        if let Some(t) = parse_tile(d, at) {
            tiles.push(t);
        }
    }
    tiles
}

fn parse_tile(d: &[u8], at: usize) -> Option<NavTile> {
    let int = |k: usize| u32_at(d, at + 8 + 4 * k).map(|v| v as i32);
    let (x, y, layer, poly_count, vert_count) = (int(0)?, int(1)?, int(2)?, int(3)?, int(4)?);
    if !(0..=4096).contains(&poly_count) || !(0..=8192).contains(&vert_count) {
        return None;
    }
    // Header ints, agent (3), bmin/bmax (6), quantisation and one more float.
    let mut o = at + 8 + 13 * 4 + 3 * 4 + 6 * 4 + 2 * 4;
    let mut verts = Vec::with_capacity(vert_count as usize);
    for _ in 0..vert_count {
        verts.push([f32_at(d, o)?, f32_at(d, o + 4)?, f32_at(d, o + 8)?]);
        o += 12;
    }
    let mut polys = Vec::with_capacity(poly_count as usize);
    for _ in 0..poly_count {
        let n = *d.get(o + 29)? as usize;
        if !(3..=6).contains(&n) {
            return None;
        }
        let vs: Vec<u16> = (0..n).map(|k| u16_at(d, o + 4 + 2 * k)).collect::<Option<_>>()?;
        if vs.iter().any(|&v| v as usize >= verts.len()) {
            return None;
        }
        let neis: Vec<u16> = (0..n).map(|k| u16_at(d, o + 16 + 2 * k)).collect::<Option<_>>()?;
        polys.push(NavPoly { verts: vs, neis });
        o += 32;
    }
    Some(NavTile { x, y, layer, verts, polys })
}

/// Detour space (x, height, Dunia y) to Bevy space.
pub fn to_bevy(v: [f32; 3]) -> [f32; 3] {
    [v[0], v[1], -v[2]]
}

/// An edge running along a tile border, in Detour space: which axis it lies
/// on (0: x is constant, 1: Dunia y is constant), where, and its span.
struct BorderEdge {
    poly: u32,
    tile: usize,
    axis: u8,
    line: f32,
    a: [f32; 3],
    b: [f32; 3],
}

impl BorderEdge {
    fn new(poly: u32, tile: usize, a: [f32; 3], b: [f32; 3]) -> Option<BorderEdge> {
        let axis = if (a[0] - b[0]).abs() < 1e-3 {
            0
        } else if (a[2] - b[2]).abs() < 1e-3 {
            1
        } else {
            return None;
        };
        let line = if axis == 0 { a[0] } else { a[2] };
        Some(BorderEdge { poly, tile, axis, line, a, b })
    }

    /// Position along the border.
    fn along(&self, p: [f32; 3]) -> f32 {
        if self.axis == 0 {
            p[2]
        } else {
            p[0]
        }
    }

    /// The point at `s` along the edge (height interpolated).
    fn at(&self, s: f32) -> [f32; 3] {
        let (sa, sb) = (self.along(self.a), self.along(self.b));
        let f = if (sb - sa).abs() > 1e-6 { ((s - sa) / (sb - sa)).clamp(0.0, 1.0) } else { 0.0 };
        let h = self.a[1] + (self.b[1] - self.a[1]) * f;
        if self.axis == 0 {
            [self.line, h, s]
        } else {
            [s, h, self.line]
        }
    }
}

/// Where two border edges meet: their overlap, if long enough and at about the
/// same height.
fn overlap(e: &BorderEdge, f: &BorderEdge) -> Option<([f32; 3], [f32; 3])> {
    if e.axis != f.axis || (e.line - f.line).abs() > 0.01 {
        return None;
    }
    let (e0, e1) = (e.along(e.a).min(e.along(e.b)), e.along(e.a).max(e.along(e.b)));
    let (f0, f1) = (f.along(f.a).min(f.along(f.b)), f.along(f.a).max(f.along(f.b)));
    let (lo, hi) = (e0.max(f0), e1.min(f1));
    if hi - lo < 0.05 {
        return None;
    }
    let (p, q) = (e.at(lo), e.at(hi));
    let (fp, fq) = (f.at(lo), f.at(hi));
    if (p[1] - fp[1]).abs() > 1.0 || (q[1] - fq[1]).abs() > 1.0 {
        return None;
    }
    Some((p, q))
}

/// One sector's tiles as a single mesh (Bevy space). `origin` is the sector's
/// south-west corner in Dunia (x, y) and `size` its width.
pub fn build_sector(tiles: &[NavTile], origin: [f32; 2], size: f32) -> CachedNav {
    let mut out = CachedNav::default();
    let mut externals: Vec<BorderEdge> = Vec::new();
    let on_sector_border = |e: &BorderEdge| {
        let (lo, hi) = if e.axis == 0 { (origin[0], origin[0] + size) } else { (origin[1], origin[1] + size) };
        (e.line - lo).abs() < 0.01 || (e.line - hi).abs() < 0.01
    };
    for (ti, tile) in tiles.iter().enumerate() {
        let vbase = out.verts.len() as u32;
        let pbase = out.polys.len() as u32;
        out.verts.extend(tile.verts.iter().map(|v| to_bevy(*v)));
        for (pi, poly) in tile.polys.iter().enumerate() {
            let me = pbase + pi as u32;
            out.polys.push(poly.verts.iter().map(|&v| vbase + v as u32).collect());
            for (k, &n) in poly.neis.iter().enumerate() {
                let a = tile.verts[poly.verts[k] as usize];
                let b = tile.verts[poly.verts[(k + 1) % poly.verts.len()] as usize];
                if n & EXTERNAL != 0 {
                    if let Some(e) = BorderEdge::new(me, ti, a, b) {
                        externals.push(e);
                    }
                } else if n > 0 {
                    // Same tile: each pair once.
                    let other = pbase + n as u32 - 1;
                    if me < other {
                        out.links.push(NavLink { a: me, b: other, left: to_bevy(a), right: to_bevy(b) });
                    }
                }
            }
        }
    }
    // Tile borders inside the sector: join overlapping edges of different tiles.
    let mut by_line: std::collections::HashMap<(u8, i64), Vec<usize>> = std::collections::HashMap::new();
    for (i, e) in externals.iter().enumerate() {
        if on_sector_border(e) {
            out.border.push(NavBorder { poly: e.poly, a: to_bevy(e.a), b: to_bevy(e.b) });
        } else {
            by_line.entry((e.axis, (e.line * 100.0).round() as i64)).or_default().push(i);
        }
    }
    for edges in by_line.values() {
        for (k, &i) in edges.iter().enumerate() {
            for &j in &edges[k + 1..] {
                let (e, f) = (&externals[i], &externals[j]);
                if e.tile == f.tile {
                    continue;
                }
                if let Some((p, q)) = overlap(e, f) {
                    out.links.push(NavLink { a: e.poly, b: f.poly, left: to_bevy(p), right: to_bevy(q) });
                }
            }
        }
    }
    out
}

/// Joins two loaded sectors' border edges (Bevy space): the polygon pairs and
/// their shared spans.
pub fn join_borders(a: &[NavBorder], b: &[NavBorder]) -> Vec<(u32, u32, [f32; 3], [f32; 3])> {
    let detour = |v: [f32; 3]| [v[0], v[1], -v[2]];
    let edges = |s: &[NavBorder]| -> Vec<BorderEdge> { s.iter().filter_map(|e| BorderEdge::new(e.poly, 0, detour(e.a), detour(e.b))).collect() };
    let (ea, eb) = (edges(a), edges(b));
    let mut out = Vec::new();
    for e in &ea {
        for f in &eb {
            if let Some((p, q)) = overlap(e, f) {
                out.push((e.poly, f.poly, to_bevy(p), to_bevy(q)));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(x: i32, y: i32, verts: Vec<[f32; 3]>, polys: Vec<(Vec<u16>, Vec<u16>)>) -> Vec<u8> {
        let mut d = b"NdNT".to_vec();
        d.extend(8u32.to_le_bytes());
        for v in [x, y, 0, polys.len() as i32, verts.len() as i32, 0, 0, 0, 0, 0, 0, 0, 0] {
            d.extend(v.to_le_bytes());
        }
        for _ in 0..(3 + 6 + 2) {
            d.extend(0f32.to_le_bytes());
        }
        for v in &verts {
            for c in v {
                d.extend(c.to_le_bytes());
            }
        }
        for (vs, ns) in &polys {
            d.extend(u32::MAX.to_le_bytes());
            for k in 0..6 {
                d.extend(vs.get(k).copied().unwrap_or(0).to_le_bytes());
            }
            for k in 0..6 {
                d.extend(ns.get(k).copied().unwrap_or(0).to_le_bytes());
            }
            d.extend([0, vs.len() as u8, 1, 2]);
        }
        d
    }

    #[test]
    fn tiles_join_across_their_border() {
        // Two 8 m tiles side by side along x, one square each (two triangles in the first).
        let mut d = b"NdNS".to_vec();
        d.extend(tile(
            0,
            0,
            vec![[0.0, 1.0, 0.0], [8.0, 1.0, 0.0], [8.0, 1.0, 8.0], [0.0, 1.0, 8.0]],
            vec![(vec![0, 1, 2], vec![0, EXTERNAL, 2]), (vec![0, 2, 3], vec![1, EXTERNAL | 2, 0])],
        ));
        d.extend(tile(1, 0, vec![[8.0, 1.2, 0.0], [16.0, 1.2, 0.0], [16.0, 1.2, 8.0], [8.0, 1.2, 8.0]], vec![(vec![0, 1, 2, 3], vec![0, 0, 0, EXTERNAL | 4])]));
        let tiles = parse_rnv(&d);
        assert_eq!(tiles.len(), 2);
        let nav = build_sector(&tiles, [0.0, 0.0], 64.0);
        assert_eq!(nav.polys.len(), 3);
        // The in-tile pair, and the first triangle with the second tile across x = 8.
        assert!(nav.links.iter().any(|l| (l.a, l.b) == (0, 1)));
        let across = nav.links.iter().find(|l| (l.a == 0 && l.b == 2) || (l.a == 2 && l.b == 0)).expect("tiles joined");
        assert!((across.left[0] - 8.0).abs() < 1e-4 && (across.right[0] - 8.0).abs() < 1e-4);
        // The second triangle's edge at y = 8 is inside the sector with nothing
        // across it, so it is dropped; nothing lies on the sector's border.
        assert!(nav.border.is_empty());
        // Bevy z is the negated Dunia y.
        assert_eq!(nav.verts[3], [0.0, 1.0, -8.0]);
    }
}
