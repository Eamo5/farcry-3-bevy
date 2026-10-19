# Skinning, skeletons and animation (Far Cry 3 / Dunia 2)

This page covers the decoder in `crates/fc3_formats/src/anim.rs` and the inspection
tool `fc3anim` (`crates/fc3_tool/src/bin/fc3anim.rs`). Everything is read at runtime
from the user's own install. No game data is stored in the repository.

Status:

* All 11,934 `.mab` clips in the install parse (`fc3anim survey`: 0 errors).
* All 5,956 `.xbg` models produce skin bindings (`fc3anim skin-survey`).
* Visual checks: I posed a pirate NPC (idle, walk, run, aim with additive offsets,
  hip-fire, shouldered recoil, reload, death) and the first-person arms with the
  AK74M and the 1911 pistol (idle, fire, reload). The poses were exported as OBJ,
  rendered to PNG, and checked by eye. Bodies are not exploded, knees and elbows
  bend the right way, and the weapons sit in the hands.
* Faces are driven by poses: `.dpax` weight clips on `.dpdx` face rigs. All named
  clips and rigs in the install decode (`fc3tool facial-survey`). See
  [Facial animation](#facial-animation-dpax-dpdx).

Credit: the packed quaternion codec and the section-table idea come from
buu342's public notes on the Dunia `.mab` format (PY-DuniaAnimationExtractor wiki).
The MIT-licensed blender-io-dunia add-on gave further hints. The rest was worked
out from the data: block/mask layout, key groups, translations, root motion,
sub-clips and the XBG skin chunks.

## Coordinate conversion

Dunia is right-handed and Z-up. Characters face **+Y**, with their left side at -X.
Bevy is Y-up. We apply the proper rotation `C: (x, y, z) -> (x, z, -y)` everywhere:

| quantity | conversion |
|---|---|
| position / translation | `(x, y, z) -> (x, z, -y)` |
| quaternion `(x, y, z, w)` | `-> (x, z, -y, w)` |
| scale `(sx, sy, sz)` | `-> (sx, sz, sy)` |
| matrix | `M' = C M C^T` (`anim::mat_to_bevy`) |

Because `C` is a rotation, local transforms converted this way still compose, and
skinning matrices still work. After conversion, characters face **-Z** (Bevy
forward), and the first-person `Camera` bone ends up with a near-identity world
rotation. A Bevy `Camera3d` parented to that bone therefore looks down -Z with no
extra correction. Character rigs need no extra turn either.

Triangle order is a separate matter. XBG triangles are clockwise seen from the
side their normals face, which is D3D's default front face. Bevy's front faces are
counter-clockwise, so `mesh.rs` swaps two indices of every triangle. Without the
swap, single-sided meshes draw inside out: from in front of a character you see
the inside of the back of his head, which reads as a hollow face with bulging
eyeballs. `fc3tool fcm-winding <file.fcm>` checks a converted model. Each
triangle's right-hand normal should agree with its vertex normals.

All matrices in the API are column-major `[f32; 16]` acting on column vectors, the
same layout as `glam::Mat4::from_cols_array`. Quaternions are `[x, y, z, w]`.

## Packed values used by `.mab`

* **Quaternion, 6 bytes** ("smallest three"). Words `w0` and `w1` are u16; `w2` is i16.
  * `f1 = (w0 & 0x7FFF) * s - b`
  * `f2 = (w1 & 0x7FFF) * s - b`
  * `f3 = w2 * s - b`
  * `f4 = sqrt(1 - f1² - f2² - f3²)`
  * Constants: `s = 4.315969e-5` (√2/32767) and `b = 0.7071068`.
  * The high bits of `w0`/`w1` choose where the implicit component goes:
    `(1,1) -> (f1,f2,f3,f4)`, `(0,1) -> (f1,f2,f4,f3)`, `(1,0) -> (f1,f4,f2,f3)`,
    `(0,0) -> (f4,f1,f2,f3)` as `(x,y,z,w)`.
* **Translation**: either 3 × i16 scaled by 1/16384 m (6 bytes), or 3 × f32
  (12 bytes). The flag byte selects which (see below).

## `.mab` animation clips

A file is a 16-byte header followed by a chain of **blocks**:

* Header: `u32 version = 0x61`, `u32 crc`, then 8 bytes.
* The first block is at file offset `0x10`. It is the main clip.
* Block section 8 points to the next block (a nested **sub-clip**), or to the end
  of the file.

### Block layout

All offsets are relative to the block base `B`.

| offset | content |
|---|---|
| `+0x00` | `u32` skeleton id. `EB63516B` = human. Weapons and props have their own opaque ids, e.g. AK74M `C31BACD8`, Kimber `DCC90202`. |
| `+0x04` | four 32-byte (256-bit) bone masks, LSB first, indexed by the skeleton's bone order: **constant rotation**, **keyed rotation**, **constant translation**, **sampled translation** |
| `+0x84` | 12 bytes, always zero |
| `+0x90` | `f32[4]` reference rotation. For sub-clips this is the attachment offset at frame 0. |
| `+0xA0` | `f32[4]` reference translation (`w = 1`) |
| `+0xB0` | `"AnD"` + flag byte |
| `+0xB4` | `f32` duration in seconds |
| `+0xB8` | `u32 offset[10]`, relative to `B`; 0 means the section is absent |

Flag byte:

* bit 0: bone translations are i16 (otherwise f32).
* bit 1: root translations are i16 (otherwise f32).
* value 5: sparse keyed root motion.
* Observed values: 0, 1, 2, 3 and 5.

Sections:

0. **Root translation** (the reference node that is the parent of the skeleton root).
   * Flags 0–3: `u16 count (1), u16 nf, f32 rate`, then `nf + 1` samples.
   * Flag 5: `u32 n, f32 time[n]`, then n × (`f32[3]` translation, packed quat, `u16` pad).
1. **Root rotation**: `u16 1, u16 nf, f32 rate`, then `nf + 1` packed quats. Absent for flag 5.
2. **Constant rotations**: `u32 n, u32 0`, then n packed quats, one per set bit of mask 0, in bit order.
3. **Keyed rotations**: `u16 bones, u16 nf, f32 rate, u32 group_offset[(nf >> 3) + 2]`.
   The last entry is the section size; offsets are relative to the section.
   * There are `nf + 1` frames (0..=nf), spaced `1/rate` s apart, so `rate = nf / duration`.
   * Group `g` starts at frame `8g`. Its block holds:
     1. `bones` packed quats for frame `8g`;
     2. `bones` mask bytes (padded to an even count);
     3. for each bone in order, for each set bit `b` in 0..6 (ascending), one packed quat
        for frame `8g + b + 1`.
   * Bit 7 is never set. Frames without a key are interpolated (slerp).
   * All 10,905 non-empty sections in the install match this layout byte-for-byte.
4. **Constant translations**: `u32 n, u32 0`, then n translations (6 or 12 bytes each).
5. **Sampled translations**: `u16 bones, u16 nf, f32 rate`, then `(nf + 1) × bones` samples,
   **frame-major** (all bones of frame 0, then frame 1, ...). The rate is often
   lower than the rotation rate.
6. **Events**: `u32 count`, then records. Each record has a 20-byte header
   (`u16 kind, u16, u32 hash, u32, f32 start, f32 end`) followed by a body that varies
   by kind and can embed `nbCF` objects.
   * Attachment and IK events carry `{u32 crc32(name), char[32]}` name slots.
   * kind 1 attaches object to bone (e.g. `ak74m` → `RightHand`).
   * kind 6 is left-hand IK onto the weapon (`LeftHand` → the weapon's `IK_LeftHand`,
     or its root for pistols).
7. Embedded `nbCF` objects with gameplay events. `PlaySound` objects hold the sound event
   as their first `"0x%08X"` string field; `FillAmmoWeaponEvent` and
   `CameraShakeAndPadRumbleEvent` are others. Each object is preceded by `f32 time, u32,
   u16, u16 object size`. `parse_clip` returns the sounds as `Clip::sounds` (`(time,
   event)`). The AK's forced reload has eight, from 0.04 s to 2.9 s (magazine out, magazine
   in, bolt...). Takedowns carry one or two.
