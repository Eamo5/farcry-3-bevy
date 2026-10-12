//! RealTree (`.rtx`) procedural trees, palms and large plants.
//!
//! A `.rtx` file holds no ready-made render mesh. It is a memory image of the
//! runtime tree object: a branch skeleton (nodes with positions and radii),
//! leaf *cards* attached to the nodes, and for palms and big plants a set of
//! small skinned "big leaf" meshes (fronds). The engine builds the trunk and
//! branch tubes and the leaf quads itself. This module decodes the skeleton
//! and the cards ([`parse`]) and rebuilds equivalent render geometry
//! ([`RealTree::build_parts`]). See `docs/realtree.md` for the byte layout.
//!
//! # Coordinates
//!
//! Everything is in the tree's local **Dunia space**: right-handed, **Z up**,
//! metres, origin at the tree's foot. Placement instances (see
//! `vegetation.rs`) add a yaw about +Z and a uniform scale. To convert to
//! Bevy (Y up) map `(x, y, z) -> (x, z, -y)`; this is a proper rotation, so
//! triangle winding is preserved.
//!
//! # Texture coordinates
//!
//! RealTree stores texture coordinates with **v pointing up** (v = 0 at the
//! bottom of the image). The parts built here already flip them
//! (`v_out = 1 - v_stored`) so they can be used directly with D3D/wgpu/Bevy
//! style textures (v = 0 at the top row).

use anyhow::{bail, ensure, Context, Result};

use crate::material::Material;

/// Material slot used by the trunk and branch tubes (`RealtreeTrunk` shader).
pub const SLOT_TRUNK: u32 = 0;
/// Material slot used by the leaf cards (`Leaf` shader, alpha tested atlas).
pub const SLOT_LEAVES: u32 = 1;
/// Material slot used by the big-leaf meshes (`BigLeaf` shader).
pub const SLOT_BIG_LEAVES: u32 = 2;

const HEADER_NAME: usize = 0x20;
const HEADER_INSTANCES: usize = 0x128;
const NODE_SIZE: usize = 80;
const NODE_PHYS_SIZE: usize = 36;
const BRANCH_SIZE: usize = 40;
const BRANCH_EXTRA_SIZE: usize = 44;
const LEAF_SIZE: usize = 92;
const LEAF_TYPE_SIZE: usize = 108;
const BIG_LEAF_VERTEX_SIZE: usize = 152;

/// Root-object offsets of the two self pointers used to recover the pointer
/// base, and the block offsets they point at.
const ROOT_SELF_PTR_A: usize = 0x70;
const ROOT_SELF_TARGET_A: usize = 0x228;
const ROOT_SELF_PTR_B: usize = 0x6c;
const ROOT_SELF_TARGET_B: usize = 0x258;

/// A decoded `.rtx` file.
#[derive(Debug, Clone, Default)]
pub struct RealTree {
    /// Tree name from the header (for example `Kawau_Bush_A`).
    pub name: String,
    /// Format version (header +0x1C): 0x8A..0x8C for trees, 0xCE/0xD0 for most palms.
    pub version: u32,
    /// Header +0x10: 0 = tree (branches + leaf cards), 1 = plant/palm (branches +
    /// big leaves), 2 = both kinds of foliage.
    pub kind: u32,
    /// Bounding box stored in the file (Dunia space). It covers the skeleton,
    /// the leaf cards and the big leaves.
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub nodes: Vec<Node>,
    pub branches: Vec<Branch>,
    pub leaves: Vec<Leaf>,
    pub leaf_types: Vec<LeafType>,
    pub big_leaves: Vec<BigLeaf>,
    /// Material slots (see [`SLOT_TRUNK`], [`SLOT_LEAVES`], [`SLOT_BIG_LEAVES`]).
    pub materials: Vec<MaterialRef>,
}

/// One `.material.bin` reference from the file's material table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialRef {
    pub slot: u32,
    /// Archive path of the `.material.bin`.
    pub path: String,
}

/// A skeleton node: one ring of the branch tube it belongs to.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub position: [f32; 3],
    /// Tube radius at this node (metres). Branch tips are close to zero.
    pub radius: f32,
    /// Distance to the next node of the same branch (0 at the tip).
    pub segment_length: f32,
    /// Previous node: the preceding node of the branch, or for the first node of
    /// a branch the parent-branch node it grows from. `None` for the root.
    pub parent: Option<u32>,
    /// First nodes of the side branches that start here.
    pub children: Vec<u32>,
    /// Index into [`RealTree::branches`].
    pub branch: u32,
    /// Position of this node within its branch (0 = branch base).
    pub index_in_branch: u32,
    /// Leaf cards attached to the segment that starts at this node:
    /// `leaves[first_leaf .. first_leaf + leaf_count]`.
    pub first_leaf: u32,
    pub leaf_count: u32,
    /// Texture V along the branch (stored convention, v up). It grows with the
    /// distance from the branch base.
    pub tex_v: f32,
    /// Texture U repeats around the circumference (+0x10; constant per branch,
    /// e.g. 3 for palm and large trunks, 0.5..1 for twigs). Chosen over the
    /// similar field at +0x14 because it gives roughly square bark texels.
    pub tex_u_repeat: f32,
    /// Number of nodes after this one in its subtree.
    pub subtree_size: u32,
    /// Undecoded per-node values: +0x14 (a second per-branch tiling factor),
    /// +0x18, +0x1C, +0x24 (0..1 along most branches), +0x28 and +0x48 (1 at the
    /// trunk base falling to 0.2 at twigs). Probably wind and shading weights.
    pub unknown: [f32; 6],
}

/// A chain of nodes forming one tube.
#[derive(Debug, Clone, Default)]
pub struct Branch {
    pub first_node: u32,
    /// Number of segments; the branch uses nodes `first_node ..= first_node + segments`.
    pub segments: u32,
    /// Node of the parent branch where this branch starts (`None` for the trunk).
    pub parent_node: Option<u32>,
    /// Depth in the hierarchy (0 = trunk).
    pub level: u32,
    /// Total length (sum of the segment lengths).
    pub length: f32,
    /// Raw 40-byte record as `i32`s (fields 4, 5, 7 and 9 are not decoded).
    pub raw: [i32; 10],
}

