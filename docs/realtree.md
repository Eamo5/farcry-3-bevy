# RealTree models (`.rtx`)

Far Cry 3 builds its trees, palms, bushes and large plants with Ubisoft's
RealTree system. RealTree trees can grow, burn, sway and drop branches. A `.rtx`
file does **not** contain a render mesh. It is a memory image of the runtime tree
object, made of three things:

* a **branch skeleton**: nodes with positions and radii, chained into branches;
* **leaf cards**: textured diamond-shaped quads attached to skeleton segments;
* **big leaves**: small skinned meshes for palm fronds, banana leaves, fern
  fronds and similar (palms and plants only).

The engine generates the trunk and branch tubes and the card quads at run time.
We do the same.

Decoder: `crates/fc3_formats/src/realtree.rs`. CLI: `fc3rtx`
(`crates/fc3_tool/src/bin/fc3rtx.rs`).

There are 82 files under
`graphics\__fc3_graphics\_common\vegetation\realtree\{trees,plants}\`.
All 82 parse. The rebuilt geometry reproduces the bounding box stored in 60 of
them to within 1 cm. In the remaining 22 it differs by 0.1 to 2 m. Rendered
views of koa, kawau bush, ironwood, hibiscus, mangrove, dead tree, coconut,
banana, tree fern, bamboo, monstera, phoenix palm, vine and seaweed all look
like the plants they name.

## Conventions

* Little-endian.
* All positions are in the tree's local **Dunia space**: right-handed, **Z up**,
  metres, with the origin at the foot of the tree. A koa is 27 m tall, a coconut
  palm 20 m and a kawau bush 1.9 m. Instances in the world add a yaw about +Z
  and a uniform scale (see `vegetation.md`). For Bevy, map
  `(x, y, z) -> (x, z, -y)`. This is a proper rotation, so triangle winding is
  preserved.
* Texture coordinates are stored with **v up** (v = 0 at the bottom row of the
  image). The decoder flips them (`v' = 1 - v`) for D3D/wgpu-style sampling.
  The atlas regions confirm the flip. For example, ironwood uses the casuarina
  branch in the top-left of `coastal_foliages_a`, and its stored rectangle is
  u 0..0.5, v 0.5..1. Palm fronds have their stem at v ≈ 0, which is the bottom
  of the image.

## File layout

```text
0x000  u32  resource_size   = tail_offset + 0x90
0x004  u32  physics_size    Havok blob size (0 for small bushes)
0x008  u32  0x118
0x00C  u32  0
0x010  u32  kind            0 = tree (branches + leaf cards)
                            1 = palm/plant (branches + big leaves)
                            2 = both kinds of foliage (tree fern, ladder, phoenix_a, taro, coconut_e)
0x014  u32  instance_count  always 2
0x018  u32  tail_offset     material table
0x01C  u32  version         0x8A, 0x8B, 0x8C (trees, monstera), 0xCE, 0xD0 (palms, plants)
0x020  char name[256]       e.g. "Koa_A" (several files reuse another tree's name)
0x120  u32  0, 0
0x128  instance_count x { u32 offset, u32 size, u32 id, u32 0 }
0x140  instance blocks ...
tail_offset:
       u32 ?, u32 ?         uninitialised
       u32 count
       count x { u32 slot, u32 len, char path[len] }   no terminator
       then an FCB ("nbCF") and the Havok packfiles (hk_2010.2.0-r1)
```

The file stores the tree **twice**, once per instance block. The two copies are
byte-identical except for their pointers and the first 8 bytes. In 5 files both
table entries point to the same block. The decoder reads only the first block.

The FCB after the material table lists Havok physics variants per scale (0.9,
1.0 and 1.1), each with an offset and a size into the Havok data. The Havok
data contains `hkpRigidBody`, capsule and cylinder shapes, and nodes named
`ROOT_NODE`/`MESH_NODE`. It is collision data only.

### Pointers

A block is a raw memory image. Pointers are absolute 32-bit addresses from the
moment the file was saved, for example `0x7CAF0520`. The address of file offset 0
is the **pointer base**. The base differs per file, but two root fields always
point back into the root object, so the base can be recovered:

