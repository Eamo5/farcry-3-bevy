//! Converted-asset cache: compact, Bevy-ready binary formats.
//!
//! The converter (`fc3_convert`) reads the installed game once and writes:
//!
//! * `textures/<hash>.dds`  merged DDS (top mip from `_mip0.xbt` + chain)
//! * `models/<hash>.fcm`    meshes for every LOD, material refs, collision, skin
//! * `materials/<hash>.fcmat` one shared material (texture refs + flags)
//! * `terrain/<id>.fct`     65x65 heights for one sector
//! * `terrain/atlas<id>_<kind>.dds`, `terrain/sd<id>_shadow.dds`
//! * `sectors/<id>.fcs`     placed static models + gameplay entities
//!
//! `<hash>` is the 16-hex-digit CRC-64 of the original archive path, so the
//! game can resolve any original path without a lookup table. All geometry is
//! already in Bevy space (Y-up). Files are little-endian, chunked
//! (`tag, len, payload`) so new data can be added without breaking readers.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::hash::path_hash;
use crate::reader::Reader;
use crate::terrain::NO_LAYER;

/// Bump when any cached format changes; the converter rebuilds on mismatch.
pub const CACHE_VERSION: u32 = 13;

pub fn hash_name(path: &str) -> String {
    format!("{:016x}", path_hash(path))
}

pub fn texture_rel(original: &str) -> String {
    format!("textures/{}.dds", hash_name(original))
}

pub fn model_rel(original: &str) -> String {
    format!("models/{}.fcm", hash_name(original))
}

pub fn material_rel(original: &str) -> String {
    format!("materials/{}.fcmat", hash_name(original))
}

/// Extracted animation clip (raw `.mab`, parsed at load time with `anim::parse_clip`).
pub fn anim_rel(original: &str) -> String {
    format!("anims/{}.mab", hash_name(original))
}

/// Extracted skeleton (raw `.skeleton`, parsed with `anim::parse_skeleton`).
pub fn skeleton_rel(original: &str) -> String {
    format!("skeletons/{}.skeleton", hash_name(original))
}

pub fn terrain_rel(sector_id: u32) -> String {
    format!("terrain/{sector_id}.fct")
}

pub fn atlas_rel(atlas_id: u32, kind: &str) -> String {
    format!("terrain/atlas{atlas_id}_{kind}.dds")
}

/// The world's terrain layer table (see `TerrainLayerTable`).
pub const TERRAIN_LAYERS_REL: &str = "terrain/layers.txt";

pub fn shadow_rel(sector_id: u32) -> String {
    format!("terrain/sd{sector_id}_shadow.dds")
}

/// The whole world's land, coarse, for far away (see `FarTerrain`).
pub const FAR_TERRAIN_REL: &str = "terrain/far.bin";

/// The world's terrain at 32 m spacing (2 samples per sector side), for the
/// land beyond the streamed sectors: heights (Dunia z, metres) and the baked
/// ground colour (sRGB), row-major with rows running north from the world's
/// south-west corner. Where there is no land, the height lies below the sea.
/// Only the islands are kept (`islands`, one flag per sector): the world also
/// holds walled-off areas far out to sea that the game never shows from them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FarTerrain {
    pub size: u32,
    pub heights: Vec<f32>,
    pub colours: Vec<[u8; 3]>,
    pub islands: Vec<bool>,
}

impl FarTerrain {
    /// Metres between samples.
    pub const SPACING: f32 = 32.0;
    /// Height where there is no land.
    pub const SEA_FLOOR: f32 = 60.0;

    pub fn new() -> FarTerrain {
        let size = (crate::terrain::WORLD_SECTORS * 2) as u32;
        let n = size as usize * size as usize;
        let sectors = (crate::terrain::WORLD_SECTORS * crate::terrain::WORLD_SECTORS) as usize;
        FarTerrain { size, heights: vec![Self::SEA_FLOOR; n], colours: vec![[70, 80, 60]; n], islands: vec![false; sectors] }
    }

    /// Whether a sector belongs to the islands.
    pub fn on_islands(&self, sector_id: u32) -> bool {
        self.islands.get(sector_id as usize).copied().unwrap_or(false)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = b"FAR1".to_vec();
        out.extend_from_slice(&self.size.to_le_bytes());
        for h in &self.heights {
            out.extend_from_slice(&h.to_le_bytes());
        }
        for c in &self.colours {
            out.extend_from_slice(c);
        }
        out.extend(self.islands.iter().map(|&b| b as u8));
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<FarTerrain> {
        let size = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?);
        let n = size as usize * size as usize;
        let heights = b.get(8..8 + n * 4)?.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        let colours = b.get(8 + n * 4..8 + n * 7)?.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        let sectors = (crate::terrain::WORLD_SECTORS * crate::terrain::WORLD_SECTORS) as usize;
        let islands = b.get(8 + n * 7..8 + n * 7 + sectors)?.iter().map(|&v| v != 0).collect();
        b.starts_with(b"FAR1").then_some(FarTerrain { size, heights, colours, islands })
    }
}

impl Default for FarTerrainBuilder {
    fn default() -> Self {
        FarTerrainBuilder(FarTerrain::new())
    }
}

/// Fills a `FarTerrain` sector by sector.
pub struct FarTerrainBuilder(pub FarTerrain);

impl FarTerrainBuilder {
    /// One sector's two-by-two samples: heights from its grid, colours from
    /// a lookup of the baked ground colour at sector-local metres.
    pub fn put(&mut self, t: &CachedTerrain, colour: impl Fn(f32, f32) -> Option<[u8; 3]>) {
        let c = crate::terrain::SectorCoord::from_id(t.sector_id);
        let size = self.0.size as usize;
        if let Some(f) = self.0.islands.get_mut(t.sector_id as usize) {
            *f = true;
        }
        for j in 0..2 {
            for i in 0..2 {
                let (gx, gy) = (i * 32, j * 32);
                let h = t.heights.get(gy * 65 + gx).map_or(FarTerrain::SEA_FLOOR, |&v| v as f32 / 64.0);
                let (x, y) = (c.x as usize * 2 + i, c.y as usize * 2 + j);
                let o = y * size + x;
                self.0.heights[o] = h;
                // A few texels round the sample, averaged.
                let mut sum = [0u32; 3];
                let mut n = 0;
                for (dx, dy) in [(0.0, 0.0), (8.0, 8.0), (-8.0, 8.0), (8.0, -8.0), (-8.0, -8.0)] {
                    if let Some(px) = colour(gx as f32 + 16.0 + dx, gy as f32 + 16.0 + dy) {
                        for k in 0..3 {
                            sum[k] += px[k] as u32;
                        }
                        n += 1;
                    }
                }
                if n > 0 {
                    self.0.colours[o] = sum.map(|v| (v / n) as u8);
                }
            }
        }
    }
}

/// Every tree's far-distance billboard (see `VistaForest`).
pub const VISTA_REL: &str = "vegetation/vista.bin";

/// One tree seen from afar: where it stands (Bevy space, its base), its
/// scale, and which billboard card stands in for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VistaTree {
    pub pos: [f32; 3],
    pub scale: f32,
    pub card: u16,
}

/// The world's trees as billboard cards for far away: the cards (cached
/// models, `*_vistabill.xbg`: a flat card cut from a sheet of tree images)
/// and every tree that has one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VistaForest {
    pub cards: Vec<String>,
    pub trees: Vec<VistaTree>,
}

