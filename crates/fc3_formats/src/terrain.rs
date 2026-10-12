//! Terrain sectors (`.sdat`) and world grid addressing.
//!
//! The main world is a 160x160 grid of 64 m sectors, grouped into 10x10
//! "levels" of 16x16 sectors (named `a0`..`j9`). Sector `id = sy * 160 + sx`.
//! World (Dunia, Z-up) coordinates put sector (80, 80) at the origin, so a
//! sector's south-west corner is `((sx - 80) * 64, (sy - 80) * 64)`.
//!
//! An `.sdat` file holds a 65x65 grid of 4-byte samples starting at 0x258:
//! `u16 height * 1/64 m`, then two bytes of packed normal. Row index is local
//! +Y (north), column is local +X (east); edges are shared with neighbours.

use anyhow::{bail, Context, Result};

use crate::reader::Reader;

pub const SECTOR_SIZE: f32 = 64.0;
pub const GRID: usize = 65;
pub const WORLD_SECTORS: i32 = 160;
pub const ORIGIN_SECTOR: i32 = 80;
pub const SECTORS_PER_LEVEL: i32 = 16;
const SAMPLES_OFFSET: usize = 0x258;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SectorCoord {
    pub x: i32,
    pub y: i32,
}

impl SectorCoord {
    pub fn new(x: i32, y: i32) -> Self {
        SectorCoord { x, y }
    }

    pub fn from_id(id: u32) -> Self {
        SectorCoord { x: id as i32 % WORLD_SECTORS, y: id as i32 / WORLD_SECTORS }
    }

    pub fn valid(self) -> bool {
        (0..WORLD_SECTORS).contains(&self.x) && (0..WORLD_SECTORS).contains(&self.y)
    }

    pub fn id(self) -> u32 {
        (self.y * WORLD_SECTORS + self.x) as u32
    }

    /// Sector containing a Dunia-space (x, y) position.
    pub fn containing(x: f32, y: f32) -> Self {
        SectorCoord {
            x: (x / SECTOR_SIZE).floor() as i32 + ORIGIN_SECTOR,
            y: (y / SECTOR_SIZE).floor() as i32 + ORIGIN_SECTOR,
        }
    }

    /// South-west corner in Dunia space (x, y).
    pub fn origin(self) -> [f32; 2] {
        [((self.x - ORIGIN_SECTOR) as f32) * SECTOR_SIZE, ((self.y - ORIGIN_SECTOR) as f32) * SECTOR_SIZE]
    }

    /// Level directory name, e.g. `c5`.
    pub fn level(self) -> String {
        let letter = (b'a' + (9 - self.y / SECTORS_PER_LEVEL) as u8) as char;
        format!("{letter}{}", self.x / SECTORS_PER_LEVEL)
    }

    fn dir(self, world: &str) -> String {
        format!("worlds\\{world}\\levels\\{}\\generated", self.level())
    }

    pub fn sdat_path(self, world: &str) -> String {
        format!("{}\\sdat\\sd{}.sdat", self.dir(world), self.id())
    }

    pub fn shadow_path(self, world: &str) -> String {
        format!("{}\\sdat\\sd{}_shadow.xbt", self.dir(world), self.id())
    }

    /// Atlas textures cover 2x2 sectors and are named after the even corner.
    pub fn atlas_coord(self) -> SectorCoord {
        SectorCoord { x: self.x & !1, y: self.y & !1 }
    }

    pub fn atlas_path(self, world: &str, kind: &str) -> String {
        let a = self.atlas_coord();
        format!("{}\\sdat\\atlas{}_{kind}.xbt", a.dir(world), a.id())
    }

    pub fn entities_path(self, world: &str) -> String {
        format!("{}\\worldsectors\\worldsector_{}.data.fcb", self.dir(world), self.id())
    }

    pub fn desc_path(self, world: &str) -> String {
        format!("{}\\worldsectors\\sector{}.desc.fcb", self.dir(world), self.id())
    }

    pub fn minimap_path(self, world: &str, lod: u32) -> String {
        format!("worlds\\{world}\\minimaps\\{lod}\\{}.xbt", self.id())
    }
}

#[derive(Debug, Clone)]
pub struct TerrainSector {
    pub coord: SectorCoord,
    pub min_height: f32,
    pub max_height: f32,
    pub material: String,
    /// Row-major `[row * 65 + col]`, row = +Y, col = +X, metres.
    pub heights: Vec<f32>,
    /// Packed normal bytes per sample (raw).
    pub normal_bytes: Vec<[u8; 2]>,
    /// Terrain painter layer per mask slot (R, G, B, remainder), as indices
    /// into the world's layer table; `NO_LAYER` when unused.
    pub layers: [u8; 4],
    /// Cells (64 x 64, row-major, row = +Y) cut out of the surface where it
    /// opens into caves. Empty when the sector has none.
    pub holes: Vec<bool>,
}

pub const NO_LAYER: u8 = 0xFF;