/// A leaf card. The card is a square diamond in the plane spanned by `axis`
/// and `half_width`; see [`Leaf::corners`].
#[derive(Debug, Clone, Default)]
pub struct Leaf {
    /// Node whose segment the card hangs from (`u32::MAX` if no node lists the
    /// card; this does not happen in the shipped files).
    pub node: u32,
    /// Distance from that node along the segment to the attachment point.
    pub offset: f32,
    /// Half diagonal of the card (metres).
    pub size: f32,
    /// Unit direction from the attachment point to the card tip (close to the
    /// branch direction).
    pub axis: [f32; 3],
    /// Unit card normal (`axis x half_width`, normalised).
    pub normal: [f32; 3],
    /// Vector from the card centre to a side corner; length `size`.
    pub half_width: [f32; 3],
    /// Index into [`RealTree::leaf_types`].
    pub leaf_type: u32,
    /// UV corner codes for the corners `[attach, tip, centre + half_width,
    /// centre - half_width]`: bit 0 selects `u_max`, bit 1 selects `v_max`
    /// (stored v-up convention). They mirror the image and keep it upright.
    pub corner_codes: [u8; 4],
    /// Raw flag bytes at +0x54 (byte 1 is the leaf type).
    pub flags: [u8; 4],
    /// The attachment point (filled in by [`parse`]).
    pub attach: [f32; 3],
}

impl Leaf {
    /// Card centre: `attach + axis * size`.
    pub fn center(&self) -> [f32; 3] {
        add(self.attach, scale(self.axis, self.size))
    }

    /// Corner positions `[attach, tip, centre + half_width, centre - half_width]`.
    pub fn corners(&self) -> [[f32; 3]; 4] {
        let c = self.center();
        [self.attach, add(c, scale(self.axis, self.size)), add(c, self.half_width), sub(c, self.half_width)]
    }
}

/// A leaf-card texture region in the leaf material's atlas.
#[derive(Debug, Clone, Default)]
pub struct LeafType {
    /// Stored at +0x00 (0 or 1). Many trees use one type of each kind;
    /// kind 1 is plausibly a camera-facing variant (unconfirmed). Built as fixed
    /// cards either way.
    pub kind: u32,
    /// Atlas rectangle in the stored v-up convention.
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    /// Number of cards using this type.
    pub count: u32,
    /// Value at +0x30 (102..105, 5 or -1; matches the card flag bytes 2 + 3).
    pub code: f32,
}

impl LeafType {
    /// Texture coordinate for a corner code (see [`Leaf::corner_codes`]),
    /// already flipped to the v-down convention.
    pub fn corner_uv(&self, code: u8) -> [f32; 2] {
        let u = if code & 1 != 0 { self.uv_max[0] } else { self.uv_min[0] };
        let v = if code & 2 != 0 { self.uv_max[1] } else { self.uv_min[1] };
        [u, 1.0 - v]
    }
}

/// A big-leaf mesh (palm frond, banana leaf, fern frond, ...).
#[derive(Debug, Clone, Default)]
pub struct BigLeaf {
    /// One to three levels of detail; LOD 0 is the most detailed.
    pub lods: Vec<BigLeafLod>,
}

#[derive(Debug, Clone, Default)]
pub struct BigLeafLod {
    /// Tree-space positions (not relative to a bone).
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Tangent xyz and handedness.
    pub tangents: Vec<[f32; 4]>,
    /// Already flipped to the v-down convention.
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex value at +92 (1.0 near the stem, down to about 0.2 at the
    /// tip); probably an occlusion/colour term.
    pub shade: Vec<f32>,
    pub indices: Vec<u32>,
}

/// Which kind of geometry a [`Part`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PartKind {
    /// Trunk and branch tubes (opaque, `RealtreeTrunk` material).
    Branches,
    /// Leaf cards (alpha tested, two sided, `Leaf` material).
    Leaves,
    /// Big-leaf meshes (alpha tested, two sided, `BigLeaf` material).
    BigLeaves,
}