impl VistaForest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = b"VST1".to_vec();
        out.extend_from_slice(&(self.cards.len() as u32).to_le_bytes());
        for c in &self.cards {
            out.extend_from_slice(&(c.len() as u16).to_le_bytes());
            out.extend_from_slice(c.as_bytes());
        }
        out.extend_from_slice(&(self.trees.len() as u32).to_le_bytes());
        for t in &self.trees {
            for v in t.pos.iter().chain(std::iter::once(&t.scale)) {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&t.card.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<VistaForest> {
        if !b.starts_with(b"VST1") {
            bail!("not a vista file");
        }
        let mut r = Reader::new(&b[4..]);
        let mut f = VistaForest::default();
        for _ in 0..r.u32()? {
            let n = r.u16()? as usize;
            f.cards.push(String::from_utf8_lossy(r.bytes(n)?).into_owned());
        }
        for _ in 0..r.u32()? {
            let pos = [r.f32()?, r.f32()?, r.f32()?];
            let scale = r.f32()?;
            f.trees.push(VistaTree { pos, scale, card: r.u16()? });
        }
        Ok(f)
    }
}

/// The whole world's horizon maps at 4 m per texel (see `HorizonAtlas`).
pub const HORIZON_ATLAS_REL: &str = "terrain/horizon.bin";

/// The sectors' horizon maps (`sd<id>_shadow`) joined into one RG8 image at
/// 4 m per texel: 16 texels per sector, rows running north from the world's
/// south-west corner (Dunia x across, y up the rows, not transposed). Red is
/// the horizon toward where the sun sets, green toward where it rises, as
/// 0..1 for -90..90 degrees; 0.5 where there is no land data.
pub struct HorizonAtlas {
    pub size: u32,
    pub data: Vec<u8>,
}

impl HorizonAtlas {
    /// Texels per sector.
    pub const PER_SECTOR: usize = 16;

    pub fn new() -> HorizonAtlas {
        let size = (crate::terrain::WORLD_SECTORS as usize * Self::PER_SECTOR) as u32;
        HorizonAtlas { size, data: vec![128; size as usize * size as usize * 2] }
    }

    /// Writes one sector's map (its cached DDS: 64x64 G16R16, transposed).
    pub fn put_sector(&mut self, sector_id: u32, dds: &[u8]) -> bool {
        const N: usize = 64;
        let Some(px) = dds.get(128..128 + N * N * 4).filter(|_| dds.starts_with(b"DDS ")) else { return false };
        let c = crate::terrain::SectorCoord::from_id(sector_id);
        let step = N / Self::PER_SECTOR;
        for j in 0..Self::PER_SECTOR {
            for i in 0..Self::PER_SECTOR {
                // Texel row = metres east (gx), column = metres north (gy).
                let mut sum = [0u32; 2];
                for gx in i * step..(i + 1) * step {
                    for gy in j * step..(j + 1) * step {
                        let t = &px[(gx * N + gy) * 4..];
                        sum[0] += t[1] as u32;
                        sum[1] += t[3] as u32;
                    }
                }
                let n = (step * step) as u32;
                let (x, y) = (c.x as usize * Self::PER_SECTOR + i, c.y as usize * Self::PER_SECTOR + j);
                let o = (y * self.size as usize + x) * 2;
                self.data[o] = (sum[0] / n) as u8;
                self.data[o + 1] = (sum[1] / n) as u8;
            }
        }
        true
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = b"HZN1".to_vec();
        out.extend_from_slice(&self.size.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<HorizonAtlas> {
        let size = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?);
        let data = b.get(8..8 + size as usize * size as usize * 2)?.to_vec();
        b.starts_with(b"HZN1").then_some(HorizonAtlas { size, data })
    }
}

impl Default for HorizonAtlas {
    fn default() -> Self {
        Self::new()
    }
}

pub fn vegetation_rel(sector_id: u32) -> String {
    format!("vegetation/{sector_id}.fcv")
}

pub fn sector_rel(sector_id: u32) -> String {
    format!("sectors/{sector_id}.fcs")
}

/// A sector's navigation mesh (see [`CachedNav`]).
/// The game's third-person death and hit clips for rifle-armed characters,
/// one path per line (see `fc3_game::npc::InjuryClips`).
pub const INJURY_CLIPS_REL: &str = "characters/injury_clips.txt";

/// Whether a clip path is one of those.
pub fn is_injury_clip(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let file = p.rsplit('\\').next().unwrap_or("");
    p.contains("\\animations\\injury\\")
        && file.starts_with("3rdge_")
        && file.ends_with("ak74.mab")
        && !["snipertowers", "doubletakedown", "chair"].iter().any(|k| p.contains(k))
}

pub fn nav_rel(sector_id: u32) -> String {
    format!("nav/{sector_id}.fcn")
}

/// One sector's navigation mesh in Bevy space: walkable polygons, the portals
/// between them, and the edges on the sector's border (joined with the
/// neighbouring sectors when both are loaded).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedNav {
    pub verts: Vec<[f32; 3]>,
    /// Vertex indices, 3 to 6 per polygon.
    pub polys: Vec<Vec<u32>>,
    pub links: Vec<NavLink>,
    pub border: Vec<NavBorder>,
}

/// Two polygons that meet, and the span they share.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NavLink {
    pub a: u32,
    pub b: u32,
    pub left: [f32; 3],
    pub right: [f32; 3],
}

/// A polygon edge on the sector's border.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NavBorder {
    pub poly: u32,
    pub a: [f32; 3],
    pub b: [f32; 3],
}

impl CachedNav {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCNV");
        w.chunk(b"VERT", |w| {
            w.u32(self.verts.len() as u32);
            for v in &self.verts {
                w.f32s(v);
            }
        });
        w.chunk(b"POLY", |w| {
            w.u32(self.polys.len() as u32);
            for p in &self.polys {
                w.u8(p.len() as u8);
                for &i in p {
                    w.u32(i);
                }
            }
        });
        w.chunk(b"LINK", |w| {
            w.u32(self.links.len() as u32);
            for l in &self.links {
                w.u32(l.a);
                w.u32(l.b);
                w.f32s(&l.left);
                w.f32s(&l.right);
            }
        });
        w.chunk(b"BORD", |w| {
            w.u32(self.border.len() as u32);
            for b in &self.border {
                w.u32(b.poly);
                w.f32s(&b.a);
                w.f32s(&b.b);
            }
        });
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<CachedNav> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCNV")?;
        let mut n = CachedNav::default();
        for (tag, body) in chunks(&mut r)? {
            let mut c = Reader::new(body);
            match &tag {
                b"VERT" => {
                    let count = c.u32()? as usize;
                    n.verts = read_f32s::<3>(&mut c, count)?;
                }
                b"POLY" => {
                    let count = c.u32()?;
                    for _ in 0..count {
                        let k = c.u8()?;
                        n.polys.push((0..k).map(|_| c.u32()).collect::<Result<_>>()?);
                    }
                }
                b"LINK" => {
                    let count = c.u32()?;
                    for _ in 0..count {
                        n.links.push(NavLink { a: c.u32()?, b: c.u32()?, left: c.vec3()?, right: c.vec3()? });
                    }
                }
                b"BORD" => {
                    let count = c.u32()?;
                    for _ in 0..count {
                        n.border.push(NavBorder { poly: c.u32()?, a: c.vec3()?, b: c.vec3()? });
                    }
                }
                _ => {}
            }
        }
        Ok(n)
    }
}

/// Default cache location: `FC3_CACHE`, else the platform's per-user cache dir.
pub fn default_cache_root() -> PathBuf {
    if let Some(p) = std::env::var_os("FC3_CACHE") {
        return PathBuf::from(p);
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    };
    base.unwrap_or_else(|| PathBuf::from(".")).join("fc3-bevy").join("cache")
}

// ---------------------------------------------------------------- writer ---

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f32s(&mut self, v: &[f32]) {
        for x in v {
            self.f32(*x);
        }
    }
    pub fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
    }
    /// Writes `tag, len, payload`.
    pub fn chunk(&mut self, tag: &[u8; 4], f: impl FnOnce(&mut Writer)) {
        let mut inner = Writer::default();
        f(&mut inner);
        self.buf.extend_from_slice(tag);
        self.u32(inner.buf.len() as u32);
        self.buf.extend_from_slice(&inner.buf);
    }
}

fn header(w: &mut Writer, magic: &[u8; 4]) {
    w.buf.extend_from_slice(magic);
    w.u32(CACHE_VERSION);
}

fn check_header(r: &mut Reader, magic: &[u8; 4]) -> Result<()> {
    if r.bytes(4)? != magic {
        bail!("bad magic, expected {}", String::from_utf8_lossy(magic));
    }
    let v = r.u32()?;
    if v != CACHE_VERSION {
        bail!("cache version {v} != {CACHE_VERSION}; re-run the converter");
    }
    Ok(())
}

/// Iterates the chunks after the header.
fn chunks<'a>(r: &mut Reader<'a>) -> Result<Vec<([u8; 4], &'a [u8])>> {
    let mut out = Vec::new();
    while r.remaining() >= 8 {
        let tag: [u8; 4] = r.bytes(4)?.try_into().unwrap();
        let len = r.u32()? as usize;
        out.push((tag, r.bytes(len)?));
    }
    Ok(out)
}

fn read_str(r: &mut Reader) -> Result<String> {
    let n = r.u32()? as usize;
    Ok(String::from_utf8_lossy(r.bytes(n)?).into_owned())
}

fn read_f32s<const N: usize>(r: &mut Reader, count: usize) -> Result<Vec<[f32; N]>> {
    let mut v = Vec::with_capacity(count);
    for _ in 0..count {
        let mut a = [0f32; N];
        for x in a.iter_mut() {
            *x = r.f32()?;
        }
        v.push(a);
    }
    Ok(v)
}

// ----------------------------------------------------------------- model ---

pub mod mat_flags {
    pub const ALPHA_TEST: u32 = 1 << 0;
    pub const DOUBLE_SIDED: u32 = 1 << 1;
    pub const ALPHA_BLEND: u32 = 1 << 2;
    pub const EMISSIVE: u32 = 1 << 3;
    pub const VEGETATION: u32 = 1 << 4;
    pub const WATER: u32 = 1 << 5;
    pub const NO_DRAW: u32 = 1 << 6;
    /// Untextured placeholder in the source data; drawn as neutral grey.
    pub const PLACEHOLDER: u32 = 1 << 7;
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedMaterial {
    /// Cache-relative texture paths (empty = none).
    pub diffuse: String,
    pub normal: String,
    pub specular: String,
    pub flags: u32,
    pub base_color: [f32; 4],
    pub roughness: f32,
    pub shader: String,
    /// Every parameter of the original material: name hash and raw value
    /// (feeds the original shaders' constants).
    pub params: Vec<(u32, Vec<u8>)>,
    /// Every usable texture parameter: name hash and cache-relative path.
    pub textures: Vec<(u32, String)>,
    /// The original shader pair chosen and validated by the converter.
    pub original: Option<OriginalBinding>,
}

/// A material's original vertex/pixel shader pair with everything needed to
/// run it: each stage's constants and the textures behind its samplers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OriginalBinding {
    pub vs: u64,
    pub ps: u64,
    /// Sparse float constants: register and value.
    pub vs_constants: Vec<(u16, [f32; 4])>,
    pub ps_constants: Vec<(u16, [f32; 4])>,
    /// Pixel shader sampler register, cache-relative texture, sRGB colour map.
    pub samplers: Vec<(u8, String, bool)>,
    /// Render target 1 holds a usable packed world normal.
    pub use_normal: bool,
    /// Vertex shader for skinned meshes (0 if none) and its constants.
    pub skinned_vs: u64,
    pub skinned_vs_constants: Vec<(u16, [f32; 4])>,
}

