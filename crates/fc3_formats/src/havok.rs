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
//! end). Local fixups are `(src, dst)` pointer pairs inside `__data__` (array
//! and string data); global fixups `(src, section, dst)` are pointers to
//! objects (child shapes); virtual fixups `(object, section, class name
//! offset)` list the objects.
//!
//! [`collision_geometry`] reads the full collision (every primitive and mesh
//! of the bodies characters collide with) as triangles; see
//! `docs/physics.md` for the object layouts it relies on.

use std::collections::{BTreeMap, HashMap};

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
    /// Global fixups into the data section: pointer location -> object.
    links: HashMap<u32, u32>,
    objects: Vec<(u32, String)>,
}

impl<'a> Packfile<'a> {
    fn parse(file: &'a [u8]) -> Result<Packfile<'a>> {
        let base = file.windows(8).take(64).position(|w| w == MAGIC).context("not a Havok packfile")?;
        let hk = &file[base..];
        let u32_at = |b: &[u8], o: usize| -> Result<u32> { Ok(u32::from_le_bytes(b.get(o..o + 4).context("truncated")?.try_into().unwrap())) };
        let sections = u32_at(hk, 0x14)? as usize;
        let mut names = None;
        let mut data = None;
        let mut data_index = 0;
        for i in 0..sections.min(8) {
            let h = 0x40 + i * 0x30;
            let name = hk.get(h..h + 20).context("truncated section")?;
            let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(20)];
            let f: Vec<u32> = (0..7).map(|k| u32_at(hk, h + 0x14 + k * 4)).collect::<Result<_>>()?;
            match name {
                b"__classnames__" => names = Some(f),
                b"__data__" => {
                    data = Some(f);
                    data_index = i as u32;
                }
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
        let mut links = HashMap::new();
        for o in (d[2] as usize..(d[3] as usize).saturating_sub(11)).step_by(12) {
            let (src, sec, dst) = (u32_at(section, o)?, u32_at(section, o + 4)?, u32_at(section, o + 8)?);
            if src != u32::MAX && sec == data_index {
                links.insert(src, dst);
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
        Ok(Packfile { data: section, fixups, links, objects })
    }

    fn u32(&self, o: u32) -> u32 {
        self.data.get(o as usize..o as usize + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    }

    fn u16(&self, o: u32) -> u16 {
        self.data.get(o as usize..o as usize + 2).map_or(0, |b| u16::from_le_bytes(b.try_into().unwrap()))
    }

    fn u8(&self, o: u32) -> u8 {
        self.data.get(o as usize).copied().unwrap_or(0)
    }

    fn f32(&self, o: u32) -> f32 {
        f32::from_bits(self.u32(o))
    }

    fn vec3(&self, o: u32) -> [f32; 3] {
        [self.f32(o), self.f32(o + 4), self.f32(o + 8)]
    }

    fn vec4(&self, o: u32) -> [f32; 4] {
        [self.f32(o), self.f32(o + 4), self.f32(o + 8), self.f32(o + 12)]
    }

    /// The NUL-terminated string a pointer at `o` points to.
    fn string(&self, o: u32) -> Option<&str> {
        let at = *self.fixups.get(&o)? as usize;
        let s = self.data.get(at..)?;
        std::str::from_utf8(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]).ok()
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
        self.fixups.iter().any(|(&src, &dst)| (body..end).contains(&src) && self.data.get(dst as usize..).is_some_and(|d| d.starts_with(b"HIGH_")))
    }

    fn of_class<'s>(&'s self, class: &'s str) -> impl Iterator<Item = u32> + 's {
        self.objects.iter().filter(move |(_, c)| c == class).map(|(o, _)| *o)
    }

    /// An `hkTransform` at `o`: three rotation columns, then the translation.
    fn transform(&self, o: u32) -> Affine {
        Affine { cols: [self.vec3(o), self.vec3(o + 16), self.vec3(o + 32)], t: self.vec3(o + 48) }
    }
}

/// The classes of a packfile's objects, in file order.
pub fn object_classes(file: &[u8]) -> Result<Vec<String>> {
    Ok(Packfile::parse(file)?.objects.into_iter().map(|(_, c)| c).collect())
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
        let indices: Option<(u32, u32, u32)> =
            [(0x14u32, 1u32), (0x20, 2), (0x2C, 4)].into_iter().find_map(|(off, size)| p.array(s + off).map(|(at, n)| (at, n, size)));
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
        let per_tri: Option<(u32, u32, u32)> = p.array(s + 0x38).map(|(at, n)| (at, n, 1)).or_else(|| p.array(s + 0x5C).map(|(at, n)| (at, n, 2)));
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

// ------------------------------------------------------------ collision ---

/// What characters collide with on a model, from its `.hkx`: every shape of
/// its bodies as triangles in the model's (Dunia, Z-up) space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollisionGeometry {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// Rigid bodies whose shapes are in.
    pub bodies: usize,
    /// Of those, detailed (`HIGH_*`) bodies, taken because the file has no
    /// coarse one.
    pub detail_bodies: usize,
    /// Every rigid body's name and whether its shapes are in.
    pub body_names: Vec<(String, bool)>,
    /// Leaf shapes triangulated, by class (mesh subparts count as their
    /// storage class).
    pub shapes: BTreeMap<String, usize>,
    /// Shapes that couldn't be read, by class (with the reason when the class
    /// is known but its data isn't plausible).
    pub unhandled: BTreeMap<String, usize>,
    /// The file's surface palette: a logic material (`logicmaterials.lib`
    /// index) per entry.
    pub palette: Vec<u32>,
}

impl CollisionGeometry {
    /// Bounding box of the triangles' corners.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut corners = self.triangles.iter().flatten().map(|&i| self.positions[i as usize]);
        let first = corners.next()?;
        Some(corners.fold((first, first), |(mn, mx), p| ([0, 1, 2].map(|k| mn[k].min(p[k])), [0, 1, 2].map(|k| mx[k].max(p[k])))))
    }
}

/// What a rigid body is for, from its name.
#[derive(Debug, Clone, Copy, PartialEq)]
enum BodyRole {
    /// What characters collide with.
    Coarse,
    /// `HIGH_*`: the detailed body shots hit.
    Detail,
    /// Navigation-only (`NAV_*`, for the AI's mesh) or a destruction state
    /// after the first.
    Skip,
}

fn body_role(name: &str) -> BodyRole {
    let (detail, base) = match name.strip_prefix("HIGH_") {
        Some(b) => (true, b),
        None => (false, name),
    };
    let lower = base.to_ascii_lowercase();
    if lower.starts_with("nav_") || later_state(&lower) {
        BodyRole::Skip
    } else if detail {
        BodyRole::Detail
    } else {
        BodyRole::Coarse
    }
}

/// A destruction state after the first (`STATE02_A`, `..._St2_B`): the
/// intact model is its first state, as the render meshes keep it.
fn later_state(lower: &str) -> bool {
    state_number(lower).is_some_and(|n| n >= 2)
}

/// The destruction state a body or node belongs to, from its name
/// (`STATE02_A__Remains`, `Rock01_St2_B`, `STATE001`: 2, 2, 1), if any.
pub fn state_number(name: &str) -> Option<u8> {
    name.to_ascii_lowercase().split('_').find_map(|tok| {
        let rest = tok.strip_prefix("state").or_else(|| tok.strip_prefix("st"))?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u8>().ok()
    })
}

/// Havok's motion types a body can have that don't move on their own.
pub const MOTION_KEYFRAMED: u8 = 4;
pub const MOTION_FIXED: u8 = 5;

/// Each rigid body's name and motion type (`MOTION_FIXED`, or one that
/// moves: dynamic, box or sphere inertia).
pub fn body_motions(file: &[u8]) -> Result<Vec<(String, u8)>> {
    let p = Packfile::parse(file)?;
    // hkpEntity: its motion at 0xE0, whose type is the byte at 0x8.
    Ok(p.of_class("hkpRigidBody").map(|o| (p.string(o + 0x78).unwrap_or_default().to_string(), p.u8(o + 0xE8))).collect())
}

/// Columns of a linear map and a translation.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Affine {
    cols: [[f32; 3]; 3],
    t: [f32; 3],
}

impl Affine {
    const IDENTITY: Affine = Affine { cols: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };

    fn translation(t: [f32; 3]) -> Affine {
        Affine { t, ..Affine::IDENTITY }
    }

    /// Scale, then rotation by a unit quaternion `(x, y, z, w)`, then translation.
    fn from_qst(q: [f32; 4], s: [f32; 3], t: [f32; 3]) -> Affine {
        let [x, y, z, w] = q;
        let cols = [
            [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)],
            [2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)],
            [2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)],
        ];
        Affine { cols: [0, 1, 2].map(|k| cols[k].map(|v| v * s[k])), t }
    }

    fn vector(&self, v: [f32; 3]) -> [f32; 3] {
        let c = &self.cols;
        [0, 1, 2].map(|k| c[0][k] * v[0] + c[1][k] * v[1] + c[2][k] * v[2])
    }

    fn point(&self, p: [f32; 3]) -> [f32; 3] {
        let v = self.vector(p);
        [v[0] + self.t[0], v[1] + self.t[1], v[2] + self.t[2]]
    }

    /// `self` after `inner`: maps `p` to `self(inner(p))`.
    fn after(&self, inner: &Affine) -> Affine {
        Affine { cols: inner.cols.map(|c| self.vector(c)), t: self.point(inner.t) }
    }

    /// Finite, with unit-length columns (a rotation, as `hkTransform`s are).
    fn is_rotation(&self) -> bool {
        self.t.iter().all(|v| v.is_finite()) && self.cols.iter().all(|c| (dot(*c, *c) - 1.0).abs() < 0.02)
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add_scaled(a: [f32; 3], b: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = dot(v, v).sqrt();
    (len > 1e-9 && len.is_finite()).then(|| v.map(|x| x / len))
}

/// Two unit vectors perpendicular to `axis` and each other.
fn perpendiculars(axis: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let helper = if axis[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let u = normalize(cross(axis, helper)).unwrap_or([1.0, 0.0, 0.0]);
    (u, cross(axis, u))
}

/// Faces each triangle of a convex shape away from its vertices' centre.
fn orient_outward(verts: &[[f32; 3]], tris: &mut [[u32; 3]]) {
    let n = verts.len().max(1) as f32;
    let centre = verts.iter().fold([0.0; 3], |c, v| add_scaled(c, *v, 1.0 / n));
    for t in tris.iter_mut() {
        let [a, b, c] = t.map(|i| verts[i as usize]);
        let normal = cross(sub(b, a), sub(c, a));
        let mid = [(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0];
        if dot(normal, sub(mid, centre)) < 0.0 {
            t.swap(1, 2);
        }
    }
}

/// Segments round cylinders, capsules and spheres.
const ROUND_SEGMENTS: u32 = 12;
/// Rings from a capsule's or sphere's pole to its equator.
const ROUND_RINGS: u32 = 3;

/// A box with half extents `h` about the origin, faces outward.
fn box_mesh(h: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let corners = (0..8).map(|i| [0, 1, 2].map(|k| if i & (1 << k) != 0 { h[k] } else { -h[k] })).collect();
    let quads = [[0, 4, 6, 2], [1, 3, 7, 5], [0, 1, 5, 4], [2, 6, 7, 3], [0, 2, 3, 1], [4, 5, 7, 6]];
    (corners, quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect())
}

/// A cylinder between the cap centres `a` and `b`.
fn cylinder_mesh(a: [f32; 3], b: [f32; 3], radius: f32) -> Option<(Vec<[f32; 3]>, Vec<[u32; 3]>)> {
    let axis = normalize(sub(b, a))?;
    let (u, v) = perpendiculars(axis);
    let n = ROUND_SEGMENTS;
    let mut verts = Vec::with_capacity(2 * n as usize + 2);
    for end in [a, b] {
        for k in 0..n {
            let angle = k as f32 * std::f32::consts::TAU / n as f32;
            verts.push(add_scaled(add_scaled(end, u, radius * angle.cos()), v, radius * angle.sin()));
        }
    }
    verts.push(a);
    verts.push(b);
    let mut tris = Vec::with_capacity(4 * n as usize);
    for k in 0..n {
        let k1 = (k + 1) % n;
        tris.push([k, k1, n + k1]);
        tris.push([k, n + k1, n + k]);
        tris.push([2 * n, k1, k]);
        tris.push([2 * n + 1, n + k, n + k1]);
    }
    orient_outward(&verts, &mut tris);
    Some((verts, tris))
}

/// A sphere of `radius` swept from `a` to `b` (a capsule; a sphere when they
/// meet).
fn capsule_mesh(a: [f32; 3], b: [f32; 3], radius: f32) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let segment = normalize(sub(b, a));
    let axis = segment.unwrap_or([0.0, 0.0, 1.0]);
    let (u, v) = perpendiculars(axis);
    let (n, m) = (ROUND_SEGMENTS, ROUND_RINGS);
    let mut verts = vec![add_scaled(b, axis, radius)];
    // Rings from the top pole down: the top half about `b`, the bottom about `a`.
    let mut rings = 0;
    for i in 1..2 * m {
        let lat = std::f32::consts::FRAC_PI_2 * (1.0 - i as f32 / m as f32);
        let centres: &[[f32; 3]] = match (i.cmp(&m), segment) {
            (std::cmp::Ordering::Less, _) => &[b],
            (std::cmp::Ordering::Equal, Some(_)) => &[b, a],
            (std::cmp::Ordering::Equal, None) => &[b],
            (std::cmp::Ordering::Greater, _) => &[a],
        };
        for &c in centres {
            let c = add_scaled(c, axis, radius * lat.sin());
            for k in 0..n {
                let angle = k as f32 * std::f32::consts::TAU / n as f32;
                verts.push(add_scaled(add_scaled(c, u, radius * lat.cos() * angle.cos()), v, radius * lat.cos() * angle.sin()));
            }
            rings += 1;
        }
    }
    verts.push(add_scaled(a, axis, -radius));
    let bottom = verts.len() as u32 - 1;
    let ring = |r: u32, k: u32| 1 + r * n + k % n;
    let mut tris = Vec::new();
    for k in 0..n {
        tris.push([0, ring(0, k), ring(0, k + 1)]);
        for r in 0..rings - 1 {
            tris.push([ring(r, k), ring(r + 1, k), ring(r + 1, k + 1)]);
            tris.push([ring(r, k), ring(r + 1, k + 1), ring(r, k + 1)]);
        }
        tris.push([bottom, ring(rings - 1, k + 1), ring(rings - 1, k)]);
    }
    orient_outward(&verts, &mut tris);
    (verts, tris)
}

/// The faces of a convex hull given its vertices and outward plane
/// equations (`normal . p + w = 0` on the face; planes may sit up to `slack`
/// outside the vertices).
fn hull_from_planes(verts: &[[f32; 3]], planes: &[[f32; 4]], slack: f32) -> Vec<[u32; 3]> {
    let size = verts.iter().flat_map(|v| v.iter()).fold(0.0f32, |m, x| m.max(x.abs()));
    let eps = slack + 1e-3 * size.max(1e-2);
    let mut tris = Vec::new();
    for plane in planes {
        let Some(normal) = normalize([plane[0], plane[1], plane[2]]) else { continue };
        let w = plane[3] / dot([plane[0], plane[1], plane[2]], normal);
        let mut on: Vec<u32> = (0..verts.len() as u32).filter(|&i| (dot(normal, verts[i as usize]) + w).abs() <= eps).collect();
        if on.len() < 3 {
            continue;
        }
        let k = on.len() as f32;
        let centre = on.iter().fold([0.0; 3], |c, &i| add_scaled(c, verts[i as usize], 1.0 / k));
        let (u, v) = perpendiculars(normal);
        let angle = |i: &u32| {
            let d = sub(verts[*i as usize], centre);
            dot(d, v).atan2(dot(d, u))
        };
        on.sort_by(|a, b| angle(a).total_cmp(&angle(b)));
        on.dedup_by(|a, b| verts[*a as usize] == verts[*b as usize]);
        for j in 1..on.len().saturating_sub(1) {
            tris.push([on[0], on[j], on[j + 1]]);
        }
    }
    tris
}

/// Triangle soup being built, in the model's space.
#[derive(Default)]
struct TriangleSoup {
    positions: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
}

impl TriangleSoup {
    /// Adds `tris` over `verts` placed by `xf`, without degenerate triangles.
    fn add(&mut self, xf: &Affine, verts: &[[f32; 3]], tris: impl IntoIterator<Item = [u32; 3]>) {
        let placed: Vec<[f32; 3]> = verts.iter().map(|&v| xf.point(v)).collect();
        // Only the vertices triangles use are kept.
        let mut slots = vec![u32::MAX; placed.len()];
        for t in tris {
            if t.iter().any(|&i| i as usize >= placed.len()) {
                continue;
            }
            let [a, b, c] = t.map(|i| placed[i as usize]);
            let n = cross(sub(b, a), sub(c, a));
            if !(dot(n, n) > 1e-14) {
                continue;
            }
            let t = t.map(|i| {
                let slot = &mut slots[i as usize];
                if *slot == u32::MAX {
                    *slot = self.positions.len() as u32;
                    self.positions.push(placed[i as usize]);
                }
                *slot
            });
            self.triangles.push(t);
        }
    }
}

/// Walks a body's shape tree, triangulating the shapes it reaches.
struct ShapeWalker<'p, 'a> {
    p: &'p Packfile<'a>,
    class_of: HashMap<u32, &'p str>,
    soup: TriangleSoup,
    shapes: BTreeMap<String, usize>,
    unhandled: BTreeMap<String, usize>,
}

impl ShapeWalker<'_, '_> {
    fn done(&mut self, class: &str) {
        *self.shapes.entry(class.to_string()).or_default() += 1;
    }

    fn bad(&mut self, what: &str) {
        *self.unhandled.entry(what.to_string()).or_default() += 1;
    }

    /// Follows the child shape pointer at `ptr`.
    fn child(&mut self, ptr: u32, xf: Affine, depth: u32, parent: &str) {
        match self.p.links.get(&ptr) {
            Some(&c) => self.shape(c, xf, depth + 1),
            None => self.bad(&format!("{parent} (missing child)")),
        }
    }

    /// Triangulates the shape object at `o`, placed by `xf`.
    fn shape(&mut self, o: u32, xf: Affine, depth: u32) {
        let p = self.p;
        let class = self.class_of.get(&o).copied().unwrap_or("(not an object)");
        if depth > 32 {
            return self.bad(&format!("{class} (nested too deep)"));
        }
        // Every shape starts hkReferencedObject (8), user data, type; convex
        // ones then have their convex radius at 0x10.
        let radius = p.f32(o + 0x10);
        let plausible = |v: f32| v.is_finite() && v.abs() < 1e4;
        match class {
            "hkpMoppBvTreeShape" => self.child(o + 0x34, xf, depth, class),
            "hkpListShape" => match p.array(o + 0x18) {
                // ChildInfo: shape, filter info, shape size, child count.
                Some((at, n)) => (0..n).for_each(|i| self.child(at + i * 16, xf, depth, class)),
                None => self.bad(&format!("{class} (no children)")),
            },
            "hkpConvexTranslateShape" => self.child(o + 0x18, xf.after(&Affine::translation(p.vec3(o + 0x20))), depth, class),
            "hkpConvexTransformShape" => {
                let t = p.transform(o + 0x20);
                if !t.is_rotation() {
                    return self.bad(&format!("{class} (bad transform)"));
                }
                self.child(o + 0x18, xf.after(&t), depth, class)
            }
            "hkpStorageExtendedMeshShape" => self.extended_mesh(o, xf, depth),
            "hkpBoxShape" => {
                let h = p.vec3(o + 0x20);
                // Some are flat (a floor plate with no thickness).
                if !(plausible(radius) && radius >= 0.0 && h.iter().all(|&v| plausible(v) && v >= 0.0) && h.iter().filter(|&&v| v + radius > 0.0).count() >= 2)
                {
                    return self.bad(&format!("{class} (bad extents)"));
                }
                // The convex radius pads the box all round.
                let (verts, tris) = box_mesh(h.map(|v| v + radius));
                self.soup.add(&xf, &verts, tris);
                self.done(class);
            }
            "hkpSphereShape" => {
                if !(plausible(radius) && radius > 0.0) {
                    return self.bad(&format!("{class} (bad radius)"));
                }
                let (verts, tris) = capsule_mesh([0.0; 3], [0.0; 3], radius);
                self.soup.add(&xf, &verts, tris);
                self.done(class);
            }
            "hkpCapsuleShape" => {
                let (a, b) = (p.vec3(o + 0x20), p.vec3(o + 0x30));
                if !(plausible(radius) && radius > 0.0 && a.iter().chain(&b).all(|&v| plausible(v))) {
                    return self.bad(&format!("{class} (bad data)"));
                }
                let (verts, tris) = capsule_mesh(a, b, radius);
                self.soup.add(&xf, &verts, tris);
                self.done(class);
            }
            "hkpCylinderShape" => {
                // Cap centres and radius are stored shrunk by the convex radius.
                let (cyl, a, b) = (p.f32(o + 0x14), p.vec3(o + 0x20), p.vec3(o + 0x30));
                let ok = plausible(radius) && radius >= 0.0 && plausible(cyl) && cyl >= 0.0 && a.iter().chain(&b).all(|&v| plausible(v));
                let mesh =
                    normalize(sub(b, a)).filter(|_| ok).and_then(|axis| cylinder_mesh(add_scaled(a, axis, -radius), add_scaled(b, axis, radius), cyl + radius));
                match mesh {
                    Some((verts, tris)) => {
                        self.soup.add(&xf, &verts, tris);
                        self.done(class);
                    }
                    None => self.bad(&format!("{class} (bad data)")),
                }
            }
            "hkpConvexVerticesShape" => self.convex_vertices(o, xf, radius),
            _ => self.bad(class),
        }
    }

    /// `hkpConvexVerticesShape`: vertices as transposed blocks of four, plane
    /// equations, and (optionally) the faces' vertex loops.
    fn convex_vertices(&mut self, o: u32, xf: Affine, radius: f32) {
        const CLASS: &str = "hkpConvexVerticesShape";
        let p = self.p;
        let Some((blocks, nb)) = p.array(o + 0x40) else { return self.bad(&format!("{CLASS} (no vertices)")) };
        let n = p.u32(o + 0x4C).min(nb * 4);
        let verts: Vec<[f32; 3]> = (0..n)
            .map(|i| {
                let at = blocks + (i / 4) * 48 + (i % 4) * 4;
                [p.f32(at), p.f32(at + 16), p.f32(at + 32)]
            })
            .collect();
        if verts.len() < 4 || verts.iter().flatten().any(|v| !(v.is_finite() && v.abs() < 1e4)) {
            return self.bad(&format!("{CLASS} (bad vertices)"));
        }
        let mut tris = Vec::new();
        // Connectivity: vertex indices (u16) and each face's vertex count (u8).
        if let Some(&conn) = p.links.get(&(o + 0x64)) {
            if let (Some((idx, ni)), Some((counts, nf))) = (p.array(conn + 0x08), p.array(conn + 0x14)) {
                let mut at = 0;
                for f in 0..nf {
                    let k = p.u8(counts + f) as u32;
                    if at + k > ni {
                        break;
                    }
                    let face: Vec<u32> = (at..at + k).map(|j| p.u16(idx + j * 2) as u32).collect();
                    at += k;
                    if face.iter().all(|&i| i < n) {
                        tris.extend((1..face.len().saturating_sub(1)).map(|j| [face[0], face[j], face[j + 1]]));
                    }
                }
            }
        }
        if tris.is_empty() {
            let planes: Vec<[f32; 4]> = match p.array(o + 0x58) {
                Some((at, np)) => (0..np).map(|i| p.vec4(at + i * 16)).collect(),
                None => Vec::new(),
            };
            tris = hull_from_planes(&verts, &planes, radius.max(0.0));
        }
        if tris.is_empty() {
            return self.bad(&format!("{CLASS} (no faces)"));
        }
        orient_outward(&verts, &mut tris);
        self.soup.add(&xf, &verts, tris);
        self.done(CLASS);
    }

    /// `hkpStorageExtendedMeshShape`: triangle subparts (0x70 bytes each, at
    /// 0xB8) whose vertices and indices live in the mesh storage objects (at
    /// 0xF0), and shape subparts (0x40 bytes each, at 0xC4) holding convex
    /// children.
    fn extended_mesh(&mut self, o: u32, xf: Affine, depth: u32) {
        const CLASS: &str = "hkpStorageExtendedMeshShape";
        const STORAGE: &str = "hkpStorageExtendedMeshShapeMeshSubpartStorage";
        let p = self.p;
        let storages = p.array(o + 0xF0);
        if let Some((at, n)) = p.array(o + 0xB8) {
            for i in 0..n {
                let storage = storages.filter(|s| i < s.1).and_then(|(s, _)| p.links.get(&(s + i * 4)).copied());
                match storage.filter(|s| self.class_of.get(s) == Some(&STORAGE)) {
                    Some(s) => self.triangles_subpart(at + i * 0x70, s, xf),
                    None => self.bad(&format!("{CLASS} (subpart without storage)")),
                }
            }
        }
        if let Some((at, n)) = p.array(o + 0xC4) {
            for i in 0..n {
                let sub = at + i * 0x40;
                let place = Affine::from_qst(p.vec4(sub + 0x20), [1.0; 3], p.vec3(sub + 0x30));
                if !place.is_rotation() {
                    self.bad(&format!("{CLASS} (bad shape subpart transform)"));
                    continue;
                }
                let xf = xf.after(&place);
                if let Some((kids, k)) = p.array(sub + 0x14) {
                    (0..k).for_each(|j| self.child(kids + j * 4, xf, depth, CLASS));
                }
            }
        }
    }

    /// One triangle subpart: its index striding and transform, with the
    /// vertices (`hkVector4`) and indices of its storage.
    fn triangles_subpart(&mut self, sub: u32, storage: u32, xf: Affine) {
        const STORAGE: &str = "hkpStorageExtendedMeshShapeMeshSubpartStorage";
        let p = self.p;
        let count = p.u32(sub + 0x14);
        let index_stride = p.u16(sub + 0x2C) as u32;
        let flip_alternate = p.u8(sub + 0x2F) != 0;
        let place = Affine::from_qst(p.vec4(sub + 0x50), p.vec3(sub + 0x60), p.vec3(sub + 0x40));
        // Index type: 1 = u8, 2 = u16, 3 = u32 (storage arrays at 0x14, 0x20, 0x2C).
        let (size, array) = match p.u8(sub + 0x2E) {
            1 => (1, 0x14),
            2 => (2, 0x20),
            3 => (4, 0x2C),
            _ => return self.bad(&format!("{STORAGE} (unknown index type)")),
        };
        let (Some((verts_at, nv)), Some((idx, ni))) = (p.array(storage + 0x08), p.array(storage + array)) else {
            return self.bad(&format!("{STORAGE} (no geometry)"));
        };
        let per = index_stride / size;
        if per < 3 || !place.t.iter().chain(place.cols.iter().flatten()).all(|v| v.is_finite()) {
            return self.bad(&format!("{STORAGE} (bad striding or transform)"));
        }
        let verts: Vec<[f32; 3]> = (0..nv).map(|i| p.vec3(verts_at + i * 16)).collect();
        if verts.iter().flatten().any(|v| !(v.is_finite() && v.abs() < 1e5)) {
            return self.bad(&format!("{STORAGE} (bad vertices)"));
        }
        let index = |k: u32| match size {
            1 => p.u8(idx + k) as u32,
            2 => p.u16(idx + k * 2) as u32,
            _ => p.u32(idx + k * 4),
        };
        let tris = (0..count).filter(|t| t * per + 2 < ni).map(|t| {
            let k = t * per;
            let tri = [index(k), index(k + 1), index(k + 2)];
            if flip_alternate && t % 2 == 1 {
                [tri[0], tri[2], tri[1]]
            } else {
                tri
            }
        });
        self.soup.add(&xf.after(&place), &verts, tris);
        self.done(STORAGE);
    }
}

/// Each rigid body's name and the raw bytes of its entity from `from` to
/// `to` (for working out field layouts).
pub fn body_bytes(file: &[u8], from: u32, to: u32) -> Result<Vec<(String, Vec<u8>)>> {
    let p = Packfile::parse(file)?;
    Ok(p.of_class("hkpRigidBody")
        .map(|o| {
            let name = p.string(o + 0x78).unwrap_or_default().to_string();
            let bytes = (from..to).step_by(4).flat_map(|i| p.u32(o + i).to_le_bytes()).collect();
            (name, bytes)
        })
        .collect())
}

/// Each object of a class: its offset, the raw words from `from` to `to`,
/// and the links its words hold (for working out field layouts).
pub fn object_words(file: &[u8], class: &str, from: u32, to: u32) -> Result<Vec<(u32, Vec<u32>, Vec<(u32, String)>)>> {
    let p = Packfile::parse(file)?;
    let class_of: HashMap<u32, &str> = p.objects.iter().map(|(o, c)| (*o, c.as_str())).collect();
    Ok(p.of_class(class)
        .map(|o| {
            let words = (from..to).step_by(4).map(|i| p.u32(o + i)).collect();
            let links = (from..to)
                .step_by(4)
                .filter_map(|i| p.links.get(&(o + i)).map(|t| (i, class_of.get(t).map_or(format!("@{t:x}"), |c| c.to_string()))))
                .collect();
            (o, words, links)
        })
        .collect())
}

/// Every shape of a model's `.hkx` that characters collide with, as
/// triangles in the model's (Dunia, Z-up) space.
///
/// Bodies are placed by their motion state's transform. Coarse bodies are
/// taken; the detailed `HIGH_*` ones only when the file has no coarse body.
/// Navigation-only bodies (`NAV_*`) and destruction states after the first
/// are left out.
pub fn collision_geometry(file: &[u8]) -> Result<CollisionGeometry> {
    let p = Packfile::parse(file)?;
    let bodies: Vec<(u32, String, BodyRole)> = p
        .of_class("hkpRigidBody")
        .map(|o| {
            let name = p.string(o + 0x78).unwrap_or_default().to_string();
            let role = body_role(&name);
            (o, name, role)
        })
        .collect();
    let has_coarse = bodies.iter().any(|b| b.2 == BodyRole::Coarse);
    let bodies = bodies.into_iter().map(|(o, name, role)| {
        let take = match role {
            BodyRole::Coarse => true,
            BodyRole::Detail => !has_coarse,
            BodyRole::Skip => false,
        };
        (o, name, role, take)
    });
    Ok(gather(&p, bodies))
}

/// What characters collide with once the model is broken: its last state's
/// bodies that stay put (the remains, not the pieces that fall), and the
/// bodies it has in every state; coarse ones, else detailed. None for a
/// model without destruction states.
pub fn broken_collision(file: &[u8]) -> Result<Option<(u8, CollisionGeometry)>> {
    let p = Packfile::parse(file)?;
    let bodies: Vec<(u32, String, Option<u8>, bool, u8)> = p
        .of_class("hkpRigidBody")
        .map(|o| {
            let name = p.string(o + 0x78).unwrap_or_default().to_string();
            let base = name.strip_prefix("HIGH_").unwrap_or(&name);
            let detail = base.len() != name.len();
            (o, name.clone(), state_number(base), detail || base.to_ascii_lowercase().starts_with("nav_"), p.u8(o + 0xE8))
        })
        .collect();
    let Some(last) = bodies.iter().filter_map(|b| b.2).max().filter(|&s| s >= 2) else { return Ok(None) };
    let wanted = |b: &(u32, String, Option<u8>, bool, u8)| b.2.is_none_or(|s| s == last) && b.4 != MOTION_KEYFRAMED && (b.2.is_none() || b.4 == MOTION_FIXED);
    let has_coarse = bodies.iter().any(|b| wanted(b) && !b.3);
    let chosen = bodies.iter().map(|b| (b.0, b.1.clone(), if b.3 { BodyRole::Detail } else { BodyRole::Coarse }, wanted(b) && (!b.3 || !has_coarse)));
    Ok(Some((last, gather(&p, chosen))))
}

/// The shapes of the bodies taken, placed by their motion states.
fn gather(p: &Packfile, bodies: impl Iterator<Item = (u32, String, BodyRole, bool)>) -> CollisionGeometry {
    let mut out = CollisionGeometry::default();
    let mut walker = ShapeWalker {
        p,
        class_of: p.objects.iter().map(|(o, c)| (*o, c.as_str())).collect(),
        soup: TriangleSoup::default(),
        shapes: BTreeMap::new(),
        unhandled: BTreeMap::new(),
    };
    for (o, name, role, take) in bodies {
        out.body_names.push((name, take));
        if !take {
            continue;
        }
        // hkpEntity: the collidable's shape at 0x10; the motion state's
        // transform at 0xF0.
        let Some(&shape) = p.links.get(&(o + 0x10)) else {
            walker.bad("hkpRigidBody (no shape)");
            continue;
        };
        let placed = p.transform(o + 0xF0);
        if !placed.is_rotation() {
            walker.bad("hkpRigidBody (bad transform)");
            continue;
        }
        walker.shape(shape, placed, 0);
        out.bodies += 1;
        out.detail_bodies += (role == BodyRole::Detail) as usize;
    }
    out.positions = walker.soup.positions;
    out.triangles = walker.soup.triangles;
    out.shapes = walker.shapes;
    out.unhandled = walker.unhandled;
    // Palette: hkReferencedObject, then hkArray<(char* name, u32 logic)>.
    if let Some((at, n)) = p.of_class("nomadSurfaceTypePalette").next().and_then(|o| p.array(o + 8)) {
        out.palette = (0..n).map(|i| p.u32(at + i * 8 + 4)).collect();
    }
    out
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

/// Name per logic material index (empty where there's none), from
/// [`LOGIC_MATERIALS`].
pub fn logic_material_names(lib: &[u8]) -> Result<Vec<String>> {
    const INDEX: u32 = 0xD16D191D;
    const NAME: u32 = 0x9D8873F8;
    let start = lib.windows(4).position(|w| w == b"nbCF").context("no FCB header")?;
    let root = crate::fcb::parse(&lib[start..])?;
    let mut out = vec![String::new(); 256];
    let mut max = 0;
    for m in &root.children {
        let (Some(&[i]), Some(name)) = (m.field_h(INDEX), m.field_h(NAME)) else { continue };
        out[i as usize] = String::from_utf8_lossy(name).trim_end_matches('\0').to_string();
        max = max.max(i as usize + 1);
    }
    out.truncate(max);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal packfile: objects laid out in `__data__` with their
    /// pointer fixups.
    #[derive(Default)]
    struct Pack {
        data: Vec<u8>,
        local: Vec<(u32, u32)>,
        global: Vec<(u32, u32)>,
        objects: Vec<(u32, &'static str)>,
    }

    impl Pack {
        fn alloc(&mut self, size: usize) -> u32 {
            self.data.resize(self.data.len().next_multiple_of(16), 0);
            let at = self.data.len() as u32;
            self.data.resize(self.data.len() + size, 0);
            at
        }

        fn object(&mut self, class: &'static str, size: usize) -> u32 {
            let at = self.alloc(size);
            self.objects.push((at, class));
            at
        }

        fn put(&mut self, at: u32, bytes: &[u8]) {
            self.data[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
        }

        fn f32s(&mut self, at: u32, v: &[f32]) {
            for (i, x) in v.iter().enumerate() {
                self.put(at + 4 * i as u32, &x.to_le_bytes());
            }
        }

        /// An `hkArray` at `at` over fresh data of `n` elements of `size`
        /// bytes; returns where the data is.
        fn array(&mut self, at: u32, n: u32, size: usize) -> u32 {
            let d = self.alloc(n as usize * size);
            self.local.push((at, d));
            self.put(at + 4, &n.to_le_bytes());
            self.put(at + 8, &(n | 0x8000_0000).to_le_bytes());
            d
        }

        fn link(&mut self, at: u32, to: u32) {
            self.global.push((at, to));
        }

        fn shape(&mut self, class: &'static str, radius: f32) -> u32 {
            let o = self.object(class, 0x70);
            self.f32s(o + 0x10, &[radius]);
            o
        }

        fn boxed(&mut self, h: [f32; 3], radius: f32) -> u32 {
            let o = self.shape("hkpBoxShape", radius);
            self.f32s(o + 0x20, &h);
            o
        }

        fn wrap(&mut self, class: &'static str, child: u32, at: u32, v: &[f32]) -> u32 {
            let o = self.shape(class, 0.0);
            self.link(o + 0x18, child);
            self.f32s(o + at, v);
            o
        }

        fn body(&mut self, name: &str, shape: u32, translation: [f32; 3]) -> u32 {
            let b = self.object("hkpRigidBody", 0x200);
            self.link(b + 0x10, shape);
            let s = self.alloc(name.len() + 1);
            self.put(s, name.as_bytes());
            self.local.push((b + 0x78, s));
            self.f32s(b + 0xF0, &[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
            self.f32s(b + 0x120, &translation);
            b
        }

        fn build(&self) -> Vec<u8> {
            let mut names: Vec<u8> = Vec::new();
            let mut name_at: HashMap<&str, u32> = HashMap::new();
            for (_, c) in &self.objects {
                name_at.entry(c).or_insert_with(|| {
                    names.extend_from_slice(&[0, 0, 0, 0, 9]);
                    let at = names.len() as u32;
                    names.extend_from_slice(c.as_bytes());
                    names.push(0);
                    at
                });
            }
            let words = |v: &mut Vec<u8>, w: &[u32]| w.iter().for_each(|x| v.extend_from_slice(&x.to_le_bytes()));
            let pad = |v: &mut Vec<u8>, with: u8| v.resize(v.len().next_multiple_of(16), with);
            let mut section = self.data.clone();
            pad(&mut section, 0);
            let local = section.len() as u32;
            self.local.iter().for_each(|&(s, d)| words(&mut section, &[s, d]));
            pad(&mut section, 0xFF);
            let global = section.len() as u32;
            self.global.iter().for_each(|&(s, d)| words(&mut section, &[s, 2, d]));
            pad(&mut section, 0xFF);
            let virt = section.len() as u32;
            self.objects.iter().for_each(|&(o, c)| words(&mut section, &[o, 0, name_at[c]]));
            pad(&mut section, 0xFF);
            let end = section.len() as u32;
            // Dunia prefix, Havok header, three section headers.
            let mut hk = MAGIC.to_vec();
            hk.resize(0x40, 0);
            hk[0x14..0x18].copy_from_slice(&3u32.to_le_bytes());
            let names_start = 0x40 + 3 * 0x30;
            let data_start = names_start + names.len().next_multiple_of(16) as u32;
            for (name, fields) in [
                ("__classnames__", [names_start, names.len() as u32, names.len() as u32, names.len() as u32, 0, 0, names.len() as u32]),
                ("__types__", [data_start; 7].map(|_| 0)),
                ("__data__", [data_start, local, global, virt, end, end, end]),
            ] {
                let mut h = name.as_bytes().to_vec();
                h.resize(20, 0);
                words(&mut h, &fields);
                hk.extend_from_slice(&h);
            }
            hk.extend_from_slice(&names);
            pad(&mut hk, 0);
            hk.extend_from_slice(&section);
            let mut file = vec![0u8; 16];
            file.extend_from_slice(&hk);
            file
        }
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3)
    }

    /// Every triangle faces away from the soup's centre (convex inputs).
    fn faces_out(g: &CollisionGeometry) -> bool {
        let (mn, mx) = g.bounds().unwrap();
        let centre = [0, 1, 2].map(|k| (mn[k] + mx[k]) * 0.5);
        g.triangles.iter().all(|t| {
            let [a, b, c] = t.map(|i| g.positions[i as usize]);
            let mid = [0, 1, 2].map(|k| (a[k] + b[k] + c[k]) / 3.0);
            dot(cross(sub(b, a), sub(c, a)), sub(mid, centre)) > 0.0
        })
    }

    #[test]
    fn boxes_are_placed_by_wrappers_and_body() {
        let mut p = Pack::default();
        let b = p.boxed([1.0, 2.0, 3.0], 0.5);
        let t = p.wrap("hkpConvexTranslateShape", b, 0x20, &[10.0, 0.0, 0.0]);
        p.body("Crate", t, [0.0, 0.0, 5.0]);
        // The surface palette: (name, logic material) pairs.
        let palette = p.object("nomadSurfaceTypePalette", 0x20);
        let entries = p.array(palette + 8, 2, 8);
        p.put(entries + 4, &7u32.to_le_bytes());
        p.put(entries + 12, &9u32.to_le_bytes());
        let g = collision_geometry(&p.build()).unwrap();
        assert_eq!((g.bodies, g.triangles.len(), g.positions.len()), (1, 12, 8));
        let (mn, mx) = g.bounds().unwrap();
        assert!(close(mn, [8.5, -2.5, 1.5]) && close(mx, [11.5, 2.5, 8.5]), "{mn:?} {mx:?}");
        assert!(faces_out(&g));
        assert!(g.unhandled.is_empty());
        assert_eq!(g.palette, vec![7, 9]);
    }

    #[test]
    fn transform_shape_rotates_by_columns() {
        let mut p = Pack::default();
        let b = p.boxed([1.0, 2.0, 3.0], 0.0);
        // 90 degrees about Z: x -> y, y -> -x; then up 1.
        let t = p.wrap("hkpConvexTransformShape", b, 0x20, &[0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        p.body("Crate", t, [0.0; 3]);
        let (mn, mx) = collision_geometry(&p.build()).unwrap().bounds().unwrap();
        assert!(close(mn, [-2.0, -1.0, -2.0]) && close(mx, [2.0, 1.0, 4.0]), "{mn:?} {mx:?}");
    }

    #[test]
    fn coarse_bodies_win_over_detailed_navigation_and_later_states() {
        let mut p = Pack::default();
        let s = p.shape("hkpSphereShape", 1.0);
        let big = p.boxed([5.0; 3], 0.0);
        p.body("Thing", s, [0.0; 3]);
        p.body("HIGH_Thing", big, [0.0; 3]);
        p.body("NAV_Thing", big, [0.0; 3]);
        p.body("STATE02_A__Debris", big, [0.0; 3]);
        let g = collision_geometry(&p.build()).unwrap();
        assert_eq!((g.bodies, g.detail_bodies, g.body_names.iter().filter(|b| b.1).count()), (1, 0, 1));
        let (mn, mx) = g.bounds().unwrap();
        assert!(close(mn, [-1.0; 3]) && close(mx, [1.0; 3]), "{mn:?} {mx:?}");
        assert!(faces_out(&g));

        // Only a detailed body: it stands in.
        let mut p = Pack::default();
        let big = p.boxed([5.0; 3], 0.0);
        p.body("HIGH_Thing", big, [0.0; 3]);
        let g = collision_geometry(&p.build()).unwrap();
        assert_eq!((g.bodies, g.detail_bodies, g.triangles.len()), (1, 1, 12));
    }

    #[test]
    fn state_names() {
        assert!(later_state("state02_a__debris") && later_state("rock01_st2_b") && later_state("st2_base") && later_state("state03"));
        assert_eq!(
            [state_number("STATE02_A__Remains_Static"), state_number("STATE001"), state_number("Rock01_St2_B"), state_number("Stairs02")],
            [Some(2), Some(1), Some(2), None]
        );
        assert!(!later_state("state01__full") && !later_state("stairs02") && !later_state("st1_bbq") && !later_state("stone_wall2"));
        assert_eq!(body_role("HIGH_State02_B"), BodyRole::Skip);
        assert_eq!(body_role("NAV_Fence"), BodyRole::Skip);
        assert_eq!(body_role("HIGH_Fence"), BodyRole::Detail);
    }

    #[test]
    fn list_mopp_and_extended_mesh_subparts() {
        let mut p = Pack::default();
        // A quad at z = 0 in a triangle subpart, u16 indices four apart.
        let storage = p.object("hkpStorageExtendedMeshShapeMeshSubpartStorage", 0x70);
        let verts = p.array(storage + 0x08, 4, 16);
        for (i, v) in [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [4.0, 4.0, 0.0], [0.0, 4.0, 0.0]].iter().enumerate() {
            p.f32s(verts + i as u32 * 16, v);
        }
        let idx = p.array(storage + 0x20, 8, 2);
        for (i, k) in [0u16, 1, 2, 0, 0, 2, 3, 0].iter().enumerate() {
            p.put(idx + i as u32 * 2, &k.to_le_bytes());
        }
        let mesh = p.object("hkpStorageExtendedMeshShape", 0x110);
        let sub = p.array(mesh + 0xB8, 1, 0x70);
        p.put(sub + 0x14, &2u32.to_le_bytes());
        p.put(sub + 0x2C, &8u16.to_le_bytes());
        p.put(sub + 0x2E, &[2]);
        p.f32s(sub + 0x40, &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
        let store = p.array(mesh + 0xF0, 1, 4);
        p.link(store, storage);
        // A shape subpart: a box turned 90 degrees about Z and raised 1.
        let b = p.boxed([1.0, 2.0, 0.5], 0.0);
        let shapes = p.array(mesh + 0xC4, 1, 0x40);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        p.f32s(shapes + 0x20, &[0.0, 0.0, half, half, 0.0, 0.0, 1.0, 0.0]);
        let kids = p.array(shapes + 0x14, 1, 4);
        p.link(kids, b);
        let mopp = p.shape("hkpMoppBvTreeShape", 0.0);
        p.link(mopp + 0x34, mesh);
        // And a list of that and a capsule.
        let capsule = p.shape("hkpCapsuleShape", 0.5);
        p.f32s(capsule + 0x20, &[0.0, 0.0, 3.0, 0.5, 0.0, 0.0, 5.0, 0.5]);
        let list = p.shape("hkpListShape", 0.0);
        let children = p.array(list + 0x18, 2, 16);
        p.link(children, mopp);
        p.link(children + 16, capsule);
        p.body("Shed", list, [0.0; 3]);
        let g = collision_geometry(&p.build()).unwrap();
        assert!(g.unhandled.is_empty(), "{:?}", g.unhandled);
        assert_eq!(g.shapes.get("hkpStorageExtendedMeshShapeMeshSubpartStorage"), Some(&1));
        assert_eq!(g.shapes.get("hkpBoxShape"), Some(&1));
        assert_eq!(g.shapes.get("hkpCapsuleShape"), Some(&1));
        let (mn, mx) = g.bounds().unwrap();
        assert!(close(mn, [-2.0, -1.0, -0.0]) && close(mx, [4.0, 4.0, 5.5]), "{mn:?} {mx:?}");
        // The quad's two triangles are in.
        assert!(g.triangles.iter().filter(|t| t.iter().all(|&i| g.positions[i as usize][2] == 0.0)).count() >= 2);
    }

    #[test]
    fn convex_vertices_from_connectivity_or_planes() {
        let cube: Vec<[f32; 3]> = (0..8).map(|i| [0, 1, 2].map(|k| if i & (1 << k) != 0 { 1.0 } else { -1.0 })).collect();
        for with_faces in [true, false] {
            let mut p = Pack::default();
            let s = p.shape("hkpConvexVerticesShape", 0.01);
            let blocks = p.array(s + 0x40, 2, 48);
            for (i, v) in cube.iter().enumerate() {
                let at = blocks + (i as u32 / 4) * 48 + (i as u32 % 4) * 4;
                p.f32s(at, &[v[0]]);
                p.f32s(at + 16, &[v[1]]);
                p.f32s(at + 32, &[v[2]]);
            }
            p.put(s + 0x4C, &8u32.to_le_bytes());
            let planes = p.array(s + 0x58, 6, 16);
            for (i, n) in [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]].iter().enumerate() {
                p.f32s(planes + i as u32 * 16, &[n[0], n[1], n[2], -1.01]);
            }
            if with_faces {
                let conn = p.object("hkpConvexVerticesConnectivity", 0x20);
                let quads: [u16; 24] = [0, 4, 6, 2, 1, 3, 7, 5, 0, 1, 5, 4, 2, 6, 7, 3, 0, 2, 3, 1, 4, 5, 7, 6];
                let idx = p.array(conn + 0x08, 24, 2);
                for (i, k) in quads.iter().enumerate() {
                    p.put(idx + i as u32 * 2, &k.to_le_bytes());
                }
                let counts = p.array(conn + 0x14, 6, 1);
                p.put(counts, &[4; 6]);
                p.link(s + 0x64, conn);
            }
            p.body("Rock", s, [0.0; 3]);
            let g = collision_geometry(&p.build()).unwrap();
            assert_eq!(g.triangles.len(), 12, "with faces: {with_faces}");
            let (mn, mx) = g.bounds().unwrap();
            assert!(close(mn, [-1.0; 3]) && close(mx, [1.0; 3]));
            assert!(faces_out(&g));
        }
    }

    #[test]
    fn cylinders_take_their_convex_radius_and_unknown_shapes_are_reported() {
        let mut p = Pack::default();
        let c = p.shape("hkpCylinderShape", 0.1);
        p.f32s(c + 0x14, &[0.4]);
        p.f32s(c + 0x20, &[0.0, 0.0, 0.1, 0.5, 0.0, 0.0, 1.9, 0.5]);
        let odd = p.shape("hkpTransformShape", 0.0);
        p.body("Post", c, [0.0; 3]);
        p.body("Sign", odd, [0.0; 3]);
        let g = collision_geometry(&p.build()).unwrap();
        let (mn, mx) = g.bounds().unwrap();
        assert!(close(mn, [-0.5, -0.5, 0.0]) && close(mx, [0.5, 0.5, 2.0]), "{mn:?} {mx:?}");
        assert!(faces_out(&g));
        assert_eq!(g.unhandled.get("hkpTransformShape"), Some(&1));
    }
}
