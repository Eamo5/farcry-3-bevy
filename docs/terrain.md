# Terrain

Notes on the terrain data as this project reads it.

## Sector files (`levels\<lv>\generated\sdat\sd<id>.sdat`)

| Offset | Type      | Meaning |
|--------|-----------|---------|
| 0x04   | u32       | version (10) |
| 0x14   | u32       | sector id (`y * 160 + x`) |
| 0x1C   | f32 ×2    | min / max height |
| 0x24   | u8 ×4     | terrain layer per mask slot, **highest slot first**: byte 3 = slot 0 (mask R), byte 2 = slot 1 (G), byte 1 = slot 2 (B), byte 0 = slot 3 (remainder). `0xFF` = unused |
| 0x34   | u32       | 1: the sector has water (see Inland water) |
| 0x38   | u32       | 1: the sector has water (either flag counts) |
| 0x3C   | f32       | the water's height, also set on dry sectors beside it |
| 0x4C   | cstring   | material path (the sector's water surface material, not the ground) |
| 0x258  | 65×65 ×4  | samples: `u16` height in 1/64 m, then 2 packed normal bytes (bit 0x10 of the last byte: see below) |

Layer indices point into the world's layer table (below). Slots are listed in
ascending layer order, so a sector with layers 11, 12 and 18 stores
`FF 12 0C 0B`.

## Inland water

About 1,800 sectors store a water height at 0x3C: those with water and the dry
ones round them. A sector has water when the flag at 0x34 or 0x38 is set; along
a river some sectors carry one flag, some the other, some both, and every
sector whose ground dips below the level has at least one. The sea's (100 m)
are the ocean's. The rest are rivers and lakes above it: the C4 and D4 rivers at
120 m (stepping down to 110 m toward the sea), the northern island's river at
176 m, the C7 lake at 195 m, and others up to 230 m. Each plane covers its whole
sector. Where the ground rises above it, the ground hides it.

The converter writes the inland ones (above 100.5 m) to
`world/water_planes.txt`, one `sector_id level` per line. The game draws a flat
plane over each one near the player, using the ocean's shader (calm, murkier,
with no crest foam) over a depth texture built from the heightfield.
`water::surface(p, ground)` gives the water height at any point: an inland
plane's level where the ground lies below it, and otherwise the sea's.
Swimming, the underwater checks, boat buoyancy, flooded cars, splashes, wading
footsteps, wildlife and boat patrols all use it.

## Levels of detail

Each streamed sector is drawn at one of three grid spacings by its distance
from the player's sector: 1 m within 3 sectors, 2 m to 8, 4 m beyond
(`terrain::lod_for`). A sector loads with only the 4 m mesh; the finer ones
are built in the background as the player nears (a few a frame) and dropped
again once a coarser one is wanted, so only the sectors close by keep their
dense meshes on the GPU. Until a finer mesh is ready the nearest one built
stands in.

## Far terrain

The streamed sectors reach 16 sectors (about 1 km, scaled by the view distance
setting). Beyond them, one coarse mesh of the islands stands in
(`far_terrain.rs`): a vertex every 32 m, built from the sectors' heights and
coloured from the baked ground atlas (`terrain/far.bin`, 320² samples). Its
shader discards fragments inside the streamed square, so the two never
overlap. Only the islands are kept. The world also holds walled-off areas far
out to sea, such as a hut village north-west of the north island, test grids
and islets used for scripted sequences, which the game never shows from the
islands. The converter keeps the land joined to a sector on either island
across at most two sectors of sea, plus the shores round it. The same mask
limits the far forest's billboards.

## Horizon maps (`sd<id>_shadow.xbt`)

There is one per sector: 64×64 texels, a texel per metre. The DDS header
declares 32-bit RGB with masks `0xFFFF` and `0xFFFF0000`, so it holds two
16-bit channels (G16R16), not 8-bit colour. Like the atlas, it is transposed:
u runs north and v east.

