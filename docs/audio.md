# Far Cry 3 audio

FC3 (Dunia 2) uses Ubisoft's **DARE** sound engine. All audio is stored as
**BAOs** (binary audio objects, version `0x280305`) packed in `.spk` packages,
plus a few hundred streamed `.sbao` files. Decoder: `crates/fc3_formats/src/audio.rs`.
Explorer CLI: `crates/fc3_tool/src/bin/fc3audio.rs`. No game data is stored in
the repo; everything is read from the install at runtime.

## Where the data lives

| archive | files | size | content |
|---|---|---|---|
| `sound.fat` | 433 `.sbao` (256 named in the filelist) | 234 MB | streamed audio (music, long ambiences) |
| `worlds/fc3_main/fc3_main.fat` | 5214 `.spk` | 386 MB | SP sound effects, ambience, music packages |
| `worlds/fc3_main/fc3_main_english.fat` | 10755 `.spk` | 216 MB | SP English voice |
| `worlds/multicommon/multicommon.fat` (+`_english`) | 470 + 7228 `.spk` | 23 + 108 MB | shared/MP sounds and voice |
| `common.fat` | 67 `.spk` + `soundbinary\7fffffff.bao` (project) | 3 MB | UI, front-end |

Paths are always `soundbinary\%08x.spk` / `soundbinary\%08x.sbao` (lower-case
hex). A package is named after its root **event id**. Game data refers to
sounds by that id: Domino scripts load `sndres0x001837ff`, and FCB fields and
XML attributes store `"0x001837FF"` strings or integers.

177 `sound.fat` entries have no filelist name. 81 of them are resolved by
hashing `soundbinary\%08x.sbao` for every stream id that a header references
(`fc3audio names`). The others aren't referenced by any header. The decoder
looks streams up by hash, so names aren't needed.

## Containers

### SPK package (little endian)

```
0x00  u8   version (4 in FC3; 1 = Avatar/FC2 adds per-entry related-id lists)
0x01  "KPS"
0x04  u32  count
0x08  u32  ids[count]
      per entry: u32 size, BAO bytes, pad to 4
```

### BAO

```
0x00  u8   format (2 = package, 1 = atomic)
0x01  u24  version, big endian (0x280305)
0x04  u32  base header size (0x28)
0x08  [16] GUID
0x18  u32 0, u32 0
0x20  u32  class << 28
0x24  u32  2
0x28  payload
```

| class | meaning | count |
|---|---|---|
| 1 | event: what the game triggers | 24487 |
| 2 | header: audio description / container | 31966 |
| 3 | memory audio: payload is encoded data (Ogg/IMA/PCM) | 28506 |
| 4 | project (`7fffffff.bao`) | 1 |
| 5 | stream audio (`.sbao` payload) | 433 |
| 7 | distance curve: volume by distance (see Distance) | 17189 |
| 9 | positioning: 3D or not, and its distance curve (`2fffffff.spk`) | 383 |
| 10 | mixer states / snapshots (`2fffffff.spk`) | 206 |

### Header BAO (class 2). Offsets are from payload start (BAO+0x28); base size 0xBC