/// Cells per sector side.
pub const CELLS: usize = GRID - 1;

/// The cut-out mark on a sample: the top three bits of its last byte all
/// set. Every sample of the placeholder sectors round the world's edge (all at
/// 200 m, with no material) carries it, and inside the islands it lines the
/// foot of the terrain where it rises over a cave's mouth (the doctor's cave
/// opens to the west through a strip of them). Bit 0x10, an earlier guess, is
/// set in only 11 sectors, in tidy rectangles with nothing beneath.
const HOLE_MARK: u8 = 0xE0;

/// Hole cells for a sector: those whose four corner samples all carry the
/// cut-out mark. Empty when the sector has none.
pub fn hole_cells(data: &[u8]) -> Vec<bool> {
    let marked = |col: usize, row: usize| data.get(SAMPLES_OFFSET + 4 * (row * GRID + col) + 3).is_some_and(|b| b & HOLE_MARK == HOLE_MARK);
    let cells: Vec<bool> =
        (0..CELLS * CELLS).map(|i| (i % CELLS, i / CELLS)).map(|(c, r)| marked(c, r) && marked(c + 1, r) && marked(c, r + 1) && marked(c + 1, r + 1)).collect();
    if cells.iter().any(|h| *h) {
        cells
    } else {
        Vec::new()
    }
}