Each channel is the elevation of the horizon seen from that point along one
direction, as `angle = (value - 0.5) * 180°`. Red looks toward 240° and green
toward 60° (counted from east toward north). `fc3tool shadow-check <sector>`
traces the horizon through the heightfield, up to a kilometre in 72
directions, and compares it with both channels. Across sectors the best
directions are 235-245° for red (correlation 0.93-0.997) and 55-60° for green
(0.92-0.995), and the mean angles agree to a degree or so (14.5° against 13.9°,
48.2° against 46.5°).

Those are the two ends of the sun's path. The world's environment settings
(`fc3_main.managers.fcb`) turn the sky about a level axis at `RotationAxisYaw`
240 (Dunia yaws count counter-clockwise from north). The sun therefore rises
toward 60°, passes overhead at noon and sets toward 240°. `SunRiseTimeHour`
is 6. The moon sits 25° up in the south at sunrise (`MoonYawAtSunrise` 180,
`MoonPitchAtSunrise` 25) and turns with the same sky. The game follows that
path (`sky::SKY_AXIS`, `SUN_RISE`).

The terrain shader uses the maps to shadow distant land. The sun is hidden
where its elevation is below the horizon on its side, which is green before
noon and red after. The term only removes sunlight the shadow cascades left,
so it matters beyond them. The loader keeps the high byte of each channel as
an RG8 image (0.7° steps). The game's own shaders also read the maps for
clustered objects (trees) out to a `MaxHemiMapDistance`. Here the converter
joins them into one world-wide atlas (`terrain/horizon.bin`, 2560² RG8 at 4 m
per texel, rows running north, about 13 MB). Trees and plants (the foliage
material, which gains a fragment shader for this) and props on the original
shaders sample the atlas, so they darken with the land around them. All three
share the test in the shader module `fc3::horizon` (`horizon.wgsl`), which
returns early wherever the sun clears the horizon. `FC3_NO_HORIZON` turns it
off.

## Environment presets

The managers file also holds the world's environment as templates in its item
database. Each has a name (field `D97A7AA9`), a GUID (`D854F1C7`) and a body of
one class: `CEnvironmentLighting`, `...Fog`, `...Exposure`, `...Sky`,
`...AdaptiveBloom`, `...AtmosphericScattering` (god rays), `...Cloud`,
`...Wind` and so on. The `DynamicEnvironmentManager` maps zones to templates
by GUID. The open-air zones (jungle, savannah and the rest) use the
`Preview_Lighting:Preview_*` set, while caves, interiors and quest areas have
their own. Curves are keyed by the time of day (0 at midnight, 0.25 at 6:00);
their knots are (time, value) plus flat Bezier handles. Gradients are (time,
RGB) knots.

`fc3_formats::environment` reads the outdoor set, and the converter writes it
to `world/environment.txt`:

- `gradSunLightColor`: black until 5:59, warming through sunrise to
  (0.92, 0.92, 0.78) by 7:00, reddening from 17:00, black again from 18:00.
  The sun takes its hue from it; its brightness still follows its elevation.
- `gradMoonLightColor`: (0.12, 0.26, 0.40) through the night; the moonlight's
  hue.
- Gradient colours are flagged `isColorType` and picked in sRGB, so they are
  decoded to linear before use: a warm yellow-white noon (linear
  (1, 1, 0.69) after normalising), a strongly orange dusk and a deep blue
  moon (linear (0.10, 0.41, 1)). Read as linear they had given a near-white
  noon, a pale dusk and a cyan moon.
- `curveLightHDRMul`: 2.5 by day, 6 by night.
- Fog (`curveFogDensity` 0.003 per metre by day, 0.007 at night, from 45 m,
  thickening to 400 m), tinted by the sky. The game's fog is all of its aerial
  perspective, and the atmosphere here already gives some, so 40% of it is
  added (`water::FOG_SHARE`; `FC3_NO_FOG` turns it off). Bevy's fog has no
  height falloff, so it thins as the eye climbs (to 30% by 450 m above the
  sea). The original-shader
  glue now sets the material's fog flag, which its blank lighting input had
  left off, so props and terrain on the game's shaders fog like the rest
  (storm fog included).
