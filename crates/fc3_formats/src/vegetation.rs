//! Baked vegetation ("collections") placed on the terrain.
//!
//! Far Cry 3 does not place trees, plants and grass as ordinary entities, and
//! does not generate them at run time either: the editor's collection painter
//! (catalogue in `ingameeditor\collection_inventory.xml`) was run offline and
//! its output baked into every world sector:
//!
//! ```text
//! worldsector_N.data.fcb
//!   WorldSector / MissionLayer / Entity (class "CSectorEntity")
//!     Components / CCollectionComponent
//!       VegetationData        { dataVersion u32, bboxMin vec3, bboxMax vec3 }
//!         VegetationZoneData  { zoneId u64, zoneData blob }   (1..n per sector)
//! ```
//!
//! `zoneData` is a little-endian blob:
//!
//! ```text
//! u32 size (= blob length - 4)
//! u32 blockCount
//! blockCount x {
//!     u64 clusterResId          -> clusterresourceslibrary.fcb
//!     u32 reserved (0)
//!     u32 groupCount
//!     groupCount x {
//!         vec3 center, f32 radius   (bounding sphere; zero for RealTree blocks)
//!         u32 instanceCount
//!         instanceCount x instance  (layout depends on the resource kind)
//!     }
//! }
//! ```
//!
//! Instance layouts (all positions are absolute Dunia coordinates, Z up):
//!
//! * RealTree (`CRealtreeResource`, `.rtx`), 20 bytes:
//!   `vec3 pos, f32 yaw (degrees about +Z), f32 scale`.
//! * Grass (`CGeometryResource` with flag bit 0x80), 24 bytes:
//!   `vec3 pos, u8[4] colour (B, G, R, A), u8[4] normal (B=z, G=y, R=x, A=0),
//!   i16[2] unit direction vector`. Colour RGB is a ground tint (64,64,64 =
//!   neutral); alpha is the scale, `A * 1.4 / 255` (182 = 1.0). The normal
//!   components map `0..255` to `-1..1` and match the terrain normal.
//! * Other geometry (`CGeometryResource`), 32 bytes:
//!   `vec3 pos, vec3 Euler angles (radians, same axes as entity hidAngles),
//!   u32[2] reserved (0xFFFFFFFF)`.
//!
//! `worlds\fc3_main\generated\clusterresourceslibrary.fcb` maps each
//! `clusterResId` to its model (`text_resId`), resource class, flags,
//! far-distance billboard (`text_vistaResId`), collision and occluder.
//!
//! See `docs/vegetation.md` for the evidence behind each field.

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

use crate::archive::Vfs;
use crate::fcb::{self, bytes_to_string, Object};
use crate::hash::name_hash;
use crate::reader::Reader;
use crate::terrain::SectorCoord;

pub const LIBRARY_PATH: &str = r"worlds\fc3_main\generated\clusterresourceslibrary.fcb";
pub const WORLD: &str = "fc3_main";

/// Library flag marking geometry that is instanced as grass (24-byte instances).
pub const FLAG_GRASS: u16 = 0x80;
/// Grass instances store their scale in a byte: `scale = byte * GRASS_SCALE_MAX / 255`.
pub const GRASS_SCALE_MAX: f32 = 1.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PlantKind {
    /// RealTree (`.rtx`) trees, palms, bushes and large plants.
    Tree,
    /// Grass/fern/ground-cover meshes (`.xbg`), terrain aligned.
    Grass,
    /// Any other clustered geometry (`.xbg`): roots, rocks, pebbles, clutter.
    Mesh,
}

impl PlantKind {
    pub fn instance_size(self) -> usize {
        match self {
            PlantKind::Tree => 20,
            PlantKind::Grass => 24,
            PlantKind::Mesh => 32,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PlantKind::Tree => "tree",
            PlantKind::Grass => "grass",
            PlantKind::Mesh => "mesh",
        }
    }
}

