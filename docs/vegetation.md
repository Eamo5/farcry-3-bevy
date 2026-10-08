# Vegetation: collection clusters

Far Cry 3 places almost all of its trees, palms, bushes, ferns, grass, seaweed,
pebbles and small clutter through the **collection** system. This is not done at
run time. The editor's collection painter ran offline, and its output is baked
into every world sector as **cluster instance lists**. Ordinary entities in
`worldsector_N.data.fcb` contain only a handful of trees (6 in level c5).

Decoder: `crates/fc3_formats/src/vegetation.rs`. CLI: `fc3veg`
(`crates/fc3_tool/src/bin/fc3veg.rs`).

## Where the data lives

```text
worlds\fc3_main\levels\<lvl>\generated\worldsectors\worldsector_<id>.data.fcb
  WorldSector
    MissionLayer (494C09F2) { missionLayerId u64 }
      Entity  hidName "SectorEntity_S<id>_Default.<hex>", class "CSectorEntity"
        Components
          CCollectionComponent (7D47618E)
            VegetationData (114CFEE7)      { dataVersion u32 = 137, bboxMin vec3, bboxMax vec3 }
              VegetationZoneData (A18C8484) { zoneId u64 (0), zoneData blob }
          CCollectionIgnitorComponent (69696FD1)   fire-spread nodes (see below)
      Entity  hidName "HDSectorEntity_S<id>_Default.<hex>"   (about 260 sectors)
        ... same structure; additional detail clusters ...
```

Every object and field name above was recovered from the CRC-32 hashes. Most
sectors have one zone. 304 sectors have two zones and 2 sectors have three.
Every zone in the shipped data uses `zoneId` 0. All instance positions are
absolute Dunia world coordinates. A cluster group can cross into a neighbouring
sector by up to about 3 m.

The model behind each instance comes from a single world-wide table:
`worlds\fc3_main\generated\clusterresourceslibrary.fcb` (see below).

## `zoneData` blob

All values are little-endian.

```text
u32 size                 blob length - 4
u32 blockCount
blockCount x Block:
    u64 clusterResId     key into clusterresourceslibrary.fcb
    u32 reserved         always 0
    u32 groupCount
    groupCount x Group:
        f32 center[3]    bounding sphere of the group (all zero for RealTree blocks)
        f32 radius
        u32 instanceCount
        instanceCount x Instance   (size depends on the resource kind)
```

The blob contains no explicit instance size. The size follows from the
resource's kind in the library:

| kind | library entry | size | layout |
|---|---|---|---|
| RealTree | `text_resClassTypeId == "CRealtreeResource"` (`.rtx`) | 20 | `f32 pos[3]`, `f32 yaw` in **degrees** about +Z, `f32 scale` |
| Grass | `CGeometryResource` with `flags & 0x80` | 24 | `f32 pos[3]`, `u8 colour[4]` (B,G,R,A), `u8 normal[4]` (B=z, G=y, R=x, A=0), `i16 dir[2]` |
| Geometry | other `CGeometryResource` | 32 | `f32 pos[3]`, `f32 euler[3]` in **radians**, `u32[2]` reserved (always `0xFFFFFFFF` where checked) |

The grass fields were derived as follows:

* **Normal.** Each byte maps `0..255` to `-1..1`. Correlation with the terrain
  normal computed from the `.sdat` heights is 0.99 for x and y, and 0.97 for z.
* **Colour.** The RGB bytes are a ground tint. Models that are not tinted
  store (64,64,64). Tinted meshgrass averages about (54,68,38). The tint
  correlates with the atlas diffuse and colour textures at the instance
  position. The renderer presumably multiplies by `value / 64`; this is
  unverified.
* **Alpha (scale).** Alpha holds the scale as `alpha * 1.4 / 255`, so 182
  means 1.0. The quantised values land exactly on round factors (0.6, 0.8,
  0.9, 1.2, 1.3, 1.4). The observed scale range is 0.5 to 1.4.
* **Direction.** `dir` is a unit 2D vector, each component `i16 / 32767`. The
  decoder reads it as the rotated +Y (forward) axis: `yaw = atan2(-x, y)`.
  The sign and axis convention is **unverified**. It only matters for grass
  meshes that are not rotationally symmetric.

Geometry blocks rotate with `R = Rz(rz) * Rx(rx) * Ry(ry)` in Dunia space. This
is the same convention as entity `hidAngles`, except that the values are in
radians. Terrain-hugging meshes such as roots match the terrain normal with a
median error of 2.5° under this convention, against 10° with no tilt.
Geometry instances carry no scale.

