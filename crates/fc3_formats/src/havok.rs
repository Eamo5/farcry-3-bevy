//! Havok 2010.2 packfiles (`.hkx`) as the game ships them next to its models:
//! just enough to read what each collision triangle is made of.
//!
//! Every mesh triangle has a material whose user data indexes the file's
//! `nomadSurfaceTypePalette`; each palette entry names a logic material
//! (`D16D191D` index in `logicmaterials.lib`), the same index terrain layers
//! give as `SurfaceTypeID`.
//!
//! Layout (32-bit pointers, little endian): a 16-byte Dunia prefix, the
//! Havok header (0x40), then 0x30-byte section headers (name[20],
//! data start, local fixups, global fixups, virtual fixups, exports, imports,
//! end). Local fixups are `(src, dst)` pointer pairs inside `__data__`;
//! virtual fixups `(object, section, class name offset)` list the objects.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

const MAGIC: [u8; 8] = [0x57, 0xE0, 0xE0, 0x57, 0x10, 0xC0, 0xC0, 0x10];

/// One collision triangle in the model's (Dunia, Z-up) space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceTriangle {
    pub corners: [[f32; 3]; 3],
    /// Logic material index (`logicmaterials.lib`).
    pub logic: u8,
    /// From the detailed body (`HIGH_*`, for shots) rather than the coarse
    /// one characters walk on.
    pub detail: bool,
}

struct Packfile<'a> {
    data: &'a [u8],
    fixups: HashMap<u32, u32>,
    objects: Vec<(u32, String)>,
}

