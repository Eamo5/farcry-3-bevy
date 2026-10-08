//! Skeletons (`.skeleton`), XBG skinning data and `.mab` skeletal animations.
//!
//! Everything returned by this module is plain data in **Bevy space** (Y-up,
//! right-handed). Dunia is Z-up right-handed; the conversion is the proper
//! rotation `C: (x, y, z) -> (x, z, -y)`:
//!
//! * positions / translations: `(x, y, z) -> (x, z, -y)`
//! * quaternions `(x, y, z, w) -> (x, z, -y, w)`
//! * matrices: `M' = C * M * C^-1`
//!
//! Because `C` is a rotation, local transforms converted this way still compose
//! (`W' = C W C^-1`) and skinning still works (`C (W B^-1) C^-1 (C v) = C (W B^-1 v)`).
//! (Mesh triangle order is flipped separately in `mesh.rs`: the game's front
//! faces are clockwise, Bevy's counter-clockwise.) Matrices are column-major `[f32; 16]`
//! (the layout of `glam::Mat4::from_cols_array`), acting on column vectors.
//!
//! See `docs/animation.md` for the byte-level format notes.

use anyhow::{bail, Context, Result};

use crate::hash::crc32;
use crate::reader::Reader;
use crate::xbg::{parse_lods, Chunk, XbgFile};

// ===========================================================================
// Small math kit (no external deps)
// ===========================================================================

/// Quaternion `[x, y, z, w]`.
pub type Quat = [f32; 4];
/// Vector `[x, y, z]`.
pub type Vec3 = [f32; 3];
/// Column-major 4x4 matrix (glam `Mat4::from_cols_array` layout).
pub type Mat4 = [f32; 16];

pub const QUAT_IDENTITY: Quat = [0.0, 0.0, 0.0, 1.0];
pub const MAT4_IDENTITY: Mat4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

/// Dunia Z-up vector -> Bevy Y-up vector.
#[inline]
pub fn vec_to_bevy(v: Vec3) -> Vec3 {
    [v[0], v[2], -v[1]]
}

/// Dunia Z-up quaternion -> Bevy Y-up quaternion.
#[inline]
pub fn quat_to_bevy(q: Quat) -> Quat {
    [q[0], q[2], -q[1], q[3]]
}

/// Dunia Z-up matrix (column-major, column vectors) -> Bevy Y-up matrix: `C * M * C^T`.
pub fn mat_to_bevy(m: &Mat4) -> Mat4 {
    // Row/column permutation: index map new<-old for axes: x<-x, y<-z, z<--y.
    let src = |c: usize, r: usize| m[c * 4 + r];
    // C as a function on axis indices with sign.
    const AX: [(usize, f32); 4] = [(0, 1.0), (2, 1.0), (1, -1.0), (3, 1.0)];
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let (sc, fc) = AX[c];
            let (sr, fr) = AX[r];
            out[c * 4 + r] = src(sc, sr) * fc * fr;
        }
    }
    out
}

pub fn quat_mul(a: Quat, b: Quat) -> Quat {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

pub fn quat_conj(q: Quat) -> Quat {
    [-q[0], -q[1], -q[2], q[3]]
}

pub fn quat_normalize(q: Quat) -> Quat {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if l > 1e-12 {
        [q[0] / l, q[1] / l, q[2] / l, q[3] / l]
    } else {
        QUAT_IDENTITY
    }
}

/// Shortest-path spherical interpolation.
pub fn quat_slerp(a: Quat, b: Quat, t: f32) -> Quat {
    let mut d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let mut b = b;
    if d < 0.0 {
        b = [-b[0], -b[1], -b[2], -b[3]];
        d = -d;
    }
    if d > 0.9995 {
        return quat_normalize([
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ]);
    }
    let th = d.min(1.0).acos();
    let s = th.sin();
    let wa = ((1.0 - t) * th).sin() / s;
    let wb = (t * th).sin() / s;
    [a[0] * wa + b[0] * wb, a[1] * wa + b[1] * wb, a[2] * wa + b[2] * wb, a[3] * wa + b[3] * wb]
}

pub fn quat_rotate(q: Quat, v: Vec3) -> Vec3 {
    let r = quat_mul(quat_mul(q, [v[0], v[1], v[2], 0.0]), quat_conj(q));
    [r[0], r[1], r[2]]
}

/// Rotation part of an (unscaled or uniformly scaled) matrix as a quaternion.
pub fn mat_rotation(m: &Mat4) -> Quat {
    let mut c = [[0.0f32; 3]; 3];
    for (i, col) in c.iter_mut().enumerate() {
        let l = (m[i * 4] * m[i * 4] + m[i * 4 + 1] * m[i * 4 + 1] + m[i * 4 + 2] * m[i * 4 + 2]).sqrt().max(1e-12);
        *col = [m[i * 4] / l, m[i * 4 + 1] / l, m[i * 4 + 2] / l];
    }
    // c[col][row]
    let (m00, m11, m22) = (c[0][0], c[1][1], c[2][2]);
    let tr = m00 + m11 + m22;
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [(c[1][2] - c[2][1]) / s, (c[2][0] - c[0][2]) / s, (c[0][1] - c[1][0]) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (c[1][0] + c[0][1]) / s, (c[2][0] + c[0][2]) / s, (c[1][2] - c[2][1]) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(c[1][0] + c[0][1]) / s, 0.25 * s, (c[2][1] + c[1][2]) / s, (c[2][0] - c[0][2]) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(c[2][0] + c[0][2]) / s, (c[2][1] + c[1][2]) / s, 0.25 * s, (c[0][1] - c[1][0]) / s]
    };
    quat_normalize(q)
}

/// Translation * Rotation * Scale.
pub fn mat_from_trs(t: Vec3, q: Quat, s: Vec3) -> Mat4 {
    let [x, y, z, w] = quat_normalize(q);
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    [
        (1.0 - (yy + zz)) * s[0],
        (xy + wz) * s[0],
        (xz - wy) * s[0],
        0.0,
        (xy - wz) * s[1],
        (1.0 - (xx + zz)) * s[1],
        (yz + wx) * s[1],
        0.0,
        (xz + wy) * s[2],
        (yz - wx) * s[2],
        (1.0 - (xx + yy)) * s[2],
        0.0,
        t[0],
        t[1],
        t[2],
        1.0,
    ]
}

pub fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut o = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + r] * b[c * 4 + k];
            }
            o[c * 4 + r] = s;
        }
    }
    o
}

pub fn mat_transform_point(m: &Mat4, p: Vec3) -> Vec3 {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

/// Inverse of an affine matrix (last row 0,0,0,1).
pub fn mat_inverse_affine(m: &Mat4) -> Mat4 {
    let a = [[m[0], m[4], m[8]], [m[1], m[5], m[9]], [m[2], m[6], m[10]]]; // a[row][col]
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-20 {
        return MAT4_IDENTITY;
    }
    let id = 1.0 / det;
    let inv = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * id,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * id,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * id,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * id,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * id,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * id,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * id,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * id,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * id,
        ],
    ];
    let t = [m[12], m[13], m[14]];
    let nt = [
        -(inv[0][0] * t[0] + inv[0][1] * t[1] + inv[0][2] * t[2]),
        -(inv[1][0] * t[0] + inv[1][1] * t[1] + inv[1][2] * t[2]),
        -(inv[2][0] * t[0] + inv[2][1] * t[1] + inv[2][2] * t[2]),
    ];
    [
        inv[0][0], inv[1][0], inv[2][0], 0.0, inv[0][1], inv[1][1], inv[2][1], 0.0, inv[0][2], inv[1][2], inv[2][2], 0.0,
        nt[0], nt[1], nt[2], 1.0,
    ]
}

// ===========================================================================
// Packed values used by .mab
// ===========================================================================

const QSCALE: f32 = 4.315_969e-5; // sqrt(2) / 32767
const QBIAS: f32 = 0.707_106_8;
/// Fixed-point scale of 16-bit translations (metres per unit).
pub const TRANSLATION_UNIT: f32 = 1.0 / 16384.0;