```text
base = u32[root + 0x70] - (root_offset + 0x228)
check  u32[root + 0x6C] - base == root_offset + 0x258
file_offset = pointer - base
```

All pointers resolve inside their own block. Arrays are 16-byte aligned.

## Root object (start of the block)

Only the fields in the first group are needed for rendering. The others are
listed as observed.

| off | type | meaning |
|---|---|---|
| 0x18 | u32 | node count |
| 0x1C | ptr | nodes, 80 bytes each |
| 0x20 | ptr | node physics, 36 bytes each, parallel to the nodes |
| 0x24 | u32 | branch count |
| 0x28 | ptr | branches, 40 bytes each |
| 0x2C | ptr | branch extras, 44 bytes each (`f32 length`, then 10 × f32 that are ±1) |
| 0x30 | u32 | leaf card count |
| 0x34 | ptr | leaf cards, 92 bytes each |
| 0x38 | u32 | total LOD-0 vertex count of all big leaves |
| 0x3C | u32 | big leaf count |
| 0x40 | ptr | table of `count` pointers to big leaf objects |
| 0x44 | u32 | size of the big leaf area |
| 0x48 | u32 | big leaf object stride (the objects are fixed-size pool slots) |
| 0x74 | vec3 ×2 | bounding box of the whole tree (skeleton, cards and big leaves) |
| 0x148 | u32 | leaf type count |
| 0x14C | ptr | leaf types, 108 bytes each |
| | | |
| 0x00 | u32 | junk or hash; differs between the two copies |
| 0x08, 0x0C | u32 | small integers |
| 0x10 | f32 | 1.0 |
| 0x14 | u32 | hash (13 of the 23 plants share `0x8315C753`; the trees all differ) |
| 0x6C, 0x70 | ptr | self pointers to root+0x258 and root+0x228 (used to find the base) |
| 0x8C | vec3 | foot point (z = minimum z) |
| 0x98.. | | trunk top, crown boxes, and two bounding spheres (`vec3 centre, f32 radius` at 0xD4 and 0xEC) |
| 0x104..0x124 | f32 ×9 | per-tree physics and wind constants (for example koa: 1, 0.5, 18, 150, 100, 3.5, 20, 200, 2000) |
| 0x174..0x188 | f32 ×6 | increasing distances (koa: 20 30 40 80 145 165; kawau bush: 4 8 12 13 14 15); probably LOD switch distances |
| 0x198..0x1AC | f32 ×6 | second increasing set (koa: 30 60 100 130 180 181) |
| 0x1E0..0x1F4 | f32 ×6 | third increasing set (4 8 15 20 25 26 for most trees) |
| 0x208 / 0x20C | u32 / ptr | small array (koa: 11 entries), not decoded |
| 0x22C..0x238 | u32 | node, leaf, big leaf and big-leaf vertex counts again |
| 0x23C.. | ptr | runtime arrays: per node 48 B, per leaf 24 B, per node 12 B, per node 32 B, per leaf 32 B, ... (simulation state; not needed) |

## Node (80 bytes)

| off | type | meaning |
|---|---|---|
| 0x00 | vec3 | position |
| 0x0C | u32 | subtree size: number of nodes below this one (the root has `count - 1`) |
| 0x10 | f32 | texture U repeats around the tube; constant per branch (3 for palm and large trunks, 0.5 to 1 for twigs) |
| 0x14 | f32 | a second per-branch tiling factor; meaning unknown |
| 0x18, 0x1C | f32 | 0..1 along the branch; probably wind weights |
| 0x20 | f32 | **texture V**; grows linearly with the distance from the branch base |
| 0x24 | f32 | normalised position along the branch; can exceed 1 |
| 0x28 | f32 | unknown (0.5 on trunks, 0 at tips) |
| 0x2C | u16 | previous node: the previous node of the branch, or the parent-branch node for a branch's first node; `0xFFFF` for the root |
| 0x2E | u16 | number of side branches starting here |
| 0x30 | ptr | list of `u16` first-node indices of those branches |
| 0x34 | u16 | branch index |
| 0x36 | u16 | leaf card count on the segment starting here |
| 0x38 | u32 | first leaf card index |
| 0x3C, 0x40 | u32 | 0 |
| 0x44 | i32 | -1 |
| 0x48 | f32 | 1.0 at the trunk base, falling to 0.2 at twigs (stiffness or occlusion) |
| 0x4C | u32 | index of the node within its branch |

