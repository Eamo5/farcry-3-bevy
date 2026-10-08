//! Decodes XBG geometry into plain vertex arrays (Bevy Y-up coordinates).
//!
//! Dunia space is right-handed Z-up; we convert (x, y, z) -> (x, z, -y).

use anyhow::{bail, Context, Result};

use crate::reader::Reader;
use crate::xbg::{parse_lods, Chunk, XbgFile};

pub const FMT_POS_F32: u32 = 1 << 0;
pub const FMT_POS_I16: u32 = 1 << 1;
pub const FMT_UV0: u32 = 1 << 3;
pub const FMT_SKIN: u32 = 1 << 4;
pub const FMT_NORMAL: u32 = 1 << 6;
pub const FMT_COLOR: u32 = 1 << 7;
pub const FMT_TANGENT: u32 = 1 << 8;
pub const FMT_BINORMAL: u32 = 1 << 9;
pub const FMT_UV1: u32 = 1 << 11;
pub const FMT_UV2_EXTRA: u32 = 1 << 12;
/// Another 4-byte element, stored after the 0x1000 slot and before skin data.
pub const FMT_EXTRA2: u32 = 1 << 13;

/// Byte offsets of each vertex element for a format.
#[derive(Debug, Clone, Copy, Default)]
pub struct Layout {
    pub pos: Option<usize>,
    pub pos_f32: bool,
    pub uv0: Option<usize>,
    pub uv1: Option<usize>,
    pub uv2: Option<usize>,
    pub skin: Option<usize>,
    pub normal: Option<usize>,
    pub color: Option<usize>,
    pub tangent: Option<usize>,
    pub binormal: Option<usize>,
    pub stride: usize,
}

pub fn layout(format: u32) -> Layout {
    let mut l = Layout::default();
    let mut o = 0;
    if format & FMT_POS_F32 != 0 {
        l.pos = Some(o);
        l.pos_f32 = true;
        o += 12;
    } else if format & FMT_POS_I16 != 0 {
        l.pos = Some(o);
        o += 8;
    }
    let mut take = |flag: u32, size: usize| -> Option<usize> {
        if format & flag != 0 {
            let r = o;
            o += size;
            Some(r)
        } else {
            None
        }
    };
    l.uv0 = take(FMT_UV0, 4);
    l.uv1 = take(FMT_UV1, 4);
    l.uv2 = take(FMT_UV2_EXTRA, 4);
    let _extra2 = take(FMT_EXTRA2, 4);
    l.skin = take(FMT_SKIN, 8);
    l.normal = take(FMT_NORMAL, 4);
    l.color = take(FMT_COLOR, 4);
    l.tangent = take(FMT_TANGENT, 4);
    l.binormal = take(FMT_BINORMAL, 4);
    // Vertices may carry trailing padding up to the buffer's stride.
    l.stride = o;
    l
}

#[derive(Debug, Clone)]
pub struct SubMesh {
    /// Index of the source primitive in the LOD (for skin/material lookups).
    pub primitive: usize,
    pub material: usize,
    /// Index into the SKND "part" list (rigid sub-object), used by skinned/multi-part meshes.
    pub part: i32,
    pub index_start: usize,
    pub index_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub uvs1: Vec<[f32; 2]>,
    pub tangents: Vec<[f32; 4]>,
    pub colors: Vec<[f32; 4]>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<SubMesh>,
}

#[derive(Debug, Clone)]
pub struct MaterialRef {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub name_hash: u32,
    pub parent: i32,
    /// Local transform in Bevy space.
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
}

#[derive(Debug, Clone)]
pub struct Model {
    pub materials: Vec<MaterialRef>,
    pub lods: Vec<(f32, MeshData)>,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub bones: Vec<Bone>,
}

#[inline]
pub fn to_bevy(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], -p[1]]
}