/// Renderable geometry for one material.
#[derive(Debug, Clone)]
pub struct Part {
    pub kind: PartKind,
    /// Material slot ([`SLOT_TRUNK`], ...).
    pub slot: u32,
    /// `.material.bin` path for the slot, if the file lists one.
    pub material: Option<String>,
    /// True for foliage: render with alpha testing.
    pub alpha_tested: bool,
    /// True for foliage: render without back-face culling.
    pub two_sided: bool,
    /// Dunia space (Z up). Triangles are counter-clockwise seen from the side
    /// the normals point to.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// v-down convention. Branch UVs repeat; multiply by the material's
    /// `DiffuseTiling1`.
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl Part {
    fn new(kind: PartKind, slot: u32, tree: &RealTree) -> Part {
        let foliage = kind != PartKind::Branches;
        Part {
            kind,
            slot,
            material: tree.material(slot).map(str::to_string),
            alpha_tested: foliage,
            two_sided: foliage,
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Axis-aligned bounds of the positions, or `None` when empty.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        bounds_of(&self.positions)
    }
}

/// Options for [`RealTree::build_parts`].
#[derive(Debug, Clone)]
pub struct MeshOptions {
    /// Level of detail: big leaves use their LOD `lod` (clamped); branch tubes
    /// halve their side count per level.
    pub lod: usize,
    /// Target edge length around a tube at its base, in metres; it sets the
    /// side count from the base radius.
    pub tube_edge: f32,
    pub min_sides: u32,
    pub max_sides: u32,
    /// Branches whose base radius is below this are skipped (0 keeps all).
    pub min_radius: f32,
    /// Branches deeper than this are skipped (0: the trunk only; `None`
    /// keeps all).
    pub max_level: Option<u32>,
}

impl Default for MeshOptions {
    fn default() -> Self {
        MeshOptions { lod: 0, tube_edge: 0.12, min_sides: 3, max_sides: 12, min_radius: 0.0, max_level: None }
    }
}

impl RealTree {
    /// Path of the `.material.bin` used by a material slot.
    pub fn material(&self, slot: u32) -> Option<&str> {
        self.materials.iter().find(|m| m.slot == slot).map(|m| m.path.as_str())
    }

    /// Builds trunk/branch tubes, leaf cards and big leaves. Empty parts are
    /// omitted.
    pub fn build_parts(&self, opts: &MeshOptions) -> Vec<Part> {
        let mut parts = Vec::new();
        let branches = self.build_branches(opts);
        if !branches.indices.is_empty() {
            parts.push(branches);
        }
        let leaves = self.build_leaves();
        if !leaves.indices.is_empty() {
            parts.push(leaves);
        }
        let big = self.build_big_leaves(opts.lod);
        if !big.indices.is_empty() {
            parts.push(big);
        }
        parts
    }

    /// Generalised cylinders along every branch. Rings use rotation-minimising
    /// frames so the tubes do not twist; U runs around the tube
    /// (`tex_u_repeat` times), V along it (`tex_v`).
    pub fn build_branches(&self, opts: &MeshOptions) -> Part {
        let mut part = Part::new(PartKind::Branches, SLOT_TRUNK, self);
        for br in &self.branches {
            let first = br.first_node as usize;
            let last = first + br.segments as usize;
            if br.segments == 0 || last >= self.nodes.len() {
                continue;
            }
            let nodes = &self.nodes[first..=last];
            let r0 = nodes[0].radius.max(0.0);
            if r0 < opts.min_radius || opts.max_level.is_some_and(|m| br.level > m) {
                continue;
            }
            let wanted = (std::f32::consts::TAU * r0 / opts.tube_edge.max(1e-3)).round() as u32;
            let sides = (wanted.clamp(opts.min_sides.max(3), opts.max_sides.max(3)) >> opts.lod.min(8))
                .max(opts.min_sides.max(3)) as usize;

            // Tangents (central differences) and parallel-transported normals.
            let n = nodes.len();
            let mut tangents = Vec::with_capacity(n);
            let mut prev_t = [0.0, 0.0, 1.0];
            for i in 0..n {
                let a = nodes[i.saturating_sub(1)].position;
                let b = nodes[(i + 1).min(n - 1)].position;
                let t = normalize_or(sub(b, a), prev_t);
                tangents.push(t);
                prev_t = t;
            }
            let mut frame_n = any_perpendicular(tangents[0]);
            let base = part.positions.len() as u32;
            for (i, node) in nodes.iter().enumerate() {
                let t = tangents[i];
                frame_n = normalize_or(sub(frame_n, scale(t, dot(frame_n, t))), any_perpendicular(t));
                let frame_b = cross(t, frame_n);
                let r = node.radius.max(0.0);
                let repeat = if node.tex_u_repeat > 0.0 { node.tex_u_repeat } else { 1.0 };
                for k in 0..=sides {
                    let a = std::f32::consts::TAU * k as f32 / sides as f32;
                    let dir = add(scale(frame_n, a.cos()), scale(frame_b, a.sin()));
                    part.positions.push(add(node.position, scale(dir, r)));
                    part.normals.push(dir);
                    part.uvs.push([repeat * k as f32 / sides as f32, 1.0 - node.tex_v]);
                }
            }
            let ring = (sides + 1) as u32;
            for i in 0..(n - 1) as u32 {
                for k in 0..sides as u32 {
                    let a = base + i * ring + k;
                    let b = a + 1;
                    let c = a + ring + 1;
                    let d = a + ring;
                    part.indices.extend_from_slice(&[a, b, c, a, c, d]);
                }
            }
        }
        part
    }

    /// One diamond-shaped quad per leaf card (two triangles, normal = card normal).
    pub fn build_leaves(&self) -> Part {
        let mut part = Part::new(PartKind::Leaves, SLOT_LEAVES, self);
        for leaf in &self.leaves {
            let Some(lt) = self.leaf_types.get(leaf.leaf_type as usize) else { continue };
            let base = part.positions.len() as u32;
            for (corner, code) in leaf.corners().into_iter().zip(leaf.corner_codes) {
                part.positions.push(corner);
                part.normals.push(leaf.normal);
                part.uvs.push(lt.corner_uv(code));
            }
            // Corners: 0 attach, 1 tip, 2 +side, 3 -side; both triangles face `normal`.
            part.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 3, base + 1]);
        }
        part
    }

    /// All big leaves at the requested LOD (clamped per leaf).
    pub fn build_big_leaves(&self, lod: usize) -> Part {
        let mut part = Part::new(PartKind::BigLeaves, SLOT_BIG_LEAVES, self);
        for bl in &self.big_leaves {
            let Some(l) = bl.lods.get(lod.min(bl.lods.len().saturating_sub(1))) else { continue };
            let base = part.positions.len() as u32;
            part.positions.extend_from_slice(&l.positions);
            part.normals.extend_from_slice(&l.normals);
            part.uvs.extend_from_slice(&l.uvs);
            part.indices.extend(l.indices.iter().map(|&i| base + i));
        }
        part
    }

    /// Bounds of the rebuilt geometry (all parts at `opts`), for comparison with
    /// the stored [`RealTree::bbox_min`]/[`RealTree::bbox_max`].
    pub fn geometry_bounds(&self, opts: &MeshOptions) -> Option<([f32; 3], [f32; 3])> {
        let pts: Vec<[f32; 3]> = self.build_parts(opts).into_iter().flat_map(|p| p.positions).collect();
        bounds_of(&pts)
    }
}

/// Texture and flags of a RealTree material, resolved from its parameters.
#[derive(Debug, Clone, Default)]
pub struct TreeMaterial {
    /// Shader name: `RealtreeTrunk`, `Leaf` or `BigLeaf`.
    pub shader: String,
    /// `DiffuseTexture1`. Foliage textures carry the alpha-test cut-out in
    /// their alpha channel. (Trunk diffuse files are often named `*_n.xbt`
    /// even though they are colour maps.)
    pub diffuse: Option<String>,
    /// `NormalTexture1` (trunk, big leaf) or `NormalTexture` (leaf).
    pub normal: Option<String>,
    /// `MaskTexture` (big leaves only): a small, mostly white mask. It is *not*
    /// the cut-out; that is in the diffuse alpha.
    pub mask: Option<String>,
    /// `DiffuseTiling1` (trunk UV scale); `[1, 1]` when absent.
    pub tiling: [f32; 2],
    /// `TwoSided`.
    pub two_sided: bool,
}

