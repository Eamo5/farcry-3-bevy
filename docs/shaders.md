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
  (volume), with its sampler at the next binding. The game drops those
  per-slot samplers when it loads a module and samples every slot through one
  of the material's two shared samplers (repeating, or clamping for the
  terrain's atlas maps): Metal allows 16 samplers per shader stage, which the
  16 slots alone would use up.
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
its right level. The rocket man's launch plays the `RPG-7`'s (its back blast).
Mounted guns play their mount's: the DShK turrets `DSHK_Mounted`, the M249
ones `M249_Mounted`. The helicopters' door guns play `Minigun230_BlackHawk`,
and the villagers' rifles the `AK47`'s. The characters' muzzles were only seen
in a fight at 30 m, too far to judge their look; the mounted, helicopter and
villager ones are not seen yet.

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
6 s burn and its 30 harm a second are ours. What sets one alight is the
game's: a character's counters (`CFCXCountersComponentAI.Burn`) give the
fire it takes before it catches (`fAccumulatedDamageBeforeBurning`) within
a time (`fBurnDamageAccumulateTime`): a merc 25 in 8 s, a villager or
companion 50 (the villagers' base prototype's, for those of no archetype),
counted as their burn multipliers take it. An animal catches at once.

The player catches fire as the counters service says
(`StatsAndCounters.Health`, whose notes in FC3.dll give the meaning):
45 of fire's harm within 8 s (`fBurnOnFireDamageThreshold`,
`fBurnOnFireTimeLimit`). Alight, he takes 5 a second more
(`fBurnOnFireDamageRate`), until he has been out of the flames for 2 s
(`fBurnDamageExpiryDelay`, whose note is a copy of the rate's, so this
reading is ours) or goes into water. Fire's harm to him is scaled by the
level's `StimBurnMultiplierPerLevel` (casual 1.2 to infamous 0.3), and the
fireproof syringe keeps him from catching. Seen (`FC3_GROUND_FIRE_TEST=<s>`
sets the ground alight under the player and everyone within 40 m): 15 a
second in the flames, alight at 3.4 s, 20 a second then, out 2 s after the
flames died, 60 lost in all. The two test guards caught at 2 s.

A person alight panics. They drop the fight and run blind, 5 to 10 m this way
and that, on the game's burning move (`pawn_generic_burn`: the
`reactonfire_runloop` it has armed, its `reactonfire_idleloop` on the spot).
They scream their voice's `reflex_flames` barks one after another, in place of
the wounded grunts. A death by fire falls with one of the three
`reactonfire` `idle_diebyfire` clips. Put out alive, they take up their own
moves again. `FC3_BURN_TEST=<s>` sets everyone within 40 m alight that long
after the start; `--npc-test guards` gives two who run, `idle` one who stays.

Flamethrowers. A flamethrower's `FireStrategyProperties` name its nozzle's
effects (`TipEffects`, by `selType`: 0 alight, 1 shooting, 4 stopping; first
person and third). They also name those it sends down the stream: its
`FlameMesh`'s and its `FireballEffects`, one every `fSpawnTime`, going at
`fSpeed` for `fDistance`. A curve of how fast the aim turns scales the wait
between them (`arkSpawnTimeAngularSpeedRatioCurve`, a `Curves:` prototype:
2.5 times as long at rest, a tenth as long swung fast). The weapons' table
keeps them (version 11) and the converter caches the effects, though
`fireball1` isn't in the library. The LPO-50 in hand burns its pilot flame on
the muzzle. Firing, it plays its shooting effect there and launches the
stream effects along the aim. Each goes its distance, or stops where it
strikes something, and what it emitted lives out. Letting go plays the
stopping effect. The flamers launch the third-person ones with each pulse,
their nozzle flaring as a burst opens and dying down as it ends. Where the
stream strikes close, its fireballs (HDR 18) pile into a white-hot blaze.
Where the stream lands it leaves sticky fire (below).

