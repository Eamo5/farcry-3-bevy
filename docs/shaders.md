# Shaders

Far Cry 3's DX9 renderer ships its shaders precompiled in `shadersobj.fat`. This
project translates them to WGSL during the up-front conversion so the game can
run them on wgpu. Nothing from the game's shaders is stored in this repository;
the translations live only in the local cache.

## Shader objects (`engine\shaders\obj\h??\x????????.vso|.pso`)

The archive holds about 34,500 entries:

- 14,284 D3D9 shader objects (2,197 vertex, 12,087 pixel; all shader model 3.0)
- 15,278 D3D11 (`DXBC`) shaders for the DX11 renderer (not used here)
- render-state snippets (`.rs`, text such as `ColorWriteEnable=0`) and an
  index (`index.rs`)

The file name is a 32-bit id; the `h??` folder is its low 7 bits. Only about 8%
of names are in the community file lists, so the converter walks the archive
entries directly.

A D3D9 shader object (decoder: `fc3_formats::d3d9`):

| Offset | Type | Meaning |
|--------|------|---------|
| 0      | u32  | magic `0x1608068D` |
| 4      | u32  | offset of the D3D9 bytecode |
| 8      | u32  | `record count << 8 \| 4` |
| 12     | 8 × count | constant records |
| …      |      | standard D3D9 token stream (version token, instructions, end token) |