/// CRC-32 hashes of the material parameter names used by RealTree shaders.
pub mod params {
    pub const DIFFUSE_TEXTURE_1: u32 = 0xF95881AF;
    pub const DIFFUSE_TEXTURE_2: u32 = 0x6051D015;
    pub const NORMAL_TEXTURE: u32 = 0x8C658791;
    pub const NORMAL_TEXTURE_1: u32 = 0x045829E2;
    pub const MASK_TEXTURE: u32 = 0xDC662227;
    pub const DIFFUSE_TILING_1: u32 = 0xE525F198;
    pub const TWO_SIDED: u32 = 0x403525B2;
}

/// Picks the textures a renderer needs from a parsed `.material.bin`.
pub fn resolve_material(m: &Material) -> TreeMaterial {
    let tex = |h: u32| m.textures.iter().find(|(k, _)| *k == h).map(|(_, p)| p.clone());
    let param = |h: u32| m.params.iter().find(|(k, _)| *k == h).map(|(_, v)| v.as_slice());
    let tiling = match param(params::DIFFUSE_TILING_1) {
        Some(v) if v.len() >= 8 => {
            let f = |o: usize| f32::from_le_bytes(v[o..o + 4].try_into().unwrap());
            [f(0), f(4)]
        }
        _ => [1.0, 1.0],
    };
    TreeMaterial {
        shader: m.shader.clone(),
        diffuse: tex(params::DIFFUSE_TEXTURE_1),
        normal: tex(params::NORMAL_TEXTURE_1).or_else(|| tex(params::NORMAL_TEXTURE)),
        mask: tex(params::MASK_TEXTURE),
        tiling,
        two_sided: param(params::TWO_SIDED).is_some_and(|v| v.first().is_some_and(|&b| b != 0)),
    }
}

/// Bounds-checked view of one instance block with its pointer base.
struct Image<'a> {
    data: &'a [u8],
    /// Pointer value of file offset 0 (pointers are absolute addresses).
    base: u32,
    block: std::ops::Range<usize>,
}

impl<'a> Image<'a> {
    fn bytes(&self, o: usize, n: usize) -> Result<&'a [u8]> {
        self.data.get(o..o + n).with_context(|| format!("read of {n} bytes at {o:#x} past end"))
    }
    fn u32(&self, o: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(o, 4)?.try_into().unwrap()))
    }
    fn u16(&self, o: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(o, 2)?.try_into().unwrap()))
    }
    fn f32(&self, o: usize) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(o, 4)?.try_into().unwrap()))
    }
    fn vec2(&self, o: usize) -> Result<[f32; 2]> {
        Ok([self.f32(o)?, self.f32(o + 4)?])
    }
    fn vec3(&self, o: usize) -> Result<[f32; 3]> {
        Ok([self.f32(o)?, self.f32(o + 4)?, self.f32(o + 8)?])
    }
    /// Resolves the pointer stored at `o` to a file offset inside the block.
    fn ptr(&self, o: usize) -> Result<Option<usize>> {
        let v = self.u32(o)?;
        if v == 0 {
            return Ok(None);
        }
        let off = v.wrapping_sub(self.base) as usize;
        ensure!(self.block.contains(&off), "pointer {v:#x} at {o:#x} points outside the tree block");
        Ok(Some(off))
    }
    /// Resolves an array pointer and checks that `count * stride` bytes fit.
    fn array(&self, o: usize, count: usize, stride: usize, what: &str) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        let start = self.ptr(o)?.with_context(|| format!("{what}: null pointer for {count} entries"))?;
        ensure!(
            count <= 1 << 20 && start + count * stride <= self.block.end,
            "{what}: {count} x {stride} bytes at {start:#x} overrun the tree block"
        );
        Ok(start)
    }
}