impl<'a> Packfile<'a> {
    fn parse(file: &'a [u8]) -> Result<Packfile<'a>> {
        let base = file.windows(8).take(64).position(|w| w == MAGIC).context("not a Havok packfile")?;
        let hk = &file[base..];
        let u32_at = |b: &[u8], o: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(b.get(o..o + 4).context("truncated")?.try_into().unwrap()))
        };
        let sections = u32_at(hk, 0x14)? as usize;
        let mut names = None;
        let mut data = None;
        for i in 0..sections.min(8) {
            let h = 0x40 + i * 0x30;
            let name = hk.get(h..h + 20).context("truncated section")?;
            let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(20)];
            let f: Vec<u32> = (0..7).map(|k| u32_at(hk, h + 0x14 + k * 4)).collect::<Result<_>>()?;
            match name {
                b"__classnames__" => names = Some(f),
                b"__data__" => data = Some(f),
                _ => {}
            }
        }
        let (names, d) = (names.context("no class names")?, data.context("no data section")?);
        let start = d[0] as usize;
        let section = hk.get(start..start + d[6] as usize).context("data section out of range")?;
        let mut fixups = HashMap::new();
        for o in (d[1] as usize..(d[2] as usize).saturating_sub(7)).step_by(8) {
            let (src, dst) = (u32_at(section, o)?, u32_at(section, o + 4)?);
            if src != u32::MAX {
                fixups.insert(src, dst);
            }
        }
        let class_names = hk.get(names[0] as usize..).context("class names out of range")?;
        let mut objects = Vec::new();
        // The tables are padded with 0xFF to 16 bytes.
        for o in (d[3] as usize..(d[4] as usize).saturating_sub(11)).step_by(12) {
            let src = u32_at(section, o)?;
            if src == u32::MAX {
                break;
            }
            let at = u32_at(section, o + 8)? as usize;
            let name = class_names.get(at..).unwrap_or(&[]);
            let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
            objects.push((src, String::from_utf8_lossy(name).into_owned()));
        }
        Ok(Packfile { data: section, fixups, objects })
    }

    fn u32(&self, o: u32) -> u32 {
        self.data.get(o as usize..o as usize + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    /// An `hkArray` at `o`: (element data offset, count).
    fn array(&self, o: u32) -> Option<(u32, u32)> {
        let n = self.u32(o + 4);
        Some((*self.fixups.get(&o)?, n)).filter(|_| n > 0)
    }

    /// Whether the rigid body owning the object at `o` (the last one laid
    /// out before it) is named `HIGH_*`.
    fn in_detail_body(&self, o: u32) -> bool {
        let mut objs = self.objects.iter().map(|(at, _)| *at).collect::<Vec<_>>();
        objs.sort_unstable();
        let Some(body) = self.objects.iter().filter(|(at, c)| *at <= o && c == "hkpRigidBody").map(|(at, _)| *at).max() else {
            return false;
        };
        let end = objs.iter().copied().find(|&at| at > body).unwrap_or(u32::MAX);
        self.fixups
            .iter()
            .any(|(&src, &dst)| (body..end).contains(&src) && self.data.get(dst as usize..).is_some_and(|d| d.starts_with(b"HIGH_")))
    }

    fn of_class<'s>(&'s self, class: &'s str) -> impl Iterator<Item = u32> + 's {
        self.objects.iter().filter(move |(_, c)| c == class).map(|(o, _)| *o)
    }
}

/// Every collision triangle of a model's `.hkx` with its logic material.
/// Triangles whose material has no palette entry are skipped.
pub fn surface_triangles(file: &[u8]) -> Result<Vec<SurfaceTriangle>> {
    let p = Packfile::parse(file)?;
    // Palette: hkReferencedObject, then hkArray<(char* name, u32 logic)>.
    let Some(palette) = p.of_class("nomadSurfaceTypePalette").next() else { bail!("no surface palette") };
    let palette: Vec<u8> = match p.array(palette + 8) {
        Some((at, n)) => (0..n).map(|i| p.u32(at + i * 8 + 4).min(255) as u8).collect(),
        None => bail!("empty surface palette"),
    };
    let mut out = Vec::new();
    for s in p.of_class("hkpStorageExtendedMeshShapeMeshSubpartStorage") {
        // vertices, indices8/16/32, materialIndices, materials, namedMaterials, materialIndices16
        let Some((verts, nv)) = p.array(s + 0x08) else { continue };
        let detail = p.in_detail_body(s);
        let vertex = |i: u32| -> Option<[f32; 3]> {
            (i < nv).then(|| {
                let at = verts + i * 16;
                [0, 4, 8].map(|k| f32::from_bits(p.u32(at + k)))
            })
        };
        let indices: Option<(u32, u32, u32)> = [(0x14u32, 1u32), (0x20, 2), (0x2C, 4)]
            .into_iter()
            .find_map(|(off, size)| p.array(s + off).map(|(at, n)| (at, n, size)));
        let Some((idx_at, idx_n, idx_size)) = indices else { continue };
        let index = |k: u32| -> u32 {
            let o = idx_at + k * idx_size;
            match idx_size {
                1 => p.data.get(o as usize).copied().unwrap_or(0) as u32,
                2 => p.u32(o) & 0xFFFF,
                _ => p.u32(o),
            }
        };
        let materials: Vec<u32> = match p.array(s + 0x44) {
            Some((at, n)) => (0..n).map(|i| p.u32(at + i * 12 + 8)).collect(),
            None => Vec::new(),
        };
        let per_tri: Option<(u32, u32, u32)> = p
            .array(s + 0x38)
            .map(|(at, n)| (at, n, 1))
            .or_else(|| p.array(s + 0x5C).map(|(at, n)| (at, n, 2)));
        let count = match per_tri {
            Some((_, n, _)) => n,
            None => idx_n / 4,
        };
        if count == 0 {
            continue;
        }
        let stride = (idx_n / count).max(3);
        for t in 0..count {
            let material = match per_tri {
                Some((at, _, 1)) => p.data.get((at + t) as usize).copied().unwrap_or(0) as u32,
                Some((at, _, _)) => p.u32(at + t * 2) & 0xFFFF,
                None => 0,
            };
            let entry = materials.get(material as usize).copied().unwrap_or(0);
            let Some(&logic) = palette.get(entry as usize) else { continue };
            let k = t * stride;
            let (Some(a), Some(b), Some(c)) = (vertex(index(k)), vertex(index(k + 1)), vertex(index(k + 2))) else { continue };
            out.push(SurfaceTriangle { corners: [a, b, c], logic, detail });
        }
    }
    Ok(out)
}

/// Twice the triangle's area and its normal's vertical share.
fn area_and_slope(t: &SurfaceTriangle) -> (f32, f32) {
    let [a, b, c] = t.corners;
    let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
    let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    (len, if len > 1e-9 { n[2].abs() / len } else { 0.0 })
}

/// Flat enough to stand on.
pub fn walkable(t: &SurfaceTriangle) -> bool {
    let (len, up) = area_and_slope(t);
    len > 1e-9 && up >= 0.7
}

/// Area per logic material over the triangles facing up (`up = true`, what
/// one walks on) or all of them, largest first.
pub fn surface_areas(tris: &[SurfaceTriangle], up: bool) -> Vec<(u8, f32)> {
    let mut by: HashMap<u8, f32> = HashMap::new();
    for t in tris {
        let (len, slope) = area_and_slope(t);
        if len <= 1e-9 || (up && slope < 0.7) {
            continue;
        }
        *by.entry(t.logic).or_default() += len * 0.5;
    }
    let mut v: Vec<(u8, f32)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

/// `logicmaterials.lib`: what each logic material index sounds like.
pub const LOGIC_MATERIALS: &str = r"generated\databases\generic\logicmaterials.lib";

/// Surface switch value per logic material index (0 where a material has
/// none), from [`LOGIC_MATERIALS`].
pub fn logic_switch_values(lib: &[u8]) -> Result<Vec<u32>> {
    const INDEX: u32 = 0xD16D191D;
    const SWITCH_VALUE: u32 = 0xA60D74FC;
    let start = lib.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&lib[start..])?;
    let mut out = vec![0u32; 256];
    let mut max = 0;
    for m in &root.children {
        let (Some(&[i]), Some(v)) = (m.field_h(INDEX), m.field_h(SWITCH_VALUE)) else { continue };
        let Ok(v) = <[u8; 4]>::try_from(v) else { continue };
        let v = u32::from_le_bytes(v);
        out[i as usize] = if v == u32::MAX { 0 } else { v };
        max = max.max(i as usize + 1);
    }
    out.truncate(max);
    Ok(out)
}