Positions sit on the terrain. For grass and trees, `|z - sdat height|`
averages 2 mm. Mangroves, underwater grass and seaweed legitimately sit below
sea level (100 m).

## `clusterresourceslibrary.fcb`

The file has a flat list of objects (1046 in fc3_main), one per cluster
resource. The fields are:

| field | type | meaning |
|---|---|---|
| `clusterResId` | u64 | key used by `zoneData` |
| `text_resId` / `resId` | string / u64 | model path (`.xbg` or `.rtx`) and its CRC-64 path hash |
| `text_resClassTypeId` / `resClassTypeId` | string / u32 | `CGeometryResource` or `CRealtreeResource` |
| `text_vistaResId`, `vistaResId`, `text_vistaResClassTypeId`, `vistaResClassTypeId` | | far-distance billboard mesh (`*_vistabill.xbg`), mainly for trees |
| `text_vistaShadowResId`, `vistaShadowResId`, ... | | billboard shadow mesh |
| `vistaMinViewDistance` | f32 | distance where the billboard takes over |
| `flags` | u16 | bit `0x80` = instanced as grass; other bits are LOD and physics related (unknown) |
| `text_physPathId` / `physPathId` | | collision `.hkx` |
| `text_OcclusionMeshResId` / `OcclusionMeshResId` | | occluder mesh (`occ_*.xbg`) |
| child `MaterialOverridesMap` → `{ text_MaterialSlot, MaterialSlot, text_MaterialRef, MaterialRef }` | | per-slot material overrides (mostly rocks) |

Every `clusterResId` referenced by the world resolves in this library. When the
library cannot classify a resource, the decoder falls back to trying the three
layouts with backtracking. The shipped data never needs this fallback.

The editor catalog's `CollectionResId` values are the same IDs as
`clusterResId`: 168 of the 206 non-zero IDs resolve here. The fc3_main world
data never references the other 38.

## Other collection data

`CCollectionIgnitorComponent` holds `IgnitorZonesList` → `IgnitorZone`
`{ zoneId u64, fireregBurnProfileSP u64, fireregBurnProfileMP u64, bboxMin,
bboxMax, fireNodesList (u32 count + vec3[]) , IgnitorSubZone }`. The fire nodes
lie on a 3 m grid over flammable vegetation and drive fire spreading. They are
not render data.

The per-sector `preload\sector<id>.preload.fcb` lists, per mission layer and
resource class, the CRC-64 hashes of every resource the sector needs. This
includes the `.rtx` and `.xbg` files used by its clusters, so it can drive
streaming.

## Editor catalog (`ingameeditor\collection_inventory.xml`)

This file describes how the painter generates instances. The runtime does not
need it because its results are baked. Structure:

* `Directory {Id, Display}` → `Entry {Id, Display, ZoneLogic, RootFillerStart,
  RootFillerCount, SectorCost}`.
* `Resources/Resource {Id, CollectionResId, ScaleMin?, ScaleMax?}` is the
  entry's local resource table. Other elements use the local `Id` as their
  `ResourceId`. `CollectionResId` equals `clusterResId`.
* `Fillers/Filler {Id, ResourceId, Bias}` are weighted random fill choices.
  The root fillers are `[RootFillerStart, +RootFillerCount)`.
* `Ingredients/Ingredient {ResourceId, Bias, SpawnRadius, DensityRadius,
  SlopeMax, PrefabStart/Count, FillerStart/Count, FillerExclusionRadius}`
  covers large items (for example trees). Judging by the names, an
  ingredient is placed with a minimum spacing (`DensityRadius`), only on
  slopes up to `SlopeMax` degrees. It optionally spawns a prefab or
  sub-fillers around itself.
* `Prefabs/Prefab {Id, ItemStart, ItemCount}` → `Items/Item {Id, ResourceId,
  LocalPos (x,y,z offset), SpawnRadius, FillerStart/Count, PrefabStart/Count,
  FillerExclusionRadius}` are fixed arrangements around an ingredient.
* Per-resource scale presumably comes from `[ScaleMin, ScaleMax]` (default
  1.0). The baked scales stay within 0.5 to 1.4.

None of these rules needs to be re-run: the shipped worlds contain the
generated result.

## Terrain atlas orientation (`sdat\atlas<id>_{diffuse,color,mask}.xbt`)

The atlas covers 2×2 sectors and is named after the even (south-west) sector.
Quadrants are laid out with **row = sector y parity, column = sector x
parity**, so the southern sectors are in the top half of the image. **Inside a
quadrant the image is transposed**: pixel rows run along +X (east) and columns
along +Y (north). For a sector-local position `(lx, ly)` in metres (0..64):

