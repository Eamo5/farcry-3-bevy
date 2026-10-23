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
(about 107 MB); `fc3tool feu-swf <archive .feu> <out.swf>` converts one. The
archives' name lists miss some movies others load (the frontend's
`mpIgeRatingPopup.feu`, without which its pages never finish loading), so the
converter also follows the `.feu` paths in each movie's bytes
(`feu::movie_paths`) to whatever the archives hold.

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

`crates/fc3_ui` plays a movie with `ruffle_core` (Ruffle `2372a4e1`,
vendored in `vendor/ruffle/core`) and draws it with the vendored
`ruffle_render_wgpu`, taken back to the wgpu 29 that Bevy uses (see
`vendor/README.md`); `fc3ui shot <movie.swf> <out.png> [frames] [w h]`
renders a movie off screen. `fc3ui hud` loads a movie over the engine shim;
`FC3UI_PRELOAD` loads others first (`MainHud.feu@1`, as the game does),
`FC3UI_LEVEL` puts it at its own level and `FC3UI_CALL` calls its functions
(`_level3.setTutorial|Title|Text|false;_level3.showTutorial`).

**Icons in text.** The game's strings put icons in their text as
`<img src="MC_TX_...">`: the bullets of the tutorials' lists, the map's icons
in the handbook and the tutorials (an outpost, fast travel, a safe house),
the found letter's or relic's in the loot messages. Scaleform draws such an
image inline: in the line like a word, the movie's symbol exported under
that name at its own size. Ruffle reads the tag but lays out nothing for it
(Flash floats an image at the margin instead, from a URL), so its core is
patched to (`fc3 patch` in `html/layout.rs`, `html/layout/context.rs`,
`display_object/edit_text.rs`, `graphic.rs` and `movie_clip.rs`): the image
takes its symbol's width (and its `hspace` either side) in the line, wraps
like a word, is centred on the line's height, which grows to hold it, and
is drawn as the shapes the symbol's first frame places (the icons are each a
sprite holding one bitmap-filled shape). A line begun by an image takes the
next line's size from the text going on, not the image's placeholder (a
2 px space).

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
* **Engine tweens**: the frontend tweens with `new fire.utils.Tween(clip,
  property, ease name, from, to, seconds, delay, -, loop, event)`, also the
  engine's; the shim makes it a `TweenEx` (an alpha given as a fraction, both
  ends at most 1, goes as a percentage; a loop goes back and forth; at its
  end the named event, `onBulletinShowEnd`, is heard on its channel).
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
  `fc3ui hud` after its events. The binder goes through an item's fields
  in reverse of their making, and a button's state binds after its other
  fields, as the engine's lists had it (the shop's button reads its price
  as its state is set, to show it `$375`), so the shim makes `iStateID`
  first and the others after in their order.
* **Answers at once**: the engine answered a page's request for a list
  (`onRequestedMenu`) before the call returned, and the shop page counts on
  it: as it hears of a weapon's customising lists it reads which weapon
  from where it stood before the request (its breadcrumb and stats name the
  weapon). Ours come a frame later, so the game makes ready the lists a
  page may ask for next (`Host::prepare_data`, keyed `<list>:<id>`) and the
  shim hands one over as it's asked; the host still hears of the request,
  its last argument `prepared`.

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

The canvas follows the window (`follow_window`): when its physical size
changes (a new resolution or window mode, a dragged edge, or the display's
scale known only after start-up), a new image is made at that size. Once
the render world has its texture, it goes into the slot Ruffle's target
reads, the stage is resized to it, and the node's material is pointed at
it. As the stage is shown whole, on screens narrower than 16:9 the HUD sits
in the same centred band the letterbox shows.

The main HUD starts as the engine starts it (`Get` answers for
high-definition, language and platform; `SetGameMode SPHud`, `EnableHud`,
`SetVisible`), and the game then drives its clips as the engine did, sending
only what changed (and everything again until the movies have loaded):

* **Health**: `HealthBarSlot0`..`9` sit round the radar, one per health
  segment (`HEALTH_SEGMENT`), each filling over its 30 frames (1 empty, 30
  full); `HealthBarBG` and `HealthBarOutline` go to the frame of the number
  of segments; `PotionCount` is the medkits carried. Below 50 health
  (`CFCXPawnHudComponent`'s `fHealthBarBlinkingThreshold`) the slots
  (`HealthOverall`) blink, their alpha pulsing down to a quarter every
  0.8 s. `FC3_PLAYER_HURT=<seconds>,<health>` holds the player at that
  health (tests). Armour worn shows
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
  `enemy`), the story's companions (following, leading or waiting for the
  player, `companion`) and vehicles free to take (`vehicle`, `boat`,
  `glider`);
  `LocMarker_<n>` (16) places: outposts (`OutpostLocked` while held,
  `FastTravel` once taken, unless their own safehouse stands for them), the
  story's safehouses (`FastTravel` open, `SafehouseLocked` not yet), their
  working alarm boxes (`Alarm`, `AlarmOn`
  while ringing), radio towers (`TowerLocked` until climbed, then `Tower`)
  collectibles within the game's radius (`Relic`, `Letter`, `Sdcard`) and
  plants within it by their colour (`plant_red`... as the plant's pickups
  name it, `CPickupCollectible`'s `sMinimapIconName`; a plant's two pickups
  one marker),
  and the story's markers the world map shows (`original_ui::radar_label`:
  `Shop`, `Poker`, `KnifeThrowing`, `ShootingChallenge`,
  `RacingChallenge`, `ChallengeBoard`, `MissionGiver`, `Quest`,
  `SideQuest`, `HuntingQuest`, `SpeedQuest`, `WantedQuest`,
  `ULCLostExpeditions`), and the encounters placed about the world while
  their people live, within the game's radius (`Encounters`; the player's
  `MiniMap` `Encounter` `fEncounterDisplayRadius`, 40 m); `ObjMarker_<n>` (11) the objectives (`Inside` the
  disc, `Outside` held on its rim, `Up` or `Down` when well above or
  below). `NorthMarkerMC` turns about the centre to the north
  (`_rotation`). The labels the movie has and the game doesn't use yet:
  `BeerPong`, photo spots (`PhotoOps`), ammo. `fc3tool swf-script <movie> labels [text]`
  lists a movie's sprites' frame labels.
  The game's own minimap icons stand down meanwhile.

* **Reticule**: `reticuleLeft`/`Right`/`Up`/`Down` sit out from the centre
  as the game's crosshair opens (`hud::crosshair_gap`, its UI pixels at 1080
  lines turned to the stage's 720, from a rest of 18), hidden when it would
  be; `reticuleHitEnemy` is the hit marker.
