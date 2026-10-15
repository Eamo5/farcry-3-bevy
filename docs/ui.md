# Interface (HUD and menus)

Far Cry 3's HUD and menus are Flash movies: 353 `.feu` files under `ui\`
(HUD pieces, menus, multiplayer, in-game editor). This project runs those
movies themselves rather than rebuilding the interface: the converter turns
each into a standard SWF in the cache, and the game plays them with Ruffle's
ActionScript 2 player and wgpu renderer (`crates/fc3_ui`, Ruffle's renderer in
`vendor/`; in the game with the `original-ui` feature, below). Nothing from the movies, their textures or their fonts
is stored in this repository.

Rebuilding them natively was ruled out on size: the movies hold about 9.9 MB
of compiled ActionScript 2 in some 7,000 script blocks (the singleplayer HUD
alone 29 movies and about 540 KB). Their art (vector shapes, bitmaps, text
fields, timelines) would convert, but their behaviour is that script.

## Movies (`.feu`)

A `.feu` is a SWF 8 movie (ActionScript 2) whose signature reads `UEF` where
SWF has `FWS`, uncompressed. Its bitmaps and fonts live outside the file,
named by the game's own tags (decoder: `fc3_formats::feu`):

| Tag | Body |
|-----|------|
| 246 | external image: character id (u16), authored width and height (u16), the texture's path as `UI\...\TX_name.png` (NUL-terminated); the archive has it as `.xbt` (DXT5) |
| 248 | the movie's id |
| 249 | a version |
| 251 | external font: character id (u16), the `.ffd` font file, its face name, then per-language variants (NUL-terminated strings) |

Movies share libraries through ordinary `ImportAssets` tags whose URLs are
archive paths (`UI\Common\lib.feu`, in any case, with either slash, sometimes
doubled); `feu::movie_rel` maps both those and archive paths to the cached
movie.

`feu::to_swf` makes a standard SWF of a movie: the signature put back, each
external image a `DefineBitsLossless2` (premultiplied ARGB, cropped from a
power-of-two texture or resampled to the authored size), each external font a
`DefineFont3` (below), tags 248 and 249 dropped, sprites rewritten the same
way. The converter writes all of them to `ui/<archive path>.swf` in the cache
(about 107 MB); `fc3tool feu-swf <archive .feu> <out.swf>` converts one.

Many images are packed into shared textures, atlases
(`UI\SuperTextures\<name>_0.png`: the radar's icons, the skill icons, the
weapons and gadgets, the world map's). Such an image keeps its own authored
size in tag 246, and the shapes filling with it reach their part of the
atlas through the fill's matrix: the engine stretches the whole atlas over
the image's authored size (a 32 px icon of a 512 px atlas is filled at 16x,
offset to its place). Cropping the atlas to the authored size gave every
icon the atlas's top-left corner. So an atlas is embedded once, whole, under
the first image's id (later images of it are a transparent pixel), and every
`DefineShape`..`DefineShape4` fill of one of its images is pointed at it with
its matrix scaled by the image's size over the atlas's (x through scale x
and skew 0, y through skew 1 and scale y). The shapes are re-encoded field by
field (styles, and the new styles inside their records); a shape with no such
fill is left as it was.

A clip made from a library's symbol (the rucksack's grid tiles are
`lib.feu`'s `mcGridTile`) attaches by name from the library's symbols, not
the movie that placed it, and its icons are the movie's (`TX_...`). Ruffle
loads a library afresh for each movie that imports it and gives it that
movie's exports, but only those registered before the import (the movie
waits at its `ImportAssets` until the library is in) and only at ids the
library doesn't use; the icons came later, at ids lib has too, so
`attachMovie` found nothing and every tile was blank. So in a movie that
imports, each sprite it exports is written again under an id from 0x8000
(no movie has a thousand and more), its frames only (the definitions in it
are the original's), and exported, ahead of its first import; the original
and its export follow as before, so in the movie itself the name stays the
original's (and its class).

The game also resolved a symbol name across the libraries a menu took in:
the crafting page builds its lists from lib.feu's `ScrollList`, which it
never imports (the rucksack does). In Ruffle a movie attaches only what it
defines or imports, so that page had no lists. So each `ImportAssets` is
written with every other symbol its library exports added (ids from
0xA000; `feu::exports` reads a library's names, the converter's callback
finds the library by its URL), save what the movie makes itself and the
libraries' class packages. The cache's movies are stamped
(`ui/stamp-movies-whole-imports.txt`) so a converter that predates this
makes them again.

A few movies name textures the game doesn't ship (demo, multiplayer and
leftover menus); those images are left transparent, as the game would show
nothing there either. One font file (Futura) isn't shipped; its one movie
falls back to a device font.