`+0x00 id`, `+0x04 type`, `+0x18 f32 gain (dB)` (every type, for example -7 dB on an
engine's 8 kHz layers). Tables follow the 0xBC-byte base header. An audio header (type 1)
names its positioning (class 9) at `+0x88` and its mixer bus (a project entry) at `+0x8C`;
a layered one (type 6) names a positioning per layer, in the last word of each layer's
table entry.

| type | name | fields |
|---|---|---|
| 1 | audio (28848) | `+08 size`, `+38 resource id`, `+3C stream flag &1`, `+48 loop &1`, `+54 channels`, `+5C rate`, `+64 samples (loop start if looping)`, `+6C loop-body samples`, `+74 codec`, `+78/+7C cue count/label bytes`, `+80 prefetch size` |
| 2 | chain (151) | `+48 count`, u32 ids (e.g. 0.5 s silence then a switch) |
| 3 | switch (172) | `+38 count`, `+3C switch group id`, entries `(u32 child, u32 value id)` |
| 4 | random (1588) | `+38 count`, entries `(u32 child, f32 weight, 0, 0)` |
| 5 | sequence (75) | `+48 count`, entries of 0x14 starting with child id |
| 6 | layers (306) | `+3C layer count`, `+40 loop`, `+44 stream flag`, `+58 total size`, `+60 table size`, `+64 prefetch size`, 0x2C-byte entries `(rate, channels, codec, ..., +1C samples)`; data (or the prefetched head of a stream) follows the table inline; streams use `<header id>.sbao` |
| 7 | parameter blend (387) | `+38 count`, 0x1C-byte child entries `(id, curve count, curve table offset, ...)` at +0x94, then 0x18-byte curves `(kind, parameter id, point count, points offset, ..)` and 0x10-byte points `(f32 x, f32 y, u32 shape, u32)`. Offsets count from +0x94. Kind 0 is volume (dB), 1 pitch (semitones); points are joined linearly and held past the ends. All children play together, each shaped by its curves. |
| 8 | silence (439) | `+38 f32 seconds` |

Data location for type 1: memory BAO with `resource id` in the same package,
or `soundbinary\<resource id>.sbao` (payload after its 0x28 header). If
`prefetch size > 0`, the first bytes come from the memory BAO and the rest
from the `.sbao`.

### Event BAO (class 1, 0x48+ bytes)

`+00 id`, `+04 kind`, `+08 target`, `+10 f32 volume`.

| kind | meaning | layout |
|---|---|---|
| 1 | play target | target at +08 |
| 2 | **stop** the target event (e.g. end of an auto-fire loop) | +08 |
| 4 | play two targets | +08, +0C |
| 6 / 10 | push / pop mixer state (class 10) | +08 |
| 8 | switch | +08 group, +0C default, +14 count, 12-byte `(target, 0, value)` entries at +48 |
| 9 | group: trigger all targets together | +0C count, u32 ids at +48 |

### Multi-track block container (layer data, some streams)

Version `0x00100009` (vgmstream's "Ubi SB layer" v9):

```
0x00 version, 0x04 config, 0x08 layers, 0x0C blocks, 0x10 block header size,
0x14 header-data size, 0x18 first block size, 0x1C.. per-layer sizes,
0x5C u32 header size per layer, then per-layer codec headers
(e.g. complete Ogg header pages).
Each block: u32 3, u32 next block size (0 = last), u32 size per layer, data per layer.
```

Concatenating each layer's header and its chunks gives an independent stream
per layer. For Ogg this is a complete, valid `.ogg` file.

## Codecs (PC)

| codec id | codec | headers | hours |
|---|---|---|---|
| 4 | **Ogg Vorbis**: passed through as `.ogg` | 20561 | 25.9 |
| 2 | **Ubisoft IMA ADPCM v6**: decoded to PCM | 7790 | 5.0 |
| 3 | **Ubisoft IMA ADPCM v6, seekable**: decoded | 84 | 0.1 |
| 1 | PCM s16le | 8 | <0.1 |

Rates are 8 to 48 kHz, mono or stereo. Layered sounds have 2 to 8 tracks.
Weapon loops use 48 kHz IMA-seek; voice uses 16/22 kHz Ogg; music uses
32 kHz stereo Ogg.

### Ubisoft IMA v6

```
0x00 u8 version 6, u8 0, 12 bytes ?, 0x0E i16 pcm_samples (=10)
0x10 per channel x2: i16 predictor, u8 step index, u8 0
0x18 u32 ?, 0x1C u32 flags (2 = seekable), 0x20 u32 seek table size, seek table
then pcm_samples raw s16 frames, then 4-bit codes.
```

* Expansion is the standard IMA tables with the **multiply** form
  `delta = ((code&7)*2+1)*step >> 3`. The shift-and-add form doesn't match.
  This was verified: decoding each seekable block reproduces the next block's
  stored predictor and step exactly (16/16; 0/16 with shift-add).
* Nibble order: mono puts the high nibble first. Stereo bytes hold left in the
  high nibble and right in the low nibble.
* Seek table: `u32 count`, then `count+1` pairs `(sample, byte offset)`.
  Blocks start at those offsets with a 4-byte `i16 predictor, u8 step, u8 0`
  per channel. The last pair marks the end.
* When the remaining sample count is odd, the encoder omits the final nibble.
  The decoder repeats the last frame so the length equals the header's
  sample count.

## Verification (`fc3audio check` / `survey --decode`)

* All 24,167 sound files parse (0 errors). Every class/type/kind above is parsed.
* **28,443 / 28,443** playable sounds (deduplicated headers plus layer tracks)
  decode through the converter API in about 5 s, with **0 errors**.
* Ogg: 20,561 streams, 93,098 s. All pass a page walk with **CRC check (0
  bad)**, channel and rate match the BAO header, and the final granule
  position matches the header sample count. That also shows the layer
  de-interleaving is byte-exact. 84 sampled files decoded cleanly with ffmpeg.
* PCM (IMA/PCM): 7,882 sounds, 18,183 s. Lengths match exactly.
  RMS (full scale = 1) histogram: 0 (silent placeholders): 10, <0.001: 4,
  <0.01: 447, <0.05: 2156, <0.15: 2806, <0.30: 2027, >=0.30: 432.
  Zero-crossing rate per sample: <0.3 for 6,937 of 7,882 sounds (88%),
  <0.4 for 7,546 (96%).
  1,050 sounds have more than 0.1% full-scale samples, mostly loud weapon,
  explosion and engine content. In the inspected examples the clipped samples
  are isolated peaks (runs of 1-2 samples) on low-centroid (~300 Hz) material.
  The worst case inspected, a seekable-IMA PKM 3P loop with 2.1% clipping
  (runs up to 50 samples), decodes bit-exactly by the block check, so the
  clipping is in the source. 373 sounds are "noise-like" (ZCR >0.35, RMS
  >0.05). The inspected ones are short (0.1-1 s), high-centroid (5-7 kHz)
  hiss, rustle or splash effects.
* Stored IMA sizes match `header + preamble + nibbles` exactly (4917) or are
  one nibble short (3190, the odd-count case).

## Animal calls

The animal archetypes in `worlds\fc3_main\generated\entitylibrary_full.fcb` give their
calls as event-id strings (`"0x001AA57E"`) in their sound settings. Six field names are
recovered from their CRC-32 hashes: `sndIdleSound` (`3F7D34A7`), `sndHurtSound` (`677C27FC`),
`sndDeathSound` (`B72DADE2`), `sndAttackSound` (`E69E0D8D`), `sndFleeSound` (`A6B9A510`) and
`sndPanicSound` (`716AC21D`). `0xFFFFFFFF` means none. Six more fields remain unnamed.
`fc3tool animal-sounds` lists every animal's six. The tiger's idle, flee and panic are all
one growl, with six alternatives of 1.4-2.9 s; its hurt is 0.65-1.05 s and its death
2-2.5 s. Sharks and fish have none. The game (`wildlife::calls`, `birds::BirdDef`) plays the
idle call every 10-25 s near the player, the panic (or flee) call when an animal bolts or
starts a hunt, the hurt call on each hit and the death call on the kill.

## Sound points

The world places 1,550 `SoundPoint_*` entities. Each has a `CSoundComponent` (`35FD2B44`)
whose `EEACC4D5` field is a u64 holding the event id. For example,
`SoundPoint_C4_waterfall_rumble` plays `0x00015BB9`, a 2.8 s looping rumble. The
converter writes them to `world/sound_points.txt` (`event x y z`, Bevy space). It leaves out
voices and crowd chatter (walla), story moments, and scripted one-offs (gas explosions,
thunder), which leaves 902. The game streams the points as far as their sounds carry
(the farthest end of their distance curves; see Distance), where they fade by those
curves. Points whose curves aren't known stream within 45 m at `(1 - d/45)^2`. Events
whose audio all loops (`SoundBank::loops`, from the manifest's loop flag) run as loops;
the rest sound every 5-14 s.

A loop stops 0.15 s after its last refresh, but a single frame counts for at most 0.05 s
of that. So a streaming hitch doesn't restart the ambience beds, and weapon loops still
stop promptly.

## Surfaces (footsteps and impacts)

Footsteps, landings, slides, bullet, arrow and melee impacts and some weapon echoes switch on
the surface group `00074C89`. The player's footstep event `004E4DC6` also switches on the
gait group `004E4AF4`. `004E4AF5` (walk) and `004E4AF6` (crouched walk) share ten soft
0.53 s steps. `004E4AF7` (run) has twelve louder 0.31 s ones, about 3x the RMS. With no
switch state set, an event plays the union of every surface (290 alternatives for the
footstep).

**Manifest.** The manifest gives every event that reaches either group one record per
switch state. The record's id is `audio::switched_event(event, surface, gait)`: the 24-bit
event id, the surface slot in bits 24-29 (1-46 for `74C8A`-`74CB7`, 47 for `9F95E`, 48 for
`1B0357`) and the gait in bits 30-31. A state that plays the same as an earlier one is
written as `same <id> <earlier id>`. Only 114 events need such records (836 records and
4,715 aliases, +0.35 MB). The game looks up the switched id first, then the id without the
gait, then the plain event.

**Where values come from** (`world/surfaces.bin`, `SurfaceTable`):

* `logicmaterials.lib`: each of the 66 logic materials has an index (`D16D191D`, u8) and a
  surface switch value (`A60D74FC`). For example, 7 `Base/Sand_NP` is `74CA9`, 16
  `Base/Wood_NP` is `74CB6` and 19 `Base/Vege_NP_Grass` is `74CB1`.
* Terrain: each painter layer in `<world>.game.xml` has a `SurfaceTypeID` that is that same
  index. Coastal sand is 7 (south coast 3), grass 19, dirt 4, dead leaves 6, and cliffs and
  coral 2 (`Base/Concrete_NP`, which shares the rock footsteps). The game samples the
  sector's atlas mask (R, G, B, remainder) at the spot and takes the strongest layer.
* Models: 3,874 of the 5,956 `.xbg` files ship a Havok 2010.2 `.hkx` beside them
  (`fc3_formats::havok`; a 16-byte Dunia prefix, then a standard packfile with 32-bit
  pointers). Its `nomadSurfaceTypePalette` lists `(char* material name, u32 logic index)`.
  Each `hkpStorageExtendedMeshShapeMeshSubpartStorage` has per-triangle material indices
  (`materialIndices` u8 at +0x38, or u16 at +0x5C). Those index its `materials` (12 bytes
  each), whose user data (+8) is the palette entry. Most files hold two rigid bodies: a
  coarse one that characters walk on and a detailed `HIGH_*` one for shots (the bodies'
  and shapes' layouts are in `physics.md`).
  The converter keeps, per model:
  * `walk`: the logic material with the most area facing up, from the coarse body.
  * `wall`: the one with the most area overall, from the detailed body.
  * patches: the walkable triangles made of something else. 466 models mix surfaces
    underfoot, about 122k triangles, 1.7 MB.

  RealTree trees count as bark and bushes as bush. `fc3tool hkx-surfaces [substr] [--each]`
  surveys the collision files. `fc3tool rml <game.xml> TerrainPainterLayerDescriptors`
  shows the layers' `SurfaceTypeID`s.
* Characters are flesh (`74C97`), vehicles vehicle metal (`74C9E`), and standing in the
  sea is shallow water (`74CB4`).

`surfaces::SurfaceProbe` turns a ray hit into a switch value. It uses the patch under the
point within 0.4 m, else `walk`, when the hit normal faces up, and `wall` otherwise. The
player's and NPCs' steps cast a short ray down from the feet. The NPCs' steps use the
walk or run event at gait 1 or 3. Landing from a fall faster than 4 m/s plays `0002F219`,
louder the faster the fall, and starting a slide plays `004B68AE`, both on the surface
underfoot. Shots and machete hits switch the impact event on what
they hit: flesh for characters, the slash event `000526BF` for melee. Steps are paced by
how far the feet actually move, so walking into a wall is silent.

## Blends and vehicles

Engines and tyres are parameter blends: loops that play together, each faded and
re-pitched by the game's parameters. The 4x4's engine `00052524` switches on
`00177A5C`. With `00177A5D` (player) it plays three RPM bands, each an 8 kHz copy and a
24 kHz copy. The bands' volume crosses over between 850 and 5,000 RPM, and their pitch
rises by up to about 13 semitones over 0-8,000 RPM. `00177A5F` (AI) plays a single loop.

| parameter | meaning (from the curves) |
|---|---|
| `00440256` | engine RPM, 0-8,000 |
| `0044025B` | throttle, 0-100: up to +2 dB and +2 semitones |
| `00440255` | speed, m/s, 0-20 |
| `00440257` | wheel slip, 0-2: fades in the shared screech layer |
| `0044F4D0` | distance, 0-1: near plays the 24 kHz copies, far the muffled 8 kHz ones |
| `00074C81` | engine wear, 0-1: at 1, -8.5 dB and -3 semitones |
| `000262B7` / `0018980E` | boat engine RPM (0-9,000) / load (0-1) |

The tyre roll `00074EC6` switches on the surface and on `001A4B38`. With `001A4B3A` it
plays a loop per surface plus the screech; with `001A4B39` one generic loop. Volume and
pitch follow speed and slip.

For events that pass through a blend, the manifest adds a gain (dB) and curves for each
voice: `mod <token> <header> <gain dB> <kind>:<param>:<x>,<y>;...`. It also writes the
event for every combination of the switch values under it, at most 256:
`event <id>@<group>=<value>,<group>=<value> ...`. That adds 1,394 `mod` lines and 296
cases, 0.8 MB. In loops, parameters not sent count as 0. One-shots are sent with no
parameters, so they leave those curves out, except distance (below).

## Distance

Each playable sound fades with its distance from the listener by its own curve.
Its header names a positioning BAO (class 9, all 383 in `2fffffff.spk`):

| offset (payload) | field |
|---|---|
| `+0xB0` | u32: 1 = placed in the world (3D), 0 = 2D (menus, music, beds) |
| `+0xC0` | its distance curve (class 7 id, `ffffffff` for none) |
| `+0xC4`-`+0xCC` | three project entries, not used here |
| `+0x108`.. | `120`, `20000`, `1` (floats; meaning unknown) |

A distance curve (class 7) is `<Binary>`, then the point count at `+0x14` and from
`+0x18` 16-byte points `(f32 metres, f32 dB, u32 shape, u32)`. The ids repeat in every
package that uses them, and there are 173 distinct curves. They reach from 2 m to
1,000 m. Footsteps (`004C6625`) fall to -45 dB at 18 m and are gone at 20 m. NPC rifle
fire (`00456A81`) is -2.3 dB at 0 m, -7 dB at 100 m and gone at 200 m.

Parameter `00005EAB` (0-300 in the project's table) is the distance in metres. Blends
cross-fade on it. NPC rifle fire `00046CBF` plays a near shot (0 dB to 4 m, -22 dB at 35 m,
gone at 44 m) and a far, echoing one (silent at 0 m, -1.6 dB at 18 m, full from 40 m).

The manifest ends each `sound` line with its fade: a curve id, `2d` or `-` (unknown). It
lists each curve once: `falloff <id> <metres>,<dB>;...`. 21,441 sounds have a curve,
6,995 are 2D, and 7 are unknown. `fc3audio manifest` rewrites it without a full
conversion.

## Gains and mixer buses

Every header has its own gain (`+0x18`, dB). Random containers often lower
theirs by 3-14 dB, layered headers by up to 17 dB, and audio headers by up
to 13 dB. A sound plays at the sum of the gains on its way from the event,
the containers' and its own. The manifest writes that sum on each
alternative as `hdr:weight:dB` (where it isn't 0). Blends keep theirs in
their `mod` records.

Each audio header also sends to a mixer bus (`+0x8C`; a layered header's at
`+0x74`). The buses are records in the project (`soundbinary\7fffffff.bao`):
`u32 id, u32 kind (3 or 5), u32 parent, u32 count`, then `count` child ids,
then a level (dB) at +0 or at +8 (never both). The levels add up from a bus to
its parents (for example rifle fire's: -9 dB, dialogue's: -2.6). One bus is
muted outright (-96 dB at +0), and a mixer state (class 10) raises it, so
it counts as 0. The project's other tables (mixer states, the roots' own
levels) aren't read. The manifest ends each `sound` line with its buses'
level.

The authored mix sits low: dialogue near -7 dB, the player's gun -12,
music -12, other people's gunfire -14 to -17, ambience -14 to -20 and
footsteps -21. The game raises the whole mix by 6 dB (`MIX_LIFT`) so that
dialogue plays near full scale. Music takes its gains the same way.

The game (`sound::hear`) sets each playing sound's volume every frame from its distance to
the listener. Unknown ones fade by a stand-in curve: full to 5 m and gone at 100 m. Bevy's
spatial sinks are kept for panning only. They fall off as 1/d² past a metre, so each
sound's spatial scale is set to 1/1024. That leaves no fall-off within a kilometre.

Each vehicle archetype in the entity library (`FC3_Vehicles:Land/4x4` and so on) names
its sounds. `fc3tool vehicle-sounds` lists them. These field names are recovered from
their CRC-32: `sndEngineLoop`, `sndEngineIgnition`, `sndEngineIgnitionStop`,
`sndTurnOffEngine`, `sndRollingSound`, `sndBrake`, `sndHandbrake`,
`sndSuspensionSound{Small,Medium,Heavy}`, `sndEngineBurning`, `sndEngineSmoke` and
`sndEngineExplosion`. The rolling loop sits in unnamed field `2142BD54`. A second block
holds the AI drivers' engines.

How the game uses them:

* Engine RPM follows the speed through four gears, or the throttle when standing.
* The player's vehicle uses the player case, plays its ignition on getting in and its
  switch-off on getting out.
* Patrol vehicles within 120 m use the AI case, with distance driving `0044F4D0`.
* The tyres sound on the probed surface. A new surface takes over after 0.35 s, since a
  change restarts the loop. Sideways sliding and braking drive the slip.
* Landings after a drop play the suspension small, medium or heavy sound. Hard braking
  above 8 m/s squeals.
* Smoking and burning engines loop. Blowing up plays `sndEngineExplosion`.
* Land vehicles' archetypes carry an unnamed field `8841202C`: a random set of short,
  sharply decaying impacts (4x4, sedans and hatchbacks `001035C8`, technical `001036D0`,
  buggy `001037F2`, truck `0000545F`, quad `00052520`). It plays as the crash when a vehicle
  loses more than 4 m/s in one step, louder for harder knocks.

`LoopSfx` carries the switch state and up to four parameters. Changing a parameter
updates each running voice's volume and playback speed.

## Outpost alarms

`CombatHelpers/AlarmPoint`, the alarm box, has four state sounds as `(start, stop)`
event pairs:

| state | start / stop | sound | use |
|---|---|---|---|
| STATE01 | `0013DA45` / `0013DA4F` | 3.6 s loop of 2.1 kHz beeps | arming |
| STATE02 | `0013DA46` / `0013DA50` | 8.2 s loop alternating 2.3 kHz and 1.66 kHz | the alarm, played from every box while it's up |
| STATE03 | `0013DA49` / `0013DA4E` | 1.7 s one-shot | switching a box off |
| STATE04 | `0013DA48` / `0013DA4D` | loud hum, bursts and noise | damaged box |

## Player foley and stings

The player archetype (`player:MainCharacter/PawnPlayer`) references about 100 events. These
field names are recovered from their CRC-32:

| field | event | use |
|---|---|---|
| `sndDetected` | `00066CED` | sting when spotted (once per encounter, 12 s apart) |
| `sndLevelUp` / `sndLootCash` | `00047221` / `0004721A` | a level gained / cash looted |
| `sndCrouchSound` | `0045129A` | crouching or standing |
| `sndUnderwaterStartSound` | `00052615` / `00052617` / `00052618` | a splash, then the underwater bed (the sea uses `00052617`) |
| `sndUnderwaterStopSound` | `00052619`-`0005261B` | the splash coming up; `00052416` adds a gasp (after 8 s under) |
| `sndUnderwaterStopDeathSound` | `00052415`, `00052418`, `0005241B` | (unused) |
| `sndPassByNearSound` / `sndPassByUnderwaterSound` | `00448BC8` / `0045546A` | bullets passing within 2.2 m |
| `sndInhaleSound` / `sndExhaleSound` | `0002EBDD` / `0002EBDC` | holding breath |
| `sndDeathSound` | `00025613` | the player dying |
| `sndWeaponWheelOpen` / `Close`, `sndMenuClose`, `sndObjective`, `sndMission*` | | UI / missions (unused) |

The zip-line anchor (`FC3_Interactive:Ziplines/ZiplineAnchor`) gives `sndOnZipline`
`00026194` (stop `sndStopOnZipline` `00026186`). It's a stereo blend of three loops on speed
`000261B5` (0-20 m/s), and `0009AC95` is the hook catching the line.

The falling wind `00046FFA` (stop `00046FFB`) is a blend on fall speed `00046FF7`
(0-40 m/s). It plays in freefall, in the wingsuit and under the canopy. Strokes on the
surface are the footstep on deep water `74CB3`.

A loop event's non-looping voices, such as the splash going under or the boat's hull slaps,
play once when the loop starts instead of repeating.

## Library API (`fc3_formats::audio`)

```rust
// whole install (converter / game)
pub fn list_sounds(vfs: &Vfs) -> Vec<SoundRef>;
pub fn decode_to_file_bytes(vfs: &Vfs, sound: &SoundRef) -> Result<(Vec<u8>, &'static str /*"ogg"|"wav"*/)>;
pub fn decode_ref(vfs: &Vfs, sound: &SoundRef) -> Result<DecodedSound>;
pub struct SoundGraph { events, headers, package_of, packages }   // SoundGraph::build(vfs)
impl SoundGraph {
    pub fn sounds(&self) -> Vec<SoundRef>;
    pub fn resolve(&self, id: u32) -> Vec<Variant>;                                  // everything an event can play
    pub fn resolve_with(&self, id: u32, switches: &HashMap<u32, u32>) -> Vec<Variant>;
    pub fn voices(&self, id: u32, switches: &HashMap<u32, u32>) -> Vec<Vec<Variant>>; // simultaneous voices x weighted alternatives
    pub fn manifest(&self) -> String;                                                // text index for a cache
}
// SoundRef { header_id, layer: Option<(i, n)>, package, codec, channels, sample_rate,
//            num_samples, looping, loop_start, streamed }  file_stem() = "001837fb" / "0001712e_l0"

// single files
pub fn decode_sound(bytes: &[u8]) -> Result<Vec<DecodedSound>>;                        // .spk, in-package data only
pub fn decode_sound_with(bytes: &[u8], files: &mut dyn SoundFiles) -> Result<Vec<DecodedSound>>; // + .sbao via callback
pub fn pcm_to_wav(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8>;
pub enum SoundData { Pcm { sample_rate, channels, samples: Vec<i16> }, Ogg(Vec<u8>) }
// low level: Spk::parse, Bao::parse, parse_header -> Header, parse_event -> Event,
// deinterleave_layers, decode_ubi_ima, ogg_info (page/CRC validator)
```

Playing an event: `graph.voices(event_id, &switch_state)` returns voices that
start together. In each voice, pick one variant by weight, then play the
header's file. A layered header plays all of its `_l<i>` tracks at once.
Stop events (kind 2) and mixer events return nothing. The game should stop the
loop that the paired start event started. The CLI command
`fc3audio resolve <id> [--switch g=v]` prints the same structure.

## How sounds are referenced (naming)

The sound data contains no names, so names come from the gameplay data.
Field names in FCB files are CRC-32 hashes. The Gibbed.Dunia2 FC3
binary-class definitions (public) name them, e.g. `BE3FDC7D` =
`sndStartAutoBulletShot`.

| source | what it names |
|---|---|
| `worlds\fc3_main\generated\entitylibrary_full.fcb`: `WeaponProperties` library, `snd*` fields (values are `"0x%08X"` strings) | weapon fire (1st/3rd person, silenced), empty click, last shots, melee hit, flame, bow draw |
| same file, `CExplosive` (`sndExplosionSound`, `sndEarsRingingExplosionSound`, `sndWaterSurfaceExplosionSound`) | explosions |
| same file, `player` library (`MainCharacter/PawnPlayer`), animals (`AIObject/.../Sound`), vehicles (`CFCXVehicle/Sound`), `CSoundComponent` (`EEACC4D5`, u64) | player foley, animal calls, engines, placed emitters |
| `generated\databases\generic\materialimpacts.lib`: per impact name, field `14732343` | bullet/arrow/melee impacts, **footsteps**, object physics hits |
| `generated\databases\generic\logicmaterials.lib`: field `A60D74FC` | surface material → value of switch group `00074C89` |
| `generated\databases\generic\soundregions_default.obj` | biome ambience (multitrack loop `415791DD`, wind `9CB97A19`, rain `61F7390B`, random one-shots `1B1A3141` per Day/Night × Near/Medium) |
| `generated\nomadobjecttemplates.fcb`: `CFCXMusicService` states (`sName`, `sndStartSoundId`) | music |
| `ui\**\*.feu`: sound id table in each Flash file | UI |
| `dialog\mpbarkdata.bark.xml` (stimulus / speaker / `barkSoundID0`), `dialog\spbarkmanager.xml` (`soundId`), `dialog\conversationtable.dat` (speaker actor + `E22BD13A` sound id) | voice barks and chatter |
| `languages\<lang>\speechlength.bin` | list of every speech sound id (21857) with its length |
| Domino `*.lua`: `sndres0x........` | mission scripted sounds |

`soundregions.xml` in `common.fat` is a stale editor copy. Its ids don't
exist; use `soundregions_default.obj`.

### Surface switch (`00074C89`) values (from logicmaterials)

`74C8A` asphalt, `74C8B/8C` bark, `74C8D/8E` brick, `74C8F/90` concrete,
`74C91` default, `74C92` dirt/dirt road, `74C93` drywall, `74C94` electronics,
`74C95/96` fabric, `74C97` flesh, `74C98` flesh head, `74C99` glass,
`74C9A` bulletproof glass, `74C9B/9C` metal, `74C9D` metal res, `74C9E`
vehicle metal/boat hull, `74C9F` fence, `74CA0` mud, `74CA1` no impact,
`74CA2/A3` plastic, `74CA4` vehicle plastic, `74CA5` rock, `74CA6/A7` rubber,
`74CA8` vehicle rubber, `74CA9` **sand**, `74CAA` sandbag, `74CAB` tile,
`74CAC` tree branch, `74CAD/AE` vegetation, `74CAF` bush, `74CB0` dry
vegetation, `74CB1` **grass**, `74CB2` soft vegetation, `74CB3` deep water,
`74CB4` shallow water, `74CB5/B6` wood, `74CB7` wood res, `9F95E` metal tower,
`1B0357` tall grass (all `0x000...`). Footstep events also switch on group
`004E4AF4` (values `004E4AF5/6/7`: walk, crouched walk and run, see Surfaces above). With no
switch state, `resolve` returns the union.

## Sound ids by category

Ids are event ids (`soundbinary\<id>.spk`). "v" = number of playable variants
behind the event. 1P = first person, 3P = third person / NPC.

### Weapon fire

Automatic weapons have a **start** event that loops (a 2-layer 48 kHz loop)
and a **stop** event. For the AK47, the stop event (kind 9) stops the loop and
plays a tail (7 variants) plus an echo switched on surface group `00074C89`
(30 variants). Single-shot weapons use one group event (36–43 variants across
voices: shot, tail and echo). Entity names are the game's internal prototype
names; the class column is inferred.

| weapon (entity) | class | 1P single | 1P auto start / stop | 3P | silenced | empty |
|---|---|---|---|---|---|---|
| FC3/AK47 | assault rifle | - | `00046CB3` / `00046CB8` | `00046CBF`/`00046CC2`, `0005274B`/`0005274E` | `001777D2` / `001777D4` | `0002EC34` |
| FC3/P416 | assault rifle | - | `001035CD` / `0018F645` | `001035E1` / `001035E2` | `00173173` / `0018F646` | `0002EC34` |
| FC3/FAMAS | assault rifle | - | `00172E90` / `0018F63F` | `00172FB1` / `00172FAE` | `0017317C` / `0018F63E` | `0002EC34` |
| FC3/GalilAce52 | assault rifle | - | `00172E8A` / `0018F638` | `00172FA9` / `00172FA6` | `001777D9` / `0018F639` | `0002EC34` |
| FC3/STGW-90 | assault rifle | - | `00172E94` / `00046CDA` | `00046CD0` / `00046CD3` | `0017316F` / `0017316E` | `0002EC34` |
| FC3/MK16 | rifle (single + auto 3P) | `001034AC` (43 v) | - | `001896E3` (12 v); auto `0005274D` / `0005274F` | `0017789D` | `0002EC34` |
| FC3/MP5 | SMG | - | `0009FA70` / `0018F643` | `001035B1` / `001035B6` | `00173176` / `0018F644` | `004493A0` |
| FC3/A2000 | SMG | - | `0004786E` / `0018F63A` | `00074B88` / `00074B87` | `001777D6` / `0018F63B` | `004493A0` |
| FC3/VZ61 | SMG | - | `00046CE9` / `0018F647` | `00046CDE` / `00046CE3` | `0017316C` / `0018F648` | `0002EC36` |
| FC3/BZ19 | SMG | - | `001035A3` / `0018F63C` | `001035AB` / `001035AA` | `0017317F` / `0018F63D` | `004493A0` |
| FC3/KSV | SMG | - | `00047874` / `0018F641` | `00074B80` / `00074B7F` | `00173179` / `0018F640` | `0002EC34` |
| FC3/U100 | LMG | - | `0017314F` / `0018F649` | `00189715` / `00189716` | - | `0002EC34` |
| FC3/PKM | LMG | - | `0002E680` / `0009FA75` | `0002E67A` / `0002E67B` | - | `0002EC36` |
| FC3/M249 | LMG / mounted | - | `001035D3` / `0018F642` | `001035EB` / `001035EC` | - | `0002EC36` |
| FC3/DSHK_Mounted | HMG | - | `00046D2C` / `00046D2B` | `00046D32` / `00046D37` | - | `004493A3` |
| FC3/6P9 | pistol | `0001706E` (36 v) | - | `00017069` (12 v) | `0009FA29` | `0002EC35` |
| FC3/KW1911 | pistol | `00046E1D` (43 v) | - | `00046E1C` (12 v) | `0005258B` | `0044939E` |
| FC3/DesertEagle | pistol | `0018F690` (43 v) | - | `00046E1C` | - | `0044939E` |
| FC3/SPAS12 | shotgun | `0009FA86` (41 v) | - | `0010347F` (6 v) | - | `0002EC3E` |
| FC3/M133 | shotgun | `00017076` (41 v) | - | `00007966` (12 v) | - | `0002EC3E` |
| FC3/1887 | shotgun | `0018F36C` (41 v) | - | `00177B64` (6 v) | - | `0002EC3E` |
| FC3/R700 | sniper | `00017025` | - | `00007963` (12 v) | `0017788C` | `0002EC3D` |
| FC3/SVD | sniper | `000262EE` (43 v) | - | `000208E4` (12 v) | `00177894` | `0002EC3D` |
| FC3/M93 | sniper (likely Z93) | `0017311F` | - | `00177B7B` | `00177897` | `0002EC3D` |
| FC3/HUNTER | handgun? (3P shared with pistols) | `001034F3` | - | `00046E1C` | - | `0044939E` |
| FC3/RPG-7 | launcher | `00189801` (8 v) | - | `000470D7` (5 v) | - | `00449403` |
| FC3/GM-94 | grenade launcher | `00173146` | - | `0018970C` | - | `0002EC34` |
| FC3/BOW | bow | `0010361C` (draw `0010361D`, abort `00103605`) | - | `00007CB7` | - | `0044939E` |
| FC3/FLARE_GUN | flare | `00007CB6` | - | `00007CB7` | - | `0044939E` |
| FC3/LPO-50 | flamethrower | - | flame `00074C7D` / `00074C7E`, idle `00074D77` / `00074D76` | `00103599` / `0010359F` | - | `0018F552` |

The game gives each weapon these events (`weapons::weapon_sounds`). Automatic weapons
loop the start event while the trigger is held. Once the gap since the last shot
exceeds 1.6 intervals, the stop event plays, with its echo switched on the ground's
surface. A fitted suppressor switches to the silenced pair, and an empty magazine clicks
with the weapon's own empty sound.

Shared: last shots in magazine `0000584D` (few left) and `0000584E` (last);
rifle-butt melee hit `001A6E08` (4 v); machete hit `001A4E0B`; bullet fly-by
for the player: `PassBy` group on `MainCharacter/PawnPlayer`
(`00448BD2`, `00448BC8`, `0045546A`).

### Reload

Not stored on weapon entities. Reload foley is triggered by `PlaySound` events in the
`.mab` clips (see `docs/animation.md`, section 7). The AK's forced reload plays
`0009F7DF`-`0009F7E4`, `00189552` and `00189551` over 2.9 s. The game plays every clip's
sounds as a rig's playhead passes them (`characters::clip_sounds`). This covers
first-person reloads, draws, takedowns and fire clips (non-spatial), and characters
within 25 m (at their position). The generic reload sound `00189820` is left for
weapons whose arms clip has none. MP voice callouts
for reloading are under Barks below.

### Footsteps / movement (switch on surface `00074C89`)

| use | id |
|---|---|
| player footstep (walk and run) | `004E4DC6` (350 v) |
| NPC footstep walk / run | `004550BA` (257 v) / `0002F20A` (317 v) |
| MP silent footsteps | `0045C068` |
| landing | `0002F219` (19 v) |
| slide | `004B68AE` (22 v) |
| player falling (start / stop) | `00046FFA` / `00046FFB` |
| hold-breath inhale / exhale | `0002EBDD` / `0002EBDC` |
| water (ocean / lake / river handler) loop | `00052617` / `00052615` / `00052618` |

### Bullet and other impacts (`materialimpacts.lib`, switch on surface)

| impact | id |
|---|---|
| Weapon/Bullet (also Blood_Bullet, Death_Bullet) | `0002572E` (146 v) |
| Arrow normal / poison | `001898B0` (132 v) |
| Arrow explosion / fire impact | `001898B1` |
| Melee slash | `000526BF` (47 v) |
| RPG ricochet | `001896EB` |
| Rock / water splash | `0002EB9C` / `00074BCB` |
| physics hits Wood S/M/B | `001AEC90` / `001AECFF` / `001AEDE8` |
| Metal S/M/B | `001AEC91` / `001AECFE` / `001AEDE7` |
| Concrete S/M/B | `001AEC8C` / `001AECFB` / `001AEDE4` |
| Glass S/M | `001AEC8E` / `001AECF7` |
| dropped weapons (assault/pistol/RPG) | `0002EE18` / `0002EE17` / `0002EE16` |

### Explosions

| source | id |
|---|---|
| frag grenade | `0002EC6E` (17 v), bounce/land `00005836`, fuse `0000583E` |
| RPG-7 rocket / auto rocket | `00177B45` / `00046D64` |
| 40 mm grenade | `00046D63` |
| IED / C4 | `00074BD3` (11 v) |
| proximity mine | `00074F08` |
| molotov | `001035B9` |
| explosive / fire arrow | `0018F31C` / `001B0229` |
| mortar | `0001FFF6` (switched, 22 v) |
| propane tank / oil barrels | `00187EC0` / `0009AD7F` |
| ear ringing after nearby blast | `001A4B9B` (9.5 s) |

### Ambience (`soundregions_default.obj`)

Each region has a looping multitrack (layered; play all tracks), wind and rain
loops, and a pool of random one-shots (birds, insects, animals) that are
scheduled around the listener.

| region | multitrack loop | wind | rain | random one-shots |
|---|---|---|---|---|
| STR_Jungle_North_Island | `001133BD` (6 tracks, 120 s) | `0000599B` | `004B1969` | `00159609` (36 v), `0015960D` |
| STR_Jungle_Scary | `001133BC` | `0000599B` | `004B1969` | `00113572`–`00113575` |
| STR_IslandForest | `0000585E` (6 tracks, 116 s) | `0000599B` | `004B1969` | day `00061874` (22 v), near `00061873` |
| MEM_IslandForest_Swamp | `000078A7` | `000171AF` | `004B196A` | `00061728` |
| IslandBeach | `00007874` (6 tracks) | `0001FC0E` | `004B196A` | `0006182C` |
| STR_BeachWaves (ocean shore) | `000A6821` | `0000599B` | `004B196A` | `00159655`, night `00159385` |
| MEM_OpenOcean_LP (open sea) | `000174E1` | `0001FC0E` | `004B196A` | - |
| STR_OpenField_Med / Heavy | `00061A27` / `00061A25` (8 tracks) | `00005996` | `004B196B` | - |
| MEM_Grassland_Field | `000D3349` | `0000599B` | `004B196B` | `001135AA`, night `000A6BA4` |
| STR_RebelVillage | `0006BC18` | `0000599B` | `004B1969` | `000A6A34` (177 v) |
| MEM_Mountain_Peak_Med | `00113374` | `00005996` | `004B196B` | - |

Bird/animal one-shot pools that are shared across regions:
`000A6BA9` (63 v, forest/village day), `000A6BA5` (23 v, night).

Where the game paints these regions isn't recovered. None of the world entities, the
sector descriptions or `game.xml` names them. So `sound::Region` picks one from the
ground:

| condition | region |
|---|---|
| on a boat over deep water | open sea |
| within 5 m of sea level, or a sand layer | beach |
| more than 230 m up | mountain peak |
| `South_*` or grassland layers | open field |
| jungle, dead-leaves and bamboo layers | north-island jungle |
| coastal and cliff layers | island forest |

The terrain layer is the strongest in the atlas mask under the player. Region beds
crossfade at 0.35 per second. Every 4-12 s one of the region's day or night calls plays
somewhere within 30 m (`FC3_REGION_DEBUG` logs the region).

### Music (`CFCXMusicService` states)

| state | id |
|---|---|
| main menu | `0019BF2A` (200 s loop) |
| loading screen / pause | `0019B827` / `001AF344` |
| `MUS_MIS_PRO_Exploration` | `0019C0AE` |
| `MUS_MIS_ACT_Assault_Play_LP` / `_Military_Play_LP` / `_Chaos_Play_LP` | `0002FCB7` / `0002FC39` / `0002FC78` |
| `MUS_MIS_DEF_Action_Play_LP` / `_Chaos_Play_LP` / `_Stealth_Play_LP` | `0002FC43` / `0002FCC1` / `0002FC82` |
| `MUS_MIS_STR_Stealth_Play_LP` / `MUS_MIS_STR_Chaos_Play_LP` | `0002FC64` / `0002FCA3` |
| `MUS_MIS_PRO_GoTo_Play_LP` / `MUS_MIS_PRO_TimedMission_Play_LP` | `0002FC4D` / `0002FC8F` |
| final credits | `0019C155` (651 s) |

About 300 further mission and cinematic states (`MUS_MIS_*`, `MUS_CIN_*`)
are 32 kHz stereo Ogg, mostly streamed. List them with
`nomadobjecttemplates.fcb` (field `A164F872`).

### UI (Flash sound tables, classified by use count and length)

| id | length | used by | likely role |
|---|---|---|---|
| `001A9CC7` | 0.11 s | pause, shop, loadout, editor menus | navigate / click |
| `00026680` | 0.09 s | inventory, decision wheel | small click |
| `001A9CC9` | 0.21 s | pause, shop, crafting | select tick |
| `001A9CC6` / `001A9CC8` | 1.05 s / 1.41 s | same menus | open / confirm and back / close |
| `001A9CCA`–`001A9CCC`, `001A9CA9` | 0.3–1.7 s | crafting, shop | craft / buy |
| `001AF35A` | 1.4 s | message / popup boxes | popup |
| `001AF37C`–`001AF37F` | 2–4 s | XP indicator | XP / level up |

### Enemy voice barks

* **Combat barks** (`dialog\mpbarkdata.bark.xml`, speaker `PIRATES` = enemy
  faction voice, `PIRATES_3P` = heard from others), by stimulus:
  CombatReaction 22 lines (e.g. `0005EEB9`), Reloading 21 (e.g. `0005B111`),
  Grunts 21 (e.g. `000678DE`), Tactics 72 (e.g. `0005B47A`), Tagging 22,
  CombatAction 10, Health 61. All are 22 kHz mono Ogg. Parse the XML attribute
  `barkSoundID0` (decimal) per `<Stimuli name>`.
* **Merc idle chatter**: `dialog\conversationtable.dat`, field `E22BD13A` (sound
  id) per line, grouped by actor `DialogActors.PIRATE_01..04` and
  `PRIVATEER_01..04` (about 100–270 lines per voice).
* SP combat stimuli (`dialog\pirate_0N\stimuli\<stimulus>`) list per-voice
  line ids, but the line-id-to-sound mapping wasn't found. Use the two sources
  above instead.
* Player barks: `dialog\spbarkmanager.xml` (bark id → `soundId` variations).

## Not done / caveats

* Blend (type 7) volume and pitch curves are applied to loops through `LoopSfx`
  parameters, and to one-shots for distance only. Curve kinds other than volume and pitch
  aren't applied.
* Mixer states (class 10, pushed and popped by events of kind 6 and 10: ducking,
  muting) and the positioning's other entries (perhaps filtering by distance)
  aren't applied.
* Event kind 4 is treated as "play both targets" (the second is often a delay
  or silence).
* Loop points: type-1 loop start and length are exposed. Layered loops loop
  the whole track.
* Where the game paints its sound regions isn't recovered; the region comes from the
  ground (see Ambience).
* The SP stimulus-to-line mapping is only partly identified (see above). Voice is out of
  scope anyway.

## CLI

```
fc3audio survey [--decode]        histogram of classes/types/codecs, decode QC
fc3audio check                    converter path over every sound + QC stats
fc3audio info <path|id> [--raw]   BAOs in a package
fc3audio tree <id>                event -> header graph
fc3audio resolve <id> [--switch g=v]  voices / weighted variants
fc3audio export <path|id> <dir>   write .wav/.ogg (+ RMS/peak/clip/ZCR)
fc3audio manifest <out.txt>       SoundGraph::manifest()
fc3audio find <id...>             BAOs with / referencing an id
fc3audio refs <out.tsv> [filter]  scan game files for sound ids
fc3audio summary <ids.txt> <out>  one-line description per id
fc3audio names                    resolve unnamed sound.fat entries
```

Build with `CARGO_TARGET_DIR=target/agent_audio cargo build --release -p fc3_tool --bin fc3audio`.
Exported audio belongs outside the repo.