* **Objective** (`original_ui::drive_objective`): `ObjContainer` fades in
  from frame 2 (it stops open at 9) and out from 10, `ObjText` takes the
  objective and `MissionName` the mission's title, and `ObjTextBox` plays
  the text in from frame 2 (it comes in with its sound and stops at 90;
  from 91 it ticks off and stops at 154 sending `ObjectiveIsValidated`,
  from 155 it fades and sends `ObjectiveHasFaded`). As in the game, it
  isn't always up: a new objective shows eight seconds, then fades, and
  the one standing comes back every 90 (the player's
  `CFCXPawnHudComponent`, `PopupReminder`'s `fObjectiveReminderTimer`);
  neither clock runs under the loading screen (`LoadingUp`). The pause menu
  shows it too. The same component sets the radar's reach (40 m on foot,
  100 in a vehicle or glider), encounters on it within 40 m, collectibles
  within 30, and the skill-point reminder (600 s).
* **Mission cards**: the objective movie's `SetMissionStart(title)` as a
  mission starts, `SetMissionTitle(text, status, succeeded)` as it is
  complete or failed (`story::MissionCard`). The status is the game's
  string for a failure (155587); the table has none for a mission
  complete, so it is the handbook's "Complete" (158104) in capitals. A
  mission ending without a title of its own (the scripts' text, else the
  one it started with) shows that alone: the movie fades a card with no
  text. `FC3_CARD_SHOT=<prefix>` (tests) captures each card 1.5 s in, to
  `<prefix>_<n>.png`.
  (`fc3tool oasis grep <text>` lists the strings containing a text.) A
  liberated outpost and a radio tower take the same card
  (`MissionCard::Event`), all the game's strings: the outpost's by how it
  fell (154986 undetected, 154987 no alarms, 111340 else, each with its XP
  line) over "%d of 34" (156253), the tower's 156250 over "%d of 18"
  (156252), each with its icon in the text (see **Icons in text**).
* **Saving**: the HUD and the pause menu each load `SaveGame\SaveGame.feu`,
  a spinner; `ShowSaving 1 <text>` brings it in as a save is written
  (`save::GameSaved`: the game's "Checkpoint reached", 159959, at a story
  checkpoint, which now saves the game with it; else its "Saving...",
  161640, for the minute's autosave and the pause menu's Save) and
  `ShowSaving 0` takes it away two seconds on.
* **Objective indicators**: `UI\Common\Hud\ObjectiveIndicator.feu` (the
  HUD loads it) has ten `ObjectiveIndicator_<n>`: each objective in the
  world (`story::ObjectiveMarkers`, a race's next gate) goes where it is on
  the stage ("ObjectiveMarker", frame 2), or, off screen, on a frame 70
  pixels in from the edges toward it, turned that way ("ObjectiveArrow",
  frame 1, its art pointing up); `PlayerMarkerIndicator` is the waypoint
  ("Inside" 1, "Outside" 2). The movie shows no distances; our diamonds
  and distances stand down.
* **Weapon wheel**: `H_Inventory.feu`'s `Wheel` while ours is open (in from
  frame 2, out from 12): its four places (top, right, bottom, left:
  `weapon_wheel::wheel_slots`, the guns first, the machete in a place
  left) each `ItemIcon<n>` at the weapon's label in
  `WeaponAndGadgetLib.feu`'s icons (the name in its shop archetype,
  `weapons:FC3/<name>`; a weapon without one leaves its place empty), the
  throwables carried in `WheelGadgetTextSlot<n>` (grenades, C4, mines,
  molotovs), by each place its number key (`KeyTextField<n>`: 1 to 4
  take the weapon in that place, `weapon_wheel::SLOT_KEYS`, as the game's
  keys did), `ItemName` the weapon under the cursor (else in hand) by
  its shop name, and two of the inner ring's arrows, which turn about
  the wheel's centre and point right at 0: `WheelCurWeaponArrowMC_0` to
  the place under the cursor, `_2` to the weapon in hand. Ours stands
  down; it still picks, and slows the world.
* **Mission timer**: `ObjectiveTimer` shows the first of the story's
  timers in `ObjectiveTimerText` (as the game's own HUD writes it), hidden
  without one: a mission's objective timer or gauge
  (`ShowObjectiveTimer`, `ShowObjectiveGageTimer`) ahead of its named
  ones.
* **A mission's loading screen**: the dream missions (BAM_020 to BAM_060)
  put the white one up (`SetFlashLoadingScreen`,
  `Loading\GameModes\Singleplayer_white.feu`: its words and tips on
  white) in place of the usual for every load while they run, and take it
  down again as they end; `GetFlashOverrideLoadingScreen` reads it back.
* **Subtitles**: `ShowSubTitle(text)` / `HideSubTitle()`.
* **Using** on foot is a hold, as the game's gameplay map has it (`use` on
  `kb:e` held, after the `ClickGenerator` `HoldTime`, 0.37 s, which the
  converter keeps in `story/actionmaps.txt`): searching bodies, skinning,
  taking things, a zip line, a radio tower, a mounted gun, the shop, a
  trial's stones, a reward, the poker and knife tables. A tap does nothing;
  getting out of a vehicle and off a gun stay presses, as do the minigames'
  own keys. Vehicles are got into with a tap (holding repairs a damaged one).
  An older cache without the hold time keeps presses.
* **Interaction prompt**: `InteractionMC` shown with `InteractionText`
  (the prompt the HUD showed this frame, `HudState::shown_prompt`). Where
  the game has the prompt and it is pressed as ours is, it is the game's
  (`hud::GameText`, `hud::prompt`, its `[Action]`s the keys bound): a zip
  line (56342, `[ZipAttach]` the interact key, `[ZipDetach]` jump), a
  plant (104574, "HOLD [Loot] to gather plant.", `[Loot]` the interact key),
  a body (59889), an animal or a shark to skin (104575, 104577), a relic, a
  letter, a memory card (104638 to 104640), loot (108976), a mission's item
  (118674), anything else picked up (119600), a glider (112303), a full rucksack (98546), an alarm box
  (71685: held a second, as the game had it, `hud::ALARM_HOLD`; the story's
  autopilot taps; the last one's notice the game's 166404), patching up
  without a medkit (56341, "Hold [Heal] to heal.": held half a second; a
  medkit is still a press), the takedowns (59890; from a ledge below 118680;
  a double 152236: the grenade, pistol and chain ones stay ours) and, falling,
  the wingsuit (80178) and the parachute (148015), whose `[Wingsuit]` and
  `[Parachute]` are the jump key.