impl CachedMaterial {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCMT");
        w.chunk(b"MAT\0", |w| write_material(w, self));
        if let Some(o) = &self.original {
            w.chunk(b"ORIG", |w| {
                w.u64(o.vs);
                w.u64(o.ps);
                for consts in [&o.vs_constants, &o.ps_constants] {
                    w.u32(consts.len() as u32);
                    for (r, v) in consts {
                        w.u16(*r);
                        w.f32s(v);
                    }
                }
                w.u32(o.samplers.len() as u32);
                for (r, rel, srgb) in &o.samplers {
                    w.u8(*r);
                    w.str(rel);
                    w.u8(*srgb as u8);
                }
                w.u8(o.use_normal as u8);
                w.u64(o.skinned_vs);
                w.u32(o.skinned_vs_constants.len() as u32);
                for (r, v) in &o.skinned_vs_constants {
                    w.u16(*r);
                    w.f32s(v);
                }
            });
        }
        w.chunk(b"PARM", |w| {
            w.u32(self.params.len() as u32);
            for (h, v) in &self.params {
                w.u32(*h);
                w.u32(v.len() as u32);
                w.buf.extend_from_slice(v);
            }
            w.u32(self.textures.len() as u32);
            for (h, p) in &self.textures {
                w.u32(*h);
                w.str(p);
            }
        });
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<CachedMaterial> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCMT")?;
        let mut material = None;
        let mut params = Vec::new();
        let mut textures = Vec::new();
        let mut original = None;
        for (tag, body) in chunks(&mut r)? {
            match &tag {
                b"ORIG" => {
                    let mut c = Reader::new(body);
                    let mut o = OriginalBinding { vs: c.u64()?, ps: c.u64()?, ..Default::default() };
                    for which in 0..2 {
                        for _ in 0..c.u32()? {
                            let r = c.u16()?;
                            let v = c.vec4()?;
                            if which == 0 {
                                o.vs_constants.push((r, v));
                            } else {
                                o.ps_constants.push((r, v));
                            }
                        }
                    }
                    for _ in 0..c.u32()? {
                        let r = c.u8()?;
                        let rel = read_str(&mut c)?;
                        o.samplers.push((r, rel, c.u8()? != 0));
                    }
                    o.use_normal = c.u8()? != 0;
                    o.skinned_vs = c.u64()?;
                    for _ in 0..c.u32()? {
                        let r = c.u16()?;
                        o.skinned_vs_constants.push((r, c.vec4()?));
                    }
                    original = Some(o);
                }
                b"MAT\0" => material = Some(read_material(&mut Reader::new(body))?),
                b"PARM" => {
                    let mut c = Reader::new(body);
                    for _ in 0..c.u32()? {
                        let h = c.u32()?;
                        let n = c.u32()? as usize;
                        params.push((h, c.bytes(n)?.to_vec()));
                    }
                    for _ in 0..c.u32()? {
                        let h = c.u32()?;
                        textures.push((h, read_str(&mut c)?));
                    }
                }
                _ => {}
            }
        }
        let mut m = material.context("material file has no MAT chunk")?;
        m.params = params;
        m.textures = textures;
        m.original = original;
        Ok(m)
    }
}

fn write_material(w: &mut Writer, m: &CachedMaterial) {
    w.str(&m.diffuse);
    w.str(&m.normal);
    w.str(&m.specular);
    w.u32(m.flags);
    w.f32s(&m.base_color);
    w.f32(m.roughness);
    w.str(&m.shader);
}

fn read_material(c: &mut Reader) -> Result<CachedMaterial> {
    Ok(CachedMaterial {
        diffuse: read_str(c)?,
        normal: read_str(c)?,
        specular: read_str(c)?,
        flags: c.u32()?,
        base_color: c.vec4()?,
        roughness: c.f32()?,
        shader: read_str(c)?,
        params: Vec::new(),
        textures: Vec::new(),
        original: None,
    })
}

#[derive(Debug, Clone, Default)]
pub struct CachedPart {
    pub material: u32,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// The second UV set (unique mapping for masks and dirt, where the first
    /// tiles), when the source has one.
    pub uvs1: Vec<[f32; 2]>,
    /// Tangent xyz + handedness, when the source has tangents.
    pub tangents: Vec<[f32; 4]>,
    pub colors: Vec<[f32; 4]>,
    /// Skinning: bone indices into `CachedModel::bones` and weights.
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// A part that turns on its own (a vehicle's wheel or steering wheel).
    pub pivot: Option<PartPivot>,
}

/// The node a moving part hangs from: its name and model-space pose.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PartPivot {
    pub name: String,
    pub at: [f32; 3],
    pub rotation: [f32; 4],
}

#[derive(Debug, Clone, Default)]
pub struct CachedLod {
    pub distance: f32,
    pub parts: Vec<CachedPart>,
}

#[derive(Debug, Clone, Default)]
pub struct CachedBone {
    pub name: String,
    /// CRC-32 of the bone name, used to match animation tracks.
    pub name_hash: u32,
    pub parent: i32,
    /// Local bind transform (Bevy space).
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    /// Inverse bind matrix (column-major), identity for unskinned nodes.
    pub inverse_bind: [f32; 16],
}

#[derive(Debug, Clone, Default)]
pub struct CachedModel {
    pub aabb_min: [f32; 3],
    pub aabb_max: [f32; 3],
    /// Cache-relative material paths (`materials/<hash>.fcmat`), one per slot.
    pub materials: Vec<String>,
    /// Per-slot flags copied from the material, so loaders can decide shadows/collision without reading it.
    pub material_flags: Vec<u32>,
    pub lods: Vec<CachedLod>,
    pub collision_positions: Vec<[f32; 3]>,
    pub collision_indices: Vec<u32>,
    pub bones: Vec<CachedBone>,
}

