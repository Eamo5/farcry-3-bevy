# Physics and collision

Notes on the collision data as this project reads it, and how the world's
static colliders are built from it.

## What collides

* Terrain: a heightfield per sector near the player (a triangle mesh where a
  sector has cave mouths), see `fc3_game::physics`.
* Placed objects, solid vegetation meshes (rocks) and tree trunks: one static
  triangle-mesh collider per model, shared by its instances. The triangles
  come from the game's own collision when the model ships an `.hkx` (below),
  else from the render mesh: its coarsest detail level without alpha-tested,
  undrawn, water or foliage materials.
* Models drawn entirely as foliage, and models whose bounds are under 0.35 m
  in radius, never block, whatever their collision.
* Models whose `.hkx` surfaces are all the logic material that leaves no
  impact (found by its name in `logicmaterials.lib`) and whose render
  mesh has nothing solid stay walk-through: the medicinal plant pickups, kelp
  and some volumes. Their shapes are cones round the plant's stem that look
  like targets for the use prompt; the file has no flag saying whether a body blocks
  characters (the collision filter info is zero everywhere). The same
  material on models that were solid already (invisible player walls, some
  volumes) keeps them solid.
* `FC3_RENDER_COLLISION=1` ignores the game's collision and builds every
  collider from the render meshes, for comparisons.

## Havok packfiles (`<model stem>.hkx`)

Most world models ship a Havok 2010.2 binary packfile beside the `.xbg`
(`fc3_formats::havok`). Layout: a 16-byte Dunia prefix, then a standard
packfile with 32-bit pointers, little endian:

| Offset | Size | Meaning |
|--------|------|---------|
| 0x00   | 8    | magic `57 E0 E0 57 10 C0 C0 10` |
| 0x10   | 4    | layout rules: pointer size (4), little endian (1), reuse padding, empty base class |
| 0x14   | u32  | section count (three: `__classnames__`, `__types__`, `__data__`) |
| 0x28   | str  | `hk_2010.2.0-r1` |
| 0x40   | 0x30 each | section headers: name[20], then absolute data start and, relative to it, local fixups, global fixups, virtual fixups, exports, imports, end |

Within `__data__`:

* local fixups, 8 bytes `(src, dst)`: pointers to plain data in the section
  (array contents, strings);
* global fixups, 12 bytes `(src, section, dst)`: pointers to objects (child
  shapes, a body's shape, storage objects);
* virtual fixups, 12 bytes `(object, section, class name offset)`: every
  object and its class. The class name offset is into `__classnames__`.

Each table is padded to 16 bytes with `0xFF`; some files end the virtual table
at the section's end without a terminator. Pointer fields hold 0 in the file;
only the fixups say where they point, which is how the layouts below were
found and are checked (an array's data or a child shape must have a fixup, and
the child must be an object of a shape class). An `hkArray` is 12 bytes:
data pointer, size, capacity with flags in the top bits.

### Rigid bodies (`hkpRigidBody`, 0x200 bytes plus name)

| Offset | Meaning |
|--------|---------|
| 0x10   | root shape (global fixup) |
| 0x2C   | collision filter info (zero in model files) |
| 0x78   | name (`hkStringPtr`, local fixup) |
| 0xE8   | motion type (u8): fixed, or one of the dynamic kinds for loose parts |
| 0xF0   | motion state transform: three rotation columns (xyz, w unused), translation at 0x120 |
| 0x130  | swept transform (centre of mass, rotation quaternions), then object radius at 0x190 |

Bodies are placed by the motion state transform (not identity for some loose
and destruction parts). Rotations are stored as columns: reading them as rows
makes noticeably more models disagree with their render bounds.

Body names say what a body is for:

* `HIGH_<name>`: the detailed body shots hit;
* `NAV_<name>`: a simplified shape for building the AI's navigation mesh;
* `STATE01…`, `STATE02_A…` (also `St1`/`St2` tokens): destruction states, the
  first being the intact model;
* anything else: the coarse body characters collide with.

`havok::collision_geometry` takes the coarse bodies, drops navigation bodies
and states after the first (as the render conversion drops later states), and
takes the `HIGH_*` bodies only when a file has no coarse body. Every model file
read so far has a coarse body.

### Shapes

Every shape starts with `hkReferencedObject` (vtable, size and reference
count), user data at 0x08 and the shape type at 0x0C. Convex shapes have their
convex radius at 0x10: the surface is the inner shape grown by that radius.

| Class | Fields |
|-------|--------|
| `hkpMoppBvTreeShape` | 0x14 MOPP code, 0x34 child shape |
| `hkpListShape` | 0x18 array of 16-byte children: shape, filter info, shape size, child count; 0x30 AABB half extents, 0x40 centre |
| `hkpConvexTranslateShape` | 0x18 child, 0x20 translation |
| `hkpConvexTransformShape` | 0x18 child, 0x20 `hkTransform` (columns at 0x20, 0x30, 0x40; translation 0x50) |
| `hkpBoxShape` | 0x20 half extents; the box is half extents plus the convex radius. Some are flat (one extent zero): floor plates |
| `hkpSphereShape` | 0x10 radius |
| `hkpCapsuleShape` | 0x10 radius, 0x20 and 0x30 segment ends (w = radius) |
| `hkpCylinderShape` | 0x10 convex radius, 0x14 cylinder radius, 0x18 base radius factor, 0x20 and 0x30 cap centres (w = full radius), 0x40 and 0x50 perpendicular axes. Centres and radius are stored shrunk by the convex radius: the full cylinder is pushed out by it along the axis and round it |
| `hkpConvexVerticesShape` | 0x20 AABB half extents, 0x30 centre, 0x40 array of `hkFourTransposedPoints` (48 bytes: four x, four y, four z; unused lanes repeat a vertex), 0x4C vertex count, 0x58 array of plane equations (outward normal, w; `n·p + w = 0` on the face, offset outward by the convex radius), 0x64 optional connectivity |
| `hkpConvexVerticesConnectivity` | 0x08 array of u16 vertex indices, 0x14 array of u8 vertices per face |
| `hkpStorageExtendedMeshShape` | see below |

#### Extended mesh shapes

`hkpStorageExtendedMeshShape`: 0x20 an embedded copy of the first triangle
subpart, 0x90 AABB half extents, 0xA0 centre, 0xB4 bits for the subpart index,
0xB8 array of triangle subparts (0x70 bytes each), 0xC4 array of shape
subparts (0x40 bytes each), 0xD0 welding info (u16 per triangle), 0xF0 array
of mesh storage pointers (one per triangle subpart), 0xFC array of shape
storage pointers.

Triangle subpart (pointers are null in the file; the storage has the data):

| Offset | Meaning |
|--------|---------|
| 0x00 | subpart type (u8, 0 = triangles), material index striding type (u8) |
| 0x08 | material index striding (u16), material count (u16) |
| 0x14 | triangle count |
| 0x1C | vertex count |
| 0x24 | vertex striding (16: `hkVector4`) |
| 0x2C | index striding in bytes (u16), index type (u8: 1 = u8, 2 = u16, 3 = u32), flip alternate triangles (u8) |
| 0x30 | extrusion |
| 0x40 | `hkQsTransform`: translation 0x40, rotation quaternion 0x50, scale 0x60 |

Shape subpart: type (1) at 0x00, 0x14 array of convex child shapes, 0x20
rotation quaternion, 0x30 translation.

`hkpStorageExtendedMeshShapeMeshSubpartStorage`: 0x08 vertices
(`hkVector4`), 0x14 u8 indices, 0x20 u16 indices, 0x2C u32 indices, 0x38 u8
material per triangle, 0x44 materials (12 bytes; user data at +8 is the
surface palette entry, see `audio.md`), 0x50 named materials, 0x5C u16
material per triangle. The model files use u16 indices four apart (the fourth
is padding). `...ShapeSubpartStorage` holds only the shape subparts'
materials.

The subpart transforms and shape subpart placements are identity in every
model file, but are applied anyway.

### Not handled

* Shapes the model files don't use (`hkpTransformShape`, plain
  `hkpExtendedMeshShape`, simple mesh shapes and the like) are reported by
  class, not read.