Each record is `u32 CRC-32 of the constant name`, `u8 kind` (low 6 bits: 8 =
float, 9 = matrix, 12 = sampler, …; high 2 bits: low bits of the register),
`u8 register / 4`, `u8 rows` (0 when this program doesn't use it) and
`u8 columns`. So the register is `(byte5 << 2) | (byte4 >> 6)`. There is no
standard D3DX constant table; names are known only by hash. Many hashes match
identifiers that appear as strings in the game's own binaries, which is how
the engine-provided constants (view and projection matrices, camera position,
fog, sun) were identified.

The constant layout is shared: engine globals occupy the low registers in the
same place in every shader (for example the view-projection matrix in
`c4–c7` and the camera position in `c38`), and material and per-object values
follow.

## Rendering model (as observed)

The DX9 renderer is deferred. Material pixel shaders write four render targets.
Lighting passes (ambient, sun, point and spot lights) read them back as
`GBufferT0`–`GBufferT2` together with the depth buffer:

- `GBufferT0.rgb` holds albedo in a gamma-like encoding; lighting squares it to
  get linear colour.
- `GBufferT1.rgb` holds the world-space normal stored unsigned (scale and bias,
  then renormalise); its alpha is also used as a material class flag.
- The sun term is the saturated dot product of that normal with the negated
  sun direction, times the sun colour.
- The light passes read the glossiness from `GBufferT2.r`, as a Blinn-Phong
  exponent of 2^(13 g), and the specular strength from `GBufferT0.a` (twice
  it, up to one half; above that it marks other material classes);
  `GBufferT1.a` above about one half marks a class the shadows touch less. A
  strand-highlight variant (hair) reads its exponent from `GBufferT2.a`.

### Surfaces (gloss and reflectance)

The paired permutations write the gloss to target 2 as the light passes read
it, but no specular strength to target 0's alpha (the specular-map variants
the game would pick by material flags are not the ones that validate), so the
strength comes from the materials' parameters.
Their descriptors give `Glossiness` (0..1), `SpecularReflectance` (F0, a few
per cent) and, for generic props, `AmbientSpecularMultiplier` (how much of the
surroundings they reflect). `ShaderSet::surface` reads them, with the
family's defaults for what a material leaves out, turns the glossiness into the
light passes' exponent, 2^(13 g), and that into Bevy's perceptual roughness
(the GGX roughness with the same highlight width), and stores roughness, reflectance
and the ambient fraction in pixel shader constant `c254`, beyond what a
ps_3_0 shader can address. The glue lights with them in place of its matte
default (roughness 0.75), and scales the sky's reflection by the ambient
fraction, as the game lights its ambient specular. Where the paired shader
writes target 2, the glue takes the gloss and reflectance from it per pixel
instead: the shader works them out from the material's maps (a metal with a
glossiness of 1 writes 0.84 under its gloss map, a generic prop its
glossiness times its map, and its specular reflectance in blue), which is
exactly what the game's light passes read. Families without a
glossiness (skin, cloth, simple, hair) keep the default. Materials drawn with
Bevy's standard material (beyond `FC3_ORIG_RANGE`, and vegetation) keep their
old roughness: the standard material reflects all of the sky, which turned
glossy leaves silver. `fc3tool gloss-survey` shows, per family, what the
paired shaders write to those channels (`FC3_SURVEY_PARAMS` lists each
material's gloss and reflectance).

Static world geometry is drawn instanced: the per-instance transform arrives in
vertex streams (`TEXCOORD2`–`TEXCOORD5`). Some vertex shader families take a
`WorldMatrix` constant instead, and skinned ones a bone palette.

## Translation (`fc3_formats::d3d9_wgsl`)

- Registers become `vec4` variables; `a0` is `vec4<i32>`, `p0` is `vec4<bool>`.
- All float, integer and boolean constants are read from one uniform block
  (`d3d_c.f[n]`, `d3d_c.i[n]`, `d3d_c.b[n/4][n%4]`). `def` constants override
  it, as in D3D9. Because every read has the same `d3d_c.f[n]` form, the
  runtime can redirect particular registers (engine globals, per-object
  transforms) by substitution.
- Sampler `s#` binds a texture at `16 + 2#` (2D), `48 + 2#` (cube) or `80 + 2#`
  (volume), with its sampler at the next binding.
- Vertex attributes get locations in declaration order. Varyings get fixed
  locations from their semantics (`TEXCOORD0–9` → 0–9, `COLOR0–1` → 10–11,
  `FOG` → 12, …), so any vertex shader links with any pixel shader that reads a
  subset of its outputs.
- Instructions follow the D3D9 definitions (for example `rsq` and `log` take the
  absolute value, `pow` uses the absolute base, `cmp` selects on `>= 0`).
- WGSL only allows implicit-derivative sampling in uniform control flow. The
  translator tracks uniformity (branches on boolean constants and fixed loops
  stay uniform). Elsewhere it samples the base mip level and drops
  derivatives. Only 1 of the 14,284 shaders needs that fallback.

Every translation is validated with naga using the same checks wgpu applies.
All 14,284 pass.

## Cache

- `shaders/index.txt`: one line per shader with its stage, location in the
  pack, inputs and outputs by semantic, samplers, named bindings and colour
  outputs (see `cache::ShaderEntry`).
- `shaders/wgsl.pak`: deflate-compressed WGSL modules without entry points
  (172 MB of WGSL packs into about 22 MB). The game adds entry points and
  bindings for its own pipeline when it loads one.

Tools: `fc3tool shader-dis <path|hash>`, `fc3tool shader-wgsl <path|hash>`,
`fc3tool shader-validate`, `fc3tool shader-survey`.

## Running the original shaders (`fc3_game/src/original_shaders.rs`)

World props render with the translated shaders by default (`--standard-shaders`
or F4 switches to plain materials). The engine computes its vertex/pixel pairing
at runtime from technique tables that aren't in the data. So the converter
pairs each material up front (`fc3_formats::original`) and bakes the result
into the cached material:

1. **Pixel shader candidates.** These are the G-buffer writers (four render
   targets, 2D samplers only) that sample the material's colour map and whose
   material samplers the material provides. They are ranked by how many of
   their material bindings the material supplies. A binding is the engine's
   when more than an eighth of all shaders use it, with two corrections. The
   hashes were named against the game's own strings. `DiffuseTexture`,
   `NormalTexture` and `DiffuseTexture1` (`EE34B08B`, `8C658791`,
   `F95881AF`) are common enough to look global but are material textures.
   So are the constants `DiffuseColor1` and an unnamed normal-map strength
   (`92B269B5`), which default to 1 when a material leaves them out. Before
   these corrections, no shader that samples only a diffuse and a normal map
   could pair, which excluded all of `Simple`. Every shader's dye and layer
   tint also read 0, so a layered `Generic_FC3` floor showed black wherever
   its mask picked the first layer.
2. **Vertex shader candidates.** These take static-mesh inputs and a
   `WorldMatrix` constant, and write every input the pixel shader reads. The
   inputs are position, normal, colour, UV0/UV1, and the tangent frame, which
   the game streams as PSIZE (tangent) and FOG (binormal). The per-shader
   dataflow summary checks how each pixel input is used:
   - one read only as a texture coordinate must be fed from the vertex UVs or
     position;
   - one only normalised must be fed from the normal or tangent frame;
   - one used both ways (a vertex colour blending layers, say) only has to be
     fed from some vertex input.

   This matters because the game reuses `TEXCOORDn` slots for different data
   in different shader families. The summary is tracked per component, and a
   write replaces what the register held, except inside branches, loops and
   predicated instructions. A register-wide union once made a UV look
   normalised whenever its temporary was later reused for the normal. That hid
   every `Simple` pair, and it let a camera-relative ambient colour pass as the
   normal for 125 `Metal` materials. Up to 24 vertex shaders are tried per pixel
   shader, smallest first, because many small ones put the UV in a different
   register.
3. **Validation on the CPU.** Each candidate pair runs on sample vertices in a
   small D3D9 bytecode interpreter (`fc3_formats::d3d9_interp`). It gets the
   material's constants, and texture reads return each texture's average colour.
   A pair is accepted only if:
   - the albedo it writes is finite and matches the colour map's brightness and
     hue, and
   - the colour map is read at coordinates that follow the vertex UV or position.
     A pair that reads it at one fixed spot would render a flat colour.

   Dyed materials (character cloth: a grey map tinted by colour parameters
   through a mask) change the hue on purpose. A pair rejected only for hue
   still counts when it passes with every colour parameter (an opaque RGB in
   0..1) set to white, since then the shift comes from the material's own
   colours. Without this, such materials fell through to a pair that ignored
   their tints and rendered grey.

   Whether render target 1 decodes to a sensible world normal is recorded as a
   flag. When it doesn't, the runtime lights with the mesh normal.
4. **Skinned vertex shaders.** The same pixel shader is also tried with the
   skinned vertex shaders (bone palette inputs; every bone is set to identity
   for the check). A pixel shader that works with both a static and a skinned
   vertex shader is preferred. The binding stores both, and skinned parts load
   the material through their own asset source, which picks the skinned vertex
   shader and the vertex layout with joints.
5. **Terrain** uses one fixed pair picked by structure (`ShaderSet::terrain_pair`):
   the smallest G-buffer terrain pixel shader without the sector-shadow and
   neighbour variants, and the smallest terrain vertex shader with a sector UV
   transform. The game feeds it up to four detail layers chosen by the sector's
   atlas mask, plus the colour atlas, and beyond about 250 m fades to the baked
   atlas diffuse.

Of the 3,762 cached materials, 3,747 get a validated pair. In a village scene,
98% of prop parts draw with the original shaders, up from 56% before the
corrections above and the cut-outs and overlays below. Of the rest, 9 are
never drawn (no-draw volumes, the cloud dome), one is the grey placeholder
below, and 5 have no pixel shader candidate: 2 `EffectEmissive` glows
(forward-pass effects, not G-buffer shaders), 2 `Generic_FC3` whose colour
map is the debug checker, and a river surface; none of the 5 is placed in the
world. `fc3tool drawn-with <material> [all]` lists the placements that draw
a material once their own material sets apply (and marks those in the
developers' test area in the far south-east corner): the grey placeholder
is left on the islands only on two small pieces, a quad under a pier and a
corner of a sandbag hut, which the entities themselves give the
placeholder; the shipped game shows its debug texture there (a white and
grey checker reading "Placeholder"), which is drawn plain grey here.

Stand-in textures. A family's material descriptor names, for some samplers,
what the engine binds when a material leaves that texture out
(`replaceTexture`), and the engine's code maps those names to its own
textures: white, black, normal, transparent and perlinNoise to
`Generic\Mask\whiteMask4x4`, `blackMask4x4`, `normalMask4x4` (a flat normal in
the game's normal map layout: x in alpha, y in green), `transparentMask4x4`
and `perlin`. A sampler with no stand-in that every shader of the family
reads (no `useDefine` switches it off) the engine leaves unbound after
loading the material (it walks the samplers without a texture and binds
only stand-ins), and an unbound D3D9 sampler reads opaque black, as
`blackMask4x4` holds. A material no pair takes as it is gets those textures
for the samplers it leaves out and is paired again; that gave a rope, a
dog's coat and four `Blended` overlays, all without normal maps, their pairs,
and the materials that name no texture at all theirs: drawn black where
their colour map is unbound, as the game draws them (a torture table,
sandbags, two fish), with the standard material further off black too. The debug
folders' checker and `invisible` maps stay out as placeholders, but their
plain grey is kept: two props are painted with it (tinted to two-thirds), and
had drawn white.

The last 13 to pair
(9 `Hair`, 3 `Simple`, a `Metal`) failed on brightness only because their own
colour darkens the map: a `Simple` material with a black `DiffuseColor` is
drawn black by the game. So when no pair passes as the material is, a second
pass takes one rejected for brightness that passes with the material's colours
whitened. (Allowed in the first pass, it displaced the pairs of 81 materials
that already had one matching their real colours.) Validation also rejects a
pair whose pixel shader kills the sample pixel (`texkill`); with vertex
outputs defaulting as on D3D9 (below), none does. `fc3tool unpaired
<cache>/materials` lists what draws without a pair; `fc3tool pair-survey`
reports which unpaired cached materials would pair now, and with
`FC3_SURVEY_REPAIR` pairs every material again and counts what would change
(nothing, for the current cache: the 13 pairs the output default changed were
regenerated).
`fc3tool pair-survey` reports the counts and rejection reasons, and
`fc3tool fcmat <file>` prints the binding baked into one cached material.

Material switches. Two boolean parameters change render state rather than shader
constants. They are named by CRC-32, like every material parameter:

- `TwoSided` (`403525B2`) turns off back-face culling. Character cloth, fences and
  thin props use it. Honouring the two switches changed the flags of 619 cached
  materials.
- `AlphaTestEnabled` (`7ABB06A6`) cuts the surface out where the colour output's
  alpha is low. In D3D9 this is render state, not shader code, so the glue fragment
  shader discards below 0.5 itself, and plain materials use an alpha mask.

The converter turns both into material flags. On two-sided surfaces, back faces
flip the geometric normal so they are lit from their own side. Water is recognised
by its `Water` shader. Foliage is recognised by its shader, or by words like
"leaf" or "grass" in the material's file name, but only when the name is
descriptive. Most material files are named `<author>-m-<date>`, and one author's
name starts with "grass": 670 of his materials, mostly weapons and equipment,
were once treated as alpha-tested foliage.

Dyes. `DiffuseColor1` (`0E44F777`) and `DiffuseColor2` (`974DA6CD`) are colour
parameters that dye the diffuse maps (`Simple` uses `DiffuseColor`, `9015351F`:
250 of its materials tint the map, and the standard material beyond the
original shaders' range now does too, so far props keep the colour they have
up close). Of 1,477 `Generic_FC3` materials, 1,037 set
`DiffuseColor1` to something other than white. One example is the DShK's
grips: a near-white tiling map dyed brown. `Metal` materials set both colours and
blend them by the mask. Now that `DiffuseColor1` reaches the shaders as a material
constant, the paired pixel shaders read the dye for nearly every dyed material.
`fc3tool pair-survey` counts 987 dyed `Generic_FC3` materials that read it and 3
that don't, and 423 against 6 for `Metal`.

For the few that don't, the dye is applied outside the shader, for the families
that dye the whole map (`Generic_FC3`, `Hair`, `Blended`, `Metal`; see
`material::single_dye`):

- Plain materials multiply their base colour by it, clamped to 0..2.
- Paired materials get it in pixel constant `c255`, which is beyond the 224
  registers a ps_3_0 shader can address, with `w = 1`. The glue multiplies
  target 0 by it. This is skipped when one of the pair's own constants already
  carries the dye.

`Metal` authors its dye around 0.5 for none, at twice the dye. `fc3tool param-survey
0e44f777 Metal` gives a histogram that peaks at 0.5-0.7, where `Generic_FC3` peaks at 1.0.
Outside the shader the glue applies `min(2 * DiffuseColor1, 1)`, which only darkens.
A constant-table register is a vec4 index plus a component, so `14.0` is `c56` and
`17.1` is `c69`. `fc3tool param-survey <hash> [shader]` counts how materials set
any float parameter. With a shader, it also gives a histogram of the parameter's
mean. `fc3tool ctab-regs <register> [ps|vs]` lists the parameters that constant
tables put at a register, counted in the same vec4 units. `fc3tool tex-params
[all]` counts the texture (or all) parameter hashes materials use.
`fc3tool slot-names <text>` lists model material slots by name.

Water masks. Boats carry a slot named `ZMask_Boat`: an unlit, black-dyed surface
inside the hull that only keeps the sea out in the game. The converter maps every
`ZMask*` slot to a never-drawn material, so the hull shows its own deck instead.

Placed particle effects (6,722 entities) name their effect in component `53D53912`, field
`1943B130`, by library path (`fc3_biome_waterfall.clear_waterfall.waterfall_hitsplash`).
`fc3tool fx-survey [depth]` counts them by name. `cache::world_fx_kind` keeps 4,421 of
them, written to `world/effects.txt`:

- waterfall and rapids spray, or mist for the `mist` and `fume` names (1,774);
- the `fc3_area_myst` and `area_mist` mist patches (1,512);
- glowing flies (875);
- candles (170) and torches (90).

The list keeps each one's rotation and effect name too (`kind x y z qx qy qz qw
name`, after the `# fc3-world-fx 2` header; an older list is written again). The
converter caches each named effect (109 of them; one the library lacks is
skipped). `world_fx` plays each as authored (`authored_fx`), turned as placed,
when it comes within its kind's reach, and takes it away a little past that.
Fireflies play only after dark. Fires set down with an encounter's props play
their kind's usual effect (`camp_fire`, `camp_fireext`, `candlelight`, the
torch's). Where an effect isn't cached, the particle
flipbooks stand in. A waterfall's splashes fade out past 30 to 40 m by their own
far clipping. Left out: quest and cinematic effects, lamp glows (the lamps draw
their own), interiors, and the rest. `FC3_FX_DEBUG` logs how many effects play
and how many particles live, every 2 s.

Authored effects. Some effects are played as their definitions have them
(`authored_fx`), not as flipbook stand-ins. `particle_effects::AUTHORED_EFFECTS`
names them: the race gates' `waypointflare` and `finishflare`, and the blasts
(`particle_effects::blast`). The converter caches each one's `PartSys` record
whole, under the hash of its name (`particle_effects::authored_rel`), with its
active emitters' diffuse textures (not heat haze's). `fc3tool particle-effect
<name>` prints a definition.

Blasts. An `Explosion` carries its effect, and `weapons::explode` plays it (our
fireball and smoke stand in when it isn't cached). Each blast's effect is its
source's `psExplosion` in the entity library (a weapon's or explosive's
`Particles`):

- frag grenades, thrown or launched, and the grenade takedown: `grenade_frag_core`;
- explosive arrows: `grenade_arrow_core`;
- rockets: `rpg7_core`;
- C4 and the story's detonations: `exp_ied_core`;
- mines: `exp_proximitymine_core`;
- molotovs bursting: `molotov_explosion`;
- a flamer's tank: the molotov pirates' `molotov_chestexplosion`;
- cars: `engine_explosion_newversion`, the 4x4's;
- boats: the patrol boat's;
- helicopters crashing: `choper_explosion`;
- explosive props, by name: `barrel_explosion`, `barrel_directional_explosion`,
  `propanetank01_explosion`, `liquidpropanetank_explosion` and
  `oxygentank_explosion`.

At the water's surface, `generic_underwater_explosion` plays instead. These are
impact effects: their frame's +Y (Dunia's) is the ground's normal, up. An
effect none of whose emitters loop goes once the last has had its time; what it
emitted lives out.

Bullet impacts. `materialimpacts.lib` lists, for each kind of impact, the effect
each logic material makes. A material's `disMatId` is its `disId` in
`logicmaterials.lib`; the effect's name sits next to its `psParticlesSystem`, in
field `31491B5F`. The converter writes the `Weapon/Bullet` kind to
`particles/bullet-impacts.txt` (65 materials, 26 effects: dirt, grass, bark,
metal, wood, sand, mud, water, flesh...) and caches each effect.

A bullet's end asks the surface probe what it struck: the terrain's dominant
layer, or a model's collision material at that point (`surfaces::material_at`).
It plays that material's effect along the hit normal, with our bullet hole as
the decal. Bodies take `Base/Flesh_P`, or `Base/Flesh_P_Head` for a headshot,
facing back along the shot. Vehicles take `Base/Metal_Vehicle`, and a round
through the sea takes `Base/Water_P_Deep`. Effects played once run their loops
once (deep water's ring). Our dust and sparks stand in when nothing is cached.

Muzzles. A weapon's `CWeaponProperties` `Particles` name its muzzle's effects:
first person (`psMuzzleParticlesId`, name in `B171F78F`), third person
(`4C222D81`) and the smoke after (`81F9EF25`). A suppressor gadget's modifiers
name its own. The weapons' table keeps them (`weapons/stats.txt`, version 9;
the placeholder effect counts as none), and the converter caches them. A shot
plays the weapon's muzzle effect as a child of the gun's muzzle node, so it
moves with the gun. The effect's frame is the muzzle bone's: Dunia's +Z down
the barrel, its +Y to the right. One shot in three, the smoke after plays the
same way. Our flash and smoke stand in when an effect isn't cached.

The characters' shots play the third-person muzzle of their role's weapon:
`AK47`, the shotgunner's `M133`, the sniper's `SVD`, the heavy's `PKM`, the
molotov thrower's `6P9`. It plays where the shot leaves, heading along it with
its right level. Turrets, helicopters and villagers keep ours. These were only
seen in a fight at 30 m, too far to judge their look.

The muzzle node is the gun model's `FX_FIRE`, taken only where it lies at the
front of the gun. The 870's chains to a pose off the gun, below and beside it,
so it takes the front of its bounds. A world-space flash would be left behind
by the shot's recoil kick, so the flash moves with the gun.

Burning. Fire's harm sets people and animals alight (`burning`): flames
underfoot, a molotov, the flamethrower. The mercs' `CBurnComponent` names the
human torch they wear (`psFireSpineParticles`: `fire_humantorch`) and the
steam when water puts one out (`psFireWaterSmokeParticles`: `water_smoke`). It
also names the start and stop sounds (`001B0477`, `001B0479`). The torch burns
on a person's chest (the `Spine2` bone; an animal's at its body's middle) and
stays upright as they move and fall. The component holds no numbers, so the
6 s burn and its 30 harm a second are ours.

A person alight panics. They drop the fight and run blind, 5 to 10 m this way
and that, on the game's burning move (`pawn_generic_burn`: the
`reactonfire_runloop` it has armed, its `reactonfire_idleloop` on the spot).
They scream their voice's `reflex_flames` barks one after another, in place of
the wounded grunts. A death by fire falls with one of the three
`reactonfire` `idle_diebyfire` clips. Put out alive, they take up their own
moves again. `FC3_BURN_TEST=<s>` sets everyone within 40 m alight that long
after the start; `--npc-test guards` gives two who run, `idle` one who stays.