impl CachedModel {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCM\0");
        w.chunk(b"BBOX", |w| {
            w.f32s(&self.aabb_min);
            w.f32s(&self.aabb_max);
        });
        w.chunk(b"MATS", |w| {
            w.u32(self.materials.len() as u32);
            for (i, m) in self.materials.iter().enumerate() {
                w.str(m);
                w.u32(self.material_flags.get(i).copied().unwrap_or(0));
            }
        });
        for lod in &self.lods {
            w.chunk(b"LOD\0", |w| {
                w.f32(lod.distance);
                w.u32(lod.parts.len() as u32);
                for p in &lod.parts {
                    write_part(w, p);
                }
            });
        }
        // Moving parts, per LOD and part (an empty name for the rest).
        if self.lods.iter().any(|l| l.parts.iter().any(|p| p.pivot.is_some())) {
            w.chunk(b"PIVT", |w| {
                w.u32(self.lods.len() as u32);
                for lod in &self.lods {
                    w.u32(lod.parts.len() as u32);
                    for p in &lod.parts {
                        let pivot = p.pivot.clone().unwrap_or_default();
                        w.str(&pivot.name);
                        w.f32s(&pivot.at);
                        w.f32s(&pivot.rotation);
                    }
                }
            });
        }
        if !self.collision_indices.is_empty() {
            w.chunk(b"COLL", |w| {
                w.u32(self.collision_positions.len() as u32);
                w.u32(self.collision_indices.len() as u32);
                for p in &self.collision_positions {
                    w.f32s(p);
                }
                for i in &self.collision_indices {
                    w.u32(*i);
                }
            });
        }
        if !self.bones.is_empty() {
            w.chunk(b"BONE", |w| {
                w.u32(self.bones.len() as u32);
                for b in &self.bones {
                    w.str(&b.name);
                    w.u32(b.name_hash);
                    w.u32(b.parent as u32);
                    w.f32s(&b.translation);
                    w.f32s(&b.rotation);
                    w.f32s(&b.scale);
                    w.f32s(&b.inverse_bind);
                }
            });
        }
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<CachedModel> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCM\0")?;
        let mut m = CachedModel::default();
        for (tag, body) in chunks(&mut r)? {
            let mut c = Reader::new(body);
            match &tag {
                b"BBOX" => {
                    m.aabb_min = c.vec3()?;
                    m.aabb_max = c.vec3()?;
                }
                b"MATS" => {
                    let n = c.u32()?;
                    for _ in 0..n {
                        m.materials.push(read_str(&mut c)?);
                        m.material_flags.push(c.u32()?);
                    }
                }
                b"LOD\0" => {
                    let distance = c.f32()?;
                    let n = c.u32()?;
                    let mut parts = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        parts.push(read_part(&mut c)?);
                    }
                    m.lods.push(CachedLod { distance, parts });
                }
                b"PIVT" => {
                    let lods = c.u32()?;
                    for li in 0..lods as usize {
                        let n = c.u32()?;
                        for pi in 0..n as usize {
                            let name = read_str(&mut c)?;
                            let at = c.vec3()?;
                            let rotation = c.vec4()?;
                            if let Some(p) = m.lods.get_mut(li).and_then(|l| l.parts.get_mut(pi)).filter(|_| !name.is_empty()) {
                                p.pivot = Some(PartPivot { name, at, rotation });
                            }
                        }
                    }
                }
                b"COLL" => {
                    let nv = c.u32()? as usize;
                    let ni = c.u32()? as usize;
                    m.collision_positions = read_f32s::<3>(&mut c, nv)?;
                    m.collision_indices = (0..ni).map(|_| c.u32()).collect::<Result<_>>()?;
                }
                b"BONE" => {
                    let n = c.u32()?;
                    for _ in 0..n {
                        let name = read_str(&mut c)?;
                        let name_hash = c.u32()?;
                        let parent = c.u32()? as i32;
                        let translation = c.vec3()?;
                        let rotation = c.vec4()?;
                        let scale = c.vec3()?;
                        let mut inverse_bind = [0f32; 16];
                        for x in inverse_bind.iter_mut() {
                            *x = c.f32()?;
                        }
                        m.bones.push(CachedBone { name, name_hash, parent, translation, rotation, scale, inverse_bind });
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }
}

mod part_flags {
    pub const UV: u8 = 1;
    pub const NORMAL: u8 = 2;
    pub const TANGENT: u8 = 4;
    pub const COLOR: u8 = 8;
    pub const SKIN: u8 = 16;
    /// The second UV set (files written before it simply lack the flag).
    pub const UV1: u8 = 32;
}

fn write_part(w: &mut Writer, p: &CachedPart) {
    let n = p.positions.len();
    let mut flags = 0u8;
    if p.uvs.len() == n {
        flags |= part_flags::UV;
    }
    if p.normals.len() == n {
        flags |= part_flags::NORMAL;
    }
    if p.tangents.len() == n {
        flags |= part_flags::TANGENT;
    }
    if p.colors.len() == n {
        flags |= part_flags::COLOR;
    }
    if p.joints.len() == n && p.weights.len() == n {
        flags |= part_flags::SKIN;
    }
    if n > 0 && p.uvs1.len() == n {
        flags |= part_flags::UV1;
    }
    w.u32(p.material);
    w.u32(n as u32);
    w.u32(p.indices.len() as u32);
    w.u8(flags);
    for v in &p.positions {
        w.f32s(v);
    }
    if flags & part_flags::NORMAL != 0 {
        for v in &p.normals {
            w.f32s(v);
        }
    }
    if flags & part_flags::UV != 0 {
        for v in &p.uvs {
            w.f32s(v);
        }
    }
    if flags & part_flags::UV1 != 0 {
        for v in &p.uvs1 {
            w.f32s(v);
        }
    }
    if flags & part_flags::TANGENT != 0 {
        for v in &p.tangents {
            w.f32s(v);
        }
    }
    if flags & part_flags::COLOR != 0 {
        for v in &p.colors {
            w.f32s(v);
        }
    }
    if flags & part_flags::SKIN != 0 {
        for j in &p.joints {
            for x in j {
                w.u16(*x);
            }
        }
        for v in &p.weights {
            w.f32s(v);
        }
    }
    // u16 indices when they fit halves index bandwidth and file size.
    let wide = n > u16::MAX as usize;
    w.u8(wide as u8);
    for &i in &p.indices {
        if wide {
            w.u32(i);
        } else {
            w.u16(i as u16);
        }
    }
}

fn read_part(r: &mut Reader) -> Result<CachedPart> {
    let material = r.u32()?;
    let n = r.u32()? as usize;
    let ni = r.u32()? as usize;
    let flags = r.u8()?;
    let mut p = CachedPart { material, ..Default::default() };
    p.positions = read_f32s::<3>(r, n)?;
    if flags & part_flags::NORMAL != 0 {
        p.normals = read_f32s::<3>(r, n)?;
    }
    if flags & part_flags::UV != 0 {
        p.uvs = read_f32s::<2>(r, n)?;
    }
    if flags & part_flags::UV1 != 0 {
        p.uvs1 = read_f32s::<2>(r, n)?;
    }
    if flags & part_flags::TANGENT != 0 {
        p.tangents = read_f32s::<4>(r, n)?;
    }
    if flags & part_flags::COLOR != 0 {
        p.colors = read_f32s::<4>(r, n)?;
    }
    if flags & part_flags::SKIN != 0 {
        p.joints = (0..n).map(|_| Ok([r.u16()?, r.u16()?, r.u16()?, r.u16()?])).collect::<Result<_>>()?;
        p.weights = read_f32s::<4>(r, n)?;
    }
    let wide = r.u8()? != 0;
    p.indices = (0..ni).map(|_| if wide { r.u32() } else { r.u16().map(u32::from) }).collect::<Result<_>>()?;
    Ok(p)
}

// --------------------------------------------------------------- terrain ---

#[derive(Debug, Clone)]
pub struct CachedTerrain {
    pub sector_id: u32,
    pub min_height: f32,
    pub max_height: f32,
    /// 65x65 heights, row-major (row = Dunia +Y), in 1/64 m units.
    pub heights: Vec<u16>,
    /// Layer table index per mask slot (R, G, B, remainder); `NO_LAYER` if unused.
    pub layers: [u8; 4],
    /// Cave-mouth cells (see `terrain::TerrainSector::holes`); empty if none.
    pub holes: Vec<bool>,
}

impl CachedTerrain {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCT\0");
        w.chunk(b"HGHT", |w| {
            w.u32(self.sector_id);
            w.f32(self.min_height);
            w.f32(self.max_height);
            w.u32(self.heights.len() as u32);
            for h in &self.heights {
                w.u16(*h);
            }
        });
        w.chunk(b"LAYR", |w| {
            for l in self.layers {
                w.u8(l);
            }
        });
        if !self.holes.is_empty() {
            // One bit per cell.
            w.chunk(b"HOLE", |w| {
                for byte in self.holes.chunks(8) {
                    w.u8(byte.iter().enumerate().fold(0u8, |b, (i, h)| b | ((*h as u8) << i)));
                }
            });
        }
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<CachedTerrain> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCT\0")?;
        let mut terrain = None;
        let mut layers = [NO_LAYER; 4];
        let mut holes = Vec::new();
        for (tag, body) in chunks(&mut r)? {
            let mut c = Reader::new(body);
            match &tag {
                b"HGHT" => {
                    let sector_id = c.u32()?;
                    let min_height = c.f32()?;
                    let max_height = c.f32()?;
                    let n = c.u32()? as usize;
                    let heights = (0..n).map(|_| c.u16()).collect::<Result<_>>()?;
                    terrain = Some(CachedTerrain { sector_id, min_height, max_height, heights, layers: [NO_LAYER; 4], holes: Vec::new() });
                }
                b"LAYR" => {
                    for l in &mut layers {
                        *l = c.u8()?;
                    }
                }
                b"HOLE" => {
                    let n = crate::terrain::CELLS * crate::terrain::CELLS;
                    holes = (0..n / 8).map(|_| c.u8()).collect::<Result<Vec<u8>>>()?.iter().flat_map(|b| (0..8).map(move |i| b >> i & 1 == 1)).collect();
                }
                _ => {}
            }
        }
        let mut t = terrain.context("terrain file has no HGHT chunk")?;
        t.layers = layers;
        t.holes = holes;
        Ok(t)
    }

    pub fn to_sector(&self) -> crate::terrain::TerrainSector {
        crate::terrain::TerrainSector {
            coord: crate::terrain::SectorCoord::from_id(self.sector_id),
            min_height: self.min_height,
            max_height: self.max_height,
            material: String::new(),
            heights: self.heights.iter().map(|&h| h as f32 / 64.0).collect(),
            normal_bytes: Vec::new(),
            layers: self.layers,
            holes: self.holes.clone(),
        }
    }
}

/// One terrain layer in the cache: tiling ground textures blended by the
/// atlas mask.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedTerrainLayer {
    /// Cache-relative textures (empty when missing).
    pub diffuse: String,
    pub normal: String,
    /// Repeats per 64 m sector.
    pub tiling: f32,
    /// Projection axis in Dunia space (0 = X, 1 = Y, 2 = Z/top-down).
    pub axis: u8,
    pub name: String,
    /// Normal map repeats per 64 m sector.
    pub normal_tiling: f32,
    /// Strength of the distance colour noise.
    pub noise_level: f32,
    pub glossiness: f32,
}

/// Enemy outposts (see `outposts`).
pub const OUTPOSTS_REL: &str = "world/outposts.txt";

/// Roads and rivers laid along splines (see `splines`): "SPL\0", a count, then
/// per spline its sector id and `SplinePrimitive::to_bytes`.
pub const SPLINES_REL: &str = "world/splines.bin";
/// The outdoor environment over the day (see `environment::Environment::to_text`).
pub const ENVIRONMENT_REL: &str = "world/environment.txt";
/// The game's tone curve and colour cube (see `grade::Grade`).
pub const GRADE_REL: &str = "sky/grade.bin";

pub fn splines_to_bytes(splines: &[(u32, crate::splines::SplinePrimitive)]) -> Vec<u8> {
    let mut out = b"SPL\0".to_vec();
    out.extend_from_slice(&(splines.len() as u32).to_le_bytes());
    for (sector, s) in splines {
        out.extend_from_slice(&sector.to_le_bytes());
        s.to_bytes(&mut out);
    }
    out
}

pub fn splines_from_bytes(data: &[u8]) -> Result<Vec<(u32, crate::splines::SplinePrimitive)>> {
    let mut r = Reader::new(data);
    if r.bytes(4)? != b"SPL\0" {
        bail!("not a splines file");
    }
    let n = r.u32()? as usize;
    (0..n).map(|_| Ok((r.u32()?, crate::splines::SplinePrimitive::read(&mut r)?))).collect()
}

/// Inland water (rivers and lakes above the sea): one line per sector with a
/// water plane, `sector_id level` (Bevy Y, metres). See
/// `terrain::water_plane`.
pub const WATER_PLANES_REL: &str = "world/water_planes.txt";

/// Water textures: ripple and swell normal maps, and shoreline foam.
pub const WATER_RIPPLES_REL: &str = "water/ripples_n.dds";
pub const WATER_SWELL_REL: &str = "water/swell_n.dds";
pub const WATER_FOAM_REL: &str = "water/foam.dds";

/// Effect textures by file stem: flipbooks (`_<cols>x<rows>` frame grids)
/// and bullet-hole decal atlases, written to `fx/<stem>.dds`.
pub const FX_TEXTURES: &[&str] = &[
    "fc3_gfx_fire2_16x4",
    "fc3_gfx_fireball_5x5",
    "fc3_gfx_explofire_8x2",
    "fc3_gfx_impactsmoke01_4x4",
    "fc3_gfx_impactsmoke06_3x4",
    "fc3_gfx_impactburst_3x2",
    "fc3_muzzleflash_4x1",
    "fc3_gfx_muzzle01_2x2",
    "fc3_gfx_smokeloop_6x5",
    "gfx_smokeexplo_4x3",
    "fc3_gfx_sparks_2x2",
    "fc3_gfx_splash_01",
    "fc3_gfx_lightglow01",
    "fc3_gfx_drop_01",
    "fc3_gfx_impactdecal_concrete_4x1",
    "fc3_gfx_impactdecal_wood_4x1",
    "fc3_gfx_impactdecal_metal_2x1",
    "fc3_gfx_impactdecal_sand_2x1",
    "fc3_gfx_bloodsplatters_4x1",
    "fc3_gfx_waterdrops_01",
    // The sun's lens flare: its glow and six ghosts, side by side.
    "lens_flare",
];