/// Decodes a 6-byte "smallest three" quaternion (Dunia Z-up space, `[x,y,z,w]`).
///
/// Words 0/1 are unsigned with the top bits selecting where the implicit
/// (largest) component goes; word 2 is signed.
pub fn unpack_quat(b: &[u8]) -> Quat {
    let w0 = u16::from_le_bytes([b[0], b[1]]);
    let w1 = u16::from_le_bytes([b[2], b[3]]);
    let w2 = i16::from_le_bytes([b[4], b[5]]);
    let f1 = (w0 & 0x7fff) as f32 * QSCALE - QBIAS;
    let f2 = (w1 & 0x7fff) as f32 * QSCALE - QBIAS;
    let f3 = w2 as f32 * QSCALE - QBIAS;
    let f4 = (1.0 - f1 * f1 - f2 * f2 - f3 * f3).max(0.0).sqrt();
    let q = match (w0 & 0x8000 != 0, w1 & 0x8000 != 0) {
        (true, true) => [f1, f2, f3, f4],
        (false, true) => [f1, f2, f4, f3],
        (true, false) => [f1, f4, f2, f3],
        (false, false) => [f4, f1, f2, f3],
    };
    quat_normalize(q)
}

fn unpack_trans(b: &[u8], fixed: bool) -> Vec3 {
    if fixed {
        let s = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]) as f32 * TRANSLATION_UNIT;
        [s(0), s(2), s(4)]
    } else {
        let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        [f(0), f(4), f(8)]
    }
}

// ===========================================================================
// XBG nodes (NODE chunk)
// ===========================================================================

/// One XBG `NODE` record (bone / dummy). Transforms are local to the parent, Bevy space.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    /// CRC-32 of `name` (as stored in the file).
    pub name_hash: u32,
    pub parent: i32,
    pub first_child: i32,
    pub next_sibling: i32,
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    /// Index into the O2BM inverse-bind table, -1 if this node has none.
    pub bind_index: i32,
}

fn node_record_len(name_len: usize) -> usize {
    72 + name_len + 1
}

/// Parses the XBG `NODE` chunk.
///
/// Record layout (little endian): `u32 name_crc, i32 first_child, i32 next_sibling,
/// i32 parent, f32 rot[4] (x,y,z,w), f32 pos[3], f32 scale[3], i32 bind_index,
/// f32 (1.0), u32 (0x00909000), u32 name_len, name, NUL`.
pub fn parse_nodes(xbg: &[u8]) -> Result<Vec<Node>> {
    let x = XbgFile::parse(xbg)?;
    match x.root.find("NODE") {
        Some(c) => parse_node_chunk(x.chunk_data(c)),
        None => Ok(vec![]),
    }
}

fn parse_node_chunk(d: &[u8]) -> Result<Vec<Node>> {
    let mut r = Reader::new(d);
    let n = r.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let start = r.pos;
        let name_hash = r.u32()?;
        let first_child = r.i32()?;
        let next_sibling = r.i32()?;
        let parent = r.i32()?;
        let rot = r.vec4()?;
        let pos = r.vec3()?;
        let scale = r.vec3()?;
        let bind_index = r.i32()?;
        let _one = r.f32()?;
        let _tag = r.u32()?;
        let name = r.lstring()?;
        debug_assert_eq!(r.pos - start, node_record_len(name.len()));
        out.push(Node {
            name,
            name_hash,
            parent,
            first_child,
            next_sibling,
            translation: vec_to_bevy(pos),
            rotation: quat_to_bevy(quat_normalize(rot)),
            scale: [scale[0], scale[2], scale[1]],
            bind_index,
        });
    }
    // Parents always precede children; a few files have a self-parented root.
    for (i, nd) in out.iter_mut().enumerate() {
        if nd.parent >= i as i32 {
            nd.parent = -1;
        }
    }
    Ok(out)
}

/// Local bind matrix of a node (Bevy space).
pub fn node_local(n: &Node) -> Mat4 {
    mat_from_trs(n.translation, n.rotation, n.scale)
}

/// World (model-space) bind matrices of all nodes.
pub fn bind_worlds(nodes: &[Node]) -> Vec<Mat4> {
    let mut w: Vec<Mat4> = Vec::with_capacity(nodes.len());
    for n in nodes {
        let l = node_local(n);
        let m = if n.parent >= 0 { mat_mul(&w[n.parent as usize], &l) } else { l };
        w.push(m);
    }
    w
}

// ===========================================================================
// Skinning (O2BM / SKND / CLUS / SKID)
// ===========================================================================

/// One XBG primitive (draw call) of a LOD, with its skinning binding.
#[derive(Debug, Clone)]
pub struct SkinPrimitive {
    /// Vertex buffer index within the LOD.
    pub buffer: u32,
    /// SKND part index (`Primitive::skeleton_index` in xbg.rs, `SubMesh::part` in mesh.rs).
    pub part: u32,
    /// Cluster index within the part (this is what xbg.rs calls `material_index`!).
    pub cluster: u32,
    /// The real material index into RMTL (from the cluster header).
    pub material: u32,
    /// First index / index count in the LOD's index buffer (as in the file).
    pub index_start: u32,
    pub index_count: u32,
    /// Vertex range in LOD-global vertex numbering (buffers concatenated in order,
    /// i.e. the same numbering mesh.rs uses).
    pub vertex_start: u32,
    pub vertex_count: u32,
    /// `Some(node)` if the primitive is rigidly bound to one node (weapons, props).
    pub rigid_node: Option<u16>,
    /// Bone palette: palette-local index (what the vertex stores) -> NODE index,
    /// trimmed to the entries this primitive's vertices reference. Empty if rigid.
    pub palette: Vec<u16>,
}

#[derive(Debug, Clone, Default)]
pub struct SkinLod {
    pub primitives: Vec<SkinPrimitive>,
    /// Per-vertex NODE indices (already remapped through the palette), LOD-global numbering.
    pub joints: Vec<[u16; 4]>,
    /// Per-vertex weights (sum to 1).
    pub weights: Vec<[f32; 4]>,
}

#[derive(Debug, Clone)]
pub struct SkinInfo {
    pub nodes: Vec<Node>,
    /// Per-NODE inverse bind matrix (Bevy space). Taken from O2BM when the node has
    /// an entry there, otherwise the inverse of the NODE hierarchy bind pose.
    pub inverse_bind: Vec<Mat4>,
    /// Per-NODE model-space bind matrix computed from the NODE hierarchy.
    pub bind_world: Vec<Mat4>,
    pub lods: Vec<SkinLod>,
}

struct Cluster {
    material: u16,
    index_count: u16,
    stride: u16,
    vertex_count: u16,
    format: u16,
    palette: [u16; 48],
}

fn parse_clusters(skin_parts: usize, d: &[u8]) -> Result<Vec<Vec<Cluster>>> {
    let mut r = Reader::new(d);
    let mut parts = Vec::with_capacity(skin_parts);
    for _ in 0..skin_parts {
        let n = r.u32()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let material = r.u16()?;
            let _tris = r.u16()?;
            let _tris2 = r.u16()?;
            let index_count = r.u16()?;
            let stride = r.u16()?;
            let vertex_count = r.u16()?;
            let format = r.u16()?;
            let mut palette = [0u16; 48];
            for p in palette.iter_mut() {
                *p = r.u16()?;
            }
            r.skip(64)?; // two 16 x u16 tables (vertex / index counts); unused
            v.push(Cluster { material, index_count, stride, vertex_count, format, palette });
        }
        parts.push(v);
    }
    Ok(parts)
}

fn skin_part_count(d: &[u8]) -> Result<usize> {
    // SKND: u32 count, then per part: f32[11], u32 index, u32 0, lstring name.
    let mut r = Reader::new(d);
    let n = r.u32()? as usize;
    for _ in 0..n {
        r.skip(44 + 8)?;
        let _name = r.lstring()?;
    }
    Ok(n)
}