/// A sector's water plane: the water level (Dunia height, metres) at +0x3C,
/// where either header flag at +0x34 or +0x38 says the sector has water (the
/// level is also set on dry sectors round a river or lake). Planes at the sea's
/// level are the ocean's.
pub fn water_plane(sdat: &[u8]) -> Option<f32> {
    let flag = |o: usize| sdat.get(o..o + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
    let level = f32::from_le_bytes(sdat.get(0x3C..0x40)?.try_into().ok()?);
    let wet = flag(0x34)? == 1 || flag(0x38)? == 1;
    (wet && level.is_finite() && level > 0.0).then_some(level)
}

/// One terrain painter layer: a tiling ground texture blended by the atlas mask.
#[derive(Debug, Clone, PartialEq)]
pub struct TerrainLayer {
    pub name: String,
    pub diffuse: String,
    pub normal: String,
    /// Texture repeats per sector.
    pub tiling: f32,
    /// Projection axis in Dunia space (0 = X, 1 = Y, 2 = Z/top-down).
    pub axis: u8,
    pub normal_tiling: f32,
    pub noise_level: f32,
    pub glossiness: f32,
    /// Logic material (index into `logicmaterials.lib`): what it sounds like underfoot.
    pub surface: u8,
}

/// Reads the layer table (`TerrainPainterLayerDescriptors`) from a world's
/// `generated\<world>.game.xml`.
pub fn parse_layers(game_xml: &[u8]) -> Result<Vec<TerrainLayer>> {
    let root = crate::rml::parse(game_xml)?;
    let Some(table) = root.find("TerrainPainterLayerDescriptors") else { bail!("no terrain layer table") };
    Ok(table
        .children
        .iter()
        .map(|l| {
            let a = |k: &str| l.attr(k).unwrap_or("").to_string();
            TerrainLayer {
                name: a("Name"),
                diffuse: a("Texture").to_ascii_lowercase(),
                normal: a("NormalMap").to_ascii_lowercase(),
                tiling: a("Tiling").parse().unwrap_or(16.0),
                axis: a("ProjAxis").parse().unwrap_or(2),
                normal_tiling: a("NormalMapTiling").parse().or_else(|_| a("Tiling").parse()).unwrap_or(16.0),
                noise_level: a("NoiseLevel").parse().unwrap_or(1.0),
                glossiness: a("Glossiness").parse().unwrap_or(0.3),
                surface: a("SurfaceTypeID").parse().unwrap_or(0),
            }
        })
        .collect())
}

/// The world's terrain noise transition (amplitude, sharpness, UV scale) from
/// the `Environment` settings of `generated\<world>.game.xml`.
pub fn parse_noise_transition(game_xml: &[u8]) -> [f32; 3] {
    let mut out = [0.0, 0.0, 4.0];
    if let Ok(root) = crate::rml::parse(game_xml) {
        if let Some(env) = root.find("Environment") {
            for (slot, key) in out.iter_mut().zip(["fTerrainNoiseTransitionAmplitude", "fTerrainNoiseTransitionSharpness", "fTerrainNoiseTransitionUvScale"]) {
                if let Some(v) = env.attr(key).and_then(|v| v.parse().ok()) {
                    *slot = v;
                }
            }
        }
    }
    out
}

impl TerrainSector {
    /// Whether the cell at local sector coordinates (metres) is cut away.
    pub fn hole_at(&self, lx: f32, ly: f32) -> bool {
        if self.holes.is_empty() {
            return false;
        }
        let (c, r) = ((lx.max(0.0) as usize).min(CELLS - 1), (ly.max(0.0) as usize).min(CELLS - 1));
        self.holes[r * CELLS + c]
    }

    pub fn height(&self, col: usize, row: usize) -> f32 {
        self.heights[row.min(GRID - 1) * GRID + col.min(GRID - 1)]
    }

    /// Bilinear height at local sector coordinates in metres (0..64).
    pub fn sample(&self, lx: f32, ly: f32) -> f32 {
        let fx = lx.clamp(0.0, 64.0);
        let fy = ly.clamp(0.0, 64.0);
        let x0 = (fx.floor() as usize).min(GRID - 2);
        let y0 = (fy.floor() as usize).min(GRID - 2);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        // Match the triangle split used by the render mesh (diagonal from (0,0) to (1,1)).
        let h00 = self.height(x0, y0);
        let h10 = self.height(x0 + 1, y0);
        let h01 = self.height(x0, y0 + 1);
        let h11 = self.height(x0 + 1, y0 + 1);
        if tx >= ty {
            h00 + (h10 - h00) * tx + (h11 - h10) * ty
        } else {
            h00 + (h11 - h01) * tx + (h01 - h00) * ty
        }
    }

    /// Normal in Dunia space from central differences (Z-up).
    pub fn normal(&self, col: usize, row: usize) -> [f32; 3] {
        let l = self.height(col.saturating_sub(1), row);
        let r = self.height(col + 1, row);
        let d = self.height(col, row.saturating_sub(1));
        let u = self.height(col, row + 1);
        let sx = if col == 0 || col == GRID - 1 { 1.0 } else { 2.0 };
        let sy = if row == 0 || row == GRID - 1 { 1.0 } else { 2.0 };
        let n = [-(r - l) / sx, -(u - d) / sy, 1.0];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

pub fn parse_sdat(data: &[u8]) -> Result<TerrainSector> {
    let mut r = Reader::new(data);
    let _magic = r.u32()?;
    let version = r.u32()?;
    if version != 10 {
        bail!("unsupported sdat version {version}");
    }
    let _size = r.u32()?;
    let _size2 = r.u32()?;
    let _zero = r.u32()?;
    let id = r.u32()?;
    let _zero2 = r.u32()?;
    let min_height = r.f32()?;
    let max_height = r.f32()?;
    // Layer slots are stored highest slot first.
    let lb = data.get(0x24..0x28).context("sdat too short")?;
    let layers = [lb[3], lb[2], lb[1], lb[0]];
    let mut mr = Reader::at(data, 0x4C);
    let material = mr.cstring()?;
    let end = SAMPLES_OFFSET + GRID * GRID * 4;
    if data.len() < end {
        bail!("sdat too short");
    }
    let mut heights = Vec::with_capacity(GRID * GRID);
    let mut normal_bytes = Vec::with_capacity(GRID * GRID);
    for s in data[SAMPLES_OFFSET..end].chunks_exact(4) {
        heights.push(u16::from_le_bytes([s[0], s[1]]) as f32 / 64.0);
        normal_bytes.push([s[2], s[3]]);
    }
    let holes = hole_cells(data);
    Ok(TerrainSector { coord: SectorCoord::from_id(id), min_height, max_height, material, heights, normal_bytes, layers, holes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressing() {
        let c = SectorCoord::from_id(18000);
        assert_eq!((c.x, c.y), (80, 112));
        assert_eq!(c.level(), "c5");
        assert_eq!(c.origin(), [0.0, 2048.0]);
        assert_eq!(SectorCoord::containing(13.0, 2076.0), c);
        assert_eq!(SectorCoord::from_id(18161).atlas_coord().id(), 18000);
    }

    #[test]
    fn a_cell_is_cut_when_all_its_corners_are_marked() {
        let mut data = vec![0u8; SAMPLES_OFFSET + GRID * GRID * 4];
        let mut mark = |col: usize, row: usize, byte: u8| data[SAMPLES_OFFSET + 4 * (row * GRID + col) + 3] = byte;
        // A 2 x 3 block of marked samples (other bits set too) cuts the two
        // cells inside it; a lone mark, or one missing a bit, cuts none.
        for (col, row) in [(10, 20), (11, 20), (10, 21), (11, 21), (10, 22), (11, 22)] {
            mark(col, row, 0xE5);
        }
        mark(40, 40, 0xE0);
        mark(30, 30, 0xC0);
        mark(31, 30, 0xC0);
        mark(30, 31, 0xC0);
        mark(31, 31, 0xC0);
        let holes = hole_cells(&data);
        assert_eq!(holes.len(), CELLS * CELLS);
        let cut: Vec<(usize, usize)> = (0..CELLS * CELLS).filter(|i| holes[*i]).map(|i| (i % CELLS, i / CELLS)).collect();
        assert_eq!(cut, [(10, 20), (10, 21)]);
        // None marked: no hole table at all.
        assert!(hole_cells(&vec![0u8; SAMPLES_OFFSET + GRID * GRID * 4]).is_empty());
    }
}