The flame mesh (`FlameMesh`) glows down the middle of the stream. It is a
tube `iRingNVertex` vertices round, built round a rope of knots sent from the
nozzle every `fSegmentLength` at `fSpeed` for `fSize` metres, or until the
stream strikes something. Up to `fFlameRestitutionStartDistance` the rope
keeps straight ahead of the gun. From `fFlameRestitutionEndDistance` it goes
its own way (in between it blends), so it swings out behind as the aim turns.
The tube widens from `sizeGrowInterpolation`'s first width to its second over
`fSizeGrowInterpolationDist`, narrows by `sizeShrinkInterpolation` from
`fSizeShrinkInterpolationDist` to its end, and grows to full strength over
`fFlameStrengthGrowthTime`. It isn't drawn nearer the nozzle than
`fMeshRingCutoffDistance`. Its texture (`texTexture`) adds light. The texture
tiles along the rope every `fTeselation` metres, so the flame comes in pulses
that travel with the knots. The texture's `fTextureFrames` slices across take
turns every `fTextureChangeTime`. The weapons' table keeps the mesh (version
13; 11 and 12 still read, without it or the variants). It also keeps, after
its weapon, a variant whose flame isn't the weapon's: the Ink Monster's breath
(`LPO-50.CitraFlamebreath`, a 100 m mesh at 40 m/s with its own stream
effect). The converter caches the mesh's texture and the variants' effects.
The flamers' bursts feed a mesh too. `FC3_ROPE_GLOW` sets how brightly it glows (3).
`FC3_TEST_MAG` keeps the magazines full, for a long burst under
`--fire-test`. `FC3_FIRE_SWEEP=<deg>,<period s>[,<pitch deg>]` swings the
test's aim from side to side (the stream's curve), raised so far.

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
from one cell in nine. A bush in a burning patch burns with a plant's fire
(`FireRealtreeSettings`): a small one with `fire_bush`, one 1.6 m tall or
more with `fire_foliage_a`, until it's burnt away. The effects' own lights
are left to our four over the fire.