fn parse_skid(d: &[u8]) -> Result<Vec<(u16, u16)>> {
    let mut r = Reader::new(d);
    let n = r.u32()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let _hash = r.u32()?;
        let part = r.u16()?;
        let node = r.u16()?;
        let _z = r.u32()?;
        v.push((part, node));
    }
    Ok(v)
}

/// Byte offset of the 8-byte skin element (4 weight bytes + 4 palette indices) in
/// a vertex of the given format, or None if the format has no skin.
///
/// Element order: position (0x1: 3xf32, else 0x2/0x4: 4xi16), 0x8 uv0, 0x800 uv1,
/// 0x1000 extra 4 bytes, 0x2000 extra 4 bytes, 0x10 skin, then normal/colour/
/// tangent/binormal and trailing padding up to the stride.
pub fn skin_offset(format: u32) -> Option<usize> {
    if format & 0x10 == 0 {
        return None;
    }
    let mut o = if format & 1 != 0 { 12 } else if format & 6 != 0 { 8 } else { 0 };
    for f in [0x8, 0x800, 0x1000, 0x2000] {
        if format & f != 0 {
            o += 4;
        }
    }
    Some(o)
}

fn read_o2bm(d: &[u8]) -> Result<Vec<Mat4>> {
    let mut r = Reader::new(d);
    let n = r.u32()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        // Stored row-major for row vectors (translation in the last row): that is
        // exactly a column-major matrix for column vectors.
        let mut m = [0.0f32; 16];
        for x in m.iter_mut() {
            *x = r.f32()?;
        }
        v.push(mat_to_bevy(&m));
    }
    Ok(v)
}

fn chunk<'a>(x: &XbgFile<'a>, kind: &str) -> Option<(&'a [u8], Chunk)> {
    x.root.find(kind).map(|c| (x.chunk_data(c), c.clone()))
}

/// Decodes the skinning data of an XBG: NODE hierarchy, inverse binds, and per LOD
/// the primitive -> palette binding plus per-vertex NODE joints/weights.
pub fn skin_info(xbg: &[u8]) -> Result<SkinInfo> {
    skin_info_lods(xbg, usize::MAX)
}

/// Like [`skin_info`] but only decodes the first `max_lods` LODs.
pub fn skin_info_lods(xbg: &[u8], max_lods: usize) -> Result<SkinInfo> {
    let x = XbgFile::parse(xbg)?;
    let nodes = match chunk(&x, "NODE") {
        Some((d, _)) => parse_node_chunk(d).context("NODE")?,
        None => vec![],
    };
    let bind_world = bind_worlds(&nodes);
    let o2bm = match chunk(&x, "O2BM") {
        Some((d, _)) => read_o2bm(d).context("O2BM")?,
        None => vec![],
    };
    let inverse_bind: Vec<Mat4> = nodes
        .iter()
        .zip(&bind_world)
        .map(|(n, w)| {
            if n.bind_index >= 0 && (n.bind_index as usize) < o2bm.len() {
                o2bm[n.bind_index as usize]
            } else {
                mat_inverse_affine(w)
            }
        })
        .collect();
    let parts = match chunk(&x, "SKND") {
        Some((d, _)) => skin_part_count(d).context("SKND")?,
        None => 0,
    };
    let clusters = match chunk(&x, "CLUS") {
        Some((d, _)) => parse_clusters(parts, d).context("CLUS")?,
        None => vec![],
    };
    let skid = match chunk(&x, "SKID") {
        Some((d, _)) => parse_skid(d).context("SKID")?,
        None => vec![],
    };
    let rigid_of_part = |part: u32| -> Option<u16> {
        skid.iter().find(|(p, _)| *p as u32 == part).map(|(_, n)| *n).filter(|n| *n != 0xffff && (*n as usize) < nodes.len())
    };

    let lods_chunk = x.root.find("LODS").context("XBG has no LODS")?;
    let lods = parse_lods(xbg, lods_chunk)?;
    let mut out_lods = Vec::new();
    for lod in lods.iter().take(max_lods) {
        let vdata = &xbg[lod.vertex_data.clone()];
        let mut base = Vec::with_capacity(lod.buffers.len());
        let mut total = 0u32;
        for b in &lod.buffers {
            base.push(total);
            total += b.count;
        }
        let mut sl = SkinLod {
            primitives: Vec::new(),
            joints: vec![[0; 4]; total as usize],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; total as usize],
        };
        let mut used: Vec<Vec<bool>> = clusters.iter().map(|c| vec![false; c.len()]).collect();
        let nidx = lod.indices.len() as u32;
        for (pi, p) in lod.primitives.iter().enumerate() {
            let bi = p.buffer_index.max(0) as usize;
            let Some(buf) = lod.buffers.get(bi) else { continue };
            let index_start = p.index_start.max(0) as u32;
            let index_end = lod.primitives.get(pi + 1).map(|n| n.index_start.max(0) as u32).unwrap_or(nidx).min(nidx);
            let index_count = index_end.saturating_sub(index_start);
            let stride = buf.stride.max(1);
            let first = (p.unk5.saturating_sub(buf.offset)) / stride;
            let last = (p.unk4).min(buf.count.saturating_sub(1));
            let vcount = if last >= first { last - first + 1 } else { 0 };
            let part = p.skeleton_index.max(0) as u32;

            // Cluster: the primitive's third field is the cluster index; verify by
            // counts, otherwise search for a matching unused cluster.
            let pc = clusters.get(part as usize);
            let matches = |c: &Cluster| {
                c.index_count as u32 == index_count
                    && c.vertex_count as u32 == vcount
                    && c.format as u32 == buf.format
                    && c.stride as u32 == buf.stride
            };
            let mut ci = p.material_index.max(0) as usize;
            if let Some(pc) = pc {
                if pc.get(ci).map(|c| !matches(c)).unwrap_or(true) {
                    if let Some(k) = (0..pc.len()).find(|&k| !used[part as usize][k] && matches(&pc[k])) {
                        ci = k;
                    }
                }
                if ci < pc.len() {
                    used[part as usize][ci] = true;
                }
            }
            let cl = pc.and_then(|pc| pc.get(ci));
            let material = cl.map(|c| c.material as u32).unwrap_or(p.material_index.max(0) as u32);
            let palette: Vec<u16> = cl.map(|c| c.palette.to_vec()).unwrap_or_default();
            let skin_off = skin_offset(buf.format);
            let palette_ok = !palette.is_empty() && (palette[0] as usize) < nodes.len();
            let rigid = if skin_off.is_some() && palette_ok { None } else { Some(rigid_of_part(part).unwrap_or(0)) };

            let gbase = base[bi] + first;
            let mut max_local = 0usize;
            for v in 0..vcount {
                let gv = (gbase + v) as usize;
                if gv >= sl.joints.len() {
                    break;
                }
                match (rigid, skin_off) {
                    (None, Some(so)) => {
                        let o = buf.offset as usize + (first + v) as usize * buf.stride as usize + so;
                        let s = &vdata[o..o + 8];
                        let sum: u32 = s[..4].iter().map(|&w| w as u32).sum();
                        let mut j = [0u16; 4];
                        let mut w = [0f32; 4];
                        for k in 0..4 {
                            let local = s[4 + k] as usize;
                            let node = palette.get(local).copied().filter(|&n| (n as usize) < nodes.len()).unwrap_or(palette[0]);
                            if s[k] > 0 {
                                max_local = max_local.max(local);
                                j[k] = node;
                                w[k] = s[k] as f32 / sum.max(1) as f32;
                            } else {
                                j[k] = palette[0];
                            }
                        }
                        if sum == 0 {
                            w = [1.0, 0.0, 0.0, 0.0];
                        }
                        sl.joints[gv] = j;
                        sl.weights[gv] = w;
                    }
                    _ => {
                        let n = rigid.unwrap_or(0);
                        sl.joints[gv] = [n, n, n, n];
                        sl.weights[gv] = [1.0, 0.0, 0.0, 0.0];
                    }
                }
            }
            // Palette slots past the highest referenced index are padding.
            let used_len = if rigid.is_some() { 0 } else { (max_local + 1).min(palette.len()) };
            sl.primitives.push(SkinPrimitive {
                buffer: bi as u32,
                part,
                cluster: ci as u32,
                material,
                index_start,
                index_count,
                vertex_start: gbase,
                vertex_count: vcount,
                rigid_node: rigid,
                palette: palette[..used_len.min(palette.len())].to_vec(),
            });
        }
        out_lods.push(sl);
    }
    Ok(SkinInfo { nodes, inverse_bind, bind_world, lods: out_lods })
}