* Rounded edges: boxes grow by the convex radius as boxes; convex-vertices
  shapes keep their vertices (their radius is small).
* Constraints, ragdolls and the surface palette don't affect the geometry.

## Survey

`fc3tool hkx-collision [substr]` prints, per model, the bodies taken, shapes by
class, triangle count, and the collision and render bounds (both Bevy space),
with how far the collision reaches past the render box ("over") and how much
of it it leaves out ("short"). `--survey` prints totals and the worst
disagreements (`--worst N`), plus how many models stay non-solid anyway
(foliage, under the size floor).

`fc3tool col-ray <x> <y> <z> <dx> <dy> <dz> [length]` casts a ray (Dunia
coordinates) against the placed models' colliders as converted, and prints
where each model's render-derived mesh and its own collision are hit: handy
for "why did I stop here" and "why did I fall through".

Nearly all models agree with their render bounds to within a few centimetres
to a few tens of centimetres. The disagreements are expected ones: interior
and cave volumes and blockout pieces whose render mesh is a tiny placeholder,
collision-only models, models whose collision also covers a neighbouring
piece that is drawn by another model, and coarse bodies that leave out thin
decorative parts (antennas, rails, debris).

## Cache

* `collision/<hash>.col` (`cache::CachedCollision`, `cache::collision_rel`),
  `<hash>` being `hash_name` of the model path, like `models/<hash>.fcm`:
  the usual header (`FCOL`, cache version), then a `TRIS` chunk: u32 vertex
  count, u32 index count, `f32 ×3` positions in Bevy model space
  (`(x, z, -y)` of the Dunia coordinates, as the model meshes), u32 indices,
  three per triangle; then a `FLAG` chunk, a u32 whose bit 0 says every
  surface in the file's palette is the no-impact material.
* `collision/index.txt` (`cache::COLLISION_INDEX_REL`): the hashes of the
  models that have one, one per line.

The converter step (`convert_collision`) runs whenever the index is missing
and keeps `.col` files already there, so it fits into an incremental
`fc3convert` run without reconverting models. Delete the `collision` folder
to rebuild it. The game reads the index once; `ModelLoader` then reads a
model's `.col` beside its `.fcm` (static loads only) and `physics` builds the
collider from it. A model without one, or whose file fails to load, keeps the
render-derived mesh stored in its `.fcm`.