Grass fires. The fire manager's settings (`firesettings_default.obj`) name,
for each kind of fire, the effects it plays and how many of each may play at
once (`iPoolMaxSize`). The converter keeps them (`particles/fire-settings.txt`)
and caches the effects, skipping those the library lacks or keeps empty. A
burning patch of ground plays the burning ground's (`FireRegionSettings`). Its
lasting fires (`fire_ground_a`, `fire_ground_b`) burn on one of our cells in
four, since each is a few metres across and our cells are 2 m. The nearest to
the camera come first, as many as their pools allow. A splash on bare ground
plays the one whose flames don't loop, burning itself out (`fire_ground_d`).
The smoke (`FireSmokeRegionSettings`: `smok_ground_a`, `smok_ground_b`) rises
from one cell in nine and hangs on 12 to 20 s after it's out. The effects'
own lights are left to our four over the fire. Our flames and smoke stand in
when nothing is cached. Not yet: the region profiles (Savannah, Jungle...)
and their timings, burning objects and plants, the flamethrower's sticky
stream.

Each active emitter waits its `SpawnDelay`, then emits for its `EmitDuration`
(again and again with `EmitLoop`). It gives a burst of `NbInitPart`, then its rate:
`EmitRateKF`'s value, times its `RatioUI` curve over the emitter's time, give or
take its `VariantKF`. `StaticSize` caps an emitter's live particles. A particle
is born with each `Init...KF` (life, speed, size, gravity, angle, spin), its value
give or take its variant. It is born in the emitter's volume: in a sphere of
`EmitVolRadius`, or a box of `EmitVolHalfSize...`, facing the camera with
`ShapeTowardCamera`. It heads along the emitter's axis: Dunia's +Y turned by
`RotOffset` (X -90 is straight up), at `EmitDirAxisAngle` and
`EmitDirPlaneAngle` within their spreads. With `DirFromVolumeSurface`, it heads
out from the volume instead. Over its life it follows the `Part...KF` curves:
speed, size, `SizeZ` (a streak's length), alpha, spin and gravity. Its colour
follows the `PartColorKF` points times `InitColorKF`, or its evenly spaced
`Key`s when it has no points. With `RandomColor`, one colour is picked at
birth. The sheet is `FrameCount` by `FrameCountY` (0 is one image),
shown at `FramePerSecond`. `BlendType` 1 adds; `RedAsAlpha` takes brightness as
coverage; else the alpha blends. A particle's size is its half-extent (its
radius): read as a width, the camp fires' flames sat hidden in their logs and
the blasts came out half the original's. `UniformSize` 0 particles (sparks,
flames, streaks of smoke) are drawn along their heading as seen. `PartSizeKF`
gives their width and `PartSizeZKF` their length (both times their size). They
hang from their point by `PartSizeOffsetX` (across) and `PartSizeOffsetY`
(along). One oriented in its effect's frame (`Flags` `Orientation`) that hardly
moves (a flame, a standing wisp) stands along the frame's Z (Dunia's) instead.
An emitter in its own space (`Flags` `LocalEmitterSpace`) keeps its particles in
its effect's frame, following the effect while it lasts. `EmitterHDRMulKF` brightens what adds or takes
brightness as coverage, compressed (1 + 1.5 log10 of it: our light isn't the
engine's). `SHLighting` sheets darken
with the daylight. The near and far fades apply. `LightActive` burns a point
light with its colour, intensity and radius, flickering by
`LightFlickeringFrequency` and `LightFlickeringAmplitude`. The world's placed
effects burn none (`NoLights`), since the converter already puts their lights
among the world's. A soft emitter (`Soft`) fades over its `SoftRange` (m) as
it nears what's behind it, so smoke meets the ground without a hard line. That
needs the camera's depth prepass, there at high effects and up (for SSAO); our
stand-in smoke and dust fade over 0.5 m. Not yet: distortion (heat haze isn't
drawn), mesh particles, child
emitters, the plotted discs and sounds. Curves are joined linearly between
knots. `FC3_AUTHORED_FX=<name>[,<metres>]` plays one as an impact, on the ground
4 m (or that far) in front of the player, once he has stood 15 s.
`FC3_AUTHORED_FX_SHOTS=<dir>` then captures the screen 0.05, 0.2, 0.5, 1, 2
and 4 s in.

Lit effects (`EffectLit`: the foam sheets of waterfalls and rapids) stay on standard
materials. They are drawn additively and unlit, since their maps are DXT1 with no alpha.
Their first layer `23BE852F` is (tile u, tile v, speed u, speed v); the waterfall's is
(2, 2, -0.025, -0.45). `cache_assets::flow_materials` scrolls their UV transform at that
speed, so the water runs down the falls.

Plants (foliage, grass) keep their wind material although they have a pair.

Blended surfaces (`Blended` and `Decal` shaders: leaking dirt, blood, stains;
`EffectLit` foam and glows) use their pair for colour and, for coverage, the
diffuse map's alpha times the vertex colour's alpha, as the game's own
blending permutations do (the paired G-buffer permutation's `oC0.a` is often
an encoded constant instead). Clear glass is lowered by the vertex alpha:
Dennis's lenses (an opaque grey map, vertex alpha 0.66) were drawn as solid
pale discs before. `FC3_BLEND_MAP_ALPHA=1` uses the map's alpha alone. They
are blended over what is behind with the same small depth
bias as on the standard material (set on the pipeline's depth state:
`Material::depth_bias` only orders transparent draws). They cast no shadow
(their prepass threshold is past any alpha). `FC3_NO_BLEND` leaves them on the
standard material. The overlay pair's pixel shader keeps a pixel only while a
distance factor lies between two components of one interpolant (a `texkill`);
the vertex shader writes only the factor, and on D3D9 hardware the unwritten
components arrive as (0, 0, 0, 1), so the range is 0..1. The translation
zero-initialised outputs, which killed every overlay pixel. Vertex shader
outputs now start as (0, 0, 0, 1) in the translation and the interpreter
alike (`d3d9_wgsl::VS_OUTPUT_DEFAULT`; the glue patches packs translated
before). It changed the pairs of 13 materials, regenerated since. Validation
now also rejects a pair that kills its sample pixel: run with the old zero
default, that check moves 87 materials (the overlays among them) off the
vertex shaders that would have killed them, so a mistake of this kind shows
up in the pairing instead of as invisible surfaces.

Alpha-tested surfaces (`AlphaTestEnabled`: rags, tarps, fences, nets) use the
original shaders but are cut out by their diffuse map's alpha. D3D9 tests the
colour output's alpha as render state, but the paired permutation's `oC0.a` is
rarely coverage. `d3d9_wgsl::colour_alpha_samplers` finds the diffuse alpha in
only 18 of 509 pairs; elsewhere it is gloss, a constant or nothing, which once
cut whole rags away. So the glue samples the diffuse map at the mesh UV times
the material's tiling (`DiffuseTiling1`, or `Simple`'s `Tiling`) and discards
below 0.5. The material reports itself as alpha-masked, and its prepass
fragment (`original_prepass.wgsl`) does the same test, so shadows and the depth
prepass are cut out too. With them, 98% of prop parts in a village scene draw
with the original shaders.