// ===========================================================================
// .skeleton (LKS)
// ===========================================================================

/// How a procedural ("helper") bone is driven by other bones.
#[derive(Debug, Clone, PartialEq)]
pub enum Procedural {
    /// Ordinary bone.
    None,
    /// Rotation is a weighted blend of the world-space rotation deltas of the
    /// drivers (knees / elbows). `(bone index, weight)`.
    Blend(Vec<(u32, f32)>),
    /// Takes `weight` of the driver's twist about the parent's X (bone) axis
    /// (fore-arm / upper-arm twist bones).
    Twist { driver: u32, weight: f32 },
}

#[derive(Debug, Clone)]
pub struct SkeletonBone {
    pub name: String,
    pub name_hash: u32,
    pub parent: i32,
    /// Local rest transform, Bevy space.
    pub translation: Vec3,
    pub rotation: Quat,
    /// Distance to the parent (informational).
    pub length: f32,
    pub procedural: Procedural,
}

/// An attachment point ("socket") defined relative to a bone.
#[derive(Debug, Clone)]
pub struct Socket {
    pub name: String,
    pub name_hash: u32,
    pub bone: String,
    pub bone_hash: u32,
    /// Offset relative to the bone, Bevy space.
    pub translation: Vec3,
    pub rotation: Quat,
}

#[derive(Debug, Clone)]
pub struct Skeleton {
    pub bones: Vec<SkeletonBone>,
    pub sockets: Vec<Socket>,
}

impl Skeleton {
    pub fn find(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }
    pub fn find_hash(&self, hash: u32) -> Option<usize> {
        self.bones.iter().position(|b| b.name_hash == hash)
    }
}

const LKS_TAG: u32 = 0x3ADE_68B1;

/// Parses an `LKS` `.skeleton` file.
///
/// Layout: `"LKS\0"`, `u32 version (27)`, then tagged records `u32 0x3ADE68B1, u32 type`:
/// type 10 = header (`u16 bone_count, u16`), type 9 = bone (`f32 rot[4], f32 pos[3],
/// f32 length, u16 parent (0xFFFF = root), u8 procedural_type, drivers, u8 0,
/// u32 crc, u32 len, name`), followed by an untagged bone-group table and type 3
/// socket records (`u16 id, crc+len+name, crc+len+bone name, f32[18]`).
pub fn parse_skeleton(data: &[u8]) -> Result<Skeleton> {
    if data.len() < 8 || &data[..4] != b"LKS\0" {
        bail!("not an LKS skeleton");
    }
    let mut r = Reader::at(data, 8);
    let mut bone_count = None;
    let mut bones = Vec::new();
    while bone_count.map(|n| bones.len() < n).unwrap_or(true) {
        let tag = r.u32()?;
        let ty = r.u32()?;
        if tag != LKS_TAG {
            bail!("LKS: bad record tag {tag:#x} at {:#x}", r.pos - 8);
        }
        match ty {
            10 => {
                bone_count = Some(r.u16()? as usize);
                let _ = r.u16()?;
            }
            9 => {
                let rot = r.vec4()?;
                let pos = r.vec3()?;
                let length = r.f32()?;
                let parent = r.u16()?;
                let ptype = r.u8()?;
                let ndrv = match ptype {
                    0 => 0,
                    2 => 2,
                    3 => 1,
                    t => bail!("LKS: unknown procedural type {t} at {:#x}", r.pos),
                };
                let mut drivers = Vec::with_capacity(ndrv);
                for _ in 0..ndrv {
                    drivers.push((r.u32()?, r.f32()?));
                }
                let _flag = r.u8()?;
                let name_hash = r.u32()?;
                let len = r.u32()? as usize;
                let name = String::from_utf8_lossy(r.bytes(len)?).into_owned();
                let procedural = match ptype {
                    2 => Procedural::Blend(drivers),
                    3 => Procedural::Twist { driver: drivers[0].0, weight: drivers[0].1 },
                    _ => Procedural::None,
                };
                bones.push(SkeletonBone {
                    name,
                    name_hash,
                    parent: if parent == 0xffff { -1 } else { parent as i32 },
                    translation: vec_to_bevy(pos),
                    rotation: quat_to_bevy(quat_normalize(rot)),
                    length,
                    procedural,
                });
            }
            t => bail!("LKS: unexpected record type {t} before all bones were read"),
        }
    }
    // Sockets: scan for tagged type-3 records after the bone-group table.
    let mut sockets = Vec::new();
    let mut p = r.pos;
    while p + 8 <= data.len() {
        let tag = u32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        let ty = u32::from_le_bytes(data[p + 4..p + 8].try_into().unwrap());
        if tag != LKS_TAG || ty != 3 {
            p += 1;
            continue;
        }
        let mut s = Reader::at(data, p + 8);
        let parsed: Result<Socket> = (|| {
            let _id = s.u16()?;
            let name_hash = s.u32()?;
            let l1 = s.u32()? as usize;
            let name = String::from_utf8_lossy(s.bytes(l1)?).into_owned();
            let bone_hash = s.u32()?;
            let l2 = s.u32()? as usize;
            let bone = String::from_utf8_lossy(s.bytes(l2)?).into_owned();
            let rot = s.vec4()?;
            let pos = s.vec3()?;
            s.skip(4 * 11)?; // inverse transform + rotation copy
            Ok(Socket {
                name,
                name_hash,
                bone,
                bone_hash,
                translation: vec_to_bevy(pos),
                rotation: quat_to_bevy(quat_normalize(rot)),
            })
        })();
        match parsed {
            Ok(sock) => {
                sockets.push(sock);
                p = s.pos;
            }
            Err(_) => p += 1,
        }
    }
    Ok(Skeleton { bones, sockets })
}

// ===========================================================================
// .mab clips
// ===========================================================================

/// Skeleton id (first u32 of a clip block) used by all human body clips. Bone
/// indices refer to `graphics\actors_\_skeleton_master\hips_ref.skeleton`
/// (and the identical first 58 bones of every human `*_hips_ref.skeleton`).
pub const SKELETON_HUMAN: u32 = 0xEB63_516B;
/// One animated bone. Keys are absolute local transforms (relative to the parent
/// bone) in Bevy space; times in seconds. Linear interpolation (slerp for
/// rotations) between keys reproduces the data; the keys are sparse (the encoder
/// dropped redundant frames).
#[derive(Debug, Clone, Default)]
pub struct Track {
    /// Bone index in the clip's skeleton (`.skeleton` bone order).
    pub bone: u16,
    /// CRC-32 of the bone name; 0 until [`Clip::resolve`] is called.
    pub bone_hash: u32,
    pub rotation_times: Vec<f32>,
    pub rotations: Vec<Quat>,
    pub translation_times: Vec<f32>,
    pub translations: Vec<Vec3>,
    /// MAB clips carry no scale; always empty (kept for API symmetry).
    pub scale_times: Vec<f32>,
    pub scales: Vec<Vec3>,
}

/// Motion of the character root ("reference" node, parent of the skeleton root).
/// Converted to be relative to the clip's first key (identity at t = 0), so it can
/// be applied directly as a displacement of the entity: `entity * root(t)`.
#[derive(Debug, Clone, Default)]
pub struct RootMotion {
    pub times: Vec<f32>,
    pub translations: Vec<Vec3>,
    pub rotations: Vec<Quat>,
    /// The first key itself (Bevy space), which the keys above are relative
    /// to: where the character starts in the scene the clip was authored in
    /// (paired clips share that scene).
    pub start_translation: Vec3,
    pub start_rotation: Quat,
}