/// One entry of `clusterresourceslibrary.fcb`.
#[derive(Clone, Debug, Default)]
pub struct ClusterResource {
    pub id: u64,
    /// `CGeometryResource` or `CRealtreeResource`.
    pub class: String,
    /// Model path (`.xbg`, or `.rtx` for RealTree).
    pub path: String,
    pub flags: u16,
    /// Far-distance billboard mesh (`.xbg`), mostly for trees.
    pub vista: Option<String>,
    /// Billboard shadow mesh.
    pub vista_shadow: Option<String>,
    /// Distance (m) beyond which the billboard replaces the model.
    pub vista_min_view_distance: f32,
    pub collision: Option<String>,
    pub occluder: Option<String>,
    /// `(slot name, material path)` overrides applied to the model.
    pub material_overrides: Vec<(String, String)>,
}

impl ClusterResource {
    pub fn kind(&self) -> PlantKind {
        if self.class == "CRealtreeResource" {
            PlantKind::Tree
        } else if self.flags & FLAG_GRASS != 0 {
            PlantKind::Grass
        } else {
            PlantKind::Mesh
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClusterLibrary {
    pub resources: HashMap<u64, ClusterResource>,
}

fn text(o: &Object, name: &str) -> Option<String> {
    let v = o.field(name)?;
    // Empty strings are stored as a single zero byte.
    if v.len() <= 1 {
        return None;
    }
    Some(bytes_to_string(v)).filter(|s| !s.is_empty())
}

impl ClusterLibrary {
    pub fn parse(data: &[u8]) -> Result<ClusterLibrary> {
        let root = fcb::parse(data)?;
        let mut resources = HashMap::new();
        for c in &root.children {
            let Some(id) = c.u64("clusterResId") else { continue };
            let flags = c.field("flags").map(|b| match b.len() {
                0 => 0,
                1 => b[0] as u16,
                _ => u16::from_le_bytes([b[0], b[1]]),
            });
            let mut material_overrides = Vec::new();
            if let Some(map) = c.child("MaterialOverridesMap") {
                for m in &map.children {
                    if let Some(path) = text(m, "text_MaterialRef") {
                        material_overrides.push((text(m, "text_MaterialSlot").unwrap_or_default(), path));
                    }
                }
            }
            resources.insert(
                id,
                ClusterResource {
                    id,
                    class: text(c, "text_resClassTypeId").unwrap_or_default(),
                    path: text(c, "text_resId").unwrap_or_default(),
                    flags: flags.unwrap_or(0),
                    vista: text(c, "text_vistaResId"),
                    vista_shadow: text(c, "text_vistaShadowResId"),
                    vista_min_view_distance: c.f32("vistaMinViewDistance").unwrap_or(0.0),
                    collision: text(c, "text_physPathId"),
                    occluder: text(c, "text_OcclusionMeshResId"),
                    material_overrides,
                },
            );
        }
        Ok(ClusterLibrary { resources })
    }

    pub fn load(vfs: &Vfs) -> Result<ClusterLibrary> {
        ClusterLibrary::parse(&vfs.read(LIBRARY_PATH)?)
    }

    /// Process-wide library, loaded on first use.
    pub fn shared(vfs: &Vfs) -> Result<&'static ClusterLibrary> {
        static LIB: OnceLock<ClusterLibrary> = OnceLock::new();
        if let Some(l) = LIB.get() {
            return Ok(l);
        }
        let lib = ClusterLibrary::load(vfs)?;
        Ok(LIB.get_or_init(|| lib))
    }

    pub fn get(&self, id: u64) -> Option<&ClusterResource> {
        self.resources.get(&id)
    }

    pub fn kind(&self, id: u64) -> Option<PlantKind> {
        self.get(id).map(ClusterResource::kind)
    }
}

/// One placed plant, rock or other clustered object.
#[derive(Clone, Debug, PartialEq)]
pub struct PlantInstance {
    /// `clusterResId` (key into [`ClusterLibrary`]).
    pub resource: u64,
    pub kind: PlantKind,
    /// Model path: `.rtx` for [`PlantKind::Tree`], `.xbg` otherwise. Empty if
    /// the resource is missing from the library.
    pub model: String,
    /// Dunia world position (Z up), metres; base of the model on the ground.
    pub pos: [f32; 3],
    /// Rotation about +Z, radians.
    pub yaw: f32,
    pub scale: f32,
    /// Full orientation in the entity `hidAngles` convention (degrees,
    /// `[2]` = yaw), usable with `world::angles_to_bevy_quat`. Only meshes
    /// carry pitch/roll; trees and grass have `[0, 0, yaw]`.
    pub angles: [f32; 3],
    /// Grass only: terrain normal at the instance (Dunia space), for aligning
    /// the clump to the slope.
    pub normal: Option<[f32; 3]>,
    /// Grass only: RGB ground tint (64,64,64 = neutral).
    pub tint: Option<[u8; 3]>,
}

/// A bounding-sphere group of instances of one resource.
#[derive(Clone, Debug)]
pub struct ClusterGroup {
    pub center: [f32; 3],
    pub radius: f32,
    pub instances: Vec<PlantInstance>,
}

/// All groups of one resource inside a zone blob.
#[derive(Clone, Debug)]
pub struct ClusterBlock {
    pub resource: u64,
    pub kind: PlantKind,
    /// True when the kind came from the library rather than from layout inference.
    pub known: bool,
    pub reserved: u32,
    pub groups: Vec<ClusterGroup>,
}

#[derive(Clone, Debug)]
pub struct VegetationZone {
    /// `hidName` of the owning sector entity, e.g. `SectorEntity_S18009_Default.<id>`;
    /// a few sectors have an extra `HDSectorEntity_...` with additional detail.
    pub owner: String,
    pub zone_id: u64,
    pub data_version: u32,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub blocks: Vec<ClusterBlock>,
}

impl VegetationZone {
    pub fn instances(&self) -> impl Iterator<Item = &PlantInstance> {
        self.blocks.iter().flat_map(|b| b.groups.iter()).flat_map(|g| g.instances.iter())
    }
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn decode_instance(kind: PlantKind, resource: u64, model: &str, b: &[u8]) -> PlantInstance {
    let pos = [f32_at(b, 0), f32_at(b, 4), f32_at(b, 8)];
    match kind {
        PlantKind::Tree => {
            let deg = f32_at(b, 12);
            PlantInstance {
                resource,
                kind,
                model: model.to_string(),
                pos,
                yaw: deg.to_radians(),
                scale: f32_at(b, 16),
                angles: [0.0, 0.0, deg],
                normal: None,
                tint: None,
            }
        }
        PlantKind::Grass => {
            let n = |v: u8| v as f32 / 127.5 - 1.0;
            let normal = [n(b[18]), n(b[17]), n(b[16])];
            let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt().max(1e-6);
            let dx = i16::from_le_bytes([b[20], b[21]]) as f32 / 32767.0;
            let dy = i16::from_le_bytes([b[22], b[23]]) as f32 / 32767.0;
            // Rotation that takes +Y (Dunia forward) onto (dx, dy).
            let yaw = (-dx).atan2(dy);
            PlantInstance {
                resource,
                kind,
                model: model.to_string(),
                pos,
                yaw,
                scale: b[15] as f32 * GRASS_SCALE_MAX / 255.0,
                angles: [0.0, 0.0, yaw.to_degrees()],
                normal: Some(normal.map(|c| c / len)),
                tint: Some([b[14], b[13], b[12]]),
            }
        }
        PlantKind::Mesh => {
            let rot = [f32_at(b, 12), f32_at(b, 16), f32_at(b, 20)];
            PlantInstance {
                resource,
                kind,
                model: model.to_string(),
                pos,
                yaw: rot[2],
                scale: 1.0,
                angles: rot.map(f32::to_degrees),
                normal: None,
                tint: None,
            }
        }
    }
}

/// Sanity limit for coordinates: the playable world spans +-5120 m.
const COORD_LIMIT: f32 = 20000.0;

/// Parses the groups of one block assuming `kind`; returns the end offset.
fn parse_groups(
    data: &[u8],
    mut p: usize,
    count: u32,
    kind: PlantKind,
    resource: u64,
    model: &str,
    out: &mut Vec<ClusterGroup>,
) -> Option<usize> {
    let size = kind.instance_size();
    for _ in 0..count {
        if p + 20 > data.len() {
            return None;
        }
        let center = [f32_at(data, p), f32_at(data, p + 4), f32_at(data, p + 8)];
        let radius = f32_at(data, p + 12);
        let n = u32::from_le_bytes(data[p + 16..p + 20].try_into().unwrap()) as usize;
        p += 20;
        if !center.iter().chain([&radius]).all(|v| v.is_finite() && v.abs() < COORD_LIMIT) {
            return None;
        }
        if n.checked_mul(size)? > data.len() - p {
            return None;
        }
        let mut instances = Vec::with_capacity(n);
        for _ in 0..n {
            let b = &data[p..p + size];
            let inst = decode_instance(kind, resource, model, b);
            if !inst.pos.iter().all(|v| v.is_finite() && v.abs() < COORD_LIMIT) {
                return None;
            }
            instances.push(inst);
            p += size;
        }
        out.push(ClusterGroup { center, radius, instances });
    }
    Some(p)
}

fn parse_blocks(data: &[u8], p: usize, left: u32, lib: &ClusterLibrary, out: &mut Vec<ClusterBlock>) -> bool {
    if left == 0 {
        return p == data.len();
    }
    if p + 16 > data.len() {
        return false;
    }
    let resource = u64::from_le_bytes(data[p..p + 8].try_into().unwrap());
    let reserved = u32::from_le_bytes(data[p + 8..p + 12].try_into().unwrap());
    let groups = u32::from_le_bytes(data[p + 12..p + 16].try_into().unwrap());
    let res = lib.get(resource);
    let model = res.map(|r| r.path.as_str()).unwrap_or("");
    // The library kind first; other layouts only as a fallback for unknown resources
    // (or if the library disagrees with the data).
    let mut candidates = vec![];
    if let Some(r) = res {
        candidates.push((r.kind(), true));
    }
    for k in [PlantKind::Grass, PlantKind::Mesh, PlantKind::Tree] {
        if !candidates.iter().any(|(c, _)| *c == k) {
            candidates.push((k, false));
        }
    }
    for (kind, known) in candidates {
        let mut gs = Vec::new();
        let Some(end) = parse_groups(data, p + 16, groups, kind, resource, model, &mut gs) else { continue };
        out.push(ClusterBlock { resource, kind, known, reserved, groups: gs });
        if parse_blocks(data, end, left - 1, lib, out) {
            return true;
        }
        out.pop();
    }
    false
}

/// Parses a `zoneData` blob into blocks of instance groups.
pub fn parse_zone_data(data: &[u8], lib: &ClusterLibrary) -> Result<Vec<ClusterBlock>> {
    let mut r = Reader::new(data);
    let size = r.u32()? as usize;
    let count = r.u32()?;
    if size + 4 != data.len() {
        bail!("vegetation zone size {size} does not match blob length {}", data.len());
    }
    let mut out = Vec::with_capacity(count as usize);
    if !parse_blocks(data, 8, count, lib, &mut out) {
        bail!("vegetation zone: could not parse {count} cluster blocks");
    }
    Ok(out)
}

fn vec3_or(o: &Object, name: &str) -> [f32; 3] {
    o.vec3(name).unwrap_or([0.0; 3])
}

/// Extracts every vegetation zone from a `worldsector_N.data.fcb` file.
pub fn parse_sector_zones(world_sector: &[u8], lib: &ClusterLibrary) -> Result<Vec<VegetationZone>> {
    struct Ctx<'a> {
        lib: &'a ClusterLibrary,
        entity_h: u32,
        veg_h: u32,
        zone_h: u32,
        out: Vec<VegetationZone>,
    }
    fn visit(o: &Object, owner: &str, cx: &mut Ctx) -> Result<()> {
        let named;
        let owner = if o.name_hash == cx.entity_h {
            named = o.str("hidName").unwrap_or_default();
            named.as_str()
        } else {
            owner
        };
        if o.name_hash == cx.veg_h {
            let data_version = o.u32("dataVersion").unwrap_or(0);
            let (bbox_min, bbox_max) = (vec3_or(o, "bboxMin"), vec3_or(o, "bboxMax"));
            for z in o.children.iter().filter(|c| c.name_hash == cx.zone_h) {
                let zone_id = z.u64("zoneId").unwrap_or(0);
                let Some(blob) = z.field("zoneData") else { continue };
                let blocks = parse_zone_data(blob, cx.lib).with_context(|| format!("{owner} zone {zone_id:#x}"))?;
                cx.out.push(VegetationZone { owner: owner.to_string(), zone_id, data_version, bbox_min, bbox_max, blocks });
            }
            return Ok(());
        }
        for c in &o.children {
            visit(c, owner, cx)?;
        }
        Ok(())
    }
    let root = fcb::parse(world_sector)?;
    let mut cx = Ctx {
        lib,
        entity_h: name_hash("Entity"),
        veg_h: name_hash("VegetationData"),
        zone_h: name_hash("VegetationZoneData"),
        out: Vec::new(),
    };
    visit(&root, "", &mut cx)?;
    Ok(cx.out)
}

/// All instances of a `worldsector_N.data.fcb` file.
pub fn parse_sector_vegetation(world_sector: &[u8], lib: &ClusterLibrary) -> Result<Vec<PlantInstance>> {
    let zones = parse_sector_zones(world_sector, lib)?;
    Ok(zones.iter().flat_map(|z| z.instances().cloned()).collect())
}

/// Path of the world sector file holding a sector's vegetation.
pub fn sector_file(sector_id: u32) -> String {
    SectorCoord::from_id(sector_id).entities_path(WORLD)
}

/// Vegetation zones of a sector (empty if the sector has no world sector file).
pub fn sector_zones(vfs: &Vfs, lib: &ClusterLibrary, sector_id: u32) -> Result<Vec<VegetationZone>> {
    let path = sector_file(sector_id);
    if !vfs.exists(&path) {
        return Ok(Vec::new());
    }
    parse_sector_zones(&vfs.read(&path)?, lib).with_context(|| format!("sector {sector_id}"))
}

/// Every baked plant, tree, grass clump and clustered rock of a sector.
pub fn sector_vegetation_with(vfs: &Vfs, lib: &ClusterLibrary, sector_id: u32) -> Result<Vec<PlantInstance>> {
    Ok(sector_zones(vfs, lib, sector_id)?.iter().flat_map(|z| z.instances().cloned()).collect())
}

/// [`sector_vegetation_with`] using the shared cluster library.
pub fn sector_vegetation(vfs: &Vfs, sector_id: u32) -> Result<Vec<PlantInstance>> {
    sector_vegetation_with(vfs, ClusterLibrary::shared(vfs)?, sector_id)
}

/// Maps a sector-local position (metres east/north of the sector's SW corner,
/// 0..64) to a pixel of the sector's 2x2 `atlas*_diffuse/color` texture, given
/// the texture size. Quadrants are laid out with the even (south/west) sector
/// first (row = sector y parity, column = sector x parity), but inside each
/// quadrant the image is transposed: rows run along +X and columns along +Y.
pub fn atlas_pixel(coord: SectorCoord, lx: f32, ly: f32, atlas_size: u32) -> (u32, u32) {
    let a = coord.atlas_coord();
    let (qx, qy) = ((coord.x - a.x) as f32, (coord.y - a.y) as f32);
    let half = atlas_size as f32 / 2.0;
    let px = |v: f32| (v.clamp(0.0, 63.999) / 64.0 * half) as u32;
    let row = (qy * half) as u32 + px(lx);
    let col = (qx * half) as u32 + px(ly);
    (row.min(atlas_size - 1), col.min(atlas_size - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(blocks: &[(u64, &[(u32, Vec<Vec<u8>>)])]) -> Vec<u8> {
        let mut b = vec![0u8; 4];
        b.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
        for (id, groups) in blocks {
            b.extend_from_slice(&id.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&(groups.len() as u32).to_le_bytes());
            for (_, insts) in groups.iter() {
                for v in [1.0f32, 2.0, 3.0, 4.0] {
                    b.extend_from_slice(&v.to_le_bytes());
                }
                b.extend_from_slice(&(insts.len() as u32).to_le_bytes());
                for i in insts {
                    b.extend_from_slice(i);
                }
            }
        }
        let n = (b.len() - 4) as u32;
        b[..4].copy_from_slice(&n.to_le_bytes());
        b
    }

    fn floats(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[test]
    fn decodes_layouts() {
        let mut lib = ClusterLibrary::default();
        lib.resources.insert(1, ClusterResource { id: 1, class: "CRealtreeResource".into(), path: "t.rtx".into(), ..Default::default() });
        lib.resources.insert(2, ClusterResource { id: 2, class: "CGeometryResource".into(), path: "g.xbg".into(), flags: 0xC0, ..Default::default() });
        lib.resources.insert(3, ClusterResource { id: 3, class: "CGeometryResource".into(), path: "m.xbg".into(), flags: 0x68, ..Default::default() });
        let tree = floats(&[10.0, 20.0, 30.0, 90.0, 1.1]);
        let mut grass = floats(&[1.0, 2.0, 3.0]);
        grass.extend_from_slice(&[38, 68, 54, 182, 255, 128, 128, 0]);
        grass.extend_from_slice(&0i16.to_le_bytes());
        grass.extend_from_slice(&32767i16.to_le_bytes());
        let mut mesh = floats(&[5.0, 6.0, 7.0, 0.1, 0.2, std::f32::consts::FRAC_PI_2]);
        mesh.extend_from_slice(&[0xFF; 8]);
        let data = blob(&[(1, &[(0, vec![tree])]), (2, &[(0, vec![grass.clone(), grass])]), (3, &[(0, vec![mesh])])]);
        let blocks = parse_zone_data(&data, &lib).unwrap();
        assert_eq!(blocks.len(), 3);
        let t = &blocks[0].groups[0].instances[0];
        assert_eq!((t.kind, t.pos, t.scale), (PlantKind::Tree, [10.0, 20.0, 30.0], 1.1));
        assert!((t.yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        let g = &blocks[1].groups[0].instances[1];
        assert_eq!(g.kind, PlantKind::Grass);
        assert!((g.scale - 0.9992).abs() < 1e-3);
        assert_eq!(g.tint, Some([54, 68, 38]));
        assert!(g.normal.unwrap()[2] > 0.99);
        assert!(g.yaw.abs() < 1e-6);
        let m = &blocks[2].groups[0].instances[0];
        assert_eq!(m.kind, PlantKind::Mesh);
        assert!((m.angles[2] - 90.0).abs() < 1e-3);
        // An unknown resource falls back to layout inference.
        let lib2 = ClusterLibrary::default();
        let blocks = parse_zone_data(&data, &lib2).unwrap();
        assert_eq!(blocks.iter().map(|b| b.kind).collect::<Vec<_>>(), [PlantKind::Tree, PlantKind::Grass, PlantKind::Mesh]);
    }

    #[test]
    fn atlas_mapping() {
        // Sector (85, 112) is the east half of atlas 18004; local x runs down the rows.
        let c = SectorCoord::new(85, 112);
        assert_eq!(atlas_pixel(c, 0.0, 0.0, 256), (0, 128));
        assert_eq!(atlas_pixel(c, 63.9, 10.0, 256), (127, 148));
        let c = SectorCoord::new(84, 113);
        assert_eq!(atlas_pixel(c, 32.0, 0.0, 256), (192, 0));
    }
}