/// Parses a `.rtx` file.
pub fn parse(data: &[u8]) -> Result<RealTree> {
    ensure!(data.len() >= HEADER_INSTANCES + 16, "file too small for a RealTree header");
    let rd = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let kind = rd(0x10);
    let instance_count = rd(0x14) as usize;
    let tail = rd(0x18) as usize;
    let version = rd(0x1C);
    ensure!((1..=16).contains(&instance_count), "implausible instance count {instance_count}");
    let name_bytes = &data[HEADER_NAME..HEADER_NAME + 0x100];
    let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
    let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();

    // Instance table: the tree is stored once per instance, identical apart
    // from the pointers. The first one is enough.
    ensure!(data.len() >= HEADER_INSTANCES + 16 * instance_count, "instance table past end");
    let blk = rd(HEADER_INSTANCES) as usize;
    let blk_size = rd(HEADER_INSTANCES + 4) as usize;
    ensure!(blk >= HEADER_INSTANCES && blk + blk_size <= data.len() && blk_size >= 0x300, "bad tree block");

    // Pointers are absolute addresses of a memory image; two root fields point
    // back into the root object and give the base.
    let base = rd(blk + ROOT_SELF_PTR_A).wrapping_sub((blk + ROOT_SELF_TARGET_A) as u32);
    ensure!(
        rd(blk + ROOT_SELF_PTR_B).wrapping_sub(base) as usize == blk + ROOT_SELF_TARGET_B,
        "RealTree root self pointers are inconsistent (unknown layout)"
    );
    let img = Image { data, base, block: blk..blk + blk_size };

    let bbox_min = img.vec3(blk + 0x74)?;
    let bbox_max = img.vec3(blk + 0x80)?;

    // Nodes and their physics records (radius, segment length).
    let node_count = img.u32(blk + 0x18)? as usize;
    let nodes_at = img.array(blk + 0x1C, node_count, NODE_SIZE, "nodes")?;
    let phys_at = img.array(blk + 0x20, node_count, NODE_PHYS_SIZE, "node physics")?;
    let mut nodes = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let o = nodes_at + i * NODE_SIZE;
        let p = phys_at + i * NODE_PHYS_SIZE;
        let parent = img.u16(o + 0x2C)?;
        let child_count = img.u16(o + 0x2E)? as usize;
        let mut children = Vec::with_capacity(child_count);
        if child_count > 0 {
            let list = img.array(o + 0x30, child_count, 2, "child list")?;
            for k in 0..child_count {
                children.push(img.u16(list + 2 * k)? as u32);
            }
        }
        nodes.push(Node {
            position: img.vec3(o)?,
            subtree_size: img.u32(o + 0x0C)?,
            tex_u_repeat: img.f32(o + 0x10)?,
            tex_v: img.f32(o + 0x20)?,
            parent: (parent != 0xFFFF).then_some(parent as u32),
            children,
            branch: img.u16(o + 0x34)? as u32,
            leaf_count: img.u16(o + 0x36)? as u32,
            first_leaf: img.u32(o + 0x38)?,
            index_in_branch: img.u32(o + 0x4C)?,
            radius: img.f32(p + 0x0C)?,
            segment_length: img.f32(p + 0x10)?,
            unknown: [
                img.f32(o + 0x14)?,
                img.f32(o + 0x18)?,
                img.f32(o + 0x1C)?,
                img.f32(o + 0x24)?,
                img.f32(o + 0x28)?,
                img.f32(o + 0x48)?,
            ],
        });
    }
    for (i, n) in nodes.iter().enumerate() {
        if let Some(p) = n.parent {
            ensure!((p as usize) < node_count, "node {i}: parent {p} out of range");
        }
        if let Some(&c) = n.children.iter().find(|&&c| c as usize >= node_count) {
            bail!("node {i}: child {c} out of range");
        }
    }

    // Branches.
    let branch_count = img.u32(blk + 0x24)? as usize;
    let br_at = img.array(blk + 0x28, branch_count, BRANCH_SIZE, "branches")?;
    let brx_at = img.array(blk + 0x2C, branch_count, BRANCH_EXTRA_SIZE, "branch lengths")?;
    let mut branches = Vec::with_capacity(branch_count);
    for i in 0..branch_count {
        let o = br_at + i * BRANCH_SIZE;
        let mut raw = [0i32; 10];
        for (k, r) in raw.iter_mut().enumerate() {
            *r = img.u32(o + 4 * k)? as i32;
        }
        let first_node = raw[0] as u32;
        let segments = raw[1] as u32;
        ensure!(
            (first_node as usize + segments as usize) < node_count.max(1),
            "branch {i}: nodes {first_node}+{segments} out of range"
        );
        branches.push(Branch {
            first_node,
            segments,
            parent_node: (raw[2] >= 0).then_some(raw[2] as u32),
            level: raw[3] as u32,
            length: img.f32(brx_at + i * BRANCH_EXTRA_SIZE)?,
            raw,
        });
    }

    // Leaf types (atlas regions).
    let leaf_type_count = img.u32(blk + 0x148)? as usize;
    let lt_at = img.array(blk + 0x14C, leaf_type_count, LEAF_TYPE_SIZE, "leaf types")?;
    let mut leaf_types = Vec::with_capacity(leaf_type_count);
    for i in 0..leaf_type_count {
        let o = lt_at + i * LEAF_TYPE_SIZE;
        let a = img.vec2(o + 4)?;
        let b = img.vec2(o + 12)?;
        leaf_types.push(LeafType {
            kind: img.u32(o)?,
            uv_min: [a[0].min(b[0]), a[1].min(b[1])],
            uv_max: [a[0].max(b[0]), a[1].max(b[1])],
            code: img.f32(o + 0x30)?,
            count: img.u32(o + 0x38)?,
        });
    }

    // Leaf cards. The owning node comes from the nodes' leaf ranges.
    let leaf_count = img.u32(blk + 0x30)? as usize;
    let leaves_at = img.array(blk + 0x34, leaf_count, LEAF_SIZE, "leaves")?;
    let mut owner = vec![u32::MAX; leaf_count];
    for (i, n) in nodes.iter().enumerate() {
        for k in 0..n.leaf_count {
            let l = (n.first_leaf + k) as usize;
            ensure!(l < leaf_count, "node {i}: leaf {l} out of range");
            owner[l] = i as u32;
        }
    }
    let mut leaves = Vec::with_capacity(leaf_count);
    for (i, &node) in owner.iter().enumerate() {
        let o = leaves_at + i * LEAF_SIZE;
        let flags: [u8; 4] = img.bytes(o + 0x54, 4)?.try_into().unwrap();
        let corner_codes: [u8; 4] = img.bytes(o + 0x58, 4)?.try_into().unwrap();
        let axis = img.vec3(o + 0x0C)?;
        let half_width = img.vec3(o + 0x3C)?;
        let stored_normal = img.vec3(o + 0x18)?;
        let mut leaf = Leaf {
            node,
            offset: img.f32(o)?,
            size: img.f32(o + 0x08)?,
            axis,
            normal: normalize_or(cross(axis, half_width), stored_normal),
            half_width,
            leaf_type: flags[1] as u32,
            corner_codes,
            flags,
            attach: [0.0; 3],
        };
        if let Some(n) = nodes.get(node as usize) {
            // The card hangs from the segment towards the next node of the branch.
            let next = nodes.get(node as usize + 1).filter(|m| m.parent == Some(node));
            let dir = next.map(|m| normalize_or(sub(m.position, n.position), axis)).unwrap_or(axis);
            leaf.attach = add(n.position, scale(dir, leaf.offset));
        }
        leaves.push(leaf);
    }

    // Big leaves (palm fronds and similar).
    let big_count = img.u32(blk + 0x3C)? as usize;
    let table = img.array(blk + 0x40, big_count, 4, "big leaf table")?;
    let mut big_leaves = Vec::with_capacity(big_count);
    for i in 0..big_count {
        let obj = img.ptr(table + 4 * i)?.with_context(|| format!("big leaf {i}: null pointer"))?;
        big_leaves.push(parse_big_leaf(&img, obj).with_context(|| format!("big leaf {i}"))?);
    }

    let materials = parse_materials(data, tail).context("material table")?;

    Ok(RealTree {
        name,
        version,
        kind,
        bbox_min,
        bbox_max,
        nodes,
        branches,
        leaves,
        leaf_types,
        big_leaves,
        materials,
    })
}