8. Offset of the next block (sub-clip), or the end of the file.
9. Unused.

### Semantics

* Values are **absolute local transforms** (relative to the parent bone), not deltas.
  The exception is additive clips (below).
* Bones in none of the masks keep their bind/rest local.
* Procedural helper bones are never in the masks (see `.skeleton`).
* **Interpolation**: slerp between rotation keys; lerp between translation samples.
* **Root motion** (sections 0/1) is the animated transform of the character root.
  * In flag 0–3 clips the first root key is rotated 180° about Z. `parse_clip` returns
    root motion relative to the first key, so `entity * root(t)` moves the character
    forward along its facing (Bevy -Z).
  * The skeleton root (`Hips`) translation is relative to the root node, i.e. it is
    the hip height above the floor. Locomotion cycles keep the hips in place and
    carry the travel in root motion.
  * Example speeds: walk-fast cycle 3.19 m per 2.1 s; run cycle 5.07 m per 1.03 s.
* **Additive clips**: most have `add_` in the name; aim-offset poses (`..._aim45up_...`)
  are additive too. All rotations are deltas, where identity means no change.
  * Compose as `local = base_local * delta` (post-multiply in the bone's own frame).
  * Add translations to the base translation.
  * This order was checked visually: `aim90up` raises the aim and `aim90lt` turns it
    left. The opposite order is visibly wrong.
  * `Clip::is_additive()` detects these from the data: the root bone has an identity
    rotation and zero translation.
* **Human skeleton**: id `EB63516B`. Bone indices refer to
  `graphics\actors_\_skeleton_master\hips_ref.skeleton` (70 bones).
  * Body clips only animate bones 0–57 (Hips … Camera).
  * The first 58 bones are identical in every human `*_hips_ref.skeleton` (NPC
    variants add facial bones after them).
* **Sub-clips**: nested blocks animate attached objects with their own skeleton.
  * The weapon's sub-clip is the one with the most tracks (`Clip::main_prop_clip`).
    Its bone indices follow the weapon's `*_ref.skeleton` order, which is not the XBG
    NODE order. Match bones by name or hash.
  * Weapon bone 0 is the weapon root, local to the `RightHand` bone (it equals the
    clip's `RightHand_GunPlacement` value).
  * Other sub-clips are single-bone props, such as the spare magazine in the left hand
    during reloads.
* I have not found a loop flag in the clip data. Treat `*cycle*`, `*loop*` and idle clips as looping.

## `.skeleton` (LKS)

* `"LKS\0"`, `u32 version (27)`, then tagged records `u32 0x3ADE68B1, u32 type`:
  * **type 10**, header: `u16 bone_count, u16 31`.
  * **type 9**, bone:
    1. `f32 rot[4]` (x,y,z,w), `f32 pos[3]` (local), `f32 length`;
    2. `u16 parent` (`0xFFFF` = root);
    3. `u8 procedural_type`, then the drivers:
       * 0 = none;
       * 2 = blend: two `(u32 bone, f32 weight)` pairs (knees, elbows);
       * 3 = twist: one pair (arm and fore-arm twist bones);
    4. `u8 0`, `u32 crc32(name)`, `u32 len`, name (no NUL).
* After the bones comes an untagged bone-group table: u16 lists, `0xFFFF` padding.
* Then **type 3** socket records: `u16 id`, `crc+len+name`, `crc+len+bone name`,
  `f32[18]` = (rotation, translation), its inverse, and a copy of the rotation.
  The first (rotation, translation) is the socket offset relative to the bone.
  Examples: `righthand_rifles`, `holster_pistol`.

Procedural bones are driven at runtime (`anim::pose_locals` implements this):

* **Blend** (knee, elbow): world rotation = weighted slerp of the drivers' world
  rotation deltas, applied to the bone's rest world rotation.
* **Twist** (`*TwistA` weight 1, `*TwistB` weight 0.5):
  1. take the driver's rotation relative to the parent, compared with rest;
  2. keep only its twist about the parent's X axis (bones point along local +X);
  3. pre-multiply `twist^weight` onto the rest local rotation.
  * Fore-arm twists follow the hand.
  * Upper-arm twists follow the shoulder, which cancels the arm's own twist.

## XBG skinning chunks

The chunk tree is in `xbg.rs`. Relevant chunks:

* **NODE**: `u32 count`, then records. One record:
  `u32 crc32(name), i32 first_child, i32 next_sibling, i32 parent, f32 rot[4], f32 pos[3],
  f32 scale[3], i32 bind_index, f32 1.0, u32 0x00909000, u32 len, name, NUL`.
  * Parents precede children.
  * Node 0 is the model root, which is the parent of `Hips` on characters.
  * `bind_index` indexes O2BM; it is -1 for nodes that are not skinned.
* **O2BM**: `u32 n`, then n × 16 f32 inverse bind matrices. They are row-major for row
  vectors, which is the same bytes as column-major for column vectors (Dunia space).
  `bind_world(node) * O2BM[bind_index] = I` holds for every model (checked: 0 mismatches).
* **SKND**: `u32 parts`, then per part: `f32[11]` (LOD distance + bounds), `u32 index,
  u32 0, lstring name` (`<NAME>_LOD<n>`). One skin "part" per sub-object per LOD.
* **CLUS**: for each SKND part, `u32 clusters`, then per cluster (174 bytes):
  `u16 material, u16 tris, u16 tris, u16 index_count, u16 stride, u16 vertex_count,
  u16 vertex_format, u16 palette[48], u16[16], u16[16]`.
  * The palette maps vertex bone indices to NODE indices.
  * Unused palette slots repeat entry 0 or hold `0xFFFF`.
* **SKID**: `u32 n`, then `{u32 hash, u16 part, u16 node, u32 0}`.
  * `node != 0xFFFF` means the part is **rigid**: all its vertices follow that node.
    Weapon parts such as `FRAME`, `CLIP` and `SLIDE` work this way, and their buffers
    have no skin element.
  * `0xFFFF` means palette-skinned.
* **LODS primitive**: 7 × u32:
  1. `buffer`;
  2. `part` (SKND index);
  3. `cluster` (index into `CLUS[part]`);
  4. `index_start`;
  5. `last_vertex` (inclusive, buffer-relative);
  6. `first_vertex_byte` (relative to the LOD vertex blob, i.e. `buffer.offset + first_vertex * stride`);
  7. `0`.
  * Indices are buffer-relative.
* **Vertex skin element** (flag `0x10`): 4 × u8 weights (summing to 255), then 4 × u8
  palette-local bone indices. Its offset follows this order:
  1. position: `0x1` = 12 bytes, or `0x2`/`0x4` = 8 bytes;
  2. `0x8` uv0 (+4);
  3. `0x800` uv1 (+4);
  4. `0x1000` (+4);
  5. **`0x2000` (+4)**;
  6. then skin.

  Normal, colour, tangent and binormal follow, and trailing padding fills up to the
  stride.

`anim::skin_info` returns per LOD:

* the primitive bindings, with the corrected material;
* per-vertex NODE-space joints and weights (vertex numbering identical to `mesh.rs`:
  buffers concatenated in order);
* per-NODE inverse binds in Bevy space (O2BM, falling back to the inverse NODE bind
  pose).

Joint indices are NODE indices, so a Bevy `SkinnedMesh` can use one joint entity per
NODE, with `inverse_bindposes = inverse_bind`.

### Mesh decoding pitfalls (all handled)

1. **NODE layout.** The quaternion comes first (`+16`, x,y,z,w), then position (`+32`),
   then scale (`+44`). The `parent` field is at `+12`; `+4`/`+8` are
   first-child/next-sibling. `anim::parse_nodes` reads it.
2. **Materials.** Primitive field 2 is the *cluster* index; the material is
   `CLUS[part][cluster].material`. Reading field 2 as the material is wrong for
   2,989 of 15,877 LOD0 primitives in the install (e.g. `dennis.xbg` would put eye,
   face and glasses on the wrong materials). The converter takes
   `SkinPrimitive::material`.
3. **Vertex format `0x2000`** (e.g. `0x3BDA`, stride 52). It adds 4 bytes after the
   `0x1000` slot, before the skin element. The trailing padding is whatever remains
   of the stride (8 bytes for `0x1BDA`, 4 for `0x3BDA`).
4. **Raw joints.** Vertex bone indices are palette-local. They are remapped per
   primitive through the cluster palette (`SkinLod::joints`). Rigid parts (no skin
   flag, or SKID node ≠ `0xFFFF`) are placed in model space at their SKID node.
5. **Winding.** Triangles are clockwise-front and are reversed for Bevy (see
   *Coordinate conversion*).
6. **States.** Destructible models carry every damage state as rigid parts under
   nodes named `STATE01__...` (intact), `STATE02__...`, `STATE03__...`, sometimes as
   bare `STATE02` / `STATE03` nodes. Vehicles have them for wheels, windshields,
   doors, hoods and trunks. Drawing them all stacks cracked glass over clean glass,
   so the converter keeps only the first state.
7. **Vehicle parts.** Wheels hang under `WHEEL_Front_Left`, `WHEEL_Back_Right`,
   `WHEEL_CenterLeft` and similar nodes, and the steering wheel is the
   `STEERINGWHEEL` node. The converter gives each its own parts and records the node
   in the cached model's `PIVT` chunk: per LOD and part, the node's name and its
   model-space position and rotation, with an empty name for parts that don't move.
   The game spins wheels about +X by distance over radius (the radius being the
   pivot's height above the model's bottom), steers the front ones about +Y, and
   turns the steering wheel about its column. `fc3tool fcm-winding` lists the parts
   with their pivots.

## Posing recipe (reference: `pose_locals` / `compose_worlds` / `skinning_matrices`)

1. Load the model parts with `skin_info`, the clip with `parse_clip`, and the model's
   `.skeleton` with `parse_skeleton`. The skeleton maps track bone index → name hash
   and supplies the procedural drivers.
2. For each NODE whose name hash matches skeleton bone `i`, take rotation and
   translation from the clip track for bone `i` where present. Otherwise keep the
   NODE's bind local; this keeps the model's own proportions for bones the clip
   doesn't translate. Then drive the procedural bones.
3. Compose world matrices, with root motion or identity at the root.
   `skin = world * inverse_bind` per NODE.
4. **Weapons**: weapon root world = `RightHand` world × the weapon sub-clip's
   bone-0 local. The other weapon nodes follow from the weapon's NODE hierarchy and
   the sub-clip tracks, matched by name against the weapon `*_ref.skeleton`.
   Third-person NPCs use the same rule with the `*_tp.xbg` model.
5. **First person**: put the camera at the `Camera` bone's world transform. It is
   ~(0, 1.47, -0.04) m with identity rotation in the idle clips, and it shakes during
   reloads.

## Recommended assets

Paths are archive paths. Clip paths are relative to `graphics\characters\_common\animations\`.

### Human NPC (pirate, assault class)

* Body parts. Skin all four with the same pose:
  * `graphics\__fc3_graphics\_common\characters\enemies\pirates\genericpirates\pirate_assault_head01.xbg`
  * `...\pirate_assault_torso01.xbg`
  * `...\pirate_assault_pants01.xbg`
  * `...\pirate_assault_amrs01.xbg` (arms; the file name really is spelled `amrs`)
* Skeleton:
  * `...\genericpirates\pirate_assault_hips_ref.skeleton` (106 bones, includes the procedural definitions);
  * `graphics\actors_\_skeleton_master\hips_ref.skeleton` also works for clip bone indices.
* Weapon: `graphics\__fc3_graphics\_common\weapons\ak74m\ak74m_tp.xbg` with `...\ak74m\ak74m_ref.skeleton`.
* Other body variants live in the same folder (`*_head02`, `*_afro_*`, `pirate_rocket_*`, `pirate_sniper_*`, ...). Privateers are under `...\enemies\privateers\`.

| purpose | clip | notes |
|---|---|---|
| idle | `locomotion\stand\idle\alert\3rdge_fulbst_idlealert_noaim_v01_+000fw_ak74.mab` | 7.5 s loop, rifle low |
| walk | `locomotion\stand\walk\alert\3rdge_fulbst_walkfastalertcycle_noaim_+000fw_prak47.mab` | 2.1 s cycle, root motion 1.5 m/s |
| walk (relaxed, unarmed) | `locomotion\stand\walk\relax\3rdden_fulbst_walkrelax_lookfwd_+000fw_noweap.mab` | 5.5 s, 1.4 m/s |
| run | `locomotion\stand\run\alert\3rdge_fulbst_runalert_noaim_v00_+000fw_ak74.mab` | 1.03 s cycle, 4.9 m/s |
| aim | `locomotion\stand\idle\alert\3rdle_fulbst_idlealert_aimnormal_v00_+000fw_ak74.mab` | plus additive offsets `weapons\npc\lotuseaters\ak47\aiming\3rdle_fulbst_idlealert_aimnormal_aim{0fw,45up,45dn,45lt,45rt,90up,90dn,90lt,90rt}_+000fw_ak74.mab` |
| fire (hip / suppressive) | `locomotion\stand\idle\envfire\3rdge_fulbst_idlealert_aimenvfire_v00_+000fw_ak74.mab` | 6.8 s loop |
| fire (shouldered recoil) | `multiplayer\weapon\primary\ak74m\mulge_uppbst_shootrecoilreg_+000fw_prak7.mab` | 0.57 s, upper body |
| reload | `weapons\npc\lotuseaters\ak47\reloads\3rdge_fulbst_idlealert_normal_reload_v00_+000fw_ak74.mab` | 3.57 s, magazine sub-clip |
| death (head shot, from front) | `injury\death\stand\head\3rdge_fulbst_diehead00_fromfw_+000fw_ak74.mab` | 1.83 s, apply root motion |
| death (legs) | `injury\death\stand\legs\3rdge_fulbst_dielegs_fromfw_v01_+000fw_ak74.mab` | 4.5 s |
| death while running | `injury\death\run\head\3rdge_fulbst_runalert_diehead01_fromfw_+000fw_ak74.mab` | |

Directional variants follow the same naming: `_frombk`, `_fromlt`, `_fromrt`, and `+090rt` / `-090lt` / `-180bk` for strafes.

### First-person arms and weapons

* Arms/body: the game uses the story hero's `characters\unique\jason\jason_fp_*.xbg`. This
  project uses the generic multiplayer arms instead,
  `graphics\__fc3_graphics\_common\characters\multiplayer\pvp\rebels\rebel_assault_fpsarms.xbg`
  (`...\pvp\pirates\pirate_assault_fpsarms.xbg` is another). Both are headless bodies on the
  same 120-bone skeleton with the `Camera` and `RightHand_GunPlacement` nodes.
* In the game (`fc3_game::fp`): the rig hangs under the view model and is moved every frame so
  its `Camera` bone sits at the camera; the weapon model hangs off `RightHand` at the main prop
  sub-clip's bone-0 pose. Clip names vary a little by weapon (`shootcycleiron` / `ironshoot`,
  `sprintcycle` / `sprint`, shotguns reload shell by shell), so the converter indexes every
  `1stge_uppb_<action>_+000fw_*.mab` per folder into `characters/fp_clips.txt` and the game picks
  each action with fallbacks. Variants such as `_zip+000fw` (holding on to a zip line) are left
  out; they have one arm raised.
* Skeleton: `graphics\actors_\_skeleton_master\hips_ref.skeleton`.
* Assault rifle: `graphics\__fc3_graphics\_common\weapons\ak74m\ak74m.xbg` with
  `ak74m_ref.skeleton`. Clips are in `weapons\primary\ak74m\`, all suffixed `_+000fw_prak7.mab`:

| purpose | clip | notes |
|---|---|---|
| idle | `1stge_uppb_btheaimslow` | 4 s loop |
| idle (aim down sights) | `1stge_uppb_btheironslow` | |
| fire | `1stge_uppb_shootcycle` | 0.1 s per shot; slide moves in the weapon sub-clip |
| fire (ADS) | `1stge_uppb_shootcycleiron` | |
| reload | `1stge_uppb_reloadregular` | 2.83 s |
| reload (empty) | `1stge_uppb_reloadforced` | |
| draw / holster | `1stge_uppb_draw`, `1stge_uppb_holster` | |
| walk / sprint | `1stge_uppb_wlk`, `1stge_uppb_sprintcycle` | |

* Pistol (1911): `graphics\__fc3_graphics\_common\weapons\kimber\kimber.xbg` with
  `kimber_ref.skeleton`. Clips are in `weapons\secondary\kimber_1911\`, all suffixed
  `_+000fw_sekimber.mab`:

| purpose | clip |
|---|---|
| idle | `1stge_uppb_btheaimslow` |
| fire | `1stge_uppb_shootcycle` (0.67 s) |
| fire, last round | `1stge_uppb_shootempty` |
| reload | `1stge_uppb_reloadregular` (2.17 s) |
| reload (empty) | `1stge_uppb_reloadforced` |
| draw / holster | `1stge_uppb_draw`, `1stge_uppb_holster` |

Other weapons use the same scheme:

* clip folders: `weapons\primary\<weapon>\`, `weapons\secondary\<weapon>\`, `weapons\special\<weapon>\`;
* models: `graphics\__fc3_graphics\_common\weapons\<weapon>\<weapon>.xbg`, with `_tp.xbg` for third person.

### First-person actions

Healing and looting use their own first-person clips, under `healing\` and `loot\`:

| action | clips | length |
|---|---|---|
| medkit | `syringe_heals\1stge_uppb_syringe_heal[_2]_+000fw_nowep` | 2 s |
| patch up after bullets | `manual\pierce_heal\..._bullet_spit`, `..._stick_bullet`, `..._medpack_scalpel` | 4-4.3 s |
| patch up after explosions | `manual\explosion_heal\..._propshealing_glass`, `..._nail` | 4.2 s |
| patch up after falls or blows | `manual\crush_heal\..._crush_heal_{thumb,wrist,elbow}` | 4 s |
| put out burns | `manual\burn_heal\..._burnshealinga` | 1.7 s |
| search a body | `1stge_fulb_loot_human_{stand,crouch}_+000fw` | 2 s |
| pick a plant | `1stge_fulb_loot_bush_{stand,crouch}_+000fw` | 1.3 s |
| skin an animal | `1stge_fulb_loot_animal_{medium,large}+000fw` | 4 s / 6 s |

* `uppb` (upper body) clips play on the arms while the player can still move.
  `fulb` (full body) clips drive the camera from the clip's `Camera` bone and hold the
  player still, like takedowns.
* Props are clip attachments (`object` on a bone): `syringe_01`, `scalpel`,
  `heal_lonebullet`, `shrapnel_*`, `bandageroll_01`, `armcloth_0n`, `meatbag`,
  `redmeat_collectible_01` and `machete`. Their models are under `props\equipment`,
  `props\gameplay`, `props\misc` and `weapons\machete`. Each attachment is posed by a
  prop sub-clip, matched in order by bone count, since the sub-clip skeleton ids aren't name
  hashes. For example, the syringe has 3 bones, the meat 10, the meat bag 1 and the
  machete 3. The prop's root follows the sub-clip's bone-0 track relative to the
  attachment bone.
* `fp_arm_wound_01` (a 7-bone wound skinned onto the forearm) is left out.

### Freefall

The wingsuit and parachute clips are under `vehicles\other\wingsuit\`:

| context | intro (once) | loop |
|---|---|---|
| a long fall (over 14 m/s, nothing open) | `1stge_fulb_jump2skydive_nodir_nowep_wingsuit` (1 s) | `..._skydivefalling_...` (2 s) |
| wingsuit | `..._deploywingsuit_nodir_...` (1.17 s) | `..._gliding_nodir_...` (4 s) |
| parachute | `..._deploychute_nodir_...` (1.67 s) | `..._chute_driving_nodir_...` |

* These clips carry the `wing_suit` attachment on `Spine2`. Its sub-clip has prop skeleton
  `62B81C19` and 23 tracks: the `Wing_Suit` root and the 22 skinned bones of
  `props\misc\wing_suit.xbg` (`{Right,Left}Bone01-05` along the arm and body, and the
  `HandleBone` chains of the wrist loops). Its skeleton is `props\misc\wing_suit_ref.skeleton`.
* Placed rigidly in its bind pose, the membrane comes out as a dark sheet across the view. So
  the game gives it a small rig of its own on the arms' `Spine2`. It plays the membrane tracks,
  pulled out of the deploy and glide clips as clips of their own, in step with the arms. Then it
  unfolds along the forearms with the deploy, wrist loops on the hands. In the glide, the arms
  (and it) are out of view.
* The deploys also attach `parachute_deploy` (the pilot chute), which is left out.

### Repair tool

Repairing a vehicle uses `weapons\special\repairtool\1stge_uppb_*_+000fw_sprt`. The game
plays `transitionaim2canrepair` (0.33 s) and then loops `canrepairshootcycle` (5.67 s). Unused:
`canrepairbthe` (idle), `canrepairwlk` (walk), and the `canrepair2aim` and `canrepair2cr`
transitions. The torch is `weapons\weldingtorch\welding_torch.xbg`, attached as `welding_torch` on
`RightHand`. Its sub-clip has prop skeleton `7DEA9E33` and 9 tracks. A second, single-track
sub-clip with the same id is the left hand's IK grip on it, kind-6 attachment `IK_LeftHand`.

### Throws, C4 and mines

Throws are one-off `uppb` clips played over the weapon, which is put away for them (at 1.3x):
`grenade\1stge_uppb_throw_+000fw_grfra` and `..._molotov` (they share the `grenade` folder, so
the index keys the molotov's as `throw_molotov`), `rock\..._throw_..._rock`,
`special\ied\..._{throw,detonate,detonate_empty}_..._spied` and
`special\proximitymine\..._place_..._proxi`. Each one carries its object as an attachment, posed
the same way as the action props: `frag_grenade`, `molotov`, `rock` and `proxmine` in the right
hand, and `ied_c4_bomb` in the left with `ied_c4_remote` in the right. The models are under
`weapons\{grenade,molotov,rock,ied,proximitymine}`.

The thrown object leaves the hand at the hand's furthest reach, then flies (or lands) in the world:

| clip | object | leaves the hand (clip time) |
|---|---|---|
| grenade, molotov and rock throws | the object | 0.12 s |
| C4 throw (left hand comes back, then forward) | `ied_c4_bomb` | 0.28 s |
| mine place (2.17 s; the view tips down to the ground) | `proxmine` | 1.65 s |

The C4 and the mine appear in the world at that moment. Grenades are already in flight, since
they leave the hand 0.09 s in.

### Mounted guns

Turrets are world entities `FC3/Turret_Ground360_DSHK_n`, `..._Ground180_DSHK_n` and
`..._Ground360_M249_n`. Their mount model is `weapons	urret	ripod_01.xbg`, or
`tripodmount_01.xbg` for the bunker mount. The gun model isn't in the entity: it's
the DShK (`weapons\dshk\dshk.xbg`) or the minimi (`weapons\lmg\minimi.xbg`), by
name. The mount's nodes:

- `Turret_Axis`: the swivel.
- `GunMount` (`GunMount001` on the bunker mount): where the gun's origin sits.
- `GunnerPh`: the gunner's feet, 0.79 m behind and turning with the gun.

The turret component gives the arcs: pitch -40..+23 degrees, and yaw ±180 degrees
(Ground360) or ±45 degrees (Ground180).

First-person clips are under `weapons\mounted\{dshk,mkg}`: `idleaim`, `idleiron`,
`shootcycle`, `shootcycleiron`, `aim2iron`, `draw`, `holster` and others. They carry
no weapon sub-clip. Instead the hands are IK attachments (kind 6) onto the gun, so
the game places the gun and pulls the hands onto it. Without IK, the gun is set at a
fixed offset from the view, so its grips sit where the clip leaves the hands: about
0.25 m below the camera bone and 0.2 m ahead of it. On the camera bone itself the
wrists are about 68 degrees below the view and out of frame. So the arms and gun
together are moved 0.2 m out and 0.05 m up, which brings the hands on the grips
into view. Moving them further out exposes the arms mesh's open shoulders.

The minimi carries both its handheld stock and the mounted spade grips, under
`PIECES_HANDHELD` and `PIECES_MOUNTED`. The converter tags parts under those nodes,
and only the set for the use shows.

Vehicles: the technical (`4x4technical_node.xbg`) has a `TurretBaseAttachement`
node, where the ring mount `4x4techturretbase.xbg` goes. The gun pivots 0.85 m above
it, and the gunner stands at `GUNNER_POSITION` in the bed. The 4x4 has only
`GUNNER_POSITION`. The Zodiac has `MountedRing` (`zodiac_mountedring_01.xbg`) and
gunner entry points.

Third-person gunner clips are `weapons\mounted\dshk\3rdge_fulbst_*_modshk`: aim
vertical at -45/0/+45, enter forward/turn, exit, and a grenade stun.

### Guard spots

The world's `SimpleAnimation.<what>[_<faction>_v]_<n>` entities (about 2,400) are places for
idle guards. Each one's contextual-action component names its moves, such as `stp_smoke_start`,
`stp_smoke_loop` and `stp_smoke_end`, with a duration (30 s for smoking).
`cache::guard_spot_kind` maps the names to 11 kinds and keeps 2,142 spots, written to
`world/guard_spots.txt` as kind, position and orientation. It leaves out heavies', civilians'
and scripted points. The clips are mostly under
`smart_terrain_points\fighters\behavior_specific\{relax\relaxguard,cautious\...}` (pirate
`3rdge` and privateer `3rdpriv` builds), plus `...\simple_animation\stand\smoke`,
`...\ground\..._sitground_loop` and `contextual_actions\patrol\{pissing,leanonwall}`. The game
(`guard_spots`) gives an idle guard who isn't patrolling the nearest free spot within 18 m of
his post. He takes its facing, and its loop replaces his idle clip; for smoking his rifle goes
away. Once he's on guard (awareness over 0.25, or any other state), his clips come back.

Marker orientations come from `to_euler(EulerRot::YXZ)`, whose first component is the yaw.
The guard posts used to read the second one (pitch, always about 0), so every sector-spawned
guard faced the same way.

### Villager spots

The civilian points (`SimpleAnimation.<what>_Civ<M|F>`, 49) are written to
`world/villager_spots.txt` by `cache::villager_spot_kind`:

* `ExtW_GardeningHoe_CivM` (hoe, 34)
* `LeanWall_CivM` (lean, 6)
* `SitOnGround_CivF` (sit, 4)
* `PrayingGround_CivF` (pray, 3)
* `Relax_LootCorpse_CivM` (loot, 2)

Their loops are under `smart_terrain_points\civilians\`. Most have man (`3rdcivm`) and woman
(`3rdcivf`) builds: `generic_natives\simple_animation\{neutral\lean,neutral\ground,praying\ground,lootingcorpse}`.
Hoeing is men only: `themespecific\extworking\simpleanim\gardening\3rdcivm_fulbst_gardenninghoe_loop`.
Its clip attaches `hoegardentool` to the right hand, as a one-track prop sub-clip, and plays
six hoeing sounds over its 13.7 s.

The game (`villagers`) works it like this:

1. When an idle villager's timer runs out, there's a 60% chance they head for the nearest
   free spot within 75 m of their zone that has a clip for their sex.
2. There they take the spot's place and facing, and its loop replaces their idle clip for
   30-80 s. A gardener holds the hoe where the prop track puts it.
3. A scare, a wound or a blocked path frees the spot and brings their own clips back.

Most hoeing spots belong to plantations with no villager zone near (28 of 34 around one
field). Hoeing spots within 40 m of each other form a garden. A garden with no zone within
75 m, and not in a held outpost, gets two farmhands of its own when the player comes near.

### Hits and deaths

`injury\death\{stand,walk,run,crouch,coverhi}\{head,torso,legs}\3rdge_*_ak74.mab` and
`injury\hits\{idle,crouch}\...` hold the game's death and flinch clips for rifle-armed
characters. The file names give the side the hit came from: `fromfw`, `frombk`
(or `from180lt/rt`), `fromlt` and `fromrt`. The converter lists them in
`characters/injury_clips.txt`: 101 deaths and 27 hits. The sniper-tower, double-takedown
and chair variants are left out.

The game picks one by the enemy's motion, the hit's region (the head for headshots,
otherwise the body or sometimes the legs), and the side of the shooter relative to
the enemy's facing. It relaxes the side, then the region, then the motion when
nothing matches. It swaps that clip into a copy of the enemy's clip set.
Takedowns and burning keep their own deaths.

Close quarters:

- **Enemies' rifle-butt strikes:**
  `weapons\npc\lotuseaters\ak47\melee\3rdge_fulbst_idlealert_aimnormal_meleehit_v00..02_+000fw_ak74`.
  Each clip is 2.1 s; the butt is furthest forward at about 0.5 s, and the blow lands at 0.48 s.
- **Struck by the player's machete:** `melee\melee_enemy_hitreacts\rifle\3rdge_fulbst_cqchit{ft,lt,rt}_+000fw_prak7_`,
  plus `cqchitrt_-180bk` for blows from behind. These are picked by the side the blow came
  from. They are hit reactions, not attacks, despite living under `melee`.

### Paired takedown clips

Takedowns are pairs under `melee\takedown\` and `melee\stealthtakedown_above\`: the player's
half `1stge_fulb_<name>.mab` and the victim's `3rdge_fulb_<name>.mab`, e.g.
`takedown{back,front}_kill_var{1,2,3}_pirateassault_+000fw_nowep` (2.1 s / 2.3 s),
`takedownback_kill_var1_pirateheavy_...` for armoured enemies, and
`stealthtakedown_above_a[_heavy]_+000fw_{hhmac,target}` (3 s).

* Both halves share one origin, without root motion. With the victim's root on the origin, the
  player's left hand meets his mouth in the back kill, the right hand is at his chest in the
  front kill, and the knife reaches his chest in the kill from above. Adding either side's root
  motion breaks that contact; the player's root motion only says where its capsule ends up.
* The victim's clip turns him by its reference rotation, read once from the kill clip: 0 for
  the back kills, a half turn for the frontal kills and the kill from above, where he faces the
  player. The player's reference rotation is ignored, since the camera bone sets the view.
* The clips carry every prop they might use, all on `RightHand`: `npc_knife`, `frag_grenade`
  and `machete`. The machete's sub-clip has prop skeleton `744CBC42` (as in the machete's own
  clips). It is not the largest sub-clip, so `main_prop_clip()` would pick the wrong one.
* Chain takedowns branch from the kill clip's hold, at about 1.2 s, where the poses match, into
  `..._kill2knife[_varN]_...` (0.5 s), then `..._knifethrow_...` (1.83 s for the player, 0.93 s
  for the victim). These three play back to back in the same frame. The knife leaves the hand
  about 0.6 s into the throw.
* Grenade takedowns branch from the same point into `..._kill2grenade[_varN]_...` (1.0 s,
  grabbing the victim's grenade), then `..._grenade_...`. In the player clip (1.23 s), the
  camera steps back as the foot comes up. In the victim's clip (2.17 s), the body is kicked
  about 6 m forward and is on the ground by about 1.2 s. The game sets the grenade off 1.7 s
  into the kick, at his `Hips` joint, raised to the terrain surface, because the clip lands him
  on its own flat ground.
* Takedowns from above come in four, by where the player lands relative to the victim's
  facing: `a_+000fw` (in front of him), `a_-180bk` (behind), `b_+090rt` (his right) and
  `b_-090lt` (his left). Heavies have `a_heavy_{+000fw,-180bk}` only. The player's halves of
  the `a_` pair are effectively the same. The victims' reference rotations are 180, 0, -90 and +90 degrees, so
  with the player turned to face him on landing, each keeps him within a few degrees of the way
  he stood, instead of swinging him round to face the player. `FC3_TD_DEBUG` logs how far he
  turns. `FC3_TAKEDOWN_MODE=above_{front,back,left,right}` drops in from that side. Leave Double
  Takedown out of `FC3_SKILLS`, since the test's second guard would turn it into a double.
* Double takedowns from above use `stealthtakedown_above\1stge_fulb_doubletakedownfront_{lt,rt}_var1_+000fw_hhmac`
  (3.8 s) with `..._target_1` (3.3 s) and `..._target_2` (2.6 s). The halves share a scene, but
  the two victims start in different places in it. `parse_clip` makes root motion relative to its
  first key, so that key is kept (`RootMotion::start_translation`/`start_rotation`).
  `target_2` starts 2 m to the side of `target_1`: x = -2.0 for `lt`, +2.0 for `rt`, z +0.1,
  with the same facing. The game places the second victim at the first plus that offset, in the
  scene's frame and on his own ground. `lt` means the second victim is to the left of the first,
  seen from where the player comes down (the player's half faces identity, its root starting
  1.74 m up and 0.65 m back).
* Pistol takedowns branch the same way into `..._kill2pistol[_varN]_...` (1.87 s for both
  halves). The player pulls the victim's pistol and holds his body up in front. The pistol is the
  `desert_eagle` object, and its sub-clip has prop skeleton `876BE34E` (10 tracks). In the
  draw, it starts about 0.7 m from the hand, at his holster. Each shot is
  `1stge_fulb_takedownfront_shoot[_lt|_rt]_...` (0.67 s, recoil only, the pistol its only
  prop). The victim meanwhile holds `3rdge_fulb_takedown{back,front}_aiming_...`, a single-frame
  pose. The player's shot and `..._takedownfront_droppistol_...` (1.4 s) serve both sides; the
  victim's drop is per side (0.83 s). The player's `aiming_{-025,+000,+025}fw` poses are
  zero-length aim offsets. The game doesn't use them: the scene swings round its origin to each
  target, as for the knife throw. The shots and the drop carry no machete track, so the machete
  is hidden while they play.
* Takedown drags branch the same way into `..._kill2drag[_varN]_...` (1.33 s, getting a grip),
  then `..._drag_...`, a 2 s hold loop with only a few tracks animated. The dragging motion
  comes from the player walking backwards. `..._dragdrop_...` (1.5 s) lets go. While
  dragging, the scene's origin follows the player, who moves and looks freely at 1.5 m/s, so
  the body stays at the clips' shared origin in front.
* Takedowns from below use `..._{water}ledgetakedown_{fw,bk}_var1_+000fw_{nowep,target_1}`
  (2.2 s for the player, 1.83 s for the victim). `fw` is for a victim facing the player. Here
  both halves have root motion, so their first keys matter. The player's root starts 2.29 m
  below the victim's and 0.99 m out from it (0.415 against -0.578). It rises the full 2.29 m by
  about 1.0 s, as the player jumps up to grab him, and ends back where it began. The victim's
  root (and his hips) stay at ledge height: he is pulled 0.1 m out and 0.3 m to the side. His
  half ends leaning over the edge, and the original's ragdoll takes the fall from there.
* The water variants (1.77 s for both halves) start the player 1.75 m down and end 0.86 m up.
  Here the victim's root does take him in: 2.7 m down and 2.8 m out past the player, turning
  a half turn, for `bk`; 1.06 m down and 2.1 m out for `fw`.
* In the game, the player's root motion is added under the camera bone (the scene's origin
  stays at the player's feet). The victim starts up at the first-key offset and follows his
  root motion, turn included. After a ledge half he tips over the edge and drops to the
  ground past the player's shoulder, on the side his half pulled him (`npc::LedgeFall`).
  From the water, only this takedown is open while swimming.
* In the game (`fc3_game::fp`), the scene stays where it started. The victim stands on the
  origin and the camera follows the player clip's `Camera` bone, eased in from the view over
  0.3 s. The player can't move or look until the clip ends. For a chain, the scene swings round
  its origin during the knife pull to face the second enemy.
* `fc3anim bones <xbg> <clip> <bone,bone..> [times] [--root-motion]` prints model-space bone
  positions over a clip, which is how the contacts above were checked.

## API (`fc3_formats::anim`)

```rust
pub fn parse_nodes(xbg: &[u8]) -> Result<Vec<Node>>;                    // corrected NODE parser
pub fn skin_info(xbg: &[u8]) -> Result<SkinInfo>;                       // all LODs
pub fn skin_info_lods(xbg: &[u8], max_lods: usize) -> Result<SkinInfo>;
pub fn parse_skeleton(bytes: &[u8]) -> Result<Skeleton>;
pub fn parse_clip(bytes: &[u8]) -> Result<Clip>;                         // main clip + sub_clips
pub fn parse_clip_with_skeleton(bytes: &[u8], skel: &Skeleton) -> Result<Clip>;
impl Clip { fn resolve(&mut self, &Skeleton); fn is_additive(&self) -> bool;
            fn main_prop_clip(&self) -> Option<&Clip>; fn track_for_bone(&self, u16) -> Option<&Track>; }
impl Track { fn sample_rotation(&self, t) -> Option<Quat>; fn sample_translation(&self, t) -> Option<Vec3>; }
impl RootMotion { fn sample(&self, t) -> (Vec3, Quat); }
pub fn pose_locals(nodes: &[Node], skel: &Skeleton, clip: &Clip, t: f32) -> Vec<Mat4>;
pub fn apply_additive(nodes, skel, locals: &mut [Mat4], additive: &Clip, t: f32, weight: f32);
pub fn compose_worlds(nodes: &[Node], locals: &[Mat4], root: &Mat4) -> Vec<Mat4>;
pub fn skinning_matrices(worlds: &[Mat4], inverse_bind: &[Mat4]) -> Vec<Mat4>;
pub fn skin_positions(positions: &[[f32; 3]], lod: &SkinLod, skin: &[Mat4]) -> Vec<[f32; 3]>;
```

Data types:

* `Track { bone, bone_hash, rotation_times, rotations, translation_times, translations, scale_times, scales }`.
  Scales are always empty.
* `Clip { skeleton_id, flags, duration, frame_rate, tracks, root_motion, reference_rotation,
  reference_translation, attachments, sub_clips }`.
* `SkinInfo { nodes, inverse_bind, bind_world, lods: Vec<SkinLod { primitives, joints, weights }> }`.
* `Skeleton { bones: Vec<SkeletonBone { name, name_hash, parent, translation, rotation, length, procedural }>, sockets }`.

Everything is already in Bevy space.

## Tool

```text
target/agent_anim/release/fc3anim.exe survey                    # parse every .mab
target/agent_anim/release/fc3anim.exe skin-survey               # every .xbg
target/agent_anim/release/fc3anim.exe skeleton <path.skeleton>
target/agent_anim/release/fc3anim.exe skin <model.xbg> [lod]    # palettes, materials, rigid parts
target/agent_anim/release/fc3anim.exe clip <clip.mab> [skeleton|-] [samples]
target/agent_anim/release/fc3anim.exe find <substr> ...          # search clip names
target/agent_anim/release/fc3anim.exe pose-obj <a.xbg,b.xbg> <clip.mab> <t|NN%> out.obj \
    [--skeleton S] [--weapon W.xbg --weapon-skeleton W.skeleton] [--additive C.mab@t] [--root-motion]
```

`pose-obj` writes a skinned OBJ in Bevy space and prints the `Camera` bone matrix
when the model has one. Any OBJ viewer, or a small PIL script, can render it.

## Facial animation (`.dpax`, `.dpdx`)

Decoder: `crates/fc3_formats/src/facial.rs`. Tools: `fc3tool facial`, `fc3tool facial-survey`
and the `--face` options of `fc3anim pose-obj` / `bindpose-obj`.

Faces have no per-bone animation tracks; they are driven by **poses**.

* A character's **face rig** (`.dpdx`) lists its face bones. For each bone it stores the
  rest transform and, for every pose that moves the bone, a rotation and a translation
  at full weight.
* A **facial clip** (`.dpax`) stores one weight curve per pose.
* The engine samples the playing clips and adds them into one weight per pose. It then
  rebuilds every rig bone's local transform from its rest transform and the weighted
  poses.

Status:

* All 16,961 named `.dpax` and all 45 `.dpdx` in the install parse (`fc3tool facial-survey`,
  0 errors). Every frame count is `duration × rate + 1` and every weight is within 0..1.
  Weights change no more across block boundaries than inside blocks.
* 36 clips have a channel that no rig defines. The engine skips those, and so does
  `FaceRig::channel_map`.
* The scale constants and the sampling and posing rules were read from the engine's own
  code. The decoder uses the same constants, bit for bit.
* Visual checks (OBJ renders of a unique character):
  * with all weights at zero, his rig reproduces his bind pose to within 0.02 mm;
  * the mouth-open pose drops the jaw;
  * the neutral blink clip closes both eyes at its peak;
  * a cutscene clip gives natural expressions at several times.

### Which rig, which clips

* `CFacialPoseComponent.facialposedefinitionFacialPoseDefinition` is a key into
  `generated\databases\generic\facialposedefinition.lib` (an FCB file).
  * The key is the CRC-32 of the entry's `hidName`; the entry stores it as `hidKey`.
  * Each entry stores its `.dpdx` path as text (field hash `CC979B17`) and as a path hash
    (`filePoseDefinitionFileName`).
* `MercKitFacialPoseDefinitions` lists `FacialPoseDefinition` entries.
  * Each has `sHeadTag` (the CRC-32 of a lower-case tag such as `head02`) and
    `facialposedefinitionFacialDefinition` (a key). The list ends with an empty entry
    (all `0xFFFFFFFF`).
  * Graphic kit head parts carry these tags as `<tag name=… type="3"/>` directly under
    `<part>`. A kit with several heads therefore gets the rig that fits the head it picked,
    and the default key covers the rest.
  * This is inferred from the data. I didn't trace the engine code that does the lookup.
* Rig bones are matched to skeleton bones and XBG nodes by the CRC-32 of the name.
  * Kit head models carry the face joints in their NODE chunk and are skinned to them.
  * One bone hash in every rig matches no node in the models checked. Skip unmatched
    bones: the engine maps rig bones to its skeleton through an index table, with
    `0xFFFF` for missing ones.
* Where clips come from:
  * Voice lines: the engine formats
    `graphics\Characters\_common\facial\generated\%08x.dpax` with the line's sound id.
  * Mission scripts load clips as `CPoseAnimationResource`.
  * Cinematics have a `CFacialAnimationSequenceTrack` with `CFacialClip` elements:
    `fileFacial` (the clip), `timeStartTime`, `timeDuration`, `nAnimStartFrame`,
    `nAnimStopFrame` and `fBlendTime`.
  * Gameplay clips sit next to their body clips as `*_facial.dpax`.
  * Short library clips (blinks, emphasis, moods) live under
    `graphics\characters\_common\facial\`.

### `.dpdx` (face rig)

Little endian; offsets are from the file start.

| offset | content |
|---|---|
| `+0x00` | `u16 version = 1`, `"Dp"`, `u32` file size |
| `+0x08` | `u32` offsets of the bone table, the entry table and the pose table |
| `+0x14` | `u16` bone count, `u16` entry count, `u32` pose count, `u32 0` |

* **Bone** (48 bytes):
  * `f32 rot[4]` (x,y,z,w) and `f32 pos[3]`: the rest transform, local to the parent bone,
    in Dunia space;
  * `u32 crc32(bone name)`;
  * `u32` offset of its entries, `u16` entry count, then 10 zero bytes.
* **Entry** (12 bytes): one per pose that moves the bone, in ascending pose order.
  * Layout: `u32` packed rotation, `i16 t[3]`, `u16` pose index.
  * Translation: `t × 4/32767` metres.
  * Rotation, packed as 10-10-10-2 "smallest three":
    * bits 0–9, 10–19 and 20–29 each hold `(v − 511) / 722`;
    * the top two bits pick the component left out: 0 = `w`, 1 = `x`, 2 = `y`, 3 = `z`;
    * the left-out component is `sqrt(max(0, 1 − a² − b² − c²))`;
    * the three fields are the components that follow it cyclically in `x, y, z, w` order.
* **Pose table**: one `u32` pose name hash per pose. Clip channels name poses by this
  hash. The names are not in the game binary, so they are unknown.

### `.dpax` (facial clip)

| offset | content |
|---|---|
| `+0x00` | `u16 version = 2`, `"Ap"`, `u32` file size |
| `+0x08` | `u32` offset of the clip info, `u32` offset of the block table |
| info `+0x00` | `u32` its own offset, `u32` info size, `u32` offset of the channel table |
| info `+0x0C` | `f32` frame rate, `f32` duration in seconds |
| info `+0x14` | `u16` frame count (`duration × rate + 1`), `u16` channel count, `u8` frames per block, 3 bytes padding |
| info `+0x1C` | channel table: one `u32` pose name hash per channel |

* **Block table**: `ceil(frames / frames per block)` `u32` offsets.
  * Blocks follow each other in time without overlap and start on 4-byte boundaries.
  * The file ends with a `u32 0`.
* **Block**:
  * Header: `u32` its own offset, `u32` its size (header, mask and data, unpadded),
    `u32` offset of the mask, `u32` offset of the data, `u16` first frame, `u16` frame count.
  * Mask: two bits per channel, least significant first, `ceil(channels / 4)` bytes.
  * Then the channel data, in channel order:

| bits | kind | data |
|---|---|---|
| 0 | raw | one `u8` per frame of the block |
| 1 | zero | none; the weight is 0 for the whole block |
| 2 | constant | one `u8` for the whole block |
| 3 | sparse | `u8 runs, u8 stored, (u8 after, u8 len) × runs, u8 values[stored]` |

* A weight is `u8 / 255`.
* Sparse channels leave runs of frames out:
  * frame `after` is stored, and the `len` frames after it are not;
  * those frames are interpolated linearly between frames `after` and `after + len + 1`,
    which is always inside the block;
  * every other frame is stored, in order, so a block's first and last frames always are.

### Sampling and posing

1. Clip time to frame: `f = rate × clamp(t, 0, duration)`. The weight interpolates
   linearly between frames `⌊f⌋` and `⌊f⌋ + 1`, across block boundaries too.
2. Pose weights: for every playing clip, add `weight × blend` to the rig pose whose hash
   matches the channel. Channels the rig lacks are skipped. Several clips (for example a
   voice line over a blink loop) add up.
3. Bones: start each rig bone from its rest transform, then go through its entries in
   order. Skip entries whose weight is at most `2e-6`. For the others:
   * `translation += w × t`;
   * `rotation = rotation ⊗ slerp(identity, q, w)`, along the shorter arc. The pose
     rotation is post-multiplied, so it acts in the bone's own frame.

   Normalise the rotation at the end. The result replaces the bone's local transform.
4. The engine also scales the final translations by a per-character factor stored next
   to its list of playing clips. I didn't find where it is set. We use 1, which
   reproduces the bind pose exactly.

### API (`fc3_formats::facial`, Bevy space)

```rust
pub fn parse_clip(bytes: &[u8]) -> Result<FaceClip>;                 // .dpax, decoded to per-frame weights
pub fn parse_rig(bytes: &[u8]) -> Result<FaceRig>;                   // .dpdx
pub fn parse_definitions(fcb: &[u8]) -> Result<Vec<FaceDefinition>>; // facialposedefinition.lib
pub fn voice_clip_path(sound_id: u32) -> String;
pub fn unpack_pose_rotation(packed: u32) -> Quat;                    // Dunia space
impl FaceClip { fn frame_at(t); fn channel_weight(channel, t); fn sample(t) -> Vec<f32>;
                fn add_weights(t, blend, map, &mut weights); }
impl FaceRig { fn pose_index(hash); fn channel_map(&FaceClip) -> Vec<Option<usize>>;
               fn zero_weights(); fn clip_weights(&FaceClip, t);
               fn bone_local(bone, &weights) -> (Vec3, Quat);
               fn apply(&nodes, &weights, &mut locals); }               // after anim::pose_locals
impl FaceChoice { fn from_component(&fcb::Object); fn definition(head_tags) -> Option<u32>; }
```

### In the game

* The converter packs every rig and clip, named or a spoken line's
  (`voice_clip_path` over the speech table's ids), into `facial/faces.pak`
  (`FPAK`, a sorted table of path hash, offset, length) and writes
  `facial/definitions.txt` (definition key -> rig path hash).
* `fc3_game::faces`: characters whose entity has a `CFacialPoseComponent` get
  its default rig; their face bones are found by name hash among the rig's
  joints (unique characters: all but one). Each frame after the body pose,
  nearby faces with a clip playing are posed from it: the scene's facial track
  (`CFacialClip`, from its start frame) and the speaker's lip-sync clip for a
  spoken line add up. `FC3_NO_FACES=1` leaves faces on the body pose (A/B).
* Faces outside scenes also play the library's neutral blink loop, with
  staggered phases.
* Generic characters' kit faces mouth what they say: barks and encounter
  conversations set their speaker's line going, and any other spoken line
  played where a face stands (within a stride of its feet: a villager calling
  the player over, the poker players, the race keepers, the knife thrower)
  goes on that face, when the game has the line's lip-sync clip (sounds
  that aren't speech have none). The library loads when conversion finishes, including
  on first launch.
* Not yet: generic characters' rigs (by archetype and kit head tag),
  gameplay `*_facial` clips.

### Tools

```text
fc3tool facial-voice                                            spoken lines with a lip-sync clip
fc3tool facial <clip.dpax> [--rig <rig.dpdx>] [--at <seconds>]   blocks, channels, sampled weights, posed bones
fc3tool facial <rig.dpdx>                                       bones and poses
fc3tool facial generated\databases\generic\facialposedefinition.lib
fc3tool facial-survey [path substring]                          parse every named clip and rig
fc3anim bindpose-obj <head.xbg> out.obj --face <rig.dpdx> [--face-clip <clip.dpax>@<t>] [--face-pose <index>=<w>,..]
```