* **Reload prompt**: `ReloadMC` with the game's "[Reload] Reload" (56331,
  by `SetReloadText`) while the weapon in hand's magazine is empty with
  more to load and no reload under way (rare: the game reloads by itself).
  `FC3_EMPTY_MAG` (tests) empties it once the player is in the world.
* **XP**: `XPKicker` plays a tier by the amount (from frame 2, 42 or 95; it
  holds at 37, 87 or 155 and runs on to its `off` label from the next
  frame), `XPKickerText` the amount (the movie adds the "+") and
  `XPBonusKickerText` the game's line for a takedown (by its `Takedown/...`
  reward: "DEATH FROM ABOVE", "CHAINED TAKEDOWN"...) or a headshot
  (`progression::kicker_line`, 162743 to 162756); other gains have none.
  A level gained plays `XPLevelUp` (in from frame 2, out from 10 four
  seconds on) with `XPLevelUpText` the game's "Skill Point Available"
  (149414, or 149413 with more than one); our level-up notices stay ours.
  `FC3_XP=20000,10` (tests) gives that much XP once, 10 s after the player
  is in the world.
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
  takes it away. A HUD popup (`DisplayCustomUIMsg <string id> <show>
  <seconds>`: the mission area's warning as the player leaves it, from
  `Lib_Common.ExitZoneWarningListener`) goes in the same box, ahead of the
  hints, until it is hidden or its seconds run out. The shim's `call` turns null arguments into empty strings
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
(`hud::ReplacedByOriginal`, `hud::OriginalHud`). So are our notices of
what it shows its own way (`HudState::notify_native`): what a body or a
chest gave, skins, rescues' and missions' cash, a new objective, an
outpost or tower taken; and our status line (an outpost's count, a
vehicle's speed, the syringe and air bars). A loot
line names its shop item (the rucksack's name for it), and a full rucksack
plays the game's "Loot Rucksack Full" (172057) as a line
(`HudState::loot_notes`). The original has no death screen in single
player: our "YOU DIED" gives way to the view fading to black over 2.5 s of
the respawn's wait, and the loading screen follows. Nor has it a panel for
phone calls (they are its voices and subtitles): ours stands down. `FC3_PLAYER_DIE=14`
(tests) kills the player once, 14 s into play.

**Pause menu** (`original_ui::drive_pause`): pausing loads
`M_PauseMenu.feu` at level 10 over the HUD (which `SetVisible 0` hides,
and our own pause menu stands down: `menu::OriginalPause`), and the game
answers it as the engine did (above): `SetPageToLoad -1` for the pause
page; on `SinglePlayerMenuLoaded`, `SetUnlockedMenus` (crafting and skills
on; save off while on a mission, whose checkpoints save, `save::on_mission`;
the last checkpoint while a mission runs; abandon while the
mission the player took on lets him, with its text, 119083 "Abandon
mission"; no restart or debug), `SetObjective` (the mission's title and objective, the money and
its most, the rucksack's loot and its size) and `NewItems`. What is chosen
comes back as `Resume`/`QuitMenu` (play on), `Quit` (to the main menu),
`QuitToDesktop` (exit), `MissionOptionSelected` 0 (back to the last checkpoint), 2
(abandon: the game's question, 119085 "Abandon the mission? All unsaved
progress will be lost.", asked over play, `story::ask_abandon`; see
`story.md`), 4 (save:
`save::SaveRequest`), 6 and 7 and
`RequestPageChange` 3 and 8 (our journal, until the skills and crafting
pages are the original's). The keyboard and mouse go to Ruffle while it is
up (`Host::input`: keys as Flash key codes, the mouse in the window's
pixels), and it draws its own cursor. The interface runs on the real clock,
so its menus animate with the game paused. `--pause` (tests) pauses three
seconds into play, `FC3_PAUSE_PAGE=13` goes on to a page from there
(`10,5` with launch arguments: the handbook's guide tab), and
`FC3_PAUSE_KEYS=Enter,Down,Enter` then presses keys on it (or on the
pause list without a page), 0.6 s apart (Enter, Escape, Space, Tab and the
arrows; another name only waits). Nothing unloads a session (the world, the
story's machine), so `Quit` saves, starts the game again at its menu
(`save::BackToMenu`; `FC3_BOOT_MENU` skips the logo and the title, and
`--continue` is left off) and ends this one; an automated run only says so. The game opens a page with
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

**Shop** (`shop.feu`, quick launch 0; `original_ui::shop`): at a shop
(`GameState::Shop`) the pause menu opens on it, in place of ours. On its
own it asks `onRequestedMenu 0 DataCategory` and says `ShopManagerLoaded`
(the game hides the stand-ins of the 3D views the engine draws in, and
draws the weapon there itself: see **3D item views**; and sends
`SetMoney <money> <most>`); then it asks for one list at a time, each
answered from the game's own tree (`shopcategory.lib` and
`shopsubcategory1.lib`, `fc3_formats::shop::tree_from_libs`) and our stock:

* `DataCategory`: buy and sell (`sName`, `iObjectID`, `sImage` the widget's
  icon, `TX_Buy`). It opens buy: `DataSub1Category 11`, its subcategories
  in their order, those with anything of ours in them, each with what it
  opens (`iTypeID`: 0 guns, 5 melee weapons, 4 ammunition, 2 other things).
* `DataWeapon <subcategory>`: its guns, the shop's that are ours (by
  model): name and description (string ids), `sImage` (the shop's icon, a
  frame of `MC_WeaponIconsRightAlign`), `iStateID` 2 to buy or 3 owned,
  `iPrice` (free once its towers are taken), the shop's bars
  (`WeaponStatistics` out of 5, as fractions: the bars run 0 to 1) and stars
  (`fRating`), and `bCustomizable`.
* `DataItem <subcategory>`: its ammunition and supplies (the database's,
  `progression::supply_items`), with how many are carried
  (`iCurrentCount` of `iMaxCount`; body armour as a percentage), its maps
  (`treasure_maps`), or its melee weapons.
* An owned gun's `DataWidgetCustom <weapon>`: its two lists (attachments,
  `TX_Attachment`, and paints, `TX_Skin`), each with an id of its own
  (`custom_id`), then `DataCustomization <list>`: the attachments it takes,
  by the shop's offers (name, description, icon and price: `iStateID` 3
  once bought, `bEquip` fitted), or the paints its model takes (ours).
* Sell: `DataSell`, the rucksack's tiles (as the rucksack page's) and one
  for each room left in it (`iStateID` 0, "empty"); the page counts them
  (`9/16`).

`OnBuy <type> <id>` buys (`progression::trade`, the same as our shop's
buttons), `EquipAttachment <id>` puts a sight bought before back on (or a
paint on or off), `SellLoot <id>` sells one of a thing (double-clicked, or
twice within a quarter second) and `OnSellAll` (Quick Sell) the
valuables; each has the list sent again and the money. A tutorial's shop
(`ShopOverride`) opens on its weapon (`OverrideShopMenu <subcategory>
<weapon>`, once the subcategories are in). Escape goes back a list, and
out from the subcategories (`QuitMenu`). `--shop 0` (tests) opens it in
play; `FC3_PAUSE_PAGE=0` from the pause menu.

Guns bought stay owned (`Progress::owned`, saved), carried or not. An owned
gun not in a holster takes the legend's Equip (`OnEquipRequest <weapon>
<from>`): the game sends `DataEquip`, five rows (`sName`, `sImage`, the
shop's icon, `iStateID`): the four holsters, the guns carried in order (a
place without one open while the holsters have room, locked beyond), then
the gun. The page shows its "Assign To Slot" wheel; a holster chosen
(`OnWeaponEquip <place>`) takes the gun, the one there put by
(`weapons::equip_gun`), and `HideEquip` closes the wheel. A gun bought with
every holster full is owned and the wheel opens for it
(`progression::buy_to_own`), rather than the gun in hand being swapped.
`FC3_SHOP_EQUIP=<gun>` (tests) owns that gun and opens the wheel for it
once the page has its subcategories.

**Main menu** (`UI\Multiplayer\Menu\MultiMain.feu`, level 20;
`original_ui::frontend`): the game's frontend in place of ours while the game
is at its main menu (its own cursor; the HUD hidden). It loads its fifteen
pages one by one (co-op and multiplayer's among them), then asks
`RequestMultiMainState`; the answer (`FrontEnd` `RequestMultiMainStateResponse
gameStart`, and `OnVersionNumber` with ours) opens its home list, which it
asks for (`OnRequestMainMenu`): `MainMenuDataProvider`, the story (`iAction`
1), the options (6) and quitting (41) by their string ids (`locTitle`);
co-op, multiplayer, the map editor and the extras aren't offered. A choice
asks `AllowFrontendAction <action>` (`FrontEnd` `AllowFrontendActionResponse
<action> 1`); quitting is `QuitToDesktop`. The story: `OnRequestLoadGameFilePreview`
is answered `InitStoryPage` ("1" with a save), and the campaign page
(`singlePlayerMenu.feu`, its class in the frontend) lists Continue, Load
Game and New Game. It asks which of its six slots hold a save
(`GetSaveGamePresence`: autosave 1, save 1, autosave 2...; ours is autosave
1) and, as a line is chosen, what is in one (`RequestSaveGameInfo <type>
<slot>`: `SaveGameProvider` with `iType`, `iSlot`, `sSlotName`,
`sCurrentChapter` "Chapter <n>", `sTimePlayed`, `sSaveLocation`,
`sDifficultyLevel`, `sTimeSaved`; `iType` -1 for none). The chapter (its
picture, 1 to 11) is ours: how far into the 38 main missions the save is.
`LoadSaveGame` goes on from the save (`save::ContinueGame`); a new game's
slot is taken (`ValidateNewGameSlot`, `NewGameSlotValidated`) and
`InitNewGame <slot> <difficulty>` starts the story. The options page takes the
pause menu's answers. Behind it plays its Bink loop (`Swan_30_Sec_loop.bik`),
which it loads into a clip the engine drew over: the host answers a `.bik`
load with an empty movie (`ui/empty.swf`, written to the cache) so the clip
empties, the loop plays as a backdrop under the interface (`video`, filling
the window), and the black under the clip goes (`Host::set_path`,
`_level20.bg.black._visible`).
Before it, the first time, come the Ubisoft logo (`M_LogoUbi.feu`, which
says `UbiLogoEvent 1` near its end) and the title (`PressStartScreen.feu`,
over the loop). The title's text is the consoles' "PRESS START" on its
timeline (`text.mc.Normal_txt` and its shadow's); its class's label and the
`SetText` it listens for aren't in this build, so the PC's prompt (159971) is
put in those fields while it shows, and a key or a click (`StartPressed`)
brings the menu a moment later (the label that would say it has gone isn't
there either). `--menu` keeps an automated run at it, `FC3_SAVE` gives it a
save, and `FC3_FRONTEND_KEYS=Enter,Down,...` presses on from the title, then
presses keys once the home list has been up two seconds.

**Loading screen** (`Loading\GameModes\Singleplayer.feu`, level 60, over
everything): while a jump keeps the player waiting for the world (the start
of play, a checkpoint reloaded, fast travel, a story's teleport: the
player's `spawning` for more than half a second) until the ground is under
him and no terrain, objects or vegetation are still to come in (for half a
second; four for the one that starts play, as the story places the player
at its checkpoint a few seconds in), and at least two seconds (at most a
minute). The movie plays itself: its shuffled
words, the game's tips (string ids of its own), and the spinner it loads
(`Loading.feu`, which says `CommonResourcesLoaded`); it says
`SinglePlayerLoadingLoaded` and asks `IsBootTitle` ("0"; "1" shows the boot
title's guns instead). The HUD hides under it. A trial's run beginning
(the player taken to its start) puts up the trials' own instead
(`SocialChallenge\(SC)Singleplayer.feu`): the challenge's pause page in
loading mode (`CChallengePausePage` 1, `ChallengePausePageLoaded`), its
titles and the points' legend (`SetChallengeSetupLegendInfo`, sent while it's
up as the page listens only once loaded), fading to the spinner after 12 s. `FC3_LOADING_SHOT=<file>` (tests) captures it
two seconds in.

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
(`syringes::inject`) and answers `CraftingUseResult` 1 or 0. The two quick
slots: as the page loads, and again whenever a syringe is put in one, the
game sends `AssignedSlots`. It carries each slot's icon (the recipe's
`sIconName`, which `DataRecipes` also carries as `sImage`), its name, its
place in the catalog (-1 when empty) and how many are carried. Then come the
keys the page shows: the camera (none here), the rock and the two slots.
`CraftingRecipeAssigned <id> <slot>` puts the recipe in the slot and takes
it out of the other. The slots are saved (`quickslot`), and their keys
(`use_recipe_a` and `_b`, the game's 7 and 8, rebindable) inject the
syringe in each if one is carried. The narrative action maps switch them off
as they do the game's signals. An arrow tip is made from the inventory: its
grenade or molotov goes, and a tipped arrow is added up to the quiver's
special arrows (`CraftCatalog::craft_arrow`). The page counts the tipped
arrows carried, as it counts syringes. The selected recipe's model shows
in the page's 3D view (`3D_Crafting_MC`; see **3D item views**), its
stand-ins (plain white and blue sheets) hidden. Blank data fields
go to the movie as null, which the shim makes blank again (a blank string
from the host reads "null" in SWF 8).

**3D item views** (`original_ui::views`): the shop and the crafting page
leave places for the engine to draw the item they show (`3D_MC`,
`3D_Weapon_MC`, `3D_Crafting_MC` and their siblings; the stand-ins in them,
plain sheets, stay hidden). The converter writes each item's view to
`ui/menu_views.txt` (`fc3_formats::menu_views`): the model its menu
archetype draws (a shop item's `arkDisplayArchetype`, a recipe's
`arkMenuArchetype`: the gear's own posed `menu_*` models, a syringe), its
turn (a shop item's `DisplayDefaultAngles`; a subcategory's own item,
`bDisplayAsDefaultItem`, with `DisplayDefaultCategoryAngles`) and its sway
(`fRotationSpeed` in turns a second, `fRotationAmplitude` in turns). The
shop asks for what to show as the player moves (`onRequestWeapon3D`,
`onRequestItem3D`, `onRequestAttachment3D` with the shop id, and
`onRequestCategory3D` with the subcategory); the crafting page's is the
selected recipe (`CraftingRecipeSelected`). Items with no display
archetype (ammunition, Refill) show nothing.

While the page shows the place (the shim's `dpsShown`: its parents all
visible, up to its level, so a page gone or a part of it hidden hides the
view), a camera of its own draws the model into an image the place's size
in window pixels. It's on render layer 6, with its own key and fill lights
and ambient. The model is seen along Dunia's +Y with Z up, framed to fill
the place (as if at least 24 cm across, so a grenade isn't blown up to a
rifle's size), and sways about the upright. A model with no turn of its own
that is long front to back (a syringe) is turned to lie across. A UI node
over the canvas shows the image. The shop's whole-page place (`3D_MC`,
where it shows items) is taken as its guns' (`3D_Weapon_MC`, the left
half). The authored on-screen positions (`vectorDisplayDefaultPosition`)
aren't used: the engine's menu camera they're relative to isn't known.
`FC3_MENU_VIEW=shop:<id>` or `recipe:<id>` (tests) shows that item in
whichever place is up; `FC3_MENU_VIEW_DEBUG` logs each place, whether it's
shown, and its bounds.

**Handbook** (`M_Handbook.feu`, quick launch 10) asks for one list at a
time, `OnEncyclopediaRequest <id> <list>`, and the game fills that
provider from the handbook (`progression::HandbookData`): `TabCategory`
(its categories by their order: statistics 2, the survival guide 5,
collectibles 1, tutorials 4, the manual 6, by `iObjectID`), `SubCategory
<tab>` (a category's groups), `WikiCategory <group>` (a group's pages:
`sName`, `sDescription`, `bUnlocked` as `handbook_open` has it). The
manual is the handbook's pages with `bIsEmanual`, each a section with a
header (`locidHeader`, a group's title or none) and its entries
(`EmanualContents`: a title and its text each, in order). The converter keeps
them with the handbook (`fc3-handbook 2`; version 1 still reads, without
them). The handbook's request for `Emanual` gets the sections (`iObjectID`,
`sSection`, `sHeader`). The manual's movie then names a section
(`OnEmanualSectionChanged <id>`) and gets its entries, one
`SetupEmanualContent <title> <text>` each.

The statistics tab lists its own eight categories and asks for one's rows
at a time, `OnEncyclopediaRequest <category> StatsCategory` (0, the
progress page, as the tab opens; its legend button flips to the lists).
The game answers as the engine's `CFCXStatsDataProvider` did, from
`statistics::rows`: each row's name (the game's string), `iValue`,
`sMilestones` (the progress page's), `sType` and, on the progress page and
the animals', `iTotal`. The movie formats a value by its type: `d` a
distance (sent in decimetres, shown in km), `t` seconds, `m` money, `p` a
percentage; a row whose value is a text (a favourite weapon's or syringe's
name, or `-`) sends the same text as its type, which the movie shows as
the row's description. The animals' skinned column is `-` for those that
give no skin. The progress page draws each total as blocks, its
milestones (`<n>-<a|b|c>`) as the achievement, signature weapon and recipe
icons, and the whole as a percentage. What the rows count is in
`docs/story.md` (the handbook's statistics). Tests: `FC3_PAUSE_PAGE=10,2`
opens the tab, and `FC3_PAUSE_KEYS=Click@1636:1050` clicks its legend's
statistics button (a `Click@x:y` step clicks there in the window's
pixels).

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
`iCurrentValue`, `bEnable`. Some places are the page's: the control type
third and customizing the controls fourth among controls. Calibration is
the page's own popup of three sliders (brightness, contrast, gamma, in that
order). They send `SetBrightness`, `SetContrast` and `SetGamma <0-100>` as
they move, and its reset sends 50 to each. The game keeps them in
`settings::Settings`, and the post-effects pass applies them (see
`shaders.md`, Calibration). `SetOptions
<values> <section>` brings the section's values back, comma-separated in
row order, into `settings::Settings` (sliders over the native menu's
ranges: sensitivity 0.1-5, field of view 55-110, volumes 0-1), which saves
as it changes.

The video and video quality sections have the engine's own rows, in its
order (`FC3.dll` builds them from its string ids; the PC page handles
several by place):

* **Video:**
  * display resolution: the primary monitor's modes, as text of ours;
  * window mode: fullscreen (exclusive, at that resolution), windowed (that
    size) or borderless. The page greys the resolution out for borderless.
  * V-Sync: off, 1 frame or 2 frames (here both present at every refresh);
  * GPU max buffered frames (1-5): the swap chain's frame latency;
  * widescreen letterbox (see below);
  * DirectX: our graphics API, greyed out;
  * MSAA (off, 2, 4, 8): here SMAA's low, medium and high presets;
  * alpha to coverage: greyed out, since it needs MSAA, which this renderer
    doesn't use;
  * SSAO method (SSAO, HDAO, HBAO): the occlusion's medium, high and ultra
    quality;
  * field of view.
* **Video quality:**
  * overall quality, of seven labels: LOW to ULTRA, OPTIMAL and CUSTOM;
  * textures, ambient lighting, shadow, post FX, geometry, vegetation,
    terrain and water, each in the engine's levels: textures, ambient and
    terrain stop at HIGH, vegetation and water at VERY HIGH, the rest go to
    ULTRA.

  When the overall level changes, the page sends the section and asks for
  it again. The game then sets every row to that level, or to the row's own
  highest below it (OPTIMAL is taken as HIGH). Any other change keeps the
  rows, and the overall level shows the preset they make, else CUSTOM. The
  rows do the following here:
  * textures: anisotropy 2/4/8, and low skips the finest mip level. It
    applies to textures loaded once the settings are, so it needs a
    restart.
  * ambient lighting: no occlusion, low occlusion, or the method's level;
  * post FX: bloom from medium, sun shafts and the distance blur from high;
  * geometry: the streaming radii;
  * vegetation: grass and small clutter reach 40/55/75/100 m and
    110/145/180/240 m;
  * terrain: how far the finer levels reach;
  * water: the ocean grid's density, and no caustics at low.

The letterbox (the engine's `WidescreenLetterbox`) shows a 16:9
band on windows narrower than that. The post-effects pass draws the bars,
and the camera's field of view is widened for rendering, so the band
frames what a 16:9 screen would. The original HUD still draws over the
bars.

**Key bindings** (`M_KeyBinding.feu`): the options page loads it into
itself (a `MovieClipLoader`) for customizing the controls. It asks
`OnKeyBindingsRequest`, and the game gives it `KeyBindings`, a row per
rebindable action: `sKey` the action's name (the game's own string where it
has one: 172370-172381 for movement through the next weapon, 156374 for
firing, 156377 for throwing a rock, 156358 and 156378 for the two syringe
slots; `original_ui::action_text`) and `sValue` its key. Activating a row
sends `OnKeyBindingChangeRequest <row>`. The game shows its key prompt with
`ShowPressKeyPrompt true`, and the next key or mouse button pressed (not
passed to the movie; Escape leaves the row) rebinds it, swapping with the
action that had it. The rows come again, and `ShowPressKeyPrompt false`
hides the prompt. The edit is a draft until the options page's Apply
(`onApplyChange`). Back drops it (`onCancelEdit`), and Reset to Defaults
(`onResetToDefault`) restores the default keys. Tests: `Bind:<key>` in
`FC3_PAUSE_KEYS` is the key pressed for a waiting row.

**World map** (`WorldMap.feu`, quick launch 2; the map key opens the pause
menu straight at it, the game's own map screen standing down while it's up,
`GameState::Map` still telling the story the map is open). The movie draws
the game's own map tiles, which it loads itself
(`worlds\fc3_main\worldmap\<level>_<row>_<column>.png`: four levels of zoom of
2, 5, 10 and 20 tiles a side, 512 pixels each; the converter writes them
from the archives' `.xbt` as PNGs where `feu::movie_rel` finds them; the
radar's map, `map/world.dds`, is their third level put together). It
centres on the player icon's own place (`CenterMap` sets `MapVisual` from
it), and the icons' places are worked out from `MapVisual` and `MapScale`
as the map moves (logged at debug level). Seen: the map over G3's coast
and pier at the player's place, the radar showing the same coast.
Everything else is the engine's to drive (`original_ui::drive_map_page`):

* On `MapLoaded` the game sends the legend (`DataMap`: `iIconID`, `bDisplay`,
  `sName`, the game's legend strings; the movie hides every icon whose type
  has no row), the fog lifted where the player has been (`SetFog 0 <row>
  <column>`: the movie masks the map with a ten by ten grid of cells, and
  the original lifted them with radio towers), and the icons (`IconMap`:
  `iType`, `iXPos`/`iYPos` as fractions of the map, `sName`, `iIndex`,
  `iAngle`, `bIsMarker`), here the player, objectives, radio towers taken
  or not, outposts held, safehouses and liberated outposts (both fast
  travel) and the waypoint. The types are the movie's `E_*` (player 0,
  mission 1, safehouse 6/7, tower 8/9, marker 20, outpost 70).
* The story's map markers (`story/markers.fcb`) add the rest. Activities,
  shops and the social challenges' boards show as they stand: enabled in a
  layer that's in (an activity's comes with its country's outpost taken).
  Markers a script shows (`GlobalSetObjectiveMarker`) show as what they
  are: a brief's marker as a mission giver, a quest giver's as its kind of
  quest, anything else as an objective. A marker's type
  (`mapobjectivetype`) and sub-type (the activity, the kind of quest) give
  the icon (`original_ui::marker_icon`: shop 5, knife 11, poker 31,
  shooting 36, racing 37, side, hunting, supply and wanted quests 62 to
  65, the challenge boards 66), each with its legend row (the game's
  legend strings; the quests share the handbook's). `fc3tool
  story-map-markers [filter]` lists the markers by type and sub-type.
  `FC3_MAP_REVEAL` (tests) lifts all the fog.
* The treasure maps bought at the shops (`treasure_maps`) show what they
  reveal in their cells, until it's picked up: relics 34, lost letters 32,
  memory cards 33, loot chests 21 and the plants by the leaf they give
  (green 57, blue 58, crimson 59, amber 60, white 61), each with the
  game's own legend row. The movie shows every type with a legend row (its
  per-zoom filter table is the engine's, which sent only what a zoom
  showed; the movie never reads it). It makes the icons twenty a frame, so
  thousands at once (every plant on the island) bog it down: only the
  cells of maps bought are sent. `FC3_MAP_FINDS=<cell>,...` (tests) shows
  everything in those cells and logs how many icons are in view.
* Once the icons are in (`IconsLoaded`), `CenterMap <level> <zoom>`. The
  zoom is the whole map's width on the stage; it opens at 7168 (the
  movie's own start) and the wheel takes it a quarter in or out between
  1024 and 20480, with `OnLevelOfZoomUpdated <level> <zoom>` (the level
  whose tiles are shown at most 1024 pixels wide; the movie loads them).
  `FC3_MAP_ZOOM_STEPS=<n>` (tests) zooms n steps once it's centred.
* The icons are placed by the engine, not the movie: each is a DPS clip
  (`Icon_<index>`) that goes, whenever the view changes, to `(its fraction
  + MapVisual's _x / its _width) * MapScale's _width` (and the same down),
  the movie's own formula for the waypoint it makes; the map's `mf_XPos`
  and `mf_YPos` get the offsets.
* Panning is the engine's as well (the movie only works out where a stick
  or the mouse would take it): a drag with the left button moves the map's
  picture (`MapVisual`, in the map's own pixels, and the fog's
  `FogVisualMC` with it) within its edges, and the movie loads the tiles
  coming into view. `FC3_MAP_PAN=<x>,<y>` (tests) drags that far in stage
  pixels once it's centred.
* The right button is the engine's `FireMouseRightButton` (its fourth
  argument "true" pressed, "false" let go; Flash has no right button),
  which sets or clears the waypoint under the cursor.
* `SetupMarker 1 <x> <y> <type> <icon>` places the waypoint (`SetupMarker
  0` takes it away), and `onFastTravel <n>` travels to the n-th fast travel
  icon sent (the movie counts safehouses and liberated outposts).

**Sharpshooter challenges** (`UI\Singleplayer\Activities\Acti_Shooting.feu`,
level 4, while a shooting gallery runs: `original_ui::drive_gallery`, the
gallery itself in `shooting_gallery`, see `docs/story.md`):

* Loaded, the movie sends `ShootingGalleryMovieLoaded`; the game answers
  `LoadShootingGalleryElements PC 0 "[Enter] " "[Esc] "` (the platform and
  the accept and back keys' text) and the movie `ShootingGalleryElementsLoaded`.
* Its boxes come and go by event as the gallery goes: `Show`/`Hide` +
  `DifficultyBox`, `ReadyBox`, `CountdownBox`, `ShootingGalleryElements`
  with `TimeRemainingBox` (the count and the clock), `ResultBox`; the clock
  turns red with `SetTimeRemainingIconToRed` (the last five seconds).
* The game fills them by DPS id: the four wagers (`WagerValue*Text_MC`),
  the target (`KillCountText_MC`), `InsufficientFundsText_MC` shown when the
  row chosen costs more than the player has, the selection bar's place
  (`SelectionArrowsContainer_MC`'s `_y`: -23, 3, 33 and 61 for the rows),
  the gallery's subtitle (`DifficultyBoxSubTitle_TX`, a string id), the
  countdown, the count, the target and the time left, and the result:
  `MedalResults_MC` at frame 1 (target reached) or 30 (short of it), the
  birds killed, the money won, and the failure's count and target.
* The mouse over a row comes as `OnDifficultyRollOver <row>` (a click
  takes it) and over the back button as `OnCancelRollOver`/`Out`.
* While it runs, Escape backs out of the difficulty box and leaves the
  result rather than pausing, and the objective box (where the count goes)
  is shut.

**Poker tables** (`UI\Singleplayer\Activities\ACTI_Poker.feu`, level 4,
while the player is at a table: `original_ui::drive_poker`, the game in
`poker`, see `docs/story.md`):

* Loaded, the movie sends `PokerMovieLoaded`; the game answers
  `LoadPokerElements PC 0 <accept> <cancel> <help back> <help next>` (the
  keys' texts before the boxes' buttons; the accept's left empty, as the
  buttons have no room for it) and the movie `PokerElementsLoaded`, after
  which `ShowPokerElements` shows it.
* Its boxes by event: `ShowDifficultyBox` (with `HideMainHUDElements`),
  `ShowMainHUDElements` (the table's own panel), `ShowQuitBox <buy-in>
  <chips>`, `ShowRestartBox` (busted out), `ShowCleanedTableBox $<won>`
  and SQC_100's `ShowCleanedTableSQC100Box`, each with its `Hide...`.
* A hand's end in two steps: `ResetEndOfHandResults 1 0 <lines> <lines>
  0` sizes the box; once it answers `ResetEndOfHandResultsComplete`,
  `UpdateHandsRevealedInfo <n> <line>...` (each `Name[c1 c2 c3 c4
  c5][<hand's string id>]`, or with n 0 just the winner's name: "all
  other players folded") and `UpdatePlayerWinningsInfo <won>-<busted>-
  <W|L per line> <amount>...` (the movie writes "WINS $amount", or
  "Busted out!" for `BustedOut`); winners' lines first, then those it
  put out, then the rest.
* By DPS id: the seats' names, chips and frames in the table's diagram
  (`P<n>Name_MC`, `P<n>Money_MC`, `P<n>Stats_MC`: 1 in the hand, 10 out
  of it, 20 nobody; P1 the player, P2 on the left, P3 across, P4 on the
  right; the diagram, `TopLeftBox_MC`, while 1 is held), the button
  (`DealerContainer_MC` 1/20/30/40), `PotCounter_MC`, `LastBetCounter_MC`,
  `PlayerChipCount_MC`, the info box's line (`InfoBoxText_MC`, the game's
  strings with `0` the player and `1` the chips), the action panel on the
  player's turn (`TopRightBoxB_MC`, `Check_MC` CHECK/CALL/RAISE,
  `BetAmount_MC`, `BetUpDownArrows_MC` dimmed at the bet's ends; at
  Hoyt's last hand `TopRightBoxBJAS220_MC`, BET LIFE, in its place), and
  the difficulty box's wagers, selection and accept button.
* The player's cards and the board: `UpdateCurrentHandInfo <cards>
  <folded> <hand's string id> True <board>` (cards as `Ah Kd ...`, hole
  cards first; the movie draws them) and `CurrentHandBox_MC` shown.
* The help screen (H at the table; the game waits): `ShowHelpScreenBox
  <cards>` opens it on the hand probabilities (it hides the table's own
  panel; `HelpScreenLoaded` answers), `ShowSpecificHelpScreen <0-4>` turns
  its pages (the probabilities, a hand's example, the basics and two of
  terminology; the arrows, A and D, or the mouse over its page names) and
  `HideHelpScreenBox` closes it (Escape or H). On the probabilities page
  each kind of hand's chance by the river goes to `OddsBox<Kind>Text_MC`
  (`holdem::odds` over 4000 deals; `<1%` for a chance under one in a
  hundred), the hand held to `OddsCurrentHandInfoText_MC` (its own line
  while no cards are dealt) and `OddsInfoText_MC` is emptied, except at a
  bar's hardest difficulty, where the chances stay blank under its note.
* The mouse over the boxes' buttons comes as `OnConfirmRollOver`,
  `OnCancelRollOver`, `OnDifficultyRollOver <row>` and the help screen's
  `OnPreviousHelpRollOver` and `OnNextHelpRollOver` (a click takes them).
  While the player is at a table the main HUD and the radar are down, as
  is the objective box.

**Trials of the Rakyat** (`trials`, see `docs/story.md`):

* While one runs, the main HUD's `Challenges_MC`
  (`UI\Singleplayer\Hud\Challenges\H_Challenges.feu`) is up
  (`ShowUIElement Challenges`, its widget `ChallengeWidget` shown): the
  score (`ChallengeTextActualScore`, `ChallengeTextActualScoreMC` played
  from `Anim` as it moves), `/` the next reward's score
  (`ChallengeTextChallengerScore`), the time left as `mm:ss`
  (`ChallengeTextRemainingScore`), and each score's popup
  (`ChallengeUpdateScore_<n>` played from `FadeIn`, its rule's name in
  `ChallengeTextUpdateScore_<n>` and `+N` in `ChallengeTextScore_<n>`; two
  slots to a kind, taken in turn: kills, headshots, stealth, takedowns,
  time).
* The stone's board (`M_ChallengeBoard.feu`, quick launch 6) and the
  result (`M_ChallengeResult.feu`, 7) open from the pause menu: when the
  game asks for one the pause page's `RequestPageToLoad` is answered with
  it. On `ChallengeBoardPageLoaded` / `ChallengeResultPageLoaded` the game
  sends `SetChallengeGlobalInfo` (the board: name, description, best; the
  result: name, best, this score, after `SetResult 1|0`),
  `SetChallengePercent ""`, `DataChallengeBoard` (`sName`, `sScore`,
  `iType` 0 the player or 1 a developer, `iClientId`) and
  `DataChallengeRewards` (`sName`, `iRewarded`). The board's
  `ChallengeBoardStart` / `ChallengeBoardQuit`, the result's
  `ChallengeResultChoice 0|2` (again, leave) and the trial's pause page's
  (`M_ChallengePause.feu`, 11: its legend from
  `SetChallengeSetupLegendInfo`) `ChallengePauseAbort 1|0` (give up, go on)
  go back to the game. The pages' start buttons answer the mouse (the
  legend's click) and the pad's button, not Enter.

**Races** (`UI\Singleplayer\Activities\ACTI_Driving.feu`, level 4, while a
race runs: `original_ui::drive_races`, the race in `races`, see
`docs/story.md`):

* Loaded, the movie sends `DrivingMovieLoaded`; the game answers
  `LoadDrivingElements PC 0 "" ""` (the accept and back keys' text left
  empty: the result box has no room for them) and the movie
  `DrivingElementsLoaded`.
* Its boxes by event: `ShowDifficultyBox`, `ShowReadyBox` then
  `ShowGeneralTextBox` (the count, then "go"), `ShowCheckpointDrivingElements`
  (the clock and the gates), `ShowQuitBox`, and `HideDrivingElements` with
  `ShowResultBox`, each with its `Hide...`.
* By DPS id: the wagers (`WagerValue<Row>Text_MC`), the row's target time
  (`WagerSelectionTargetTime_MC`, `m:ss.s`), `InsufficientFundsText_MC`,
  the track's name (`DifficultyBoxSubTitle_TX`), the cursor
  (`SelectionArrowsContainer_MC` played from 1, 10, 20 or 30), the count
  and "go" (`MessageBoxText_MC`), the clock (`TimeBoxText_MC`, its
  `textColor` red, 0xDA4024, in the last ten seconds) and the gates
  (`CheckpointBoxText_MC`, `passed/expected`), and the result's medal
  frame (`MedalResults_MC` 20 won, 30 lost), status (`RaceStatus_MC`), the
  target time, the time left and the money won.
* The mouse over a row comes as `OnDifficultyRollOver <row>`, over the
  boxes' buttons as `OnConfirmRollOver` and `OnCancelRollOver`.

**Knife throwing** (`UI\Singleplayer\Activities\ACTI_knifeThrowing.feu`,
level 4, while the player is at a knife thrower's game:
`original_ui::drive_knives`, the game in `knives`, see `docs/story.md`):

* Loaded, the movie sends `KnivesMovieLoaded`; the game answers
  `LoadKnivesElements <knives> PC 0 "[Enter] " "[Esc] "` (the knives a
  side, the platform and the accept and back keys' text) and the movie
  `KnivesElementsLoaded`. It names three of its loaders' DPS ids
  `_fireDPSID` (`LosingMsgBoxLoader_MC`, `CancelMsgBoxLoader_MC`,
  `CrosshairLoader_MC`), which the engine took too, so the shim registers
  both spellings.
* The boxes are their loaders played to their `In` and `Out` labels by
  DPS id, each with `Show...`/`Hide...` (which only starts and stops the
  movie reporting the mouse over the box's buttons): `BettingLoader_MC`
  (`BettingBox`), `CancelMsgBoxLoader_MC` (`CancelBox`, Escape mid-game),
  `WinMsgBoxLoader_MC` (`WinBox`), `LosingMsgBoxLoader_MC` (`LoseBox`) and
  `DrawMsgBoxLoader_MC` (`DrawBox`). The game's own elements come in the
  same way: `ScoreLoader_MC` from the first throw, `BarAnimLoader_MC` and
  `CrosshairLoader_MC` on the player's turn, `KnifeContainer_MC0`.. while
  it's played.
* By DPS id: the wagers (`BetValue<Row>_DT`), the row (`WagerCursor_MC`
  played to `Practice`, `Novice`, `Skilled` or `Expert`), not enough money
  (`NotEnoughMoney_MC`, the accept prompt dimmed:
  `PromptAcceptContainer_MC` frame 10), the subtitle, the scores
  (`PointsPlayer_DT`, `PointsNpc_DT`), the knives (`Knife_MC<n>`: `Faded`
  once thrown, the player's next playing `Blink`, else `Normal`), the
  power bar (`BarAnim_MC`, a frame a percent from 1), the throw's prompt
  (`ThrowKnifePrompt_MC`) between throws and the skip's
  (`SkipCinematicBox_MC`) over the challenger's, each throw's points as it
  lands (`PointsPopUpText_MC`, the number: the movie has the plus;
  `PointsPopUp_MC` played from 2) and the amount won or lost
  (`WinAmount_DT`, `LostAmount_DT`).
* The movie adds the knives and the popup at its top left: the engine
  placed them. The knives stand in a row leftward from the bar's mark
  (`KnifeInitialPosition_MC`, at the bar's left end) and the popup is
  centred over the crosshair.
* The mouse over the boxes comes as `OnBetRollOver <row>`,
  `OnConfirmRollOver` and `OnCancelRollOver` (a click takes them).

While the pause menu is up the HUD's level is hidden (`Host::level_visible`;
the shim's `levelVisible`), as the game's was: subtitles showed through.

Ruffle gives a SWF 8 movie a blank string from the host as the text
"null" (as Flash did), so the host sends blanks as null and the shim takes
null as empty.

`fc3ui hud` helps work out what a clip's frames mean: `FC3UI_INFO=1` lists
every DPS clip's kind, frames, visibility and bounds, and `FC3UI_SET="id:
visible=1,frame=12,text=99;..."` sets clips before the frame is saved.