### Node physics (36 bytes, parallel array)

| off | type | meaning |
|---|---|---|
| 0x00 | vec2 | small values (rest bend?) |
| 0x08 | f32 | 0 |
| 0x0C | f32 | **radius** (m); about 0.001 at tips |
| 0x10 | f32 | **segment length** to the next node of the branch (0 at the tip) |
| 0x14..0x20 | f32 ×4 | spring and mass constants |

## Branch (40 bytes)

| off | type | meaning |
|---|---|---|
| 0x00 | u32 | first node |
| 0x04 | u32 | segment count; the branch uses nodes `first ..= first + segments` |
| 0x08 | i32 | parent node (-1 for the trunk). The first node's position equals the parent node's position. |
| 0x0C | u32 | level (0 = trunk) |
| 0x10, 0x14, 0x1C, 0x24 | u32 | not decoded (0x14 ∈ 2..5 might be a side count; 0x24 looks like a sort order) |
| 0x18 | i32 | -1 |

Every node belongs to exactly one branch (checked on all 82 files). Roots are
ordinary level-1 branches that grow downwards from low trunk nodes to about
-2.5 m: koa has buttress roots and mangroves have stilt roots. Clump plants
(cannabis, sugarcane) have a 1 mm "trunk" lying 0.25 m below the ground that
joins the stalks.

## Leaf card (92 bytes)

| off | type | meaning |
|---|---|---|
| 0x00 | f32 | offset: distance from the owning node along its segment to the attachment point A |
| 0x04 | u32 | 0 |
| 0x08 | f32 | size: half diagonal of the card |
| 0x0C | vec3 | axis: unit direction from A towards the card tip (≈ segment direction, randomised by up to about 45°) |
| 0x18 | vec3 | card normal (`axis × half_width`, normalised) |
| 0x24 | vec3 | `axis * size` (A to centre) |
| 0x30 | vec3 | nearly the same as 0x24 (rest copy?) |
| 0x3C | vec3 | half_width: centre to side corner, length `size`, ⟂ axis |
| 0x48 | vec3 | `-half_width` |
| 0x54 | u8[4] | flags: `[0, leaf_type, 100 or 0, n]`; byte 2 + byte 3 equals the leaf type's code |
| 0x58 | u8[4] | corner UV codes |