/// An attachment event in the clip (e.g. "object `ak74m` attached to `RightHand`",
/// or left-hand IK onto the weapon's `IK_LeftHand` bone).
#[derive(Debug, Clone)]
pub struct ClipAttachment {
    pub kind: u16,
    pub object: String,
    pub bone: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Clip {
    /// Skeleton id the bone indices refer to (see [`SKELETON_HUMAN`]; other ids are
    /// opaque per-skeleton hashes).
    pub skeleton_id: u32,
    /// Format flags (the byte after "AnD"): bit0 = 16-bit bone translations,
    /// bit1 = 16-bit root translations, 5 = sparse keyed root motion.
    pub flags: u8,
    pub duration: f32,
    /// Sample rate of the rotation key grid (frames per second).
    pub frame_rate: f32,
    pub tracks: Vec<Track>,
    pub root_motion: Option<RootMotion>,
    /// Reference rotation/translation from the block header (for sub-clips this is
    /// the attachment offset at the first frame), Bevy space.
    pub reference_rotation: Quat,
    pub reference_translation: Vec3,
    pub attachments: Vec<ClipAttachment>,
    /// Sounds the clip plays as it runs (`PlaySound` events): `(time s, sound
    /// event id)`, in time order. Reloads, draws, takedowns...
    pub sounds: Vec<(f32, u32)>,
    /// Nested clips for attached objects (weapons, magazines, props), in file order.
    pub sub_clips: Vec<Clip>,
}

impl Clip {
    /// Fills `bone_hash` of every track from the skeleton the clip targets.
    pub fn resolve(&mut self, skel: &Skeleton) {
        for t in &mut self.tracks {
            t.bone_hash = skel.bones.get(t.bone as usize).map(|b| b.name_hash).unwrap_or(0);
        }
    }

    pub fn track_for_bone(&self, bone: u16) -> Option<&Track> {
        self.tracks.iter().find(|t| t.bone == bone)
    }

    /// True for additive (delta) clips: every rotation is stored relative to the
    /// pose it will be layered on (identity = no change) and translations are
    /// offsets. Detected from the data: the skeleton root has a near-identity
    /// rotation and ~zero translation, which no absolute pose has (absolute human
    /// poses store the hips about 0.8-1.0 m above the root, rotated 90 degrees).
    pub fn is_additive(&self) -> bool {
        let Some(root) = self.tracks.iter().min_by_key(|t| t.bone) else { return false };
        let near_identity = |q: &Quat| q[3].abs() > 0.996; // < ~10 degrees
        let rot_ok = root.rotations.iter().all(near_identity);
        let tr_ok = root.translations.iter().all(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() < 0.05);
        let mostly_identity =
            self.tracks.iter().filter(|t| t.rotations.first().is_some_and(near_identity)).count() * 10 >= self.tracks.len() * 7;
        rot_ok && tr_ok && mostly_identity
    }

    /// The nested clip that animates the most bones. In first/third-person weapon
    /// clips this is the held weapon (its bone 0 is the weapon root relative to the
    /// `RightHand` bone; other sub-clips are e.g. a spare magazine in the left hand).
    /// Sub-clip skeleton ids are opaque per-skeleton hashes (AK74M `C31BACD8`,
    /// Kimber `DCC90202`, ...); bone indices follow that weapon's `*_ref.skeleton`.
    pub fn main_prop_clip(&self) -> Option<&Clip> {
        self.sub_clips.iter().filter(|c| c.skeleton_id != SKELETON_HUMAN).max_by_key(|c| c.tracks.len())
    }
}

/// Samples a sparse key list at `t` (clamped), with the given lerp.
fn sample_keys<T: Copy>(times: &[f32], vals: &[T], t: f32, lerp: impl Fn(T, T, f32) -> T) -> Option<T> {
    if vals.is_empty() {
        return None;
    }
    if t <= times[0] || vals.len() == 1 {
        return Some(vals[0]);
    }
    let i = times.partition_point(|&x| x <= t);
    if i >= times.len() {
        return Some(vals[vals.len() - 1]);
    }
    let (t0, t1) = (times[i - 1], times[i]);
    let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
    Some(lerp(vals[i - 1], vals[i], f))
}

impl Track {
    pub fn sample_rotation(&self, t: f32) -> Option<Quat> {
        sample_keys(&self.rotation_times, &self.rotations, t, quat_slerp)
    }
    pub fn sample_translation(&self, t: f32) -> Option<Vec3> {
        sample_keys(&self.translation_times, &self.translations, t, |a, b, f| {
            [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
        })
    }
}

impl RootMotion {
    pub fn sample(&self, t: f32) -> (Vec3, Quat) {
        let tr = sample_keys(&self.times, &self.translations, t, |a, b, f| {
            [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
        })
        .unwrap_or([0.0; 3]);
        let r = sample_keys(&self.times, &self.rotations, t, quat_slerp).unwrap_or(QUAT_IDENTITY);
        (tr, r)
    }
}

fn mask_bits(d: &[u8]) -> Vec<u16> {
    (0..256u16).filter(|&i| d[(i / 8) as usize] >> (i % 8) & 1 != 0).collect()
}

/// Parses a `.mab` animation file (version 0x61) including nested sub-clips.
pub fn parse_clip(data: &[u8]) -> Result<Clip> {
    if data.len() < 0x10 + 0xE0 {
        bail!("MAB too small");
    }
    let version = u32::from_le_bytes(data[0..4].try_into().unwrap());
    if version != 0x61 {
        bail!("unsupported MAB version {version:#x} (expected 0x61)");
    }
    let mut blocks = Vec::new();
    let mut base = 0x10usize;
    loop {
        let (clip, next) = parse_block(data, base).with_context(|| format!("MAB block at {base:#x}"))?;
        blocks.push(clip);
        match next {
            Some(n) if n > base && n + 0xE0 <= data.len() => base = n,
            _ => break,
        }
        if blocks.len() > 64 {
            bail!("MAB: too many nested blocks");
        }
    }
    let mut main = blocks.remove(0);
    main.sub_clips = blocks;
    Ok(main)
}

/// Like [`parse_clip`] and resolves bone hashes of the main clip against `skel`.
pub fn parse_clip_with_skeleton(data: &[u8], skel: &Skeleton) -> Result<Clip> {
    let mut c = parse_clip(data)?;
    c.resolve(skel);
    Ok(c)
}

fn parse_block(d: &[u8], b: usize) -> Result<(Clip, Option<usize>)> {
    let rd = |o: usize| -> Result<u32> {
        d.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).context("truncated MAB header")
    };
    let rf = |o: usize| -> Result<f32> { Ok(f32::from_bits(rd(o)?)) };
    let skeleton_id = rd(b)?;
    let masks: Vec<Vec<u16>> = (0..4).map(|k| mask_bits(&d[b + 4 + 32 * k..b + 36 + 32 * k])).collect();
    let ref_rot = [rf(b + 0x90)?, rf(b + 0x94)?, rf(b + 0x98)?, rf(b + 0x9C)?];
    let ref_tr = [rf(b + 0xA0)?, rf(b + 0xA4)?, rf(b + 0xA8)?];
    if &d[b + 0xB0..b + 0xB3] != b"AnD" {
        bail!("missing AnD marker");
    }
    let flags = d[b + 0xB3];
    let duration = rf(b + 0xB4)?;
    let mut secs = [0usize; 10];
    for (i, s) in secs.iter_mut().enumerate() {
        let v = rd(b + 0xB8 + 4 * i)? as i32;
        *s = if v > 0 { b + v as usize } else { 0 };
    }
    let sec = |i: usize| -> Result<usize> {
        let s = secs[i];
        if s == 0 || s + 8 > d.len() {
            bail!("section {i} missing or out of range");
        }
        Ok(s)
    };
    let fixed_bone_tr = flags & 1 != 0;
    let fixed_root_tr = flags & 2 != 0;