- `curveSkyLightSaturation` (1 at night, 0.65 by day): how blue the sky's
  fill light is.
- `kfgradSkyDomeColorSunSide` and `kfgradSkyDomeColorSunOppositeSide`: the
  sky dome's colour, keyed by the time of day (15 and 10 keys), each a
  gradient from the zenith (0) to the horizon (1): pale blue by day
  ((0.46, 0.60, 0.84) overhead at noon, (0.54, 0.65, 0.75) at the horizon),
  an orange band low on the sun's side at dawn and dusk, dark teal at night
  with a lighter horizon. The cloud dome draws it behind the panoramas
  (gradient position by elevation angle, the two sides mixed by how near the
  sun's bearing the view is), so it replaces the atmosphere's own sky, and
  draws the sun's disc and glow, which the atmosphere's would have. With
  the game's grade (below) the dome is lit as the game lights it: the colour
  (decoded) times the sky's multiplier (`curveSkyColorHDRMul`) times the
  exposure, which the game's own tone curve and colour cube then show as the
  game does. Without it (Bevy's AgX, `FC3_GRADE=0` or an old cache) the
  colours are taken as what the game shows on screen and go through the
  inverse of AgX (the published polynomial fit, inverted by iteration) to
  show as picked. The haze over distant land takes the horizon colour (the
  game's fog has the sky's colour, `fSkyColorFactor` 1) the same way. The
  sky's light comes from the same colours: `sky_probe.rs` draws them (as
  picked, at the sky light's saturation, a dimmed horizon colour below it)
  into a 32-texel cubemap whenever the hour, weather, exposure or shelter
  changes, and Bevy filters it into the camera's diffuse and specular
  environment light. With the game's grade it is as bright as the game's
  light probes make the shade: the sky as drawn (its multiplier and the
  exposure) times the probes' scale and multiplier (`curveProbeScale`, 0.6 by
  day and 0.9 at night, and `curveProbeHDRMul`); without it, a gain and a
  daytime boost set by eye for AgX (`FC3_SKY_PROBE_GAIN`, `FC3_SKY_FILL`). Under a roof it warms toward bounce light, as the
  ambient light does. Bevy's atmosphere still gives the aerial perspective
  and lights the scene when the cache has no presets.
- Exposure (`CEnvironmentExposure`): the eye adapts the scene toward a middle
  grey (`curveMiddleGrey`), but only within `curveExposureMin` ..
  `curveExposureMax` (a multiplier on the scene: 1.45 to 2.45 by day, 1 to 2
  at night). That range is narrow, so a typical scene sits at its lower end
  day and night. With the game's grade the light and the exposure follow
  the presets as the game's do (`sky.rs`, `NOON_SUN_LUX`): the light is the
  sun's or moon's colour times `curveLightHDRMul` in game units, at the same
  strength whatever the sun's height (its colour does the dawning), and the
  exposure is the game's adapted multiplier on it. Game units become lux at
  the noon sun's (90,000 lux) and the moon's (40 lux) brightness here, easing
  from one to the other in the few dark minutes at 6:00 and 18:00 when the
  presets hand the light over, so the EV comes out at about 14.6 by day and
  6.75 under the moon. Through the game's own tone curve that shows a
  moonlit night as the game does; the earlier setting of EV 6 by eye made
  up for AgX darkening the shadows more than the game's curve. Without the
  grade the exposure is the old time-of-day setting for AgX. Either way it
  opens up by up to 0.8 EV indoors and 1.6 EV in storms, like the game's
  adaptation (interiors have their own exposure preset). The god-ray
  preset's intensity, tint and cloud masking are constant over the day;
  only its fade angles (40 to 90 degrees off the view's centre) are used.
- Depth of field (`CEnvironmentDepthOfField`, the outdoor `..._DoF`): from
  its focus distance (practically at the eye) to its far distance (1,500 m)
  the scene takes up to a quarter (its blur scale) of a blur as wide as its
  circle of confusion (a hundredth of the view's height); the sky is spared.
  `dof.rs` draws it from the depth prepass with High effects and up: each
  pixel mixes in that much of a 12-tap disc blur that leaves out nearer,
  sharper taps. How Dunia reads the four numbers is not recorded anywhere;
  this reading gives the faint softening of far land the preset describes.
- Storms (`Default:Storm.*`): the weather blends toward a preset of its own.
  Its sky is a white dome at a much lower multiplier, its clouds take a grey
  that brightens toward noon, and its fog is thick (it stops thickening at
  150 m) and takes half the sky's colour, covering the sky itself low down
  (`fSkyFogPower`, `fSkyFogIntensity`). With the game's grade the storm
  panorama is shaded with these (its own channels: shading in green, full
  cover), the fog takes them, and the sky fades into the fog toward the
  horizon. The storm sky had drawn black before: the panorama's shading went
  through the clear-weather cloud lighting dimmed for the storm.
- The weather's rhythm (`CEnvironmentWeather`, the single-player
  `Default:StormIntensity.Default`, kept as the environment's `weather`
  line): a storm comes after a clear spell of 2,000 to 4,840 s and lasts
  120 to 240 s. `clouds::update_weather` keeps its fair-weather skies
  (light to heavy cloud, a new one every 4 to 9 minutes: what picks the
  game's cloud panorama isn't recovered). When a storm is due the clouds
  gather into heavy cloud at their usual pace, then the storm comes on over
  its transition (10 s), and its time runs from there. Before, a storm came
  one sky change in twelve, every few minutes.
- Lightning (`fLightningIntensity` 24, `fLightningSpeed` 12, every
  `fLightningMinimumTime` to `fLightningMaximumTime`, 4 to 16 s, while the
  storm is more than half on): a flash multiplies the sky's light (the
  ambient light and the light probes, less indoors), the clouds and the
  storm sky by up to 25, fading at its speed (e^(-12 t)). At night a burst of
  frames goes from a mean brightness of 36 to 135 at the flash, the storm
  clouds showing, and back within about 0.4 s. Thunder follows each flash
  (see `docs/audio.md`). `FC3_LIGHTNING_GAP=<s>` (tests) flashes that
  often.

## Render world settings

`generated\databases\generic\renderworldsettings_render\worldsettings.obj`
(`Render/WorldSettings`) holds settings the environment presets don't:

- `HDR/curveBloomResponseCurve`: how much of an exposed brightness blooms,
  nothing below about 1 rising to a cap of about one half by 3.6. With the
  game's grade, `bloom.rs` shrinks the scene to half size through it, then to
  a quarter, an eighth and a sixteenth, and the grade pass adds the four
  levels to the scene before the tone curve, as the game's combine and final
  passes do: the combine pass decodes each level (stored as a square root
  with a shared scale in alpha), sums them and stores the sum the same way,
  and the final pass decodes it and adds it times `BloomParams.x`, which the
  adaptive bloom's parameter provider fills; the outdoor preset's
  `fIntensity` is 1, with its time-of-day curve off, so the sum goes in
  whole.
  Bevy's own bloom had glowed over the whole picture; this one keeps to
  highlights, the sun's glitter on water and the sky round the sun.
- `VegetationSSS`: light through leaves. The deferred sun pass adds, for
  pixels of the vegetation class (target 1's alpha past one half), the sun
  from behind the leaf, wrapped (0.6 times the back-facing cosine plus 0.2),
  in the colour `clrColor` (a yellow green) times `BacklightStrength`, times
  an amount each material writes to target 2's alpha, all on the albedo and
  in shadow as the sun is. The converter records that amount per material
  by running its pair (grass a quarter to a half, leaves all of it; big
  leaves, ground cover and trunks are not in the class), and the foliage
  shader adds the term (`FC3_NO_SSS` leaves it out). The highlight, strength
  and shadow-strength values feed other passes and are not used.
- `SSAO` and `HBAO` strengths, which Bevy's ambient occlusion has no
  counterpart for.

## Colour grade

Dunia ends each frame with one pass that adds the bloom, maps each channel of
the exposed HDR colour through a filmic tone curve into a gamma-like encoding,
and looks the result up in a colour cube: the world names one 32³ volume
texture for it (`fileColorRemapVolumeMapTexture` on the environment manager).
The cube is a strong grade, an S-curve that lifts the mid-tones and clips the
top quarter to white, so the game's look (bright, saturated greens, deep
blues) comes from it as much as from the lighting.

The converter reads both off the installed game into the cache
(`sky/grade.bin`, see `fc3_formats::grade`): it finds the pass's pixel shader
variants by their samplers, runs them on the CPU (`d3d9_interp`) with the
bloom off and a stand-in cube that returns its coordinate, and tabulates the
curve from what comes out for grey inputs (the variants that tone-map agree;
others only encode). Nothing of the shader is copied. The cube is stored as
read. `grade.rs` then replaces Bevy's tone mapping on the player's camera
with a pass that applies the curve and the cube and writes the cube's display
values out linear, so the sRGB window shows them as the game would, with a
little dither against banding. Without the grade in the cache, or with
`FC3_GRADE=0`, the camera keeps Bevy's AgX. `fc3tool grade` prints the curve.

`fc3tool fcb <file>` names fields with identifiers taken from the installed
game's own `FC3.dll` at run time (nothing of it is stored).

## Roads and rivers (spline primitives)

The roads, tracks and rivers are `SplinePrimitiveEntity` entities in the world
sectors (about 6,000 roads and 470 river pieces), each with a primitive
component (`E2FD401E`) giving the material (`EF034902`), its draw distance
(`56FF0AED`) and a spline object that is already tessellated:

| Field      | Meaning |
|------------|---------|
| `546E67DF` | count, then cross-sections of three points each: the centre, then the two edges (Dunia space), so the ribbon follows the slope across it |
| `E0387957` | count, then per point the distance along the whole road (metres, continuing across sector pieces) |
| `B115A242` | count, then per section a weight, 0 to 1 at a road's ends (it fades in where roads meet) |
| `96F35D9E` | the half-width |

Road materials (shader `Road`) carry the road's map (`6051D015`, repeated
`7C2CA022`.x times across), a coverage map (`F95881AF`) whose alpha frays the
edges (and whose colour holds cracks on asphalt), and a dye (`974DA6CD`). River
materials use the `WaterRiver` shader, with a ripple normal map (`8C658791`),
its tile size in metres (`4D26EAD9`, 6 for calm water, 12 in rapids), a flow
speed (`94448C99`, 0.9 m/s calm, 3.9 m/s in rapids), a foam map in rapids
(`EE34B08B`) and the water's colour (`2591A0B2`). River splines lie flat at
their sector's water level. The drops between levels are the waterfall and
cascade models. The converter writes every spline to `world/splines.bin`. The
game builds roads as ribbons on the ground near the player (`splines.rs`,
`road.wgsl`), and rivers as ribbons just above their water plane that carry the
current downstream (the spline's distance increases downstream) with foam in
the rapids (`river.wgsl`).

The roads also make the GPS's network (`splines::RoadGraph`): points every
4 m along each road, linked where two roads cross and where a road's end
reaches another road. Splines usually stop short of the road they meet, by
15-25 m. So an end joins any road within 12 m, or within 30 m in a cone
ahead of it. That leaves two networks, one per island, of about 30,000 and
27,000 points, plus 41 scraps of a few hundred points at most. Before the
cone there were 143 pieces, and the largest held a quarter of the points. A
route can still jump a gap of up to 80 m from a dead end, at 2.5 times the
cost. `FC3_GPS_DEBUG` logs the pieces and where the biggest come closest.

## Atlases (`atlas<id>_<kind>.xbt`)

Each atlas covers 2×2 sectors and is named after the even-coordinate corner.
The quadrant for a sector is chosen by sector parity, with southern sectors in
the top half, and each quadrant is transposed: `u = (qx·64 + gy) / 128`,
`v = (qy·64 + gx) / 128`, where `qx, qy` are the sector's offset in the atlas
and `gx, gy` are metres within the sector.

| Kind      | Size | Contents |
|-----------|------|----------|
| `diffuse` | 256² | pre-baked low-resolution ground colour (used far away) |
| `color`   | 256² | grey colour modulation, applied ×2 (mid grey = unchanged) |
| `mask`    | 128² | layer weights: R, G, B for slots 0–2; slot 3 takes `1 − R − G − B` |

In practice R + G + B is close to 1 almost everywhere; slot 3 rarely gets weight.

## Layer table (`worlds\<world>\generated\<world>.game.xml`)

A Dunia binary XML file (see `fc3_formats::rml`): a zero byte, packed u32s
(one byte, or `0xFF` plus a little-endian u32) for an unknown value, string
table size, node count and attribute count, then the node tree depth first.
Each node is `name, value, attribute count, child count`, followed by its
attributes (`flag byte, name, value`) and children. Names and values are byte
offsets into the NUL-terminated string table at the end of the file.

`TerrainPainterLayerDescriptors` holds one `Layer` node per layer index. The
attributes used here:

| Attribute   | Meaning |
|-------------|---------|
| `Name`      | layer name; `Cliff` layers come in X/Y/Z projection variants |
| `Texture`   | tiling diffuse texture |
| `NormalMap` | tiling normal map (DXT5: X in alpha, Y in green) |
| `Tiling`    | texture repeats per 64 m sector |
| `ProjAxis`  | projection axis in Dunia space (0 = X, 1 = Y, 2 = Z / top-down) |

Other attributes (slope and altitude ranges, noise textures, gloss, surface
type) drive the editor's auto-painting and are not needed at runtime.

## In this project

The converter writes `terrain/layers.txt` (one line per layer: tiling, axis,
cached diffuse and normal textures, name) and stores each sector's four layer
slots in its `.fct` file. The game's terrain shader (`terrain_detail.wgsl`)
blends the slots' textures by the mask and multiplies by the colour atlas. It
uses triplanar mapping for cliff layers and the layers' normal maps up close.

## Holes and caves

A sample is marked cut out when the top three bits of its last byte are all
set (`0xE0`; `fc3tool sdat-bits 3 e0 all` lists the sectors). The mark settles
it:

* Every sample of 397 placeholder sectors round the world's edge carries it.
  Those sectors are flat at exactly 200 m with no material name, so the game
  cuts them entirely.
* In 386 island sectors it marks a few samples, at the foot of the terrain
  where it rises steeply over a cave's mouth. The doctor's cave
  (sector 19089) opens to the west through a one-cell strip of them, at
  x -1972 to -1971, y 2548 to 2556, where the terrain climbs 6 to 7 m in a
  metre. Its exit trigger stands just beyond. Elsewhere, slopes of 70° and
  more carry no mark.

A cell is cut when all four of its corner samples are marked
(`terrain::hole_cells`). The cached sector keeps the hole table; the mesh,
collider (then a triangle mesh) and height queries leave those cells open. A
sector cut away entirely gets no collider. The converter rewrote the sectors
with holes once (`terrain/stamp-holes.txt`).

Bit 0x10, the earlier guess, is set in only 11 sectors, in tidy rectangles.
One of them is a 10 x 7 m opening across sectors 22122 and 22282, beside a
story scene's room placeholder, with nothing walkable under it.

Most of the game's caves are separate meshes set into hillsides and under
them, so they need no hole. For example, the floor of the bear cave at -2599,
-893 is 43 m below the terrain surface, and it opens on a cliff. Besides
that, the player's safety clamp to the terrain surface
only acts within about a metre of it, and NPCs follow the navigation mesh's floor
where it lies well below the terrain. `--pos x,y,z --underground` spawns on the
floor just below z, for testing caves.