/// Cache path of an effect texture.
pub fn fx_rel(stem: &str) -> String {
    format!("fx/{stem}.dds")
}

/// Animal skin materials the models only reference through their archetypes
/// (the models themselves carry grey placeholders), by file stem under
/// `graphics\_materials\`.
pub const ANIMAL_SKINS: &[&str] = &[
    "yanzhou-m-2106201150629658",
    "yanzhou-m-20120208181926",
    "yanzhou-m-20120201170940",
    "yanzhou-m-0909201161474399",
    "yanzhou-m-0711201150689792",
    "yanzhou-m-0811201155154915",
    "vfortin-m-2010120863940176",
    "yanzhou-m-0407201152986069",
    "yanzhou-m-2107201165520254",
    "yanzhou-m-1908201167678161",
    // The vulture (its model carries a grey placeholder).
    "yanzhou-m-0811201153918833",
];

/// Original path of an animal skin material.
pub fn animal_skin_path(stem: &str) -> String {
    format!("graphics\\_materials\\{stem}.material.bin")
}

/// First-person weapon clips: `<folder> <action> <clip path>` per line, where
/// the folder is relative to `FP_CLIP_ROOT` (e.g. `primary\ak74m`) and the
/// action is the clip name's `1stge_uppb_<action>_` part.
pub const FP_CLIPS_REL: &str = "characters/fp_clips.txt";
pub const FP_CLIP_ROOT: &str = "graphics\\characters\\_common\\animations\\weapons\\";

/// One first-person clip from its path: `(folder, action)`.
///
/// ```
/// use fc3_formats::cache::fp_clip_key;
/// let key = |f: &str| fp_clip_key(&format!("{}{f}", fc3_formats::cache::FP_CLIP_ROOT));
/// assert_eq!(key(r"primary\ak74m\1stge_uppb_draw_+000fw_prak7.mab"), Some((r"primary\ak74m".into(), "draw".into())));
/// assert_eq!(key(r"secondary\kimber_1911\1stge_uppb_btheaimslow_zip+000fw_sekimber.mab"), None);
/// assert_eq!(key(r"primary\ak74m\cover\1stge_uppb_draw_+000fw_prak7.mab"), None);
/// ```
pub fn fp_clip_key(path: &str) -> Option<(String, String)> {
    let lower = path.to_ascii_lowercase();
    let rest = lower.strip_prefix(&FP_CLIP_ROOT.to_ascii_lowercase())?;
    let (folder, file) = rest.rsplit_once('\\')?;
    if folder.ends_with("\\cover") || folder == "cover" {
        return None;
    }
    // `<action>_+000fw_<weapon>.mab`; variants (`_zip+000fw`: on a zip line) have no `_+`.
    let (action, _) = file.strip_prefix("1stge_uppb_")?.split_once("_+")?;
    Some((folder.to_string(), action.to_string()))
}

/// Vehicle patrol routes (see `patrols`).
pub const PATROLS_REL: &str = "world/patrols.txt";

/// Cables, ladders and radio towers (see `traversal`).
pub const TRAVERSAL_REL: &str = "world/traversal.txt";

/// Enemy outfits (see `kits`), from the game's graphic kits.
pub const KITS_REL: &str = "characters/kits.txt";

/// Weapon paint jobs (see `fc3_convert::convert_paints`): one line per paint a
/// material can take, `base_material_rel paint variant_original_path
/// variant_material_rel`.
pub const PAINTS_REL: &str = "weapons/paints.txt";

/// A weapon paint job: a material that can be swapped for a painted variant.
#[derive(Debug, Clone, PartialEq)]
pub struct Paint {
    pub base: String,
    pub name: String,
    pub variant_path: String,
    pub variant: String,
}

pub fn paints_from_text(text: &str) -> Vec<Paint> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some(Paint { base: it.next()?.into(), name: it.next()?.into(), variant_path: it.next()?.into(), variant: it.next()?.into() })
        })
        .collect()
}

pub fn paints_to_text(paints: &[Paint]) -> String {
    paints.iter().map(|p| format!("{}\t{}\t{}\t{}\n", p.base, p.name, p.variant_path, p.variant)).collect()
}

/// The graphic kits dressing the generic enemy pool and villagers (no named
/// characters).
pub const GENERIC_KITS: &[&str] = &[
    "Pirate_Assault",
    "Pirate_sniper",
    "Pirate_Rocket",
    "Pirate_Shotgun",
    "Pirate_Beheader",
    "Pirate_Molotov",
    "Pirate_HeavyGunner",
    "Privateer_Assault",
    "Privateer_Recruit",
    "Privateer_Charger",
    "Privateer_Sniper",
    "Privateer_Rocket",
    "Privateer_HeavyGunner",
    "Privateer_Defender",
    "Privateer_HeavyFlammer",
    "Rebel_Assaulter",
    "Rebel_Sniper",
    // Generic villagers (the island's locals, no named characters).
    "Villager_Men",
    "Local_Women",
];

/// Placed lights and camp fires (see `lights`).
pub const LIGHTS_REL: &str = "world/lights.txt";
/// The engine's flicker wave (`lights::FlickerWave`), read out of its code.
pub const FLICKER_REL: &str = "world/flicker.txt";

/// Spots where idle guards stand doing something (the world's
/// `SimpleAnimation` points), one per line: `kind x y z qx qy qz qw`
/// (see `guard_spot_kind`; Bevy space).
pub const GUARD_SPOTS_REL: &str = "world/guard_spots.txt";

/// Spots where villagers stop to do something (the civilian
/// `SimpleAnimation` points), one per line like the guard spots: `kind x y
/// z qx qy qz qw` (see `villager_spot_kind`; Bevy space).
pub const VILLAGER_SPOTS_REL: &str = "world/villager_spots.txt";

/// What a villager does at a civilian `SimpleAnimation.<what>_Civ<M|F>`
/// point: hoe the garden, lean on a wall (men); sit on the ground, pray
/// (women); go through a body's pockets (men).
pub fn villager_spot_kind(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let what = n.split("simpleanimation.").nth(1)?;
    if !what.contains("_civ") {
        return None;
    }
    let kinds: [(&str, &str); 5] = [
        ("gardeninghoe", "hoe"),
        ("leanwall", "lean"),
        ("sitonground", "sit"),
        ("prayingground", "pray"),
        ("lootcorpse", "loot"),
    ];
    kinds.iter().find(|(k, _)| what.contains(k)).map(|(_, v)| *v)
}

/// What a guard does at a `SimpleAnimation.<what>[_<faction>_v]_<n>` point,
/// for the ones the regular builds have clips for (heavies' and civilians'
/// points, and the scripted ones, are left out).
pub fn guard_spot_kind(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let what = n.split("simpleanimation.").nth(1)?;
    if n.contains("animalsimpleanimation") || ["heavy", "beheader", "civ", "extw_"].iter().any(|k| what.contains(k)) {
        return None;
    }
    let kinds: [(&str, &str); 19] = [
        ("relax_swatflies", "flies"),
        ("cautious_guard", "cautious"),
        ("cautiousguard", "cautious"),
        ("relax_guard", "relax"),
        ("relaxguard", "relax"),
        ("campfireguardstand", "relax"),
        ("snipersmoking", "smoke"),
        ("smoke", "smoke"),
        ("piss", "piss"),
        ("leanwall", "lean"),
        ("lookoverobject", "lookover"),
        ("lookaroundcorner_left", "corner_left"),
        ("lookaroundcorner_right", "corner_right"),
        ("verifygear", "gear"),
        ("sitonground", "sit"),
        ("campfireguardground", "sit"),
        ("useflare", "cautious"),
        ("lookunderobject", "cautious"),
        ("sniperbinocular", "cautious"),
    ];
    kinds.iter().find(|(k, _)| what.starts_with(k)).map(|(_, kind)| *kind).or_else(|| {
        ["lookdowncliff", "validaterisk", "standandwatch"].iter().any(|k| what.starts_with(k)).then_some("cautious")
    })
}

/// The world's placed effects the game plays, one per line: `kind x y z`
/// (see `world_fx_kind`; Bevy-space position).
pub const WORLD_FX_REL: &str = "world/effects.txt";

/// What a placed effect is, by its library name, for the ones the game plays:
/// a waterfall's spray where it lands or its mist, low drifting mist,
/// fireflies, candle and torch flames. Quest and cinematic effects, lamp glows
/// (the lamps draw their own) and the rest are left out.
pub fn world_fx_kind(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    if n.starts_with("sha_") || ["cinematic", "jas_", "debug", "interior", "hallucination"].iter().any(|k| n.contains(k)) {
        return None;
    }
    if n.contains("clear_waterfall") || n.contains(".rapids.") {
        return Some(if n.contains("mist") || n.contains("fume") { "falls_mist" } else { "falls_spray" });
    }
    if n.contains("weather_fx.fc3_area_myst") || n.contains("area_mist") {
        return Some("mist");
    }
    if n.contains("_flies") && (n.contains("lightglow") || n.contains("lightspot") || n.contains("lightflares")) {
        return Some("fireflies");
    }
    if n.contains("candlelight") {
        return Some("candle");
    }
    if n.contains("fire_torch") {
        return Some("torch");
    }
    // Camp fires' flames, and the embers of dying ones.
    if n.contains("fire.camp_fireext") {
        return Some("embers");
    }
    if n.contains("fire.camp_fire") {
        return Some("camp_fire");
    }
    None
}