fn parse_big_leaf(img: &Image, obj: usize) -> Result<BigLeaf> {
    let lod_count = img.u32(obj + 0x12C)? as usize;
    ensure!(lod_count <= 8, "implausible LOD count {lod_count}");
    let table = img.array(obj + 0x130, lod_count, 16, "big leaf LOD table")?;
    let mut lods = Vec::with_capacity(lod_count);
    for k in 0..lod_count {
        let e = table + 16 * k;
        let vcount = img.u32(e)? as usize;
        let icount = img.u32(e + 8)? as usize;
        let vat = img.array(e + 4, vcount, BIG_LEAF_VERTEX_SIZE, "big leaf vertices")?;
        let iat = img.array(e + 12, icount, 2, "big leaf indices")?;
        ensure!(icount.is_multiple_of(3), "index count {icount} is not a multiple of 3");
        let mut l = BigLeafLod::default();
        for j in 0..vcount {
            let v = vat + j * BIG_LEAF_VERTEX_SIZE;
            let t = img.vec3(v + 36)?;
            let uv = img.vec2(v + 76)?;
            l.positions.push(img.vec3(v)?);
            l.normals.push(img.vec3(v + 12)?);
            l.tangents.push([t[0], t[1], t[2], img.f32(v + 48)?]);
            l.uvs.push([uv[0], 1.0 - uv[1]]);
            l.shade.push(img.f32(v + 92)?);
        }
        for j in 0..icount {
            let idx = img.u16(iat + 2 * j)? as u32;
            ensure!((idx as usize) < vcount, "index {idx} out of range ({vcount} vertices)");
            l.indices.push(idx);
        }
        lods.push(l);
    }
    Ok(BigLeaf { lods })
}

/// Material table at the header's tail offset: `u32 ?, u32 ?, u32 count`, then
/// `count x { u32 slot, u32 len, char[len] }` (no terminator).
fn parse_materials(data: &[u8], tail: usize) -> Result<Vec<MaterialRef>> {
    let rd = |o: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(data.get(o..o + 4).context("material table past end")?.try_into().unwrap()))
    };
    let count = rd(tail + 8)? as usize;
    ensure!(count <= 16, "implausible material count {count}");
    let mut o = tail + 12;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let slot = rd(o)?;
        let len = rd(o + 4)? as usize;
        let s = data.get(o + 8..o + 8 + len).context("material path past end")?;
        out.push(MaterialRef { slot, path: String::from_utf8_lossy(s).trim_end_matches('\0').to_string() });
        o += 8 + len;
    }
    Ok(out)
}

