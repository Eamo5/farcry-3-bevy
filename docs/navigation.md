# Navigation meshes (`.rnv`)

The game ships a navigation mesh for every world sector:
`worlds\<world>\levels\<level>\generated\nv\sector<id>.rnv`. That is 12,800 files and
430 MB for `fc3_main`. The sector id and level folder are the same as for the
other per-sector files (see `terrain::SectorCoord`).

## Sector file

* `NdNS` header: version 9, then a few counts and two 64-byte tables, one entry
  per 8 m cell of the 64 m sector. The tables are skipped here.
* Then one `NdNT` tile per cell and layer. Cells with a floor over a floor
  (buildings, bridges) have several layers. Each tile is preceded by a `u32`
  size.

## Tile (`NdNT`, version 8)

The layout follows Recast/Detour's `dtMeshHeader` with one extra field.

| field | type | notes |
|---|---|---|
| magic, version | `u32 x2` | `NdNT`, 8 |
| x, y | `i32 x2` | cell: `floor(coord / 8) + 640` |
| layer | `i32` | |
| polygons, vertices | `i32 x2` | |
| ? | `i32` | |
| links, detail meshes, detail verts, detail tris, bv nodes, off-mesh links, off-mesh base | `i32 x7` | |
| walkable height, radius, climb | `f32 x3` | 5.4, 0.4, 0.22 m |
| bmin, bmax | `f32 x6` | x, height, Dunia y |
| bv quantisation, ? | `f32 x2` | |
| vertices | `f32 x3` each | x, height, Dunia y (Detour's y-up) |
| polygons | 32 bytes each | see below |

A polygon:

| offset | type | field |
|---|---|---|
| 0 | `u32` | first link (`0xFFFFFFFF`: none) |
| 4 | `u16 x6` | vertex indices |
| 16 | `u16 x6` | neighbours: 0 wall, `n` the tile's polygon `n - 1`, `0x8000 \| side` into the next tile |
| 28 | `u8 x4` | ?, vertex count (3-6), flags, area |

Links (12 bytes), detail meshes (12), detail triangles (4) and the
bounding-volume tree (16) follow the polygons. They aren't needed here.

## Conversion (`fc3_formats::nav`)

`parse_rnv` reads the tiles, and `build_sector` merges them into one mesh in Bevy
space (`x, height, -Dunia y`):

* Polygons that are neighbours inside a tile become links whose portal is the
  shared edge.
* Edges into another tile lie on an axis-aligned tile border. Within the
  sector they are matched by overlap: same border line, an overlap of at least
  5 cm, and heights within 1 m. The portal is the overlap.
* Edges on the sector's own border are kept as border edges.

The result is cached as `nav/<sector id>.fcn` (`cache::CachedNav`): vertices,
polygons, links and border edges. 10,423 sectors have one (293 MB).

## In the game (`fc3_game::nav`)

* The 5 x 5 sectors around the player are read in the background. Whenever that
  set changes, they are joined into one graph, with neighbouring sectors' border
  edges matched the same way (`nav::join_borders`).
* `NavMesh::path(from, to)` finds the polygons under both ends and runs A* over
  polygon centres through portal midpoints, opening at most 6,000 polygons. It
  straightens the corridor with the funnel algorithm. If the goal can't be
  reached, it leads to the nearest polygon it found.
* Polygon selection measures distance to the surface, including its edges,
  rather than its centre. It also checks adjacent sectors. This keeps an NPC
  near a floor's edge on that floor instead of routing over an overlapping roof.
  Endpoints outside the walkable area are projected onto the selected surface.
  The funnel compares corners in the ground plane, so small height differences
  across portals do not repeat a waypoint.
* NPCs follow these paths even for nearby investigation targets or guard posts:
  a fence can separate two points only a few metres apart. Only short combat
  strafes (under 4 m) bypass the mesh. NPCs re-plan when the goal moves 2 m or
  every 1.5-2.5 s, with three searches a frame shared between them. Local
  obstacle avoidance still uses a ray-cast sidestep.
* Cover: polygon edges with no portal along them are walls. Spots 0.45 m in
  from them, every 2 m or so, are kept on an 8 m grid. An NPC in a fight takes
  the nearest spot whose wall faces its target, checked with two rays toward
  the target's eyes. The ray from 0.9 m must be blocked within 1.6 m (cover),
  and the ray from 1.6 m must be clear (a view over it). It then alternates
  between crouching (the game's `cover_system` idle) and standing to fire, and
  moves on after 12-25 s or when rushed.
* `NavMesh::height_at` gives the walkable height. NPCs stand on it where it is
  clearly above the terrain (floors and platforms) or well below it (caves).

`FC3_NAV_TRACE=1` with `RUST_LOG=info,farcry3::nav=debug,farcry3::npc=debug`
logs path endpoints, selected polygons, portals and waypoints, plus half-second
NPC movement and blocking-collider samples. It is opt-in to keep normal runs
quiet.