Depth. For static props, the glue keeps the original vertex shader's x, y and w,
which include any vertex animation, but takes depth from Bevy's transform of the
same point. The game's clip depth differs from Bevy's by more than a decal's few
millimetres of offset. That hid every decal drawn by the standard path in front
of an original-shaded wall: door signs, leaking dirt and posters. Skinned meshes
and terrain keep the original position, since their geometry exists only inside
the original shader.

Tiling and UV sets. Each shader family's material descriptor
(`engine\shaders\materialdescriptors\<family>.xml`) names its parameters, gives
their defaults, and says for each texture which tiling parameter and which UV
group parameter apply (`DiffuseTexture1`: `DiffuseTiling1` and
`UVGroupMapChannel1`). The shaders don't read those parameters: they read a
constant the engine works out from them (`DiffuseTiling1AndGroup1`, or a name
the descriptor gives such as `Diffuse2UVTiling`), which no material file
contains. The pixel shader forms its coordinate as `c.xy * uv.xy + c.zw *
uv.zw`, where the four-component texture-coordinate input carries both UV sets
side by side, so the constant holds the tiling in its first half for the first
UV set or in its second half for the second. Before this, every material got
(1, 1, 0, 0): textures never tiled (1,071 materials tile their colour map 2 to
12 times, so walls and floors came out stretched and blurred), and masks
mapped to the second UV set (dirt, wear and layer blends on 803 materials)
were read with the first, repeating with the tiling. The converter now reads
the descriptors from the game (nothing of them is stored), fills these
constants from each material's tiling and UV group, and uses each family's
own defaults for parameters a material leaves out (a Metal material without
`Diffuse2UVTiling` had sampled its second layer at a single texel, which
turned the AK's wood black). Models now keep their second UV set, which the
glue feeds as the second half of that input. The standard material beyond the
original shaders' range applies the colour map's tiling too.

Paint jobs. The game keeps each painted weapon as materials of their own: the
weapon's maps plus a pattern as the second colour layer, which the Metal
shader blends in by the weapon's mask. The converter groups every material by
its colour map and lists, for each one without a pattern, the painted
versions sharing it (`weapons/paints.txt`; four patterns, named by us; a fifth
showing a face is left out). A painted weapon in first person draws those
parts with their painted material on the original shaders.

Runtime glue:

- Each stage's constant block comes from the baked binding: material
  parameters written by name hash (vertex and pixel constants are separate
  register files), defaults for common parameters, and `MeshDecompression` set
  to identity because the cache stores decompressed vertices.
- Engine globals (view/projection matrices, camera, viewport) come from one
  shared buffer updated every frame, in Dunia space. The per-object
  `WorldMatrix` rows come from Bevy's mesh transform.
- Colour maps are sampled with sRGB decoding and other textures raw, matching
  the game's sampler state. The shaders store the square root of linear albedo,
  so target 0 is squared back.
- Target 0 (albedo) and target 1 (world normal) are decoded into Bevy's PBR
  input. Lighting, shadows, fog and the atmosphere stay Bevy's. Prepass and
  shadow pipelines use Bevy's default shaders.
- Vertex colours (blend weights, baked occlusion) are passed as a custom
  attribute that standard materials ignore.
- Self-lit materials (`Unlit`, `EffectLit`: signs, lamp glass, screens) show
  target 0 as it is, without lighting, as their standard materials do.
- Meshes carry their tangents (Bevy's tangent attribute). The glue packs the
  tangent and the binormal (normal x tangent, times the handedness) like the
  normal, for the PSIZE and FOG inputs. Meshes without tangents get a made-up
  tangent across the normal.
- Wind: foliage sways in its own material, and the engine's wind registers
  (`WindSimParamsX` / `WindSimParamsY`, four oscillators per horizontal axis)
  are filled every frame from the weather's wind for the original shaders that
  read them.
- Rigid parts of skinned models (gun parts, attachments bound to one node) are
  stored node-local in the game files. The converter bakes each such part
  through its node's bind pose, so they line up whether drawn rigid or skinned.
- Character outfits swap materials per enemy (see the graphic kits in
  `fc3_formats::kits`). Swapped materials are ordinary cached materials with
  their own baked pairs.

Cost. Each translated pair is its own pipeline and each original material its
own bind group, so every one in view is a batch of its own: a busy scene has a
couple of hundred (props within `ORIGINAL_PROP_RANGE`, 150 m, and the terrain
sectors within five of the player). In the sun's shadow cascades that repeated
per cascade, and the per-batch CPU work (buffer writes, bind groups, the
bin-unpacking dispatch and the draw) cost more than the rest of the frame:
turning shadows off took an outpost from 58 to 104 fps. Shadow map size and
the number of casters made no difference. So these props and sectors cast
through proxies (`shadow_lod.rs`): the same mesh with one shared standard
material, on a render layer only the sun sees. Cut-out surfaces (whose shadow
needs their alpha) and skinned meshes still cast themselves. The outpost went
from about 60 to about 90 fps with the proxies, grass tints moved onto mesh
tags (see `docs/vegetation.md`) and small casters dropping their shadows with
distance. `FC3_PERF` logs the distinct materials in view.

Debugging switches (game): `FC3_ORIG_DEBUG=n` shows render target `n` (or pixel
input `v<n-10>` for `n >= 10`) instead of lighting. `FC3_ORIG_DUMP=<dir>`
writes every generated module. `FC3_ORIG_STATS=1` logs coverage.
`FC3_ORIG_POS=1` keeps the original vertex shader's clip x, y and w for props
(with Bevy's depth) instead of Bevy's whole clip position, which the depth
prepass needs to match. `FC3_ORIG_WHITE=1` ignores mesh vertex colours. Converter:
`FC3_DEBUG_INTERP=1` prints interpreter failures and static-UV rejections during
`fc3tool pair-survey`.

## Blended `Unlit` effects and the lights' falloff

Light shafts, glows and lit spray are `Unlit` materials that blend as their
`BlendingType` says (1 alpha, 2 additive, 4 premultiplied; see
`engine\shaders\materialdescriptors\unlit.xml`), or `EffectLit` ones that add
their light. The shader pair the converter finds for them is the opaque,
deferred permutation (it writes the G-buffer), so drawn by the original
shaders they came out as solid surfaces: the dark room's light shaft over the
rock bridge (DOC_020) was a brown cone filling the view. They now keep their
standard material (`original_shaders::keeps_standard_blend`), and the
additive and premultiplied `Unlit` ones then go onto their own material
(`unlit_effects.rs`), which draws them as the descriptor's parameters say:

- the colour map dyed by `DiffuseColor1`, times `HDRMul` (0.3 for the shaft);
- faded toward grazing angles by `NormalAttenuationPower`: |N·V| to that
  power (3 for the shaft), which takes a cone's edges to nothing;
- moved by `AnimType`: 1 and 2 scroll at `USpeed`/`VSpeed`, 3 goes back and
  forth, 4 turns at `AngularSpeed`.

They cast no shadow. What the game's eye adaptation does to `HDRMul` isn't
modelled: the multiplier is taken as it is, as the other self-lit materials
take their maps.

The deferred omni and spot light passes (`PointLightAttenuation`,
`SpotLightAttenuation`) compute their falloff with one `mad_sat`:
`saturate(a / d² + b')`, where the engine sets `a = R²·Ri² / (R² − Ri²)` and
`b = −Ri² / (R² − Ri²)` from the light's radius `R` and inner radius `Ri`
(1 at `Ri`, 0 at `R`), and `b' = b − t / I` with the light's occlusion
threshold `t` (`fOcclusionAttThreshold`, 0.07 unless set) over its live
intensity `I`. So a light is flat at full strength inside its inner radius.
Spots multiply in their cone, `saturate(cos θ · scale + bias)` squared, from
their inner and outer angles. Bevy's lights fall off as 1/d² with a range
window, which matches outside `Ri` but goes on rising inside it (7 times at
5 m under the dark room's spot, inner radius 10 m). Each pooled lamp is
dimmed to match the game's falloff at the eye's distance, never brightened
(`lamps::inner_clamp`): what is near the eye (the hands, a wall beside it)
is lit as in the game. The threshold `t` isn't read yet.

Parts that leave the standard and original materials for one of another
kind (the tattooed arm, these effects) are swapped over two frames
(`original_shaders::swap_next_frame`): the old material comes off in one
frame and the new one goes on in the next. Swapped within one frame, while
the original shaders' own swap touched the same part, a part was drawn with
the old kind's pipeline and the new kind's bind group, a GPU validation
error that quits the game. It happened only with some system orders (a
diagnostic system added was enough to bring it on every run).

`FC3_CAPTURE_NEAR=x,z,radius,path[|...]` (game, tests) saves a frame when the
player first comes within `radius` of each point, to look at a place along a
scripted walk. `FC3_BURST=<dir>,<seconds>` captures 24 frames 40 ms apart from
that long after start (real time), for what lasts a moment.