fn bounds_of(pts: &[[f32; 3]]) -> Option<([f32; 3], [f32; 3])> {
    let first = *pts.first()?;
    Some(pts.iter().fold((first, first), |(mn, mx), p| {
        ([mn[0].min(p[0]), mn[1].min(p[1]), mn[2].min(p[2])], [mx[0].max(p[0]), mx[1].max(p[1]), mx[2].max(p[2])])
    }))
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize_or(v: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    let l = dot(v, v).sqrt();
    if l > 1e-6 && l.is_finite() {
        scale(v, 1.0 / l)
    } else {
        fallback
    }
}

/// A unit vector perpendicular to `t` (which must be unit length).
fn any_perpendicular(t: [f32; 3]) -> [f32; 3] {
    let helper = if t[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    normalize_or(cross(helper, t), [1.0, 0.0, 0.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x7CAF_0000;

    /// Builds a minimal `.rtx` image: a three-node trunk with a two-node side
    /// branch, one leaf card and one big leaf with one triangle.
    struct Builder {
        d: Vec<u8>,
    }

    impl Builder {
        fn put_u32(&mut self, o: usize, v: u32) {
            if self.d.len() < o + 4 {
                self.d.resize(o + 4, 0);
            }
            self.d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn put_u16(&mut self, o: usize, v: u16) {
            if self.d.len() < o + 2 {
                self.d.resize(o + 2, 0);
            }
            self.d[o..o + 2].copy_from_slice(&v.to_le_bytes());
        }
        fn put_f32(&mut self, o: usize, v: f32) {
            self.put_u32(o, v.to_bits());
        }
        fn put_vec(&mut self, o: usize, v: &[f32]) {
            for (k, x) in v.iter().enumerate() {
                self.put_f32(o + 4 * k, *x);
            }
        }
        fn put_ptr(&mut self, o: usize, target: usize) {
            self.put_u32(o, BASE + target as u32);
        }
    }

    const BLK: usize = 0x140;
    const NODES: usize = 0x700;
    const PHYS: usize = NODES + 5 * NODE_SIZE;
    const BRANCHES: usize = 0xA00;
    const BRANCH_X: usize = BRANCHES + 2 * BRANCH_SIZE;
    const CHILDREN: usize = 0xB00;
    const LEAVES: usize = 0xB40;
    const LEAF_TYPES: usize = 0xC00;
    const BIG_TABLE: usize = 0xD00;
    const BIG_OBJ: usize = 0xD10;
    const BIG_LODS: usize = BIG_OBJ + 0x140;
    const BIG_VERTS: usize = BIG_LODS + 0x10;
    const BIG_IDX: usize = BIG_VERTS + 3 * BIG_LEAF_VERTEX_SIZE;
    const BLK_END: usize = 0x1400;

    fn synthetic() -> Vec<u8> {
        let mut b = Builder { d: vec![0; BLK_END] };
        // Header.
        b.put_u32(0x10, 2); // kind: leaf cards and big leaves
        b.put_u32(0x14, 2); // instances
        b.put_u32(0x18, BLK_END as u32); // material table
        b.put_u32(0x1C, 0xD0);
        b.d[0x20..0x26].copy_from_slice(b"Test_A");
        for inst in 0..2 {
            b.put_u32(HEADER_INSTANCES + 16 * inst, BLK as u32);
            b.put_u32(HEADER_INSTANCES + 16 * inst + 4, (BLK_END - BLK) as u32);
        }
        // Root.
        b.put_ptr(BLK + ROOT_SELF_PTR_A, BLK + ROOT_SELF_TARGET_A);
        b.put_ptr(BLK + ROOT_SELF_PTR_B, BLK + ROOT_SELF_TARGET_B);
        b.put_u32(BLK + 0x18, 5);
        b.put_ptr(BLK + 0x1C, NODES);
        b.put_ptr(BLK + 0x20, PHYS);
        b.put_u32(BLK + 0x24, 2);
        b.put_ptr(BLK + 0x28, BRANCHES);
        b.put_ptr(BLK + 0x2C, BRANCH_X);
        b.put_u32(BLK + 0x30, 1);
        b.put_ptr(BLK + 0x34, LEAVES);
        b.put_u32(BLK + 0x3C, 1);
        b.put_ptr(BLK + 0x40, BIG_TABLE);
        b.put_vec(BLK + 0x74, &[-1.0, -1.0, 0.0, 1.0, 1.0, 4.0]);
        b.put_u32(BLK + 0x148, 1);
        b.put_ptr(BLK + 0x14C, LEAF_TYPES);
        // Nodes: trunk 0-1-2 straight up, branch 3-4 from node 1 along +X.
        let pos = [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, 2.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0]];
        let parent = [0xFFFF, 0, 1, 1, 3];
        let radius = [0.2, 0.15, 0.0, 0.05, 0.0];
        for i in 0..5 {
            let o = NODES + i * NODE_SIZE;
            b.put_vec(o, &pos[i]);
            b.put_f32(o + 0x10, if i < 3 { 2.0 } else { 1.0 });
            b.put_f32(o + 0x20, i as f32 * 0.5);
            b.put_u16(o + 0x2C, parent[i]);
            b.put_u16(o + 0x34, if i < 3 { 0 } else { 1 });
            b.put_u32(o + 0x4C, if i < 3 { i as u32 } else { i as u32 - 3 });
            b.put_f32(PHYS + i * NODE_PHYS_SIZE + 0x0C, radius[i]);
            b.put_f32(PHYS + i * NODE_PHYS_SIZE + 0x10, 1.0);
        }
        // Node 1 has a side branch (child 3); node 3 carries the leaf.
        b.put_u16(NODES + NODE_SIZE + 0x2E, 1);
        b.put_ptr(NODES + NODE_SIZE + 0x30, CHILDREN);
        b.put_u16(CHILDREN, 3);
        b.put_u16(NODES + 3 * NODE_SIZE + 0x36, 1);
        b.put_u32(NODES + 3 * NODE_SIZE + 0x38, 0);
        // Branches.
        for (i, (first, segs, par, level)) in [(0u32, 2u32, -1i32, 0u32), (3, 1, 1, 1)].into_iter().enumerate() {
            let o = BRANCHES + i * BRANCH_SIZE;
            b.put_u32(o, first);
            b.put_u32(o + 4, segs);
            b.put_u32(o + 8, par as u32);
            b.put_u32(o + 12, level);
            b.put_f32(BRANCH_X + i * BRANCH_EXTRA_SIZE, segs as f32);
        }
        // Leaf: 0.5 m along the branch (+X), axis +X, size 0.25, side vector +Y.
        let size = 0.25f32;
        b.put_f32(LEAVES, 0.5);
        b.put_f32(LEAVES + 8, size);
        b.put_vec(LEAVES + 0x0C, &[1.0, 0.0, 0.0]);
        b.put_vec(LEAVES + 0x18, &[0.0, 0.0, 1.0]);
        b.put_vec(LEAVES + 0x24, &[size, 0.0, 0.0]);
        b.put_vec(LEAVES + 0x30, &[size, 0.0, 0.0]);
        b.put_vec(LEAVES + 0x3C, &[0.0, size, 0.0]);
        b.put_vec(LEAVES + 0x48, &[0.0, -size, 0.0]);
        b.d[LEAVES + 0x54..LEAVES + 0x58].copy_from_slice(&[0, 0, 100, 4]);
        b.d[LEAVES + 0x58..LEAVES + 0x5C].copy_from_slice(&[0, 3, 2, 1]);
        // Leaf type: atlas cell u 0.5..1, v 0..0.5 (v up).
        b.put_vec(LEAF_TYPES + 4, &[0.5, 0.0, 1.0, 0.5, 1.0, 0.0, 0.5, 0.5]);
        b.put_f32(LEAF_TYPES + 0x30, 104.0);
        b.put_u32(LEAF_TYPES + 0x38, 1);
        // Big leaf: one LOD, one triangle.
        b.put_ptr(BIG_TABLE, BIG_OBJ);
        b.put_u32(BIG_OBJ + 0x12C, 1);
        b.put_ptr(BIG_OBJ + 0x130, BIG_LODS);
        b.put_u32(BIG_LODS, 3);
        b.put_ptr(BIG_LODS + 4, BIG_VERTS);
        b.put_u32(BIG_LODS + 8, 3);
        b.put_ptr(BIG_LODS + 12, BIG_IDX);
        for (j, p) in [[0.0, 0.0, 3.0], [1.0, 0.0, 3.0], [0.0, 1.0, 3.0]].iter().enumerate() {
            let v = BIG_VERTS + j * BIG_LEAF_VERTEX_SIZE;
            b.put_vec(v, p);
            b.put_vec(v + 12, &[0.0, 0.0, 1.0]);
            b.put_vec(v + 76, &[p[0], p[1]]);
            b.put_u16(BIG_IDX + 2 * j, j as u16);
        }
        // Material table.
        b.put_u32(BLK_END + 8, 2);
        let mut o = BLK_END + 12;
        for (slot, path) in [(0u32, "graphics\\_materials\\trunk.material.bin"), (1, "graphics\\_materials\\leaf.material.bin")] {
            b.put_u32(o, slot);
            b.put_u32(o + 4, path.len() as u32);
            b.d.extend_from_slice(path.as_bytes());
            o += 8 + path.len();
        }
        b.d
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5)
    }

    #[test]
    fn parses_synthetic_tree() {
        let t = parse(&synthetic()).unwrap();
        assert_eq!(t.name, "Test_A");
        assert_eq!((t.kind, t.version), (2, 0xD0));
        assert_eq!(t.nodes.len(), 5);
        assert_eq!(t.nodes[0].parent, None);
        assert_eq!(t.nodes[4].parent, Some(3));
        assert_eq!(t.nodes[1].children, vec![3]);
        assert!((t.nodes[0].radius - 0.2).abs() < 1e-6);
        assert_eq!(t.branches.len(), 2);
        assert_eq!(t.branches[1].parent_node, Some(1));
        assert_eq!(t.branches[0].parent_node, None);
        assert_eq!(t.leaf_types.len(), 1);
        assert_eq!(t.leaf_types[0].uv_min, [0.5, 0.0]);
        assert_eq!(t.leaf_types[0].uv_max, [1.0, 0.5]);
        assert_eq!(t.materials.len(), 2);
        assert_eq!(t.material(SLOT_LEAVES), Some("graphics\\_materials\\leaf.material.bin"));
        assert_eq!(t.material(SLOT_BIG_LEAVES), None);
        assert_eq!(t.big_leaves.len(), 1);
        assert_eq!(t.big_leaves[0].lods[0].indices, vec![0, 1, 2]);
    }

    #[test]
    fn leaf_card_is_a_diamond_along_the_branch() {
        let t = parse(&synthetic()).unwrap();
        let leaf = &t.leaves[0];
        assert_eq!(leaf.node, 3);
        // Attached 0.5 m from node 3 towards node 4.
        assert!(close(leaf.attach, [0.5, 0.0, 1.0]));
        let c = leaf.corners();
        assert!(close(c[0], [0.5, 0.0, 1.0]));
        assert!(close(c[1], [1.0, 0.0, 1.0]));
        assert!(close(c[2], [0.75, 0.25, 1.0]));
        assert!(close(c[3], [0.75, -0.25, 1.0]));
        // axis x half_width = +X x +Y = +Z.
        assert!(close(leaf.normal, [0.0, 0.0, 1.0]));
        // Codes [0,3,2,1]: attach = (umin,vmin), tip = (umax,vmax), +side = (umin,vmax), -side = (umax,vmin);
        // then v is flipped to the v-down convention.
        let part = t.build_leaves();
        assert_eq!(part.uvs, vec![[0.5, 1.0], [1.0, 0.5], [0.5, 0.5], [1.0, 1.0]]);
        assert_eq!(part.indices.len(), 6);
        // Both triangles face the card normal.
        for tri in part.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|k| part.positions[tri[k] as usize]);
            let n = cross(sub(b, a), sub(c, a));
            assert!(dot(n, leaf.normal) > 0.0);
        }
    }

    #[test]
    fn corner_codes_select_rectangle_corners() {
        // Bit 0 picks u_max, bit 1 picks v_max (v up); output is v-down.
        let lt = LeafType { uv_min: [0.25, 0.5], uv_max: [0.5, 1.0], ..Default::default() };
        assert_eq!(lt.corner_uv(0), [0.25, 0.5]);
        assert_eq!(lt.corner_uv(1), [0.5, 0.5]);
        assert_eq!(lt.corner_uv(2), [0.25, 0.0]);
        assert_eq!(lt.corner_uv(3), [0.5, 0.0]);
    }

    #[test]
    fn branch_tubes_face_outwards() {
        let t = parse(&synthetic()).unwrap();
        let opts = MeshOptions { min_sides: 4, max_sides: 4, ..Default::default() };
        let part = t.build_branches(&opts);
        // Trunk: 3 rings, branch: 2 rings, 5 vertices per ring (seam duplicated).
        assert_eq!(part.positions.len(), 5 * 5);
        assert_eq!(part.triangle_count(), (2 + 1) * 4 * 2);
        // Base ring of the trunk lies on a 0.2 m circle around the origin.
        for p in &part.positions[..5] {
            assert!(((p[0] * p[0] + p[1] * p[1]).sqrt() - 0.2).abs() < 1e-5 && p[2].abs() < 1e-6);
        }
        for tri in part.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|k| part.positions[tri[k] as usize]);
            let n = cross(sub(b, a), sub(c, a));
            if dot(n, n) < 1e-10 {
                continue; // collapsed tip
            }
            let avg_normal = add(add(part.normals[tri[0] as usize], part.normals[tri[1] as usize]), part.normals[tri[2] as usize]);
            assert!(dot(n, avg_normal) > 0.0, "inward-facing triangle {tri:?}");
        }
        // V follows tex_v (flipped), U wraps from 0 to the repeat count.
        assert_eq!(part.uvs[0], [0.0, 1.0]);
        assert_eq!(part.uvs[4], [2.0, 1.0]);
        // The trunk alone: its 3 rings, without the branch.
        let trunk = t.build_branches(&MeshOptions { max_level: Some(0), ..opts });
        assert_eq!(trunk.positions.len(), 3 * 5);
        assert_eq!(trunk.positions[..], part.positions[..15]);
    }

    #[test]
    fn build_parts_and_bounds() {
        let t = parse(&synthetic()).unwrap();
        let parts = t.build_parts(&MeshOptions::default());
        let kinds: Vec<PartKind> = parts.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, vec![PartKind::Branches, PartKind::Leaves, PartKind::BigLeaves]);
        assert!(parts[1].alpha_tested && parts[1].two_sided && !parts[0].alpha_tested);
        assert_eq!(parts[0].material.as_deref(), Some("graphics\\_materials\\trunk.material.bin"));
        // Big-leaf UVs are flipped to v-down.
        assert_eq!(parts[2].uvs[2], [0.0, 0.0]);
        let (mn, mx) = t.geometry_bounds(&MeshOptions::default()).unwrap();
        assert!(mn[2] >= -1e-6 && (mx[2] - 3.0).abs() < 1e-5);
    }

    #[test]
    fn rejects_bad_pointers() {
        let mut d = synthetic();
        // Leaf pointer outside the block.
        d[BLK + 0x34..BLK + 0x38].copy_from_slice(&(BASE + 0x10_0000).to_le_bytes());
        assert!(parse(&d).is_err());
        let mut d = synthetic();
        // Broken self pointer.
        d[BLK + ROOT_SELF_PTR_B..BLK + ROOT_SELF_PTR_B + 4].copy_from_slice(&1234u32.to_le_bytes());
        assert!(parse(&d).is_err());
        assert!(parse(&[0u8; 64]).is_err());
    }

    #[test]
    fn resolves_material_params() {
        let m = Material {
            name: "x".into(),
            shader: "RealtreeTrunk".into(),
            textures: vec![(params::NORMAL_TEXTURE_1, "n.xbt".into()), (params::DIFFUSE_TEXTURE_1, "d.xbt".into())],
            params: vec![
                (params::DIFFUSE_TILING_1, [2.0f32.to_le_bytes(), 3.0f32.to_le_bytes()].concat()),
                (params::TWO_SIDED, vec![1]),
            ],
        };
        let r = resolve_material(&m);
        assert_eq!(r.diffuse.as_deref(), Some("d.xbt"));
        assert_eq!(r.normal.as_deref(), Some("n.xbt"));
        assert_eq!(r.tiling, [2.0, 3.0]);
        assert!(r.two_sided);
    }
}