How long a patch burns is the fire manager's region profile's
(`fireregion.lib`, kept as `particles/fire-regions.txt`): its fire's growing,
burning and dying-down times (`fStartingTime`, `fAliveTime`,
`fExtinctionTime`), give or take 15 %. Its flames go out halfway through the
dying down, and its smoke lingers the profile's smoke life
(`fSmokeMinLifeTime` to `fSmokeMaxLifeTime`). The open world's ground
burns with the profile of the logic zone painted on it. Each sector paints
its zones in `zonesector<id>.zsr`: 4,096 bytes, 64×64 cells of 1 m, row by
row from the sector's south-west corner (with that order 6% of the cells
along sector edges differ from their neighbours', against 35-63% with the
rows flipped or turned), `0xFF` where none is painted. The converter keeps
them as `world/logic_zones.bin` (laid out as the sound regions' map:
`sound_regions::RegionMap`). `burnablesettings_default.obj`'s `Regions_SP`
gives each zone its profile (`zoneLogicZone`, `fireregProfile_SP`), kept
as `particles/fire-zones.txt`. The zones are `logiczones.lib`'s (`disId`;
each also names an environment zone template, but the outdoor ones share
their presets). Zones 0-2 (`Logic/JungleLight`, `Medium`, `Dense`) burn as
`Region/Jungle`; 3-5 and 8 (`Default`, `Desert`, `DefaultMedium`,
`DefaultDense`), 10 and 11 (`DesertMedium`, `DesertDense`), 16-18 (the
`Coastal` ones) and 19-21 (the `Grasslands` ones) as `Region/Savannah`; 89
(`VegetationMediumFire`) and 94 (`Cannabis`) as `Region/Cannabis`. The rest
have none and don't burn: zone 90 (`VegetationNonBurnable`, the shores),
and the cells painted with no zone (roads, paths, rivers, villages). The
main map paints 0-4, 16-21, 90 and 94. So fire spreads only into
cells whose ground burns, with that ground's profile, and stops at a road.
A splash on ground that doesn't burn still flares up for 3-5 s. One island
is mostly savannah zones, the other jungle. Without the zone map the ground
burns as the savannah's. `RUST_LOG=farcry3::fire=debug` tallies the
burning and burnt cells and the plant cells kept from catching. A burnable
region burns as its ignitor names (`fireregRegionProfile_SP`: WIL_020's
drug fields are `Region/Cannabis`). Animals within a fire's `Fire_FearStim` radius run from
it, unless their personality ignores fear (`bIgnorFearStim`). One alight
bolts this way and that until it's out. Without the profiles, a patch burns
7 to 11 s and its smoke lingers 12 to 20 s. Our flames and smoke stand in
when nothing is cached. A burning patch hurts whoever stands in it its
profile's `fFireDmg` a second (15 for every region; 28 without the
profiles). A splash where the ground doesn't burn hurts as its sticky
stream says (a flamer's and a Disciple's molotov's 1, the rest 15). Not
read: `fDmgBuffer` (0.5 to 10 on regions, 0 to 100 on objects). FC3.dll
registers it with no note, and nothing found reads it beside the damage.

Sticky fire (`sticky_fire`). The fire manager's sticky streams
(`firestickystream.lib`, kept as `particles/fire-sticky.txt`) say what fire a
weapon leaves where it lands: its times, how near it burns
(`Fire_BurnStim`), its flames' kind (`selTypeOfStickyFlames`: a
flamethrower's or a molotov's) and their spacing (`fResolution`). The
player's flamethrower (`Weapon/Flamethrower`), the flamers'
(`Weapon/Flamethrower_NPC`) and a molotov (`Weapon/Molotov`) lay one where
they land, on any surface, turned to it. It plays the sticky stream effect
for its kind (`FireStickyStreamSettings`: `fire_cyltree_a`, then
`molotov_impact`) for its fire's growing, burning and dying-down time. While
it burns it sets fire about it every half second as far as its burn reaches,
lighting the grass and burning whoever stands in it. The stream landing again
within its spacing keeps it burning instead of laying another. As many burn
at once as the effect's pool allows, and the oldest gives way. A molotov now
leaves fire burning some six seconds even on bare ground or rock.

Burning objects (`burnables`). The fire manager's object profiles
(`fireobject.lib`) say how a burnable object burns: its fire's times, its
smoke's life, the colour it chars to (`BurnColor`) and whether it smokes
black. `burnablesettings_default.obj` names the models that burn as each.
The converter keeps both (`particles/fire-objects.txt`). In the world each
such object carries the ignitor its profile puts on it (`CIgnitorComponent`,
`fireobjObjectProfile_SP`). Of the list's 1193 models only three are in this
game's archives: the villages' wooden house platform and frames (162
placements, `WoodObjects/Large`). The rest name the last game's props. One
catches when fire is set about it (`fire::Ignite`, within its reach) or the
ground under it burns. The fire manager's object fires (`fire_object_a` to
`_e`) then burn over its top, up to eight, 1.5 m apart or more, as their
pools allow. The burning ground's smoke rises over it, since the objects' own
smokes aren't in the library. It chars to its burn colour as it burns (its
parts' materials, taken off the original shaders) and sets the ground under
it alight once its fire has grown. Its fires go out halfway through dying
down, its smoke lingers its smoke's life, and it stays charred and doesn't
burn again.