    let mut tracks: Vec<Track> = Vec::new();
    let track = |tracks: &mut Vec<Track>, bone: u16| -> usize {
        if let Some(i) = tracks.iter().position(|t| t.bone == bone) {
            i
        } else {
            tracks.push(Track { bone, ..Default::default() });
            tracks.len() - 1
        }
    };

    // Constant rotations.
    {
        let s = sec(2)?;
        let n = rd(s)? as usize;
        if n != masks[0].len() {
            bail!("constant rotation count {n} != mask popcount {}", masks[0].len());
        }
        for (i, &bone) in masks[0].iter().enumerate() {
            let o = s + 8 + 6 * i;
            let q = unpack_quat(d.get(o..o + 6).context("truncated constant rotations")?);
            let ti = track(&mut tracks, bone);
            tracks[ti].rotation_times = vec![0.0];
            tracks[ti].rotations = vec![quat_to_bevy(q)];
        }
    }
    // Keyed rotations.
    let frame_rate;
    {
        let s = sec(3)?;
        let nb = u16::from_le_bytes([d[s], d[s + 1]]) as usize;
        let nf = u16::from_le_bytes([d[s + 2], d[s + 3]]) as usize;
        let fps = rf(s + 4)?;
        frame_rate = fps;
        if nb > 0 {
            if nb != masks[1].len() {
                bail!("keyed rotation count {nb} != mask popcount {}", masks[1].len());
            }
            let groups = (nf >> 3) + 1;
            let mut offs = Vec::with_capacity(groups + 1);
            for g in 0..=groups {
                offs.push(rd(s + 8 + 4 * g)? as usize);
            }
            let mut keys: Vec<(Vec<f32>, Vec<Quat>)> = vec![(Vec::new(), Vec::new()); nb];
            let to_time = |f: usize| if fps > 0.0 { f as f32 / fps } else { 0.0 };
            for g in 0..groups {
                let blk = s + offs[g];
                let bm = blk + 6 * nb;
                let mut p = bm + ((nb + 1) & !1);
                if s + offs[g + 1] > d.len() || p > d.len() {
                    bail!("keyframe block {g} out of range");
                }
                for (i, k) in keys.iter_mut().enumerate() {
                    let o = blk + 6 * i;
                    k.0.push(to_time(g * 8));
                    k.1.push(unpack_quat(&d[o..o + 6]));
                }
                for (i, k) in keys.iter_mut().enumerate() {
                    let m = d[bm + i];
                    for bit in 0..7 {
                        if m >> bit & 1 != 0 {
                            let o = p;
                            p += 6;
                            let q = unpack_quat(d.get(o..o + 6).context("truncated keyframe block")?);
                            k.0.push(to_time(g * 8 + bit + 1));
                            k.1.push(q);
                        }
                    }
                }
                if p != s + offs[g + 1] {
                    bail!("keyframe block {g} size mismatch");
                }
            }
            for (i, &bone) in masks[1].iter().enumerate() {
                let (times, quats) = std::mem::take(&mut keys[i]);
                let ti = track(&mut tracks, bone);
                // Make consecutive keys take the short way round for linear blending.
                let mut rots: Vec<Quat> = Vec::with_capacity(quats.len());
                for q in quats {
                    let mut q = quat_to_bevy(q);
                    if let Some(prev) = rots.last() {
                        if prev[0] * q[0] + prev[1] * q[1] + prev[2] * q[2] + prev[3] * q[3] < 0.0 {
                            q = [-q[0], -q[1], -q[2], -q[3]];
                        }
                    }
                    rots.push(q);
                }
                tracks[ti].rotation_times = times;
                tracks[ti].rotations = rots;
            }
        }
    }
    // Constant translations.
    {
        let s = sec(4)?;
        let n = rd(s)? as usize;
        if n != masks[2].len() {
            bail!("constant translation count {n} != mask popcount {}", masks[2].len());
        }
        let sz = if fixed_bone_tr { 6 } else { 12 };
        for (i, &bone) in masks[2].iter().enumerate() {
            let o = s + 8 + sz * i;
            let v = unpack_trans(d.get(o..o + sz).context("truncated constant translations")?, fixed_bone_tr);
            let ti = track(&mut tracks, bone);
            tracks[ti].translation_times = vec![0.0];
            tracks[ti].translations = vec![vec_to_bevy(v)];
        }
    }
    // Sampled translations (frame-major: all bones of frame 0, then frame 1, ...).
    {
        let s = sec(5)?;
        let nb = u16::from_le_bytes([d[s], d[s + 1]]) as usize;
        let nf = u16::from_le_bytes([d[s + 2], d[s + 3]]) as usize;
        let fps = rf(s + 4)?;
        if nb > 0 {
            if nb != masks[3].len() {
                bail!("animated translation count {nb} != mask popcount {}", masks[3].len());
            }
            let sz = if fixed_bone_tr { 6 } else { 12 };
            for (i, &bone) in masks[3].iter().enumerate() {
                let mut times = Vec::with_capacity(nf + 1);
                let mut vals = Vec::with_capacity(nf + 1);
                for f in 0..=nf {
                    let o = s + 8 + sz * (f * nb + i);
                    let v = unpack_trans(d.get(o..o + sz).context("truncated translation samples")?, fixed_bone_tr);
                    times.push(if fps > 0.0 { f as f32 / fps } else { 0.0 });
                    vals.push(vec_to_bevy(v));
                }
                let ti = track(&mut tracks, bone);
                tracks[ti].translation_times = times;
                tracks[ti].translations = vals;
            }
        }
    }
    tracks.sort_by_key(|t| t.bone);

    // Root motion.
    let root_motion = parse_root_motion(d, &secs, flags, fixed_root_tr).ok().flatten();
    // Attachment events (best effort).
    let attachments = if secs[6] != 0 {
        let end = secs.iter().copied().filter(|&x| x > secs[6]).min().unwrap_or(d.len());
        parse_attachments(d, secs[6], end).unwrap_or_default()
    } else {
        vec![]
    };
    // Sound events, in the event sections (6 and 7) up to the next block.
    let sounds = match [secs[6], secs[7]].into_iter().filter(|&x| x != 0).min() {
        Some(start) => {
            let end = if secs[8] > start { secs[8] } else { d.len() };
            parse_sounds(d, start, end, duration)
        }
        None => vec![],
    };