```text
row = qy * size/2 + lx / 64 * size/2
col = qx * size/2 + ly / 64 * size/2        (qx, qy = sector x, y parity)
```

As UVs for a sector mesh vertex at grid `(gx, gy)`, this gives
`u = (qx*64 + gy) / 128` and `v = (qy*64 + gx) / 128`. Three independent
checks support the mapping:

* Seam continuity of the mosaic, which is best among 16 candidate layouts.
* The grass tint matches the atlas colour at its position (correlation 0.28,
  against 0.00 for "north-up").
* Plots show vegetation stopping exactly at the roads and beaches drawn in the
  atlas.

`vegetation::atlas_pixel` implements the mapping.

At runtime each grass instance carries its tint in Bevy's per-instance
`MeshTag` (linear RGB scaled into bytes, top byte set), which the foliage
shader multiplies into the colour map, so all grass of a type shares one
material and draws in one batch. One material per quantised tint had put
about 150 grass materials in view at once, each a batch of its own.

## Other per-sector files (checked, not vegetation)

| file | content |
|---|---|
| `sector<id>.srl` | 1024 bytes: 32×32 `u8` region ids, 2 m cells (large uniform areas; likely sound or ambient regions) |
| `zonesector<id>.zsr` | 4096 bytes: 64×64 `u8` zone ids, 1 m cells, `0xFF` = none (gameplay zones) |
| `sector<id>.desc.fcb` | sector descriptor: minimap path, lists of related sector and metasector ids |
| `metasectors\worldsector_*.data.fcb` | spline collections and similar, no plant instances |
| `sd<id>.sdat` tail | terrain only. Tentative layout: a 65×65 2-bit vertex LOD-level map (0x445C, 1057 bytes + pad); a 65×65 `u8` map (0x4880); 672 bytes of per-cell flag bytes (0x5904); an `0xFF`-padded list of up to 7 ids (0x5BA4); four `u16` LOD vertex counts (4225/1089/289/81 + stitching extras); a `u32` count at header +0x2C of 12-byte records for 8×8 patches (`u8[4]`, `u16 minH*64`, `u16 maxH*64`, `u32 patchIndex`); then 4 cumulative record offsets, sector bounds floats and a 9×9 `f32` grid of heights × 64. The file length is 23868 + 12 × count. |

## RealTree models (`.rtx`)

Trees use a custom RealTree container, not `.xbg`. It is decoded by
`fc3_formats::realtree`; see `docs/realtree.md`. The file stores the tree's
branch skeleton, leaf cards and big-leaf (frond) meshes rather than a finished
mesh. The converter rebuilds trunk tubes, leaf cards and fronds at three
detail levels, and uses the low-detail trunk as the collision mesh. The
resource's `text_vistaResId` billboard (`*_vistabill.xbg`) stands in for the
tree's `.rtx` when that is missing. 110 of the 119 tree resources have one.
Each card is a flat polygon of about 10 vertices, 15-20 m tall, cut from a
sheet of tree images, with rounded normals (pointing down at its foot and up
at its crown) and a 1,000 m LOD distance.

The far forest uses the cards as the game does. The converter lists every
tree on the islands with its card, base and scale (`vegetation/vista.bin`,
137,306 trees on 56 cards). The game merges them into 522 meshes, one per
8×8-sector block and card sheet. Its shader (`vista.wgsl`) turns each card
about its upright to face the eye, lights it with the card's normals, and
folds away the cards in sectors whose trees are loaded as models (the 5-sector
radius, scaled by the view distance setting). So the forest runs to the
horizon and meets the near trees without overlap. The cards have no prepass
and cast no shadows; distant-land shadows and haze apply to them.

## Numbers (fc3_main)

* 12800 world-sector files, 5528 with vegetation, 0 parse errors.
* 4,032,833 instances in total: 144,927 RealTrees, 3,492,222 grass, and
  395,684 geometry.
* Per vegetated sector: about 730 instances (mean; median 779; max 3128) and
  about 26 RealTrees (mean; median 23; p90 56; max 228).
* 595 distinct models.

## Tooling

```text
fc3veg survey [level]                 parse everything, totals and sanity stats
fc3veg sector <id>                    zones, blocks, per-model counts, height check
fc3veg dump <id> [out.csv]            instance list
fc3veg plot <id> <out.png> [radius] [px_per_m]
                                      atlas background (correctly oriented), blue tint below
                                      100 m; trees = red rings, grass = yellow, other geometry
                                      = cyan, placed entities = white x
fc3veg find <model substring> [level] sectors with the most instances of a model
```