Each active emitter takes part in an effect's play by its `ActiveChance` (a
share of the plays: some of a blast's sparks or smoke only now and then). It
waits its `SpawnDelay`, then emits for its `EmitDuration` (again and again with
`EmitLoop`). It gives a burst of `NbInitPart` (or a count between
`NbInitPartMin` and `NbInitPartMax` when the latter is set), then its rate:
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
`Key`s when it has no points. The colours are sRGB, as the editor shows them:
blended so, then made linear, as the sheets are. Read as linear, a ground
fire's flames (orange, 0.85, 0.40, 0.26, fading to black) had shown as a pale
salmon wash, and greys came out too light. With `RandomColor`, one colour is picked at
birth. The sheet is `FrameCount` by `FrameCountY` (0 is one image),
shown at `FramePerSecond`. `BlendType` 1 adds; `RedAsAlpha` takes brightness as
coverage; else the alpha blends. A particle's size is its half-extent (its
radius): read as a width, the camp fires' flames sat hidden in their logs and
the blasts came out half the original's. `UniformSize` 0 particles (sparks,
flames, streaks of smoke) are drawn along their heading as seen. `PartSizeKF`
gives their width and `PartSizeZKF` their length (both times their size). They
hang from their point by `PartSizeOffsetX` (across) and `PartSizeOffsetY`
(along). One oriented in its effect's frame (`Flags` `Orientation` 1) that hardly
moves (a flame, a standing wisp) stands along the frame's Z (Dunia's) instead.
An evenly sized one oriented so lies in the frame's plane, turned by its angle:
foam, rings of smoke on the ground, a water impact's ring. (`Orientation` is
one bit of several, one at a time, each its own mode. Over the library's 4,058
emitters: 1 (146: flat on the ground), 8 (676: sparks, drips, trails, mostly
sized unevenly, drawn along their heading anyway), 16 (260: the fires' flames:
camp fires', the fire manager's ground, bush and foliage fires', burning
people's, molotovs', and the heat haze over them), 4 (72: sand kicked up,
drips, a wall of water), 65536 (30: clouds, lights), 131072 (102: blood
sprays), 262144 (150: splashes, distortion rings).) One with 16 stands upright:
turned only about the vertical to face the camera (its width levelled, its
length or height straight up), so a fire's flames stay standing seen from
above instead of tipping toward the camera. Two more are fixed in the
emitter's frame, not turned to the view, the frame turned by the emitter's
whole `RotOffset` (X, then Y, then Z): one with 262144 lies in the
emitter's plane turned by its `OrientationX`, `Y`, `Z` (degrees): a ground
fire's embers on the ground, a river's ripples (their `RotOffset` Y -90 and
`OrientationY` 90 undo each other), god rays as two upright sheets crossed
at 45 degrees either way, a vehicle's splash sheets. One with 131072 faces
along the emitter's axis and stands along the frame's Z: a shot's blood
spurt facing back along the shot, a boat's wake turned (`RotOffset` X 90)
to lie on the water. Their `FadeAngle` (a takedown's blood splat, the
embers) fades them edge-on as for the flat ones. One with 4 stands along
its emitter's axis (the way it emits: Dunia's Y turned by its `RotOffset`),
turned only about it to face the camera, as one with 16 stands along the
world's vertical and one with 8 along its heading: sand kicked up by a step
or an impact, smoke out of a bullet hole, columns of smoke rising from a
blast, water running down a wall, a wheel's spray thrown back at its tilt.
(Seen: a sand impact's grains rise as a fan, where they had been a round
puff.) 65536 (the sky's clouds, a vista's, a helicopter's lights) is drawn
facing the view, as the unflagged ones are; nothing in the data or the
binaries says how the engine's differs. `FadeAngle` fades a flat one out as the line of sight nears its plane,
and a stretched one as it nears its heading, over that many degrees, so
neither shows edge-on as a sliver.
An emitter in its own space (`Flags` `LocalEmitterSpace`) keeps its particles in
its effect's frame, following the effect while it lasts. A particle takes on
its effect's own motion by `InitExtSpdRatioKF` (its share over the emitter's
time), and keeps it over its life by `PartExtSpdRatioKF`: a flamethrower's
fireballs fly with the stream. `EmitterHDRMulKF` brightens what adds or takes
brightness as coverage, compressed (1 + 1.5 log10 of it: our light isn't the
engine's). `SHLighting` sheets darken with the daylight. `NearClippingAlpha`
fades a sprite out between `NearClippingEnd` and `NearClippingStart` from the
camera (a flamethrower's fireballs, impacts' smoke); `FarClippingAlpha` or
`FarClippingRate` fades it out far off. A sprite shrinks as it nears the
camera, from its size `CameraScalingDistance` off to `CameraScaling` of it at
the eye. The flamethrower's flames, fires and their smoke, and the sea's motes
are set up like this, so the flames in hand stay a stream and don't white out
the screen. `LightActive` burns a point
light with its colour, intensity and radius, flickering by
`LightFlickeringFrequency` and `LightFlickeringAmplitude`. The world's placed
effects burn none (`NoLights`), since the converter already puts their lights
among the world's. A soft emitter (`Soft`) fades over its `SoftRange` (m) as
it nears what's behind it, so smoke meets the ground without a hard line. One
that dissolves (`AlphaDissolve`: splashes, blood, mist, some flames) loses
its sheet's thin parts first as its alpha falls: the sheet's coverage from
one less its alpha up to one is what shows, so at full alpha it is all
there and the dense core goes last, instead of all of it fading alike. That
needs the camera's depth prepass, there at high effects and up (for SSAO); our
stand-in smoke and dust fade over 0.5 m.

Heat haze. A distortion emitter (`DistortionEnabled`, in 55 of the cached
effects: fires, explosions, muzzles) draws no colour. Its particles bend the
view behind them instead (`particles::HazeMaterial`, `haze.wgsl`). How much
is the sprite's shape (its sheet's red with `RedAsAlpha`, else its alpha)
times its alpha, as any sprite's. Which way is its `DistortionTexture`, an
offset map (red one way, green the other, 0.5 none). It is tiled across the
sprite (`DistortionTilingU`, `...V`) and scrolls (`DistortionSpeedU`,
`...V`, a second). The bend reaches `DistortionStrength` of the view and is
blurred by `DistortionBlurStrength` as far again. The haze is drawn among
Bevy's transmissive things, which see the scene as it stood before them,
copied once. Those include the opaque world, but not the clouds, sea and
other particles drawn after, so it doesn't bend the sky or the flames in
front of it. The converter now caches haze emitters' sheets and maps, even
for effects cached before. `DistortionDistanceScale` isn't used.
`FC3_HAZE_SCALE=<times>` (tests) bends that much more, to be seen.