/// The world's sound points, one per line: `event x y z` (hex event id,
/// Bevy-space position).
pub const SOUND_POINTS_REL: &str = "world/sound_points.txt";

/// Sound points left out: voices and crowd chatter, story moments and
/// scripted one-offs (explosions, thunder claps).
pub fn sound_point_kept(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    !["walla", "_vo", "vo_", "chase", "jas_", "jas0", "collapse", "elevator", "gazexplosion", "thunder", "cinematic", "mission", "scream", "radio", "music"].iter().any(|k| n.contains(k))
}

/// The moon sprite (alpha: the disc and its glow).
pub const SKY_MOON_REL: &str = "sky/moon.dds";
/// The sky's cloud panoramas (360 degrees around, horizon at the bottom row),
/// from clear to storm. Alpha is cover, green the lit cloud, red the sun rim.
pub const SKY_CLOUDS_REL: [&str; 5] = ["sky/clouds_clear.dds", "sky/clouds_medium.dds", "sky/clouds_dense.dds", "sky/clouds_cumulonimbus.dds", "sky/clouds_storm.dds"];

/// The baked world map (see `worldmap`).
pub const WORLD_MAP_REL: &str = "map/world.dds";

/// The engine's Perlin noise texture (used by the original terrain shaders).
pub const TERRAIN_PERLIN_REL: &str = "terrain/perlin.dds";
/// The engine's distance detail noise for terrain (original terrain shaders).
pub const TERRAIN_VISTA_NOISE_REL: &str = "terrain/vista_noise.dds";

/// The world's terrain layer table, indexed by `CachedTerrain::layers`.
/// Stored as text, one layer per line:
/// `tiling axis diffuse normal name normal_tiling noise_level glossiness`
/// (`-` for a missing texture).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerrainLayerTable {
    pub layers: Vec<CachedTerrainLayer>,
    /// The world's terrain noise transition (amplitude, sharpness, UV scale),
    /// stored as a `! noise_transition a s u` line.
    pub noise_transition: [f32; 3],
}

impl TerrainLayerTable {
    pub fn to_text(&self) -> String {
        let field = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
        let mut out = String::from("# tiling axis diffuse normal name normal_tiling noise_level glossiness\n");
        let [a, s, u] = self.noise_transition;
        out += &format!("! noise_transition {a} {s} {u}\n");
        for l in &self.layers {
            out += &format!(
                "{} {} {} {} {} {} {} {}\n",
                l.tiling,
                l.axis,
                field(&l.diffuse),
                field(&l.normal),
                field(&l.name),
                l.normal_tiling,
                l.noise_level,
                l.glossiness,
            );
        }
        out
    }

    pub fn from_text(text: &str) -> TerrainLayerTable {
        let field = |s: Option<&str>| match s {
            Some("-") | None => String::new(),
            Some(s) => s.to_string(),
        };
        let mut noise_transition = [0.0, 0.0, 4.0];
        for line in text.lines().filter_map(|l| l.strip_prefix("! noise_transition")) {
            for (slot, v) in noise_transition.iter_mut().zip(line.split_whitespace()) {
                *slot = v.parse().unwrap_or(*slot);
            }
        }
        let layers = text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with('!') && !l.trim().is_empty())
            .map(|line| {
                let mut it = line.split_whitespace();
                let tiling = it.next().and_then(|v| v.parse().ok()).unwrap_or(16.0);
                CachedTerrainLayer {
                    tiling,
                    axis: it.next().and_then(|v| v.parse().ok()).unwrap_or(2),
                    diffuse: field(it.next()),
                    normal: field(it.next()),
                    name: field(it.next()),
                    normal_tiling: it.next().and_then(|v| v.parse().ok()).unwrap_or(tiling),
                    noise_level: it.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
                    glossiness: it.next().and_then(|v| v.parse().ok()).unwrap_or(0.3),
                }
            })
            .collect();
        TerrainLayerTable { layers, noise_transition }
    }
}

// ---------------------------------------------------------------- sector ---

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Index into `CachedSector::models`.
    pub model: u32,
    /// Bevy-space translation and rotation (x, y, z, w).
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
    /// Index into `CachedSector::entities` of the owning entity.
    pub entity: u32,
    /// Index into `CachedSector::material_sets`, or `NO_MATERIAL_SET`.
    pub material_set: u32,
}

pub const NO_MATERIAL_SET: u32 = u32::MAX;

/// Gameplay-relevant entity info (spawn points, pickups, regions...).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedEntity {
    pub id: u64,
    pub class: String,
    pub name: String,
    pub archetype: String,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    /// Its mission layer (`world::DEFAULT_MISSION_LAYER` for the world as it
    /// always is).
    pub mission_layer: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CachedSector {
    pub sector_id: u32,
    /// Cache-relative model paths.
    pub models: Vec<String>,
    pub placements: Vec<Placement>,
    pub entities: Vec<CachedEntity>,
    /// Per-placement material replacements: cache-relative material paths, one per model slot.
    pub material_sets: Vec<Vec<String>>,
}

impl CachedSector {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCS\0");
        w.chunk(b"MDLS", |w| {
            w.u32(self.sector_id);
            w.u32(self.models.len() as u32);
            for m in &self.models {
                w.str(m);
            }
        });
        w.chunk(b"PLCE", |w| {
            w.u32(self.placements.len() as u32);
            for p in &self.placements {
                w.u32(p.model);
                w.f32s(&p.translation);
                w.f32s(&p.rotation);
                w.f32(p.scale);
                w.u32(p.entity);
                w.u32(p.material_set);
            }
        });
        w.chunk(b"MSET", |w| {
            w.u32(self.material_sets.len() as u32);
            for set in &self.material_sets {
                w.u32(set.len() as u32);
                for m in set {
                    w.str(m);
                }
            }
        });
        w.chunk(b"ENTS", |w| {
            w.u32(self.entities.len() as u32);
            for e in &self.entities {
                w.u64(e.id);
                w.str(&e.class);
                w.str(&e.name);
                w.str(&e.archetype);
                w.f32s(&e.translation);
                w.f32s(&e.rotation);
            }
        });
        // Mission layers, when any entity is on one other than the default.
        if self.entities.iter().any(|e| e.mission_layer != crate::world::DEFAULT_MISSION_LAYER) {
            w.chunk(b"LAYR", |w| {
                for e in &self.entities {
                    w.u64(e.mission_layer);
                }
            });
        }
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<CachedSector> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCS\0")?;
        let mut s = CachedSector::default();
        for (tag, body) in chunks(&mut r)? {
            let mut c = Reader::new(body);
            match &tag {
                b"MDLS" => {
                    s.sector_id = c.u32()?;
                    let n = c.u32()?;
                    for _ in 0..n {
                        s.models.push(read_str(&mut c)?);
                    }
                }
                b"PLCE" => {
                    let n = c.u32()?;
                    for _ in 0..n {
                        s.placements.push(Placement {
                            model: c.u32()?,
                            translation: c.vec3()?,
                            rotation: c.vec4()?,
                            scale: c.f32()?,
                            entity: c.u32()?,
                            material_set: c.u32()?,
                        });
                    }
                }
                b"MSET" => {
                    let n = c.u32()?;
                    for _ in 0..n {
                        let k = c.u32()?;
                        s.material_sets.push((0..k).map(|_| read_str(&mut c)).collect::<Result<_>>()?);
                    }
                }
                b"ENTS" => {
                    let n = c.u32()?;
                    for _ in 0..n {
                        s.entities.push(CachedEntity {
                            id: c.u64()?,
                            class: read_str(&mut c)?,
                            name: read_str(&mut c)?,
                            archetype: read_str(&mut c)?,
                            translation: c.vec3()?,
                            rotation: c.vec4()?,
                            mission_layer: crate::world::DEFAULT_MISSION_LAYER,
                        });
                    }
                }
                b"LAYR" => {
                    for e in &mut s.entities {
                        e.mission_layer = c.u64()?;
                    }
                }
                _ => {}
            }
        }
        Ok(s)
    }
}

pub mod veg_kind {
    pub const TREE: u8 = 0;
    pub const GRASS: u8 = 1;
    pub const MESH: u8 = 2;
}

/// One placed plant / clutter instance (Bevy space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VegInstance {
    pub model: u16,
    pub kind: u8,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
    /// Ground colour under the instance (grass is tinted by it), sRGB.
    pub tint: [u8; 3],
}

/// Baked vegetation of one sector.
#[derive(Debug, Clone, Default)]
pub struct CachedVegetation {
    pub sector_id: u32,
    /// Cache-relative model paths.
    pub models: Vec<String>,
    pub instances: Vec<VegInstance>,
}