#[inline]
fn unorm_vec(b: &[u8]) -> [f32; 3] {
    // Stored B,G,R(,A) like a D3DCOLOR: byte 2 = x, byte 1 = y, byte 0 = z.
    let f = |v: u8| v as f32 / 127.5 - 1.0;
    [f(b[2]), f(b[1]), f(b[0])]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-6 {
        [v[0] / l, v[1] / l, v[2] / l]
    } else {
        [0.0, 1.0, 0.0]
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn read_pair(d: &[u8], o: usize) -> [f32; 2] {
    // Floats preferably decoded by caller via scale/bias; return raw i16s here.
    [i16::from_le_bytes([d[o], d[o + 1]]) as f32, i16::from_le_bytes([d[o + 2], d[o + 3]]) as f32]
}

fn chunk_f32x2(x: &XbgFile, kind: &str) -> Option<[f32; 2]> {
    let c = x.root.find(kind)?;
    let d = x.chunk_data(c);
    if d.len() < 8 {
        return None;
    }
    Some([f32::from_le_bytes(d[0..4].try_into().unwrap()), f32::from_le_bytes(d[4..8].try_into().unwrap())])
}

fn parse_materials(x: &XbgFile) -> Result<Vec<MaterialRef>> {
    let Some(c) = x.root.find("RMTL") else { return Ok(vec![]) };
    let mut r = Reader::new(x.chunk_data(c));
    let n = r.u32()?;
    let _unk = r.u32()?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let path = r.lstring()?;
        let name = r.lstring()?;
        v.push(MaterialRef { name, path });
    }
    Ok(v)
}

fn parse_nodes(x: &XbgFile, c: &Chunk) -> Result<Vec<Bone>> {
    let mut r = Reader::new(x.chunk_data(c));
    let n = r.u32()?;
    let mut nodes = Vec::with_capacity(n as usize);
    let mut parents = vec![-1i32; n as usize];
    let mut raw = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let name_hash = r.u32()?;
        let next = r.i32()?;
        let first_child = r.i32()?;
        let _prev = r.i32()?;
        let pos = r.vec3()?;
        let rot = r.vec4()?;
        let _rest = r.bytes(4 * 3 + 4 + 4 * 2)?;
        let name = r.lstring()?;
        raw.push((name_hash, next, first_child, pos, rot, name));
    }
    // Derive parents from first-child / next-sibling links.
    for (i, item) in raw.iter().enumerate() {
        let mut c = item.2;
        let mut guard = 0;
        while c >= 0 && (c as usize) < raw.len() && guard < 10000 {
            parents[c as usize] = i as i32;
            c = raw[c as usize].1;
            guard += 1;
        }
    }
    for (i, (name_hash, _, _, pos, rot, name)) in raw.into_iter().enumerate() {
        // Rotation quaternion (x, y, z, w) in Z-up space -> Y-up space.
        let q = [rot[0], rot[2], -rot[1], rot[3]];
        nodes.push(Bone { name, name_hash, parent: parents[i], translation: to_bevy(pos), rotation: q });
    }
    Ok(nodes)
}