Mesh particles. A mesh emitter (`Geometry`, in 19 of them: the bullet
impacts' chips of rock, wood, plaster and grass, the explosions' metal and
wood debris) shows each particle as its model. The `.glm` it names is
exported as the `.xbg` of the same name, which the converter caches with
every model. Each particle is that model's first LOD, turned at random as it
is born (the emitters give no angles) and sized by its size (`InitSizeKF`
times `PartSizeKF`). The models are a metre or so in radius, as a sprite's
size is its half-extent, so 0.03 makes a chip a few centimetres across. It shows while it
has a size and isn't beyond its far fade. A pool of each model's entities is
kept, as many as have lived at once, placed each frame and hidden when not
needed. The effects' sounds are all unset (`-1`).

Child emitters. An emitter's child (`PartChildEmitUnit`, in 12 of them:
mostly the explosions' `SparksGuide`, whose few fast particles leave trails
of sparks) emits from each of its particles as an emitter would. Its clock
is the particle's age: its burst at once, then its rate over its time
(`EmitDuration`, again and again with `EmitLoop`). Each child particle is
born where the parent particle is, heading about the way it goes (the
child's +Y, Dunia's, along the parent's motion, within its spreads), and
takes on its speed by `InitExtSpdRatioKF`. A child's pool (`StaticSize`) is
each parent particle's in the engine. Here an effect may have eight pools of
a child's particles at once. The converter caches children's sheets with
their parents'.

Not yet: the plotted discs. Curves are joined linearly between knots. `FC3_AUTHORED_FX=<name>[,<metres>]` plays one as an impact, on the ground
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

## Post-effects (`fc3_game/src/post_effects.rs`)

The game's screen effects are sequences in `cinema_postfx.xml` (305 of them),
played by name through the PostFX database (`PostFXs:PostFXs/Database` in the
entity library, 183 effects). Each effect names its loop sequence, a priority,
an intensity control (`selIntensityControl`; the game sets a strength for the
ones it drives, like blasts and speed) and a fade in and out. `selFadeType` 1
plays a fade sequence, 2 ramps the strength over `fFadeTime`, 0 cuts. An
effect's id (`sFX`) is the CRC-32 of its lower-case name. Scripts start and
stop them by name (`CAPI_Player.StartPostFX`). The engine starts the rest from
the player's state; no data names those triggers, so the game picks them by
the effects' names:

| effect | when |
|---|---|
| `sp_health_hit`, `meleehitfx`, `animalslash_leftside`/`_rightside` | a hit, a blow, an animal's strike (sides alternate) |
| `nearmiss` | a round passing close (its whizz sound) |
| `criticalhealth`, `death` | health in its last segment, dead |
| `grenade_explosionfx` | a blast within three of its radii, stronger nearer |
| `sprintingfx`, `slidingfx`, `fallingfx`, `landingfx` | sprinting, sliding, falling fast, a hard landing |
| `ziplinespeedfx`, `handgliderspeedfx`, `wingsuitspeedfx`, `vehiclesspeedfx` | by speed |
| `diving`, `drowning_health` | head under water, air below a third |
| `ironsightfx`, `binocularfx` | aiming down iron sights, binoculars |
| a syringe's own (`healthboostfx`, `animalrepellantfx`, ...) | as it goes in |

A state only ends the effects it started, so a script's effect of the same
name plays on. Scripts that listen (`PostFxListener`, CIT_050 B40E's fade to
white) hear `DominoEvent_FxStart` and `DominoEvent_FxStop` on the player as
effects start and end, with the effect's id. The mission graphs mostly start
fades: `cinematicfadetoblack` (105 boxes), `blackscreenfx` (91),
`blackscreenwithfadeinandout2sec` (43), `blackscreennowfx` (20), the fades to
white, `hallumaster`, `heat_distortion`, `weedbuzzfx` and the mushroom trips.

Each sequence's clips are post-effect clips (`fc3_formats::cinema`,
`ClipKind::Blur` and `ClipKind::Post`): blur (203), colour mask (99), bloom
(67), RGB separation (59), colour burn (29), desaturation (26), source
distortion (21), camera shake (10), noise (4), hue and saturation (4), slow
motion (2), motion blur (2) and field of view (1). Every frame, the strongest
playing effect's clip of each kind wins (by effect priority, then the clip's
`nPriority`, then the latest started). Up to two colour masks play at once.
They all go to one full-screen pass after tone mapping
(`post_effects.wgsl`).