    let next = if secs[8] != 0 { Some(secs[8]) } else { None };
    Ok((
        Clip {
            skeleton_id,
            flags,
            duration,
            frame_rate,
            tracks,
            root_motion,
            reference_rotation: quat_to_bevy(quat_normalize(ref_rot)),
            reference_translation: vec_to_bevy(ref_tr),
            attachments,
            sounds,
            sub_clips: vec![],
        },
        next,
    ))
}

/// `PlaySound` events in sections 6-7: each is an embedded nbCF object whose
/// first `"0x%08X"` string field is the sound event, preceded by
/// `f32 time, u32, u16, u16 object size`.
fn parse_sounds(d: &[u8], s: usize, end: usize, duration: f32) -> Vec<(f32, u32)> {
    let end = end.min(d.len());
    let mut out = Vec::new();
    let mut q = s;
    while q + 4 <= end {
        let Some(at) = d[q..end].windows(4).position(|w| w == b"nbCF") else { break };
        let o = q + at;
        q = o + 4;
        if o < s + 12 {
            continue;
        }
        let time = f32::from_le_bytes(d[o - 12..o - 8].try_into().unwrap());
        let size = u16::from_le_bytes([d[o - 2], d[o - 1]]) as usize;
        if !(time.is_finite() && (-0.01..=duration + 0.5).contains(&time)) || size < 16 || o + size > end {
            continue;
        }
        let Ok(obj) = crate::fcb::parse(&d[o..o + size]) else { continue };
        let mut is_sound = false;
        let mut event = None;
        obj.walk(&mut |c, _| {
            for (_, v) in &c.fields {
                let text = String::from_utf8_lossy(v);
                let text = text.trim_end_matches('\0');
                if text == "PlaySound" {
                    is_sound = true;
                } else if event.is_none() {
                    event = text.strip_prefix("0x").and_then(|h| u32::from_str_radix(h, 16).ok()).filter(|&e| e != u32::MAX && e != 0);
                }
            }
        });
        if let (true, Some(e)) = (is_sound, event) {
            out.push((time.max(0.0), e));
        }
        q = o + size;
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

fn parse_root_motion(d: &[u8], secs: &[usize; 10], flags: u8, fixed: bool) -> Result<Option<RootMotion>> {
    let mut times = Vec::new();
    let mut trs: Vec<Vec3> = Vec::new();
    let mut rots: Vec<Quat> = Vec::new();
    if flags == 5 {
        // Sparse keys: u32 n, f32 time[n], n x (f32[3] translation, packed quat, u16 pad)
        let s = secs[0];
        if s == 0 {
            return Ok(None);
        }
        let mut r = Reader::at(d, s);
        let n = r.u32()? as usize;
        if n > 4096 {
            bail!("implausible root key count {n}");
        }
        for _ in 0..n {
            times.push(r.f32()?);
        }
        // Keys follow the time table directly.
        for _ in 0..n {
            let t = r.vec3()?;
            let q = unpack_quat(r.bytes(6)?);
            r.skip(2)?;
            trs.push(t);
            rots.push(q);
        }
    } else {
        let (st, sr) = (secs[0], secs[1]);
        if st == 0 || sr == 0 {
            return Ok(None);
        }
        let nf = u16::from_le_bytes([d[st + 2], d[st + 3]]) as usize;
        let fps = f32::from_le_bytes(d[st + 4..st + 8].try_into().unwrap());
        let nbt = u16::from_le_bytes([d[st], d[st + 1]]) as usize;
        let nbr = u16::from_le_bytes([d[sr], d[sr + 1]]) as usize;
        let nfr = u16::from_le_bytes([d[sr + 2], d[sr + 3]]) as usize;
        if nbt == 0 || nbr == 0 || nfr != nf {
            return Ok(None);
        }
        let sz = if fixed { 6 } else { 12 };
        for f in 0..=nf {
            let o = st + 8 + sz * f;
            trs.push(unpack_trans(d.get(o..o + sz).context("root translation")?, fixed));
            let o = sr + 8 + 6 * f;
            rots.push(unpack_quat(d.get(o..o + 6).context("root rotation")?));
            times.push(if fps > 0.0 { f as f32 / fps } else { 0.0 });
        }
    }
    if times.is_empty() {
        return Ok(None);
    }
    // Make relative to the first key: delta(t) = inv(R0) * (T(t) - T0), inv(R0) * R(t).
    let r0i = quat_conj(rots[0]);
    let t0 = trs[0];
    let mut out = RootMotion { start_translation: vec_to_bevy(t0), start_rotation: quat_to_bevy(rots[0]), ..Default::default() };
    for i in 0..times.len() {
        let dt = [trs[i][0] - t0[0], trs[i][1] - t0[1], trs[i][2] - t0[2]];
        let lt = quat_rotate(r0i, dt);
        let lr = quat_normalize(quat_mul(r0i, rots[i]));
        out.times.push(times[i]);
        out.translations.push(vec_to_bevy(lt));
        out.rotations.push(quat_to_bevy(lr));
    }
    Ok(Some(out))
}

fn fixed_str(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Section 6 holds `u32 count` event records (`u16 kind, u16, u32 hash, u32,
/// f32 start, f32 end, ...`) whose bodies vary by kind and may embed nbCF objects.
/// Attachment / IK events contain consecutive 36-byte name slots
/// `{u32 crc32(name), char[32]}`: object, bone, optional IK target. We scan for
/// those slots instead of walking the records.
fn parse_attachments(d: &[u8], s: usize, end: usize) -> Result<Vec<ClipAttachment>> {
    let end = end.min(d.len());
    let slot = |q: usize| -> Option<String> {
        if q + 36 > end {
            return None;
        }
        let h = u32::from_le_bytes(d[q..q + 4].try_into().unwrap());
        let name = fixed_str(&d[q + 4..q + 36]);
        let printable = !name.is_empty() && name.bytes().all(|c| (0x20..0x7f).contains(&c));
        (h != 0 && printable && crc32(name.as_bytes()) == h).then_some(name)
    };
    let mut out = Vec::new();
    let mut q = s + 4;
    while q + 36 <= end {
        let Some(first) = slot(q) else {
            q += 4;
            continue;
        };
        let mut names = vec![first];
        let mut k = q + 36;
        while let Some(n) = slot(k) {
            names.push(n);
            k += 36;
        }
        if names.len() >= 2 {
            let kind = if q >= s + 24 { u16::from_le_bytes([d[q - 20], d[q - 19]]) } else { 0 };
            out.push(ClipAttachment {
                kind,
                object: names[0].clone(),
                bone: names[1].clone(),
                target: names.get(2).cloned(),
            });
        }
        q = k;
    }
    Ok(out)
}

// ===========================================================================
// Posing helpers (reference implementation for the engine and the OBJ tool)
// ===========================================================================

/// Local transforms of `nodes` posed by `clip` at time `t` (Bevy space), as
/// matrices. Nodes are matched to clip bones by name through `skel` (the
/// skeleton the clip targets). Bones the clip doesn't animate keep the node's
/// own bind local transform. When `skel` provides procedural definitions, twist
/// and blend helper bones are driven from their drivers.
pub fn pose_locals(nodes: &[Node], skel: &Skeleton, clip: &Clip, t: f32) -> Vec<Mat4> {
    let mut rot: Vec<Quat> = nodes.iter().map(|n| n.rotation).collect();
    let mut tr: Vec<Vec3> = nodes.iter().map(|n| n.translation).collect();
    let skel_idx: Vec<Option<usize>> = nodes.iter().map(|n| skel.find_hash(n.name_hash).or_else(|| skel.find(&n.name))).collect();
    for (i, si) in skel_idx.iter().enumerate() {
        let Some(si) = si else { continue };
        if let Some(track) = clip.track_for_bone(*si as u16) {
            if let Some(q) = track.sample_rotation(t) {
                rot[i] = q;
            }
            if let Some(v) = track.sample_translation(t) {
                tr[i] = v;
            }
        }
    }
    // Procedural helpers.
    let node_of_skel = |sb: u32| -> Option<usize> {
        let b = skel.bones.get(sb as usize)?;
        nodes.iter().position(|n| n.name_hash == b.name_hash || n.name == b.name)
    };
    let has_proc = skel_idx.iter().flatten().any(|&si| skel.bones[si].procedural != Procedural::None);
    if has_proc {
        let rest_w = world_rotations(nodes, &nodes.iter().map(|n| n.rotation).collect::<Vec<_>>());
        let cur_w = world_rotations(nodes, &rot);
        for (i, si) in skel_idx.iter().enumerate() {
            let Some(si) = *si else { continue };
            let p = nodes[i].parent;
            let pw = if p >= 0 { cur_w[p as usize] } else { QUAT_IDENTITY };
            let pw_rest = if p >= 0 { rest_w[p as usize] } else { QUAT_IDENTITY };
            match &skel.bones[si].procedural {
                Procedural::None => {}
                Procedural::Twist { driver, weight } => {
                    let Some(dn) = node_of_skel(*driver) else { continue };
                    // Driver relative to parent, now vs rest, in the parent's frame.
                    let rel = quat_mul(quat_conj(pw), cur_w[dn]);
                    let rel0 = quat_mul(quat_conj(pw_rest), rest_w[dn]);
                    let delta = quat_mul(rel, quat_conj(rel0));
                    let twist = quat_normalize([delta[0], 0.0, 0.0, delta[3]]);
                    let tw = quat_slerp(QUAT_IDENTITY, twist, *weight);
                    rot[i] = quat_normalize(quat_mul(tw, nodes[i].rotation));
                }
                Procedural::Blend(drivers) => {
                    let mut acc: Option<(Quat, f32)> = None;
                    for &(drv, w) in drivers {
                        let Some(dn) = node_of_skel(drv) else { continue };
                        let delta = quat_mul(cur_w[dn], quat_conj(rest_w[dn]));
                        acc = Some(match acc {
                            None => (delta, w),
                            Some((q, wsum)) => (quat_slerp(q, delta, w / (wsum + w).max(1e-6)), wsum + w),
                        });
                    }
                    if let Some((delta, _)) = acc {
                        let world = quat_mul(delta, rest_w[i]);
                        rot[i] = quat_normalize(quat_mul(quat_conj(pw), world));
                    }
                }
            }
        }
    }
    nodes.iter().enumerate().map(|(i, n)| mat_from_trs(tr[i], rot[i], n.scale)).collect()
}

/// Layers an additive clip on top of already posed local matrices (from
/// [`pose_locals`]): `rotation = base * delta^weight`, `translation = base + weight * delta`.
pub fn apply_additive(nodes: &[Node], skel: &Skeleton, locals: &mut [Mat4], additive: &Clip, t: f32, weight: f32) {
    for (i, n) in nodes.iter().enumerate() {
        let Some(si) = skel.find_hash(n.name_hash).or_else(|| skel.find(&n.name)) else { continue };
        let Some(track) = additive.track_for_bone(si as u16) else { continue };
        let m = &locals[i];
        let base_q = mat_rotation(m);
        let mut base_t = [m[12], m[13], m[14]];
        let q = match track.sample_rotation(t) {
            Some(d) => quat_normalize(quat_mul(base_q, quat_slerp(QUAT_IDENTITY, d, weight))),
            None => base_q,
        };
        if let Some(d) = track.sample_translation(t) {
            for k in 0..3 {
                base_t[k] += d[k] * weight;
            }
        }
        locals[i] = mat_from_trs(base_t, q, n.scale);
    }
}

fn world_rotations(nodes: &[Node], local: &[Quat]) -> Vec<Quat> {
    let mut w: Vec<Quat> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let q = if n.parent >= 0 { quat_mul(w[n.parent as usize], local[i]) } else { local[i] };
        w.push(quat_normalize(q));
    }
    w
}

/// Composes local matrices into model-space matrices; `root` is applied to nodes
/// without a parent.
pub fn compose_worlds(nodes: &[Node], locals: &[Mat4], root: &Mat4) -> Vec<Mat4> {
    let mut w: Vec<Mat4> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let m = if n.parent >= 0 { mat_mul(&w[n.parent as usize], &locals[i]) } else { mat_mul(root, &locals[i]) };
        w.push(m);
    }
    w
}

/// Skinning matrices `world * inverse_bind` for each node.
pub fn skinning_matrices(worlds: &[Mat4], inverse_bind: &[Mat4]) -> Vec<Mat4> {
    worlds.iter().zip(inverse_bind).map(|(w, ib)| mat_mul(w, ib)).collect()
}

/// CPU-skins positions (already in Bevy space, e.g. from mesh.rs) with `SkinLod` joints.
pub fn skin_positions(positions: &[[f32; 3]], lod: &SkinLod, skin_mats: &[Mat4]) -> Vec<[f32; 3]> {
    positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (Some(j), Some(w)) = (lod.joints.get(i), lod.weights.get(i)) else { return *p };
            let mut o = [0.0f32; 3];
            for k in 0..4 {
                if w[k] == 0.0 {
                    continue;
                }
                let Some(m) = skin_mats.get(j[k] as usize) else { continue };
                let q = mat_transform_point(m, *p);
                o[0] += q[0] * w[k];
                o[1] += q[1] * w[k];
                o[2] += q[2] * w[k];
            }
            o
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quat_unpack_identity() {
        // FW/SW high bits set -> (f1,f2,f3,f4) with f1=f2=f3=0 -> identity.
        let q = unpack_quat(&[0xff, 0xbf, 0xff, 0xbf, 0xff, 0x3f]);
        assert!((q[3] - 1.0).abs() < 1e-3 && q[0].abs() < 1e-3);
    }

    #[test]
    fn bevy_matrix_conversion_matches_trs() {
        let q = quat_normalize([0.1, 0.2, 0.3, 0.9]);
        let t = [1.0, 2.0, 3.0];
        let m = mat_from_trs(t, q, [1.0; 3]);
        let a = mat_to_bevy(&m);
        let b = mat_from_trs(vec_to_bevy(t), quat_to_bevy(q), [1.0; 3]);
        for i in 0..16 {
            assert!((a[i] - b[i]).abs() < 1e-5, "{i}: {} vs {}", a[i], b[i]);
        }
    }

    fn node(name: &str, parent: i32, t: Vec3) -> Node {
        Node {
            name: name.into(),
            name_hash: crc32(name.as_bytes()),
            parent,
            first_child: -1,
            next_sibling: -1,
            translation: t,
            rotation: QUAT_IDENTITY,
            scale: [1.0; 3],
            bind_index: -1,
        }
    }

    fn bone(name: &str, parent: i32, procedural: Procedural) -> SkeletonBone {
        SkeletonBone {
            name: name.into(),
            name_hash: crc32(name.as_bytes()),
            parent,
            translation: [0.0; 3],
            rotation: QUAT_IDENTITY,
            length: 0.0,
            procedural,
        }
    }

    #[test]
    fn procedural_twist_and_blend() {
        // Arm -> ForeArm -> Hand, plus a twist bone (follows the hand's twist about X)
        // and an elbow helper (half-way between Arm and ForeArm).
        let nodes = vec![
            node("Arm", -1, [0.0; 3]),
            node("ForeArm", 0, [0.3, 0.0, 0.0]),
            node("Hand", 1, [0.25, 0.0, 0.0]),
            node("Twist", 1, [0.1, 0.0, 0.0]),
            node("Elbow", 0, [0.3, 0.0, 0.0]),
        ];
        let skel = Skeleton {
            bones: vec![
                bone("Arm", -1, Procedural::None),
                bone("ForeArm", 0, Procedural::None),
                bone("Hand", 1, Procedural::None),
                bone("Twist", 1, Procedural::Twist { driver: 2, weight: 1.0 }),
                bone("Elbow", 0, Procedural::Blend(vec![(0, 0.5), (1, 0.5)])),
            ],
            sockets: vec![],
        };
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let rot_x90 = [s, 0.0, 0.0, s]; // hand twisted 90 degrees about the bone axis
        let rot_z90 = [0.0, 0.0, s, s]; // fore-arm bent 90 degrees
        let clip = Clip {
            tracks: vec![
                Track { bone: 1, rotation_times: vec![0.0], rotations: vec![rot_z90], ..Default::default() },
                Track { bone: 2, rotation_times: vec![0.0], rotations: vec![rot_x90], ..Default::default() },
            ],
            ..Default::default()
        };
        let locals = pose_locals(&nodes, &skel, &clip, 0.0);
        let twist = mat_rotation(&locals[3]);
        assert!((twist[0].abs() - s).abs() < 1e-3 && (twist[3].abs() - s).abs() < 1e-3, "twist {twist:?}");
        let elbow = mat_rotation(&locals[4]);
        let half = (std::f32::consts::PI / 8.0).sin();
        assert!((elbow[2].abs() - half).abs() < 1e-3, "elbow {elbow:?}");
    }

    #[test]
    fn inverse_affine() {
        let m = mat_from_trs([1.0, -2.0, 0.5], quat_normalize([0.3, -0.1, 0.2, 0.9]), [1.5, 1.5, 1.5]);
        let p = mat_mul(&m, &mat_inverse_affine(&m));
        for i in 0..16 {
            assert!((p[i] - MAT4_IDENTITY[i]).abs() < 1e-5);
        }
    }
}