The owning node is found through the nodes' `first_leaf`/`leaf_count` ranges.
Every card is owned. About a fifth of the consecutive card pairs (for example
most of koa's) share A and the axis but are rotated differently about it,
forming crossed cards. The rest are independent.

**Geometry.** The card is a square diamond whose diagonal lies along the axis:

```text
A      = node.position + offset * normalize(next_node.position - node.position)
C      = A + axis * size
corners: [A, C + axis*size (tip), C + half_width, C - half_width]
triangles: (A, tip, +side), (A, -side, tip)    both face the card normal
```

The hypothesis was tested against the stored bounding box. A diamond
reproduces it exactly (koa: 0.001 m summed over the six faces). The
alternatives tested, rectangles centred on A or extending from A, are off by
1 to 9 m.

**UVs.** The four corner codes at 0x58 apply to the corners in the order
`[A, tip, +side, -side]`. Code `c` selects `u = c & 1 ? u_max : u_min` and
`v = c & 2 ? v_max : v_min` from the card's leaf type rectangle (v up). The
four code sets seen are `(0,3,1,2)`, `(0,3,2,1)`, `(1,2,0,3)` and `(1,2,3,0)`:

* The order of the last two codes follows the sign of `half_width.z`. The
  higher side corner always gets `v_max`, so the image stays upright.
* The first pair, `(0,3)` or `(1,2)`, looks random and mirrors the image
  horizontally.

With `(0,3,..)` the image's bottom-left corner, where the artists put the stem,
lands on the attachment point. Textured renders look natural with this
mapping.

## Leaf type (108 bytes)

| off | type | meaning |
|---|---|---|
| 0x00 | u32 | kind (0 or 1). In many trees one of the two types in use has kind 1; possibly a camera-facing variant (unconfirmed) |
| 0x04 | vec2 ×4 | atlas rectangle corners `(u0,v0) (u1,v1) (u1,v0) (u0,v1)`, v up |
| 0x24 | vec3 | 0.502 grey (tint) |
| 0x30 | f32 | code: 102..105 (mostly), 5 or -1 (mostly unused types) |
| 0x34 | u32 | 0 |
| 0x38 | u32 | number of cards using this type (the counts sum to the card count, except in `hy_ladder_a` where all are 0) |
| 0x3C | f32 ×12 | unknown (lighting?) |

Unused types (count 0) are probably alternative or LOD regions.

## Big leaf

Each object sits in a fixed-size pool slot (root+0x48 bytes):

```text
+0x000  u32 stride, u32 stride
+0x008  u32 node a, u32 node b       nodes the leaf hangs from
+0x010  u32 ?, u32 ?, u32 first bone, u32 ?, u32 ?, u32 ?
+0x028  char id[]                    numeric string, e.g. "3548819"
+0x12C  u32 lod_count                1..3
+0x130  ptr lod table: lod_count x { u32 vertex_count, ptr vertices, u32 index_count, ptr u16 indices }
+0x134  u32, f32 250, ...            constants
```

Vertex, 152 bytes:

| off | type | meaning |
|---|---|---|
| 0 | vec3 | position (**tree space**, ready to draw) |
| 12 | vec3 | normal |
| 24 | vec3 | about 1e-6 (unknown) |
| 36 | vec3 | tangent |
| 48 | f32 | bitangent sign (-1) |
| 52 | f32 ×6 | about 1e-6 (unknown) |
| 76 | vec2 | UV (v up) |
| 84 | vec2 | small values (\|x\| < 0.4; second UV or wind offset) |
| 92 | f32 | 1.0 near the stem down to 0.2 at the tip (shade or wind weight) |
| 96 | f32 | 0 |
| 100 | u32 | influence count (≤ 6) |
| 104 | u32 ×6 | bone indices (global across the plant's big leaves) |
| 128 | f32 ×6 | weights |

Triangles are counter-clockwise with respect to the vertex normals. All LOD-0
vertex totals match root+0x38 on all 23 plant files.

## Materials

The tail table lists `.material.bin` paths by **slot**:

| slot | shader | used by |
|---|---|---|
| 0 | `RealtreeTrunk` | branch tubes |
| 1 | `Leaf` | leaf cards (missing in monstera, which has only big leaves) |
| 2 | `BigLeaf` | big leaves |

Resolve each `.material.bin` with `material::parse`, then pick the parameters
by CRC-32 name hash (`realtree::resolve_material` does this):

| parameter | hash | notes |
|---|---|---|
| `DiffuseTexture1` | F95881AF | the colour texture. The trunk file is often named `bark_n.xbt`, which is **not** a normal map. Leaf atlases and big leaves carry the alpha-test cut-out in its alpha channel. |
| `NormalTexture1` / `NormalTexture` | 045829E2 / 8C658791 | trunk and big leaf / leaf |
| `MaskTexture` | DC662227 | big leaves. A small, almost white mask; **not** the cut-out. |
| `DiffuseTexture2` | 6051D015 | trunk detail mask (vertex colour green), e.g. `bark_mask_a.xbt` |
| `DiffuseTiling1` | E525F198 | `float2` multiplier for the trunk UVs, e.g. (2, 2) |
| `TwoSided` | 403525B2 | `u8`; 1 for foliage |

Texture paths are ordinary `.xbt` files. Use `texture::assemble_dds` to add
the separate `_mip0.xbt` top level.

## Rebuilding render geometry

`RealTree::build_parts(&MeshOptions)` returns up to three parts: `Branches`
(slot 0, opaque), `Leaves` (slot 1, alpha tested and two sided) and
`BigLeaves` (slot 2, alpha tested and two sided). Each part has positions,
normals, UVs (v down), `u32` indices and its material path.

* **Tubes.** Every branch becomes a generalised cylinder through its nodes,
  with a ring of `radius` at each node. Rings use rotation-minimising frames;
  the seam vertex is duplicated. The side count comes from the base radius
  (`2πr / tube_edge`, clamped to 3..12) and halves per LOD. U = `k / sides *
  u_repeat`, V = node `tex_v`. Tips taper to about 1 mm.
* **Cards.** One diamond per card, as described above.
* **Big leaves.** Copied from the chosen LOD.

The default options give 74k vertices and 89k triangles for all 82 trees
together. Typical counts are 2–3k triangles for a large tree and 100–200 for a
bush.

## Placed RealTrees and their growth

Most RealTrees are vegetation. Some are placed entities instead, with a
`CRealtreeComponent` naming the tree (field `0xF9F2D5F4`, the same field as a
skeleton's path): the trees the story grows, such as the doctor's
hallucination path (`RT_Hibiscus_A_2100`, `RT_Dead_A_130` and others on a
mission layer). The world parser takes that `.rtx` as the entity's model, and
it is converted and placed as any other. It counts as solid, and like a
planted tree it blocks by its trunk alone (see `docs/physics.md`); a bush
blocks nothing. The converter rewrote the
sectors holding such trees once (`sectors/stamp-realtree-entities.txt`).

The story sizes them through `CAPI_Entity`:

* `UpdateRealTreeRegen(tree, size)` sets the size at once: 0 is nothing, 1
  fully grown.
* `SetRealTreeGrowthSpeed(tree, size, speed, box, method)` moves it to that
  size at `speed`, a share of the full size a second (the sign is
  direction; the hallucination grows its trees at 0.1 and shrinks one at
  -0.2). It returns an id, and the box's method is called once the tree is
  there.
* `StopRealTreeGrowth(tree, id)` stops it where it is; it never calls back.

`fc3_story` keeps each callback until the host reports the growth done
(`Story::growth_finished`). `fc3_game::tree_growth` moves the sizes on and
draws each placed tree at its size from its foot, hidden at nothing. The
game grows a RealTree branch by branch. Here the whole tree scales, which is
an approximation.

## Known gaps

* **Growth.** Trees scale as a whole rather than extending branch by branch
  (see above).
* **Wind and animation.** The per-node weights (node 0x18/0x1C/0x24/0x28/0x48),
  the physics constants and the big-leaf skin weights are decoded only as raw
  values. Trees sway with the foliage wind instead (`vegetation`'s
  `TREE_WIND`: bending with height, leaves fluttering, as strong as the
  weather's wind), not branch by branch.
* **LOD switching.** The root has three sets of increasing distances that
  probably drive LOD and leaf thinning. `defaultrealtreeconfig.xml` has
  `TreeLODAgressivity`, `LeavesRatio` and `ShadowLeavesRatio`. Not
  implemented. Beyond `vistaMinViewDistance`, use the cluster library's
  `*_vistabill.xbg` billboard.
* **Card billboarding.** Leaf types with kind 1 might face the camera
  (rotating about the card axis). They are built as fixed cards.
* **U tiling.** Node +0x10 is used as the U repeat count around the tube
  because it gives roughly square bark texels; +0x14 is a similar field with
  unknown meaning. The angular origin of the tube texture (seam position) is
  not stored as far as we know.
* **Bounds mismatch.** 22 trees differ from the stored bounding box by 0.1 to
  2 m (hero, hala and naupaka_c the most), probably because the box was
  computed for a slightly different state of the tree.
* Not needed for rendering and not decoded: the root fields listed as
  unknown, the branch fields at 0x10/0x14/0x1C/0x24, the 12 floats per leaf
  type, the runtime arrays after root+0x23C, the second instance block, and
  the Havok data.

## Tooling

```text
fc3rtx list [filter]          one line per tree: version, kind, counts, built vertices/triangles, size
fc3rtx info <filter>          skeleton per level, trunk nodes, leaf types, materials with
                              resolved textures, built parts and bounds
fc3rtx check                  parse all .rtx files, check structure and stored bounds
fc3rtx obj <filter> <out.obj> [--part branches|leaves|bigleaves] [--lod N] [--yup]
                              Wavefront export (+ .mtl naming the diffuse textures)
```

A filter that equals a file stem (`rt_koa_a`) selects that file only. Otherwise
it is a case-insensitive substring of the path.