The game's own pixel shaders for most of these were found by their
constants' CRC-32. The constant names are strings in `FC3.dll`, and so are the
techniques (`PostFxColorMask`, `PostFxRGBSeparation`, `PostFxBlur_Radial`, ...)
and the enum names, registered in code in order:

* **Colour mask** (`Color`, `SrcSampler`; textured: `TextureSampler`,
  `Tiling`, `AlphaOffset`). Every variant squares the source (gamma to near
  linear) and takes a square root at the end. `selBlendType` is None, Alpha,
  Additive, Screen, Multiply, Multiply2X, Substractive (0-6). None and Alpha
  are `lerp(src, C, a)`, Additive `src + C a`, Screen `C a + src (1 - C a)`,
  Multiply `src lerp(1, C, a)`, Multiply2X `2 src (a (C - 0.5) + 0.5)` and
  Substractive `src - C a`. Textured, the colour is the texture's times the
  clip's and the alpha is `saturate(C.a t.a + AlphaOffset)`. The texture's
  place comes from `ScaleOffset` and `Tiling` (vertex shader
  `QuadParams`/`Tiling`/`UVScaleOffset`). The fades to black and white are
  colour masks (None); a hit's blood and the scope mask textured ones.
* **Desaturation** (`DesaturationColour`, `DesaturationScale`):
  `mix(src, dot(src, (0.3, 0.59, 0.11)) * colour, scale)`.