pub fn decode(data: &[u8], max_lods: usize) -> Result<Model> {
    let x = XbgFile::parse(data)?;
    if x.major != 52 {
        bail!("unsupported XBG version {}", x.major);
    }
    let pcmp = chunk_f32x2(&x, "PCMP").unwrap_or([0.0, 1.0]);
    let ucmp = chunk_f32x2(&x, "UCMP").unwrap_or([0.0, 1.0]);
    let materials = parse_materials(&x)?;
    let lods_chunk = x.root.find("LODS").context("XBG has no LODS")?;
    let lods = parse_lods(data, lods_chunk)?;
    let bones = match x.root.find("NODE") {
        Some(c) => parse_nodes(&x, c).unwrap_or_default(),
        None => vec![],
    };

    let (bmin, bmax) = match x.root.find("BBOX") {
        Some(c) => {
            let mut r = Reader::new(x.chunk_data(c));
            let a = r.vec3()?;
            let b = r.vec3()?;
            let a2 = to_bevy(a);
            let b2 = to_bevy(b);
            (
                [a2[0].min(b2[0]), a2[1].min(b2[1]), a2[2].min(b2[2])],
                [a2[0].max(b2[0]), a2[1].max(b2[1]), a2[2].max(b2[2])],
            )
        }
        None => ([0.0; 3], [0.0; 3]),
    };

    let mut out_lods = Vec::new();
    for lod in lods.iter().take(max_lods.max(1)) {
        let mut m = MeshData::default();
        let vdata = &data[lod.vertex_data.clone()];
        // Each buffer becomes a contiguous vertex range; track bases for index remap.
        let mut buffer_base = Vec::with_capacity(lod.buffers.len());
        for b in &lod.buffers {
            buffer_base.push(m.positions.len() as u32);
            let l = layout(b.format);
            if l.stride > b.stride as usize {
                bail!("vertex format {:#x}: computed size {} exceeds stride {}", b.format, l.stride, b.stride);
            }
            let stride = b.stride as usize;
            let start = b.offset as usize;
            for i in 0..b.count as usize {
                let o = start + i * stride;
                let v = &vdata[o..o + stride];
                let p = if l.pos_f32 {
                    let f = |k: usize| f32::from_le_bytes(v[k..k + 4].try_into().unwrap());
                    [f(0), f(4), f(8)]
                } else {
                    let s = |k: usize| i16::from_le_bytes([v[k], v[k + 1]]) as f32 * pcmp[1] + pcmp[0];
                    [s(0), s(2), s(4)]
                };
                m.positions.push(to_bevy(p));
                if let Some(uo) = l.uv0 {
                    let r = read_pair(v, uo);
                    m.uvs.push([r[0] * ucmp[1] + ucmp[0], r[1] * ucmp[1] + ucmp[0]]);
                }
                if let Some(uo) = l.uv1 {
                    let r = read_pair(v, uo);
                    m.uvs1.push([r[0] * ucmp[1] + ucmp[0], r[1] * ucmp[1] + ucmp[0]]);
                }
                let n = l.normal.map(|no| normalize(to_bevy(unorm_vec(&v[no..no + 4]))));
                if let Some(n) = n {
                    m.normals.push(n);
                }
                if let Some(co) = l.color {
                    let c = &v[co..co + 4];
                    m.colors.push([c[2] as f32 / 255.0, c[1] as f32 / 255.0, c[0] as f32 / 255.0, c[3] as f32 / 255.0]);
                }
                if let (Some(to), Some(n)) = (l.tangent, n) {
                    let t = normalize(to_bevy(unorm_vec(&v[to..to + 4])));
                    let w = match l.binormal {
                        Some(bo) => {
                            let b = to_bevy(unorm_vec(&v[bo..bo + 4]));
                            if dot(cross(n, t), b) < 0.0 {
                                -1.0
                            } else {
                                1.0
                            }
                        }
                        None => 1.0,
                    };
                    m.tangents.push([t[0], t[1], t[2], w]);
                }
                if let Some(so) = l.skin {
                    let s = &v[so..so + 8];
                    let w = [s[0] as f32, s[1] as f32, s[2] as f32, s[3] as f32];
                    let sum = (w[0] + w[1] + w[2] + w[3]).max(1.0);
                    m.weights.push([w[0] / sum, w[1] / sum, w[2] / sum, w[3] / sum]);
                    m.joints.push([s[4] as u16, s[5] as u16, s[6] as u16, s[7] as u16]);
                }
            }
        }
        let total = lod.indices.len();
        for (pi, p) in lod.primitives.iter().enumerate() {
            let start = p.index_start.max(0) as usize;
            let end = lod.primitives.get(pi + 1).map(|n| n.index_start as usize).unwrap_or(total).min(total);
            if end <= start {
                continue;
            }
            let base = buffer_base.get(p.buffer_index.max(0) as usize).copied().unwrap_or(0);
            let istart = m.indices.len();
            let vcount = m.positions.len() as u32;
            for tri in lod.indices[start..end].chunks_exact(3) {
                let a = tri[0] as u32 + base;
                let b = tri[1] as u32 + base;
                let c = tri[2] as u32 + base;
                if a >= vcount || b >= vcount || c >= vcount || a == b || b == c || a == c {
                    continue;
                }
                // The game draws clockwise triangles as front faces (D3D's
                // default); Bevy's front faces are counter-clockwise. The axis
                // change is a proper rotation, so only the order flips.
                m.indices.extend_from_slice(&[a, c, b]);
            }
            m.submeshes.push(SubMesh {
                primitive: pi,
                material: p.material_index.max(0) as usize,
                part: p.skeleton_index,
                index_start: istart,
                index_count: m.indices.len() - istart,
            });
        }
        out_lods.push((lod.distance, m));
    }
    Ok(Model { materials, lods: out_lods, bbox_min: bmin, bbox_max: bmax, bones })
}

/// Reads only the material slot list (`RMTL`) of an XBG, without decoding geometry.
pub fn read_material_refs(data: &[u8]) -> Result<Vec<MaterialRef>> {
    let x = XbgFile::parse(data)?;
    parse_materials(&x)
}