## Fonts (`.ffd`)

Only three font files are named, and the movies use two (Trade Gothic LT Bold
and Trade Gothic Neurotic); a Japanese set ships alongside. They are not
outlines but glyph tables over **distance-field atlases**: the page textures
`<name>_<n>.xbt` hold, in alpha, a field that crosses one half at the glyph's
edge, at about 18 page pixels per em. The engine draws text from the field
(its text styles add outlines and the like the same way).

Layout (little-endian, decoder `fc3_formats::ffd`):

| Where | What |
|-------|------|
| byte 3 | page count |
| 0x30 | face name: length (u8, NUL included), then the name |
| then | glyph count `n` (u16); `4n + 4` bytes not used here |
| | `n` characters (u16) |
| | ascent, descent (u16; they sum to 20480, the em) and a u16 |
| | `n` advances (u16, 1/20480 em, in the character list's order) |
| | `n` u16 and two bytes not used here |
| | the pages' paths (`UI\...\name_1.png`, NUL-terminated) |
| | `n` records of 27 bytes: character (u16), page (u8), the glyph's rectangle on its page (u0, v0, u1, v1: f32), its cell's x offset from the pen and y offset from the line's top, width and height (i16, eighths of a page pixel) |
| last 3 bytes | not used here |

A SWF font can't be a bitmap (glyph shapes are filled with the text colour),
so `ffd::TracedFont` traces each glyph's edge: the field is sampled four
times per page pixel (bilinear, nothing outside the glyph's rectangle),
marching squares finds the contours at one half (crossings named by the grid
edge they lie on, so contours meeting at a point stay apart; saddles decided
by the cell's centre), points within 0.06 page pixels of the line through
their neighbours are dropped, and the result goes to em units from the pen and
the baseline. The pages' scale comes from the font itself: the baseline is
where flat-bottomed glyphs (`H`, `E`, `I`, `x`, ...) end, and it lies the
ascent below the line's top. Contours keep the inside on their left, so
outlines and holes wind opposite ways, as the non-zero rule Flash fills glyphs
with needs. `TracedFont::define_font3` writes the glyphs, codes, ascent,
descent and advances (no kerning) as a `DefineFont3` (about 41 KB a font)
under the character id the movie gave the external font.

## Runtime (Ruffle)

`crates/fc3_ui` plays a movie with `ruffle_core` (pinned to Ruffle
`2372a4e1`) and draws it with the vendored `ruffle_render_wgpu`, taken back
to the wgpu 29 that Bevy uses (see `vendor/README.md`); `fc3ui shot
<movie.swf> <out.png> [frames] [w h]` renders a movie off screen.

`ruffle_core`'s build compiles Ruffle's ActionScript 3 globals with a Java
tool, even though these movies are ActionScript 2 (the player sets AVM2 up
regardless). The patched `vendor/build_playerglobal` reads that compiler's
output from a file instead, so the build needs no Java. The file is made
without Java too: `fc3tool playerglobal-abc` takes the compiled globals out
of a Ruffle release built from the same revision and puts back the
`[Ruffle(...)]` metadata that release builds strip, from the AS3 sources
(see `vendor/README.md`). Until the file is there, `fc3_ui` does not build;
nothing else depends on it.

### Talking to the game

The movies are written against Ubisoft's own ActionScript 2 framework
(`fire.*`) and talk to the engine three ways (`fc3tool swf-script <movie>
names|strings|dis <block>|expr <block>|fsc2` reads a converted movie's
script; `expr` reads it as statements, the stack folded into expressions,
jump targets labelled and a function's registers by name):

* **Calls on the engine** are Flash Lite's `FSCommand2` (opcode 0x2D: it pops
  an argument count, the command and its arguments, and pushes a result),
  which desktop players don't know (Ruffle logs it and carries on). The
  converter turns each into a call of the global function
  `__fireEngineCall` (`fc3_formats::avm1::replace_fscommand2`: the stack
  `FSCommand2` reads is what `CallFunction` reads after the function's name;
  branches, function bodies and `with`/`try` blocks spanning one are
  resized). Across the singleplayer and common movies there are about 640
  `Send(event, args...)` (a movie telling the engine something: each HUD
  piece reports it has loaded, the ammo bar sends the frame counts of its
  bullet strips), 94 `Get(key, variable)` (the engine puts a setting in the
  named variable: `IsHighDefinition`, `IsInvertActivateCancel`,
  `GetLanguage`, `ShowGamePadInterface`, `IsSplitscreen`, `IsBootTitle`,
  `GamerIndex`...) and 27 `GetPlatform(variable)`. One multiplayer movie
  (the tattoo editor) has a branch that no longer fits once its calls grow,
  and is left as it was.
* **Events from the engine** come on channels: `_global.ExtendedEvents.<name>`
  is a broadcaster the movies add listeners (`{onEvent: f}`) to, directly or
  through `fire.managers.ExtendedEventRegister`; the engine broadcasts
  `onEvent(args...)` (the ammo bar's handler reads `arguments[0]` as the
  sub-event, `SetWeaponHudVisibility`, and `arguments[2]` as "1" or "0").
* **Clips the engine drives**: the movies name clips and text fields with a
  `_fireDPSId` (`AmmoBarText`, `AmmoType0`..`7`, `WeaponAmmoHud`,
  `CrouchIconMC`...), and the engine sets their text, frame and visibility
  itself. Text fields also get `_fireRenderOptions` (a distance-field text
  style such as `MENU_TITLE_OUTLINE_20`: the engine's outlines).
* **Strings**: `fire.utils.Localizer.getText(id)` (a string id as text)
  gives the text of one of the game's strings; no movie defines the
  Localizer (the engine did), and the menus call it some 200 times, the
  camera's legend six. The shim defines it, asking the host
  (`ExternalInterface` `engineText`), which reads the string table the
  converter keeps (`story/strings.bin`, `fc3_formats::oasis`).
* **Tweens**: `new fire.utils.TweenEx(clip, seconds, {prop: to or
  [from, to], ease, delay, repeat, yoyo, onComplete, onCompleteParams})`,
  with `stop()`, is the engine's too (the movies define only
  `fire.utils.EaseType`: None 0, Regular/Strong/Back/Bounce In, Out, InOut
  1-12, Bezier 13). The menus fade and slide everything with it; the pause
  menu starts its root at alpha -20 and fades it in, so without it the
  whole menu stayed invisible. The shim defines it, stepping its tweens
  every frame by `getTimer` (Penner's quadratic, quintic, back and bounce
  curves; `repeat` -1 is for ever, `yoyo` runs back on each repeat).
* **Data providers**: `new fire.utils.DataProvider(name)` is the engine's
  as well (lib defines only `DataProviderAS`, the script-side kind): a
  named list the engine fills (`DataLootInventory`, `DataRecipes`,
  `SkillsDP`, `DataMap`, `KeyBindings`, ...). A page reads it with
  `getLength()` and `request(index, count)` (a `fire.utils.DataResult`:
  `status` 0 failed, 1 pending, 2 success, and `data`), and builds its grid
  or list when it hears of a list (`onDataReceived`), which the engine sends
  once the page says it is loaded (`PlayerInventoryLoaded`); a list sent
  earlier would refresh a grid not yet made. An item's fields bind by name
  (lib's `fire.utils.DataBinderUtils`): `iStateID` the state, `iObjectID`,
  `sImage` (an exported symbol attached as the icon, `TX_AnimalHideBoar`),
  `iNameID`, `iTypeID`, `bEquip`; any other field goes to the tile's clip
  of that name (a text field's text). The rucksack's items also carry
  `sName`, `sInfo`, `sSubCategory` (loot, animal, plant), `iPrice` and
  `bRare`. The shim keeps the lists by name (`Host::set_data`);
  `FC3UI_DATA="DataLootInventory=sName:...,iStateID:2;..."` sets them in
  `fc3ui hud` after its events.

`crates/fc3_ui/src/shim.as` is the engine's side, compiled at start with
`rascal` (the AS2 compiler Ruffle's build uses) and played at level 0:
`ExtendedEvents` (a broadcaster made on first use of each channel),
`__fireEngineCall` (hands the call to the host through `ExternalInterface`
and, for `Get`/`GetPlatform`, puts the answer where the movie said), and
callbacks the host calls: `load`/`unload` a movie at a level, `event`
(broadcast on a channel), `call` (a function a movie defines), and
`dpsList`/`dpsText`/`dpsHtml`/`dpsVisible`/`dpsFrame`/`dpsPlay`/`dpsSet`/
`dpsSetParent` (a property of the clip's parent)/`dpsGet` on the clips by DPS
id (found by walking the loaded movies). `fc3_ui::Host`
wraps it: the player (transparent window mode, no scaling), a navigator that
fetches the movies a movie names by archive path (`UI\Common\lib.feu`) from
the cache, the answers to `Get`, and the calls the movies made.

The main HUD (`UI\Singleplayer\Hud\MainHud\MainHud.feu`) places its
container (`MainHud.HudContainer`), which asks for the language and waits for
`SetGameMode` ("SPHud" builds the singleplayer HUD: it loads some twenty
pieces, ammo, minimap, health, objective, reticule, subtitles, interaction,
loot, XP, stealth and hit indicators..., each reporting `...Loaded`), then
`EnableHud` and `SetVisible` show it. `fc3ui hud <movie> <out.png> [frames]
[event <channel> <args> [+ ...]]` plays that off screen and lists the calls
and DPS ids:

    fc3ui hud UI\singleplayer\hud\mainhud\mainhud.feu out.png 30 \
        event SetGameMode SPHud + EnableHud 1 + SetVisible 1

The minimap draws a placeholder disc where the engine paints its radar.

The tool runs frames in real time (the tweens go by `getTimer`, the wall
clock). `FC3UI_GET="_level1;_level1.mcBack"` prints what is at paths (a
clip's frame, visibility, alpha, place and size, else the value) and
`FC3UI_LIST="_level1"` an object's members (the shim's `getPath` and
`listPath`).

The pause menu (`UI\Singleplayer\Menu\Pause\M_PauseMenu.feu`) is a page
manager: it asks the usual `Get`s, then `Send RequestPageToLoad`, and the
engine answers on `SetPageToLoad` with a quick-launch id (`QUICKLAUNCH_*`:
shop 0, weapon chest 1, world map 2, skills 3, ... inventory 13, debug
14; any other, -1, is the pause page). The page (`M_Pause.feu`) reports
`SinglePlayerMenuLoaded`; the engine sends `SetUnlockedMenus` (crafting,
skills, save, checkpoint, restart, abandon as "1"/"0", then debug and the
restart and abandon text ids) to build its list, `SetObjective` (title,
objective, money as "have/most", rucksack as "have/most") and `NewItems`
(skills, crafting, handbook). The page sends back `RequestPageChange`
(quick-launch id, selection), `MissionOptionSelected`, `Resume` and
`QuitMenu`. Off screen with made-up values:

    fc3ui hud UI\Singleplayer\Menu\Pause\M_PauseMenu.feu out.png 30 \
        event SetPageToLoad -1 + SetUnlockedMenus 1 1 1 1 0 0 0 \
        + SetObjective "Title" "Objective" "120/500" "7/20"

## In the game (`original-ui`)

`crates/fc3_game/src/original_ui.rs`, built with the `original-ui` feature
(`cargo build -p fc3_game --features original-ui`; `FC3_ORIGINAL_UI=0` turns
it off at run time), plays the interface on Bevy's own GPU: Bevy makes an
sRGB image the window's size; once the render world has made its texture,
the texture comes back to the main world (it holds the player, which isn't
`Send`) and Ruffle draws into it through a plain RGBA view
(`fc3_ui::SharedTarget`, Ruffle's descriptors built from Bevy's instance,
adapter, device and queue). A full-window UI node shows it with
`original_ui.wgsl`, which divides out Ruffle's premultiplied alpha and
decodes the sRGB itself. The stage is the movies' own 1280x720, shown whole
(`STAGE`, `ShowAll`) and see-through (`transparent` window mode).

The main HUD starts as the engine starts it (`Get` answers for
high-definition, language and platform; `SetGameMode SPHud`, `EnableHud`,
`SetVisible`), and the game then drives its clips as the engine did, sending
only what changed (and everything again until the movies have loaded):

* **Health**: `HealthBarSlot0`..`9` sit round the radar, one per health
  segment (`HEALTH_SEGMENT`), each filling over its 30 frames (1 empty, 30
  full); `HealthBarBG` and `HealthBarOutline` go to the frame of the number
  of segments; `PotionCount` is the medkits carried. Armour worn shows
  as the arc round them (`ArmorBar` over `ArmorBarBG`, frames 1 empty to
  61 full: what is left of the most worn since it was put on); without
  armour both hide.
* **Ammo**: a strip per kind of round (`AmmoType<n>` over `AmmoBGType<n>`,
  with the frame counts the movie reports in `AmmoLoaded`: 20, 16, 80, 20,
  50, 7, 4), the weapon class picking it (pistol 1, SMG and rifle 2, sniper
  3, flamethrower 4 as a fuel gauge, shotgun 5, grenade launcher 6, machine
  gun the four 50-round rows of 7). A strip's frame 1 shows every slot and
  each later frame one fewer, so the background goes to `total - magazine +
  1` and the rounds to `total - rounds + 1`. `AmmoBarText` is the reserve;
  `ThrowableIcon` shows the throwable last thrown or laid (frames 2
  molotov, 3 grenade, 4 C4, 5 mine; a grenade until one is) and
  `ThrowableCounter` how many of it are carried; `WeaponHudEvent` /
  `SetWeaponHudVisibility` hides the panel without a gun.
* **Radar**: the game's own minimap moves into the radar disc
  (`MaskMC`'s bounds on the stage, mapped to the window and to the UI's
  pixels: `map::MinimapPlacement`). The engine drew its map inside the
  movie, in the disc over the radar's dark backing; ours is under the whole
  canvas, so the disc (`MaskMC`, the placeholder) erases the backing instead
  (`blendMode` "erase", its container made a "layer" so the erasing stays
  in the radar), and the map shows through. The markers are placed as the
  engine placed them, in stage pixels from the disc's centre (the disc's
  radius, 78.5, being the map's reach, `map::MinimapReach`), turned with the
  player as the map is, and shown by their frame labels: `Object_<n>` (21)
  the enemies the game's own minimap shows (tagged, or on to the player,
  `enemy`) and vehicles free to take (`vehicle`, `boat`, `glider`);
  `LocMarker_<n>` (16) places: outposts (`OutpostLocked` while held,
  `FastTravel` once taken), their working alarm boxes (`Alarm`, `AlarmOn`
  while ringing), radio towers (`TowerLocked` until climbed, then `Tower`)
  and collectibles within the game's radius (`Relic`, `Letter`, `Sdcard`);
  `ObjMarker_<n>` (11) the objectives (`Inside` the disc, `Outside` held on
  its rim, `Up` or `Down` when well above or below). `NorthMarkerMC` turns
  about the centre to the north (`_rotation`). The labels the movie has and
  the game doesn't use yet: `companion`; mission givers and quests, shops,
  the games (poker, knife throwing, shooting, beer pong, racing),
  safehouses, photo spots, ammo, encounters, plants (`plant_<colour>`).
  The game's own minimap icons stand down meanwhile.

* **Reticule**: `reticuleLeft`/`Right`/`Up`/`Down` sit out from the centre
  as the game's crosshair opens (`hud::crosshair_gap`, its UI pixels at 1080
  lines turned to the stage's 720, from a rest of 18), hidden when it would
  be; `reticuleHitEnemy` is the hit marker.
* **Objective**: `ObjContainer` goes to frame 8 (the box open; 1 is shut),
  `ObjText` takes the objective and `MissionName` the mission's title, and a
  new objective plays `ObjTextBox` from frame 2 (it comes in with its sound
  and stops at 90; from 91 it ticks off and stops at 154 sending
  `ObjectiveIsValidated`, from 155 it fades and sends `ObjectiveHasFaded`).
* **Mission cards**: the objective movie's `SetMissionStart(title)` as a
  mission starts, `SetMissionTitle(text, status, succeeded)` as it is
  complete or failed (`story::MissionCard`).
* **Mission timer**: `ObjectiveTimer` shows the first of the story's
  timers in `ObjectiveTimerText` (as the game's own HUD writes it), hidden
  without one.
* **Subtitles**: `ShowSubTitle(text)` / `HideSubTitle()`.
* **Interaction prompt**: `InteractionMC` shown with `InteractionText`
  (the prompt the HUD showed this frame, `HudState::shown_prompt`).
* **XP**: `XPKicker` plays a tier by the amount (from frame 2, 42 or 95; it
  holds at 37, 87 or 155 and runs on to its `off` label from the next
  frame), `XPKickerText` the amount (the movie adds the "+") and
  `XPBonusKickerText` what it was for.
* **Loot and cash**: each thing looted plays a line (`LootContainer<n>`
  from frame 2 with `LootItemText<n>`, the two lines in turn; it fades by
  itself and sends `LootAnimationFading`); a change of money shows the cash
  counter (`CashContainer` from 2, holding at 9, gone from 10 three seconds
  after the last change) with `CashText` and a kicker (`CashUpdateAnim<n>`
  from 2 for a gain, 42 for a loss, `CashUpdateText<n>` the amount).
* **Stealth indicators**: up to three enemies noticing the player (the
  most suspicious first; one that has spotted the player counts most), each an
  arrow round the screen's centre (`StealthIndicator_<n>`'s `_rotation`,
  the enemy's bearing from where the player faces) filling as its
  suspicion grows (`StealthIndicatorAnim_<n>`, frames 1 to 100) and white
  while suspicious, red once spotted (`StealthIndicatorState_<n>` frame 1
  or 8).
* **Hit indicators**: each hit on the player plays the next of six arrows
  round the centre (`HitIndicator_<n>` from frame 2; it fades over its 23
  frames and turns itself off), turned toward where the hit came from
  (`PlayerStats::hurt_dir`).
* **Grenade indicators**: the three nearest live grenades within
  `hud::GRENADE_WARN` each turn a `GrenadeIndicator_<n>` toward them (its
  `_rotation`); the others hide.
* **Tutorial box**: `UI\Singleplayer\Hud\Tutorial\H_TutorialBox.feu`
  plays at level 3; a mission's hint (`HudText`, the newest when there
  are more) goes in through the movie's own functions, `setTutorial(title,
  text, prompt)` and `showTutorial()` (it fades in), and `hideTutorial()`
  takes it away. The shim's `call` turns null arguments into empty strings
  (the host can only send a blank string as null). Checklists stay in the
  game's own text for now.
* **Camera**: while the camera is up its viewfinder
  (`UI\Singleplayer\Hud\Camera\H_Camera.feu`) plays at level 2 over the
  HUD (and the reticule hides): `CameraZoomLevel` shows the magnification
  (frames 1 to 42 for 1.0x to 7.5x; the wheel zooms the camera between
  them, `stealth::CAMERA_ZOOM`), `CameraCursor` turns from 1 to 2 when
  something to tag or take in is under it, and `ShutterAnim` goes to frame
  2 (the view blacked out) a moment as the camera takes in a story
  character. Its button bar (`InitCameraButtons`) isn't shown on PC.
* **Shown at all**: `SetVisible` follows the game's own HUD (not in menus,
  scenes, missions' no-HUD stretches or death).

While it runs, the game's own health, ammo, compass, crosshair, hit marker,
prompt, objective, subtitles, banners and XP popups are hidden or left empty
(`hud::ReplacedByOriginal`, `hud::OriginalHud`).

**Pause menu** (`original_ui::drive_pause`): pausing loads
`M_PauseMenu.feu` at level 10 over the HUD (which `SetVisible 0` hides,
and our own pause menu stands down: `menu::OriginalPause`), and the game
answers it as the engine did (above): `SetPageToLoad -1` for the pause
page; on `SinglePlayerMenuLoaded`, `SetUnlockedMenus` (crafting, skills
and save on; the last checkpoint while a mission runs; no restart, abandon
or debug), `SetObjective` (the mission's title and objective, the money and
its most, the rucksack's loot and its size) and `NewItems`. What is chosen
comes back as `Resume`/`QuitMenu` (play on), `Quit`/`QuitToDesktop` (exit),
`MissionOptionSelected` 0 (back to the last checkpoint), 6 and 7 and
`RequestPageChange` 3 and 8 (our journal, until the skills and crafting
pages are the original's). The keyboard and mouse go to Ruffle while it is
up (`Host::input`: keys as Flash key codes, the mouse in the window's
pixels), and it draws its own cursor. The interface runs on the real clock,
so its menus animate with the game paused. `--pause` (tests) pauses three
seconds into play, `FC3_PAUSE_PAGE=13` goes on to a page from there
(`10,5` with launch arguments: the handbook's guide tab), and
`FC3_PAUSE_KEYS=Enter,Down,Enter` then presses keys on it, 0.6 s apart
(Enter, Escape, Space and the arrows). The game opens a page with
`SetPageToLoad <page> -1 -1 -1`: a page takes the further arguments as
where to open it (the crafting page's tab, a group, an entry), and without
them the crafting page read a blank as its tab.

**Loot rucksack** (`M_PlayerInventory.feu`, quick launch 13): on
`PlayerInventoryLoaded` the game sends `DataLootInventory`, a tile for each
thing carried (skins, plants, loot), each from its shop item
(`progression::LootCatalog`): `sName`, `sInfo` (its description),
`sSubCategory` (animal, plant, loot), `iPrice` (what it sells for, with the
perks), `iStateID` 1, `bRare`, `iObjectID` (the item id), and `sImage`, the
item's own icon when the movie exports it (`TX_HumanAccessories`), else
the hide of its animal (`TX_AnimalHideBoar`, `TX_AnimalHideRare...`), a
part, a leaf or a stranger's things; then `SetMoney` and `SortText` 0.
`RemoveLoot <id>` drops one of it, and the list goes again. Back from the
page is `RequestPageChange -1`, the pause page. Each page says
`SinglePlayerMenuLoaded` too, so `FC3_PAUSE_PAGE` is sent once (sent again
it reloads the page for ever). The main pause page makes a widget
(`A_Widget`, `attachMovie("mcWidget")`) it doesn't import, so that one
attaches nothing.

**Skills** (`M_Skills.feu`, quick launch 3): on `PlayerSkillsLoaded` the
game sends `InitChapter` (each of the three trees' name, description and a
blank), `InitCollectibles` (the tattoo's collectibles: relics, then 1, 0,
0), `SetSkillPoint`, the `SkillsDP` list and `SetLevel` (level, XP, the
next level's XP; it builds the tree tabs, and the first tree's icon map,
`M_SkillIcons1..3.feu`, loads and draws). A skill's item: `iSkillTreeId`
(0-2), `iSkillIndex` (its place in its tree: the first is the root, and
the map draws the icon by it), `iAbilityState` (3 learned, 0 shut by the
story, else 2: the map greys one whose parent, `sDependancy`, its index,
isn't learned, or whose statistic isn't met, `sfulfilledPreReq` 0),
`iMatrixRowIndex`/`iMatrixColumnIndex`, `iMaxSkillPoint` (its cost),
`sSkillnName` (so spelled), `sSkillDescription`, `sPreReqString`, the
tutorial fields (none yet), and `itattooPartName`, the CRC-32 of its
tattoo part's name as a decimal string (the page maps 12 of them to the
tattoo's segments). `BuyAbility <tree> <index>` learns it if it can be
learned (`progression::skill_state`) and is answered
`DisplaySkillTutorial` 1 or 0, then the points and the list again.

**Crafting** (`M_Crafting.feu`, quick launch 8) drills down, the page
asking each level as the player moves: on `CraftingPageLoaded` the game
sends `BagSizes` (syringes, crafted arrows, arrows held) and
`DataRecipesTypes` (the tabs by the table's order: syringes, equipment,
ammo, arrows, which the page's `C_CAT_*` take as places);
`CraftingRecipeTypeChanged <tab>` gets `DataRecipeSubTypes` (the groups
that tab's recipes fall in: `iID`, `sName`, `bCraftable`, `iQuantity`
carried, `iFinishedCrafting` when all its gear is made, `iSortID`);
`CraftingRecipeSubTypeChanged <tab> <group>` gets `DataRecipes` (by
`iObjectID`, here the recipe's place in the catalog: `sName`,
`sDescription`, `iUnlocked`, `bCraftable`, `iFinishedCrafting`,
`iQuantity`, `sDurationValue`...; the page locks a tier after one not
made); `CraftingRecipeSelected <id>` gets `DataRequirements` (each
ingredient's `sName`, `iNeeded`, `iPossessed`, `sImage`). The page sends
`CraftingRecipeCrafted` with no id, so the game makes the recipe last
selected (`CraftCatalog::craft`) and sends the groups, recipes and
ingredients again; `CraftingRecipeUsed` injects the selected syringe
(`syringes::inject`) and answers `CraftingUseResult` 1 or 0. Assigning
syringes to the quick slots (`CraftingRecipeAssigned`, `AssignedSlots`)
and crafting arrows aren't there yet, and the 3D
item views (`3D_Crafting_MC` and the like, which the engine draws) are
hidden: their stand-ins are plain white and blue sheets. Blank data fields
go to the movie as null, which the shim makes blank again (a blank string
from the host reads "null" in SWF 8).

**Handbook** (`M_Handbook.feu`, quick launch 10) asks for one list at a
time, `OnEncyclopediaRequest <id> <list>`, and the game fills that
provider from the handbook (`progression::HandbookData`): `TabCategory`
(its categories by their order: statistics 2, the survival guide 5,
collectibles 1, tutorials 4, the manual 6, by `iObjectID`), `SubCategory
<tab>` (a category's groups), `WikiCategory <group>` (a group's pages:
`sName`, `sDescription`, `bUnlocked` as `handbook_open` has it). The
statistics (`StatsCategory`) go empty for now, and the manual (`Emanual`,
its own movie) isn't answered.

**Options** (`M_Options_PC.feu`, quick launch 12): on `OnOptionLoaded` the
game sends `OptionSections`, at the places the PC page's code
(`OptionsPage_PC`) takes them by: video 0, calibration 1, video quality 2,
controls 3, gameplay 4, audio 5 (the shared page's own order, gameplay
first, put video's third row where the PC page draws its controller
widget). `OnRequestOptions <section>` gets `OptionValues`, a row each:
`sName` and `sDescription` (the game's strings: `Host::text`; `fc3tool
oasis find <text>` finds a string's id), `iWidgetType` (0 a choice, 1 a
slider 0-100, 7 a row the page opens itself), `sLocCurrentValues` (a list
of `sLocCurrentValue` labels: ON/OFF, LOW..ULTRA, the difficulties...; the
row's binder wraps it in a list of its own), `iNbValues`,
`iCurrentValue`, `bEnable`. Some places are the page's: brightness first
in calibration (its own popup; we have no brightness), the control type
third and customizing the controls fourth among controls. `SetOptions
<values> <section>` brings the section's values back, comma-separated in
row order, into `settings::Settings` (sliders over the native menu's
ranges: sensitivity 0.1-5, field of view 55-110, volumes 0-1), which saves
as it changes. Display resolution, the calibration popup's gamma and
contrast, the key bindings page and the vegetation, terrain and water
qualities aren't ours to set yet.

While the pause menu is up the HUD's level is hidden (`Host::level_visible`;
the shim's `levelVisible`), as the game's was: subtitles showed through.

Ruffle gives a SWF 8 movie a blank string from the host as the text
"null" (as Flash did), so the host sends blanks as null and the shim takes
null as empty.

`fc3ui hud` helps work out what a clip's frames mean: `FC3UI_INFO=1` lists
every DPS clip's kind, frames, visibility and bounds, and `FC3UI_SET="id:
visible=1,frame=12,text=99;..."` sets clips before the frame is saved.