impl CachedVegetation {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCV\0");
        w.chunk(b"VEGM", |w| {
            w.u32(self.sector_id);
            w.u32(self.models.len() as u32);
            for m in &self.models {
                w.str(m);
            }
        });
        w.chunk(b"VEGI", |w| {
            w.u32(self.instances.len() as u32);
            for i in &self.instances {
                w.u16(i.model);
                w.u8(i.kind);
                w.f32s(&i.translation);
                w.f32s(&i.rotation);
                w.f32(i.scale);
                w.buf.extend_from_slice(&i.tint);
            }
        });
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<CachedVegetation> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCV\0")?;
        let mut v = CachedVegetation::default();
        for (tag, body) in chunks(&mut r)? {
            let mut c = Reader::new(body);
            match &tag {
                b"VEGM" => {
                    v.sector_id = c.u32()?;
                    let n = c.u32()?;
                    for _ in 0..n {
                        v.models.push(read_str(&mut c)?);
                    }
                }
                b"VEGI" => {
                    let n = c.u32()? as usize;
                    v.instances.reserve(n);
                    for _ in 0..n {
                        v.instances.push(VegInstance {
                            model: c.u16()?,
                            kind: c.u8()?,
                            translation: c.vec3()?,
                            rotation: c.vec4()?,
                            scale: c.f32()?,
                            tint: [c.u8()?, c.u8()?, c.u8()?],
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(v)
    }
}

/// Written last by the converter; its presence means the cache is complete.
#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub version: u32,
    pub sectors_with_terrain: Vec<u32>,
    pub sectors_with_objects: Vec<u32>,
    pub models: u32,
    pub textures: u32,
    /// Sectors with a `vegetation/<id>.fcv` file (optional chunk).
    pub sectors_with_vegetation: Vec<u32>,
}

impl Manifest {
    pub const FILE: &'static str = "manifest.bin";

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"FCX\0");
        w.chunk(b"INFO", |w| {
            w.u32(self.models);
            w.u32(self.textures);
            w.u32(self.sectors_with_terrain.len() as u32);
            for s in &self.sectors_with_terrain {
                w.u32(*s);
            }
            w.u32(self.sectors_with_objects.len() as u32);
            for s in &self.sectors_with_objects {
                w.u32(*s);
            }
        });
        if !self.sectors_with_vegetation.is_empty() {
            w.chunk(b"VEG\0", |w| {
                w.u32(self.sectors_with_vegetation.len() as u32);
                for s in &self.sectors_with_vegetation {
                    w.u32(*s);
                }
            });
        }
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<Manifest> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"FCX\0")?;
        let mut m = Manifest { version: CACHE_VERSION, ..Default::default() };
        for (tag, body) in chunks(&mut r)? {
            if &tag == b"INFO" {
                let mut c = Reader::new(body);
                m.models = c.u32()?;
                m.textures = c.u32()?;
                let n = c.u32()?;
                m.sectors_with_terrain = (0..n).map(|_| c.u32()).collect::<Result<_>>()?;
                let n = c.u32()?;
                m.sectors_with_objects = (0..n).map(|_| c.u32()).collect::<Result<_>>()?;
            } else if &tag == b"VEG\0" {
                let mut c = Reader::new(body);
                let n = c.u32()?;
                m.sectors_with_vegetation = (0..n).map(|_| c.u32()).collect::<Result<_>>()?;
            }
        }
        Ok(m)
    }

    pub fn load(root: &std::path::Path) -> Result<Manifest> {
        let p = root.join(Self::FILE);
        let b = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
        Self::from_bytes(&b)
    }
}

// --------------------------------------------------------------- shaders ---

/// One line per translated shader (see `ShaderEntry`).
pub const SHADER_INDEX_REL: &str = "shaders/index.txt";
/// Deflate-compressed WGSL modules, addressed by `ShaderEntry::offset/size`.
pub const SHADER_PACK_REL: &str = "shaders/wgsl.pak";

/// A shader input or output: D3D usage code, usage index, register, location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderIo {
    pub usage: u8,
    pub usage_index: u8,
    pub register: u32,
    /// `None` for the clip-space position output.
    pub location: Option<u32>,
    /// Vertex outputs: mask of the inputs that feed it. Pixel inputs: role
    /// bits (1 = texture coordinate, 2 = normalised direction).
    pub flags: u32,
}

/// A named constant or sampler used by a shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderBinding {
    pub name_hash: u32,
    pub register: u32,
    pub rows: u8,
    pub sampler: bool,
}

/// Metadata for one translated shader (a line of `shaders/index.txt`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderEntry {
    /// Archive path hash of the original `.vso`/`.pso`.
    pub hash: u64,
    pub vertex: bool,
    pub offset: u64,
    pub size: u32,
    pub inputs: Vec<ShaderIo>,
    pub outputs: Vec<ShaderIo>,
    /// Sampler register and texture type (1 = 2D, 2 = cube, 3 = volume).
    pub samplers: Vec<(u32, u8)>,
    pub bindings: Vec<ShaderBinding>,
    /// Pixel shader colour outputs (bit per render target).
    pub color_outputs: u8,
    /// Texture reads that had to drop implicit derivatives.
    pub nonuniform: u32,
}

impl ShaderEntry {
    pub fn to_line(&self) -> String {
        let list = |v: Vec<String>| if v.is_empty() { "-".to_string() } else { v.join(",") };
        let io = |v: &[ShaderIo]| {
            list(v
                .iter()
                .map(|i| {
                    format!("{}.{}:{}:{}:{}", i.usage, i.usage_index, i.register, i.location.map_or("-".into(), |l| l.to_string()), i.flags)
                })
                .collect())
        };
        format!(
            "{:016x} {} {} {} {} {} {} {} {} {}",
            self.hash,
            if self.vertex { "vs" } else { "ps" },
            self.offset,
            self.size,
            io(&self.inputs),
            io(&self.outputs),
            list(self.samplers.iter().map(|(r, t)| format!("{r}:{t}")).collect()),
            list(self
                .bindings
                .iter()
                .map(|b| format!("{:08x}:{}:{}{}", b.name_hash, b.register, b.rows, if b.sampler { "s" } else { "" }))
                .collect()),
            self.color_outputs,
            self.nonuniform,
        )
    }

    pub fn from_line(line: &str) -> Option<ShaderEntry> {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 10 {
            return None;
        }
        let items = |s: &str| -> Vec<String> { if s == "-" { vec![] } else { s.split(',').map(str::to_string).collect() } };
        let io = |s: &str| -> Option<Vec<ShaderIo>> {
            items(s)
                .iter()
                .map(|i| {
                    let mut p = i.split(':');
                    let (usage, index) = p.next()?.split_once('.')?;
                    Some(ShaderIo {
                        usage: usage.parse().ok()?,
                        usage_index: index.parse().ok()?,
                        register: p.next()?.parse().ok()?,
                        location: p.next()?.parse().ok(),
                        flags: p.next().and_then(|f| f.parse().ok()).unwrap_or(0),
                    })
                })
                .collect()
        };
        Some(ShaderEntry {
            hash: u64::from_str_radix(f[0], 16).ok()?,
            vertex: f[1] == "vs",
            offset: f[2].parse().ok()?,
            size: f[3].parse().ok()?,
            inputs: io(f[4])?,
            outputs: io(f[5])?,
            samplers: items(f[6])
                .iter()
                .map(|s| {
                    let (r, t) = s.split_once(':')?;
                    Some((r.parse().ok()?, t.parse().ok()?))
                })
                .collect::<Option<_>>()?,
            bindings: items(f[7])
                .iter()
                .map(|b| {
                    let mut p = b.split(':');
                    let name_hash = u32::from_str_radix(p.next()?, 16).ok()?;
                    let register = p.next()?.parse().ok()?;
                    let rows = p.next()?;
                    let sampler = rows.ends_with('s');
                    Some(ShaderBinding { name_hash, register, rows: rows.trim_end_matches('s').parse().ok()?, sampler })
                })
                .collect::<Option<_>>()?,
            color_outputs: f[8].parse().ok()?,
            nonuniform: f[9].parse().ok()?,
        })
    }
}

pub fn compress(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).expect("in-memory write");
    e.finish().expect("in-memory write")
}

pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

pub const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// What things are made of, for footstep and impact sounds (the game's
/// surface switch, see `audio::SURFACE_GROUP`).
pub const SURFACES_REL: &str = "world/surfaces.bin";

/// Logic material (index into `logicmaterials.lib`) of terrain layers and
/// placed models, and the sound switch value of each logic material.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurfaceTable {
    /// Surface switch value per logic material index (0 = none).
    pub switch_values: Vec<u32>,
    /// Logic material per terrain layer (the layer table's order).
    pub layers: Vec<u8>,
    pub models: Vec<ModelSurfaces>,
}

/// One model's surfaces, from the collision file shipped next to it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelSurfaces {
    /// `path_hash` of the original model path (as in `model_rel`).
    pub model: u64,
    /// What most of its walkable area is.
    pub walk: u8,
    /// What most of it is overall (walls, for shots).
    pub wall: u8,
    /// Walkable triangles (Bevy model space) made of something else, with
    /// their logic material.
    pub patches: Vec<([[f32; 3]; 3], u8)>,
}