* **RGB separation** (`Opacity`, `MaskSampler`, and the vertex shader's
  `UV0Params`..`UV2Params`). Three samples of the source at
  their own places, each channel lerped from the source's by the mask's
  channel. The result is mixed back toward the source by `Opacity` (1 shows
  only the source). The clip's per-channel scale and offset (pixels) become
  those places (here: zoomed about the centre by the scale).
* **Masked blur composite** (`TextureAlpha`, `MaskOffset`, `SrcSampler`,
  `TextureSampler`): `lerp(src, blurred, saturate((mask.r + MaskOffset) *
  TextureAlpha))` (the blurred copy's alpha also adds; it is taken as 0). The
  vignette mask (`mushbuzz_vignette_01`) is black in the middle and white at
  the edges. `selBlurType` is Gaussian, Radial, Diamond, Directional,
  GaussianSeparate (0-4). The game blurs a quarter-size copy with 9-tap
  passes (`UVOffsets`, weights 0.07-0.15). Here the radial blur takes 16 taps
  along the line from the centre, spanning 0.3% of the distance per unit of
  factor times `Direction` (20 by 5 for speed), each tap spread a little. The
  2D kinds take a 25-tap disc of 10 px per unit of factor.

Colour burn, hue and saturation, noise and distortion weren't found as
separate programs. They follow the editor's help text: colour burn greys all
but the colours near its own (threshold) with contrast; `SaturationRatio` is a
saturation offset (-1 grey) and `HUE_Rotation` a fraction of a turn; noise
modulates by its texture, tiled; distortion moves the source by its texture's
offsets (pixels, `DistortionFactor`). Bloom clips (`fIntensity`, `fExposure`)
multiply the grade's bloom and the scene's exposure. The fades to white go to
1,500 times. Camera-shake clips add to the view's shake. A motion-blur clip
(a melee hit's, the river rapids' QTE hit's) smears the view along the way it
turned that frame: eight taps across the screen over the turn times the clip's
intensity (`fIntensity`, 0.65), no longer than 6% of the screen times
`fMaxBlurModifier`. A slow-motion clip (the drunk walk's, `DrunkFX_NoCamShake`:
down to half speed over half a second, for five) sets the game's speed
(`Time<Virtual>`), unless the weapon wheel is slowing it; the effects
themselves run on the wall clock. Clips name four textures the game doesn't ship
(`critical_health`, `bossinksplash`, `animalslash_center`, `animalslash01`);
those stay plain.

`FC3_POSTFX_TEST=<effect or sequence>` plays one held on, and
`FC3_POSTFX_TEST_TIME=<s>` holds it at a time (job tmp `postfx_run.ps1`).

### Calibration

The options' calibration sliders (0-100) become the engine's brightness
`clamp(0.9 + v * 0.002, 0.9, 1.1)`, contrast `v * 0.02` and gamma ramp
`clamp(0.8 + v * 0.004, 0.8, 1.2)`. These are the `SetBrightness`,
`SetContrast` and `SetGamma` handlers in `FC3.dll`; a render setting holds
them at `+0x12C`, `+0x130` and `+0x134`. Each frame the renderer packs them
into `ColorAdjustment`:

* `x`: (brightness - 1) / 6, clamped from -1 to 1 first;
* `y`: 1 + (contrast - 1) / 2, likewise;
* `z`: 1 / gamma, with gamma clamped from 0.5 to 5;
* `w`: 0.

About 250 final pixel shaders apply it to the squared colour. The UI's
shaders apply it too. The colour is raised to the power `z`, `x` is added,
the result saturated and multiplied by `y`, and the square root is written.
At 50 each slider leaves the picture as it is. Here the post-effects pass
does the same on its linear colour (`Settings::color_adjustment`), as the
last step. It runs only when a slider is off 50. The UI isn't adjusted.