impl SurfaceTable {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::default();
        header(&mut w, b"SRF\0");
        w.chunk(b"LOGC", |w| {
            w.u32(self.switch_values.len() as u32);
            for v in &self.switch_values {
                w.u32(*v);
            }
        });
        w.chunk(b"LAYR", |w| {
            w.u32(self.layers.len() as u32);
            w.buf.extend_from_slice(&self.layers);
        });
        w.chunk(b"MODL", |w| {
            w.u32(self.models.len() as u32);
            for m in &self.models {
                w.u64(m.model);
                w.u8(m.walk);
                w.u8(m.wall);
                w.u32(m.patches.len() as u32);
                for (tri, logic) in &m.patches {
                    for c in tri {
                        w.f32s(c);
                    }
                    w.u8(*logic);
                }
            }
        });
        w.buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<SurfaceTable> {
        let mut r = Reader::new(data);
        check_header(&mut r, b"SRF\0")?;
        let mut t = SurfaceTable::default();
        for (tag, body) in chunks(&mut r)? {
            let mut r = Reader::new(body);
            match &tag {
                b"LOGC" => {
                    let n = r.u32()? as usize;
                    t.switch_values = (0..n).map(|_| r.u32()).collect::<Result<_>>()?;
                }
                b"LAYR" => {
                    let n = r.u32()? as usize;
                    t.layers = r.bytes(n)?.to_vec();
                }
                b"MODL" => {
                    let n = r.u32()? as usize;
                    for _ in 0..n {
                        let mut m = ModelSurfaces { model: r.u64()?, walk: r.u8()?, wall: r.u8()?, patches: Vec::new() };
                        let k = r.u32()? as usize;
                        for _ in 0..k {
                            let tri = read_f32s::<3>(&mut r, 3)?;
                            m.patches.push(([tri[0], tri[1], tri[2]], r.u8()?));
                        }
                        t.models.push(m);
                    }
                }
                _ => {}
            }
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vista_and_far_terrain_round_trip() {
        let forest = VistaForest {
            cards: vec!["models/a.fcm".into(), "models/b.fcm".into()],
            trees: vec![VistaTree { pos: [1.0, 120.5, -3.0], scale: 1.2, card: 1 }, VistaTree { pos: [-5.0, 101.0, 7.5], scale: 0.8, card: 0 }],
        };
        assert_eq!(VistaForest::from_bytes(&forest.to_bytes()).unwrap(), forest);
        let mut far = FarTerrainBuilder::default();
        let mut t = CachedTerrain { sector_id: 81 * 160 + 82, min_height: 0.0, max_height: 150.0, heights: vec![0; 65 * 65], layers: [0; 4], holes: Vec::new() };
        t.heights[32 * 65 + 32] = 150 * 64;
        far.put(&t, |_, _| Some([10, 20, 30]));
        let f = far.0;
        let size = f.size as usize;
        // Sample (1, 1) of sector (82, 81) holds the grid's (32, 32).
        assert_eq!(f.heights[(81 * 2 + 1) * size + 82 * 2 + 1], 150.0);
        assert_eq!(f.colours[(81 * 2) * size + 82 * 2], [10, 20, 30]);
        assert!(f.on_islands(81 * 160 + 82) && !f.on_islands(0));
        assert_eq!(FarTerrain::from_bytes(&f.to_bytes()), Some(f));
    }

    /// A sector's map lands in its 16x16 block, untransposed (texel rows are
    /// metres east in the map, atlas rows run north).
    #[test]
    fn horizon_atlas_places_sectors() {
        let mut dds = b"DDS ".to_vec();
        dds.resize(128, 0);
        for gx in 0..64u32 {
            for gy in 0..64u32 {
                // Red high byte grows east, green north.
                dds.extend_from_slice(&[0, (gx * 4) as u8, 0, (gy * 4) as u8]);
            }
        }
        let mut atlas = HorizonAtlas::new();
        let id = 81 * 160 + 82; // sector x 82, y 81
        assert!(atlas.put_sector(id, &dds));
        let at = |x: usize, y: usize| {
            let o = (y * atlas.size as usize + x) * 2;
            (atlas.data[o], atlas.data[o + 1])
        };
        // The block's south-west texel averages gx, gy in 0..4: 6 = mean of 0,4,8,12.
        assert_eq!(at(82 * 16, 81 * 16), (6, 6));
        // One texel east: red grows, green doesn't.
        assert_eq!(at(82 * 16 + 1, 81 * 16), (22, 6));
        assert_eq!(at(0, 0), (128, 128));
        assert_eq!(HorizonAtlas::from_bytes(&atlas.to_bytes()).map(|a| a.data == atlas.data), Some(true));
    }

    #[test]
    fn model_roundtrip() {
        let m = CachedModel {
            aabb_min: [-1.0, 0.0, -1.0],
            aabb_max: [1.0, 2.0, 1.0],
            materials: vec!["materials/a.fcmat".into()],
            material_flags: vec![3],
            lods: vec![CachedLod {
                distance: 10.0,
                parts: vec![CachedPart {
                    material: 0,
                    positions: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    normals: vec![[0.0, 1.0, 0.0]; 3],
                    uvs: vec![[0.0; 2]; 3],
                    joints: vec![[1, 2, 3, 4]; 3],
                    weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
                    indices: vec![0, 1, 2],
                    ..Default::default()
                }],
            }],
            collision_positions: vec![[0.0; 3]; 3],
            collision_indices: vec![0, 1, 2],
            bones: vec![CachedBone { name: "root".into(), parent: -1, inverse_bind: IDENTITY, ..Default::default() }],
        };
        let b = m.to_bytes();
        let m2 = CachedModel::from_bytes(&b).unwrap();
        assert_eq!(m2.materials, m.materials);
        assert_eq!(m2.material_flags, m.material_flags);
        assert_eq!(m2.lods[0].parts[0].indices, vec![0, 1, 2]);
        assert_eq!(m2.lods[0].parts[0].joints[0], [1, 2, 3, 4]);
        assert_eq!(m2.bones[0].parent, -1);
        assert_eq!(m2.bones[0].inverse_bind[15], 1.0);
        assert_eq!(m2.collision_indices.len(), 3);
    }

    #[test]
    fn sector_roundtrip() {
        let s = CachedSector {
            sector_id: 18000,
            models: vec!["models/x.fcm".into()],
            placements: vec![Placement { model: 0, translation: [1.0, 2.0, 3.0], rotation: [0.0, 0.0, 0.0, 1.0], scale: 1.0, entity: 0, material_set: 0 }],
            entities: vec![CachedEntity { id: 7, class: "CEntity".into(), ..Default::default() }],
            material_sets: vec![vec!["materials/b.fcmat".into()]],
        };
        let s2 = CachedSector::from_bytes(&s.to_bytes()).unwrap();
        assert_eq!(s2.placements, s.placements);
        assert_eq!(s2.entities, s.entities);
        assert_eq!(s2.material_sets, s.material_sets);
        let v = CachedVegetation {
            sector_id: 3,
            models: vec!["models/a.fcm".into()],
            instances: vec![VegInstance { model: 0, kind: veg_kind::GRASS, translation: [1.0, 2.0, 3.0], rotation: [0.0, 0.0, 0.0, 1.0], scale: 1.2, tint: [10, 20, 30] }],
        };
        let v2 = CachedVegetation::from_bytes(&v.to_bytes()).unwrap();
        assert_eq!(v2.instances, v.instances);
        assert_eq!(v2.models, v.models);
        let m = CachedMaterial { diffuse: "textures/x.dds".into(), flags: 5, base_color: [1.0; 4], ..Default::default() };
        assert_eq!(CachedMaterial::from_bytes(&m.to_bytes()).unwrap(), m);
    }

    #[test]
    fn terrain_layers_roundtrip() {
        let t = CachedTerrain { sector_id: 9842, min_height: 1.0, max_height: 2.0, heights: vec![7; 4], layers: [11, 12, 18, NO_LAYER], holes: Vec::new() };
        let t2 = CachedTerrain::from_bytes(&t.to_bytes()).unwrap();
        assert_eq!(t2.layers, t.layers);
        assert_eq!(t2.heights, t.heights);
        let table = TerrainLayerTable {
            layers: vec![
                CachedTerrainLayer {
                    diffuse: "textures/a.dds".into(),
                    normal: String::new(),
                    tiling: 18.5,
                    axis: 2,
                    name: "Grass".into(),
                    normal_tiling: 9.0,
                    noise_level: 1.5,
                    glossiness: 0.2,
                },
                CachedTerrainLayer {
                    diffuse: String::new(),
                    normal: "textures/b.dds".into(),
                    tiling: 2.0,
                    axis: 0,
                    name: "Cliff".into(),
                    normal_tiling: 2.0,
                    noise_level: 1.0,
                    glossiness: 0.0,
                },
            ],
            noise_transition: [0.5, 2.0, 4.0],
        };
        assert_eq!(TerrainLayerTable::from_text(&table.to_text()), table);
    }

    #[test]
    fn material_original_roundtrip() {
        let m = CachedMaterial {
            diffuse: "textures/a.dds".into(),
            params: vec![(7, vec![1, 2, 3, 4])],
            textures: vec![(9, "textures/a.dds".into())],
            original: Some(OriginalBinding {
                vs: 1,
                ps: 2,
                vs_constants: vec![(56, [0.0, 1.0, 0.0, 1.0])],
                ps_constants: vec![(69, [0.5; 4])],
                samplers: vec![(1, "textures/a.dds".into(), true)],
                use_normal: true,
                skinned_vs: 3,
                skinned_vs_constants: vec![(69, [1.0, 0.0, 0.0, 0.0])],
            }),
            ..Default::default()
        };
        assert_eq!(CachedMaterial::from_bytes(&m.to_bytes()).unwrap(), m);
    }

    #[test]
    fn surface_table_roundtrip() {
        let t = SurfaceTable {
            switch_values: vec![0x74C91, 0x74C96],
            layers: vec![7, 19, 4],
            models: vec![ModelSurfaces {
                model: 0x0123_4567_89ab_cdef,
                walk: 17,
                wall: 2,
                patches: vec![([[0.0, 1.0, 2.0], [3.0, 4.0, 5.0], [6.0, 7.0, 8.0]], 12)],
            }],
        };
        assert_eq!(SurfaceTable::from_bytes(&t.to_bytes()).unwrap(), t);
    }

    #[test]
    fn shader_entry_roundtrip() {
        let e = ShaderEntry {
            hash: 0x0123_4567_89ab_cdef,
            vertex: false,
            offset: 1234,
            size: 99,
            inputs: vec![ShaderIo { usage: 5, usage_index: 1, register: 1, location: Some(1), flags: 1 }],
            outputs: vec![ShaderIo { usage: 0, usage_index: 0, register: 0, location: None, flags: 5 }],
            samplers: vec![(0, 1), (3, 2)],
            bindings: vec![
                ShaderBinding { name_hash: 0xdeadbeef, register: 38, rows: 1, sampler: false },
                ShaderBinding { name_hash: 0x12345678, register: 2, rows: 1, sampler: true },
            ],
            color_outputs: 7,
            nonuniform: 0,
        };
        assert_eq!(ShaderEntry::from_line(&e.to_line()), Some(e));
        let text = b"some wgsl text some wgsl text";
        assert_eq!(decompress(&compress(text)).unwrap(), text);
    }
}
