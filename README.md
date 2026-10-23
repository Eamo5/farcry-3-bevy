# FC3 Bevy

An open-world shooter engine written in Rust on [Bevy](https://bevyengine.org) that runs on the data of
an installed copy of Far Cry 3. All engine code here is original: it reads the game's archives,
converts them once into a local cache, and renders and simulates the island from that cache.

No game data is included in this repository, and none is written into it.

## What works

- **World**: streams the full Rook Islands terrain (beyond about a kilometre, a coarse mesh of
  both islands keeps far shores, ridges and the other island in view, under a forest of the
  game's own tree billboards running to the horizon), the world's placed props and buildings, and
  4 million baked vegetation instances (grass, ground clutter, and trees rebuilt as full 3D models from the
  game's RealTree data), with distance-based detail levels. Grass, bushes and trees sway in the wind, and grass and bushes part around the player, people, animals and vehicles moving through them; their shadows and the depth prepass sway with them. The roads and tracks (asphalt, dirt, sand, jungle footpaths) are laid along the world's road splines with the game's road materials, frayed at the edges and faded where they meet.
  The ground blends the world's own tiling terrain layers (grass, dirt, sand, cliffs) by the
  per-sector paint masks, with normal-mapped close-up detail. Placed objects take the
  materials their entities override them with, so the waterfalls and rapids run with foaming
  water over the mossy rock that the models only carry as grey placeholders. Volumes the
  engine never draws (interior and cave volumes, sound rooms, blockout pieces) are left out, and
  invisible collision boxes keep their collision but aren't drawn. A sector appears once its models
  are in. It waits up to four seconds for their textures, then shows anyway, so one texture load
  that never finishes can't hold back a whole sector, as it used to; vehicles, animals, birds,
  characters and the first-person arms do the same (a stuck texture under the parked 4x4s had kept
  every parked vehicle away in about one start in three). Parked vehicles appear once the ground
  under them has its collider, so they no longer drop through the world as it streams in.
- **Environment**: the world's own sky colours over the day (pale blue by day, glowing toward
  the sun at dawn and dusk, teal at night), which also light the shade and reflections, with the
  haze over distant land taking the sky's colour at the horizon; the game's own cloud panoramas, with the weather drifting
  from clear through cumulus to storms (darker light, haze, rain and the game's rain ambience); a
  running day and night cycle on the world's own sky (the sun rises toward NNE at 6:00, passes
  overhead and sets toward SSW; the moon rides the same turning sky) whose sun and moonlight take
  their colours over the day, and the haze its density, from the world's own environment presets,
  with moonlight, stars, the
  game's moon and the game's own exposure (from its presets, day and night) and final colour grade
  (its tone curve and colour cube, read off the installed game), its bloom (only what its response
  curve lets through: highlights, the sun's glitter on water), sunlight glowing through grass and
  leaves as the game's vegetation lighting has it, storms with the game's own storm
  sky, cloud colour and thick fog; sun shadows through four cascades out to
  400 m (small props and bushes drop theirs nearer, as in the original); distant land shadowed by the game's baked
  horizon maps beyond the shadow cascades, and the game's lens flare (glow and ghosts) when looking toward an unobstructed sun, and sun shafts streaming through leaves and past roofs and ridges toward it; an animated ocean (Gerstner swell, the game's water normal maps, colour and clarity by
  depth, shoreline foam, the bed seen through the ripples with sunlight's caustics playing over it in the shallows, the shore and trees mirrored in it, underwater fog); the rivers and lakes above the sea at the water
  levels the game's sectors give them (calmer, murkier water, swimmable, with boats on them), the rivers
  running downstream at their own pace with foam through the rapids; and spatial ambient sound: the game's own region beds and calls by day and by night (jungle, island forest, open grassland, beach, open sea, mountain peaks, picked from the ground underfoot), and the world's 900
  placed sound points: waterfalls rumble, wind whistles through catwalks, branches creak, flies
  buzz and fires crackle where the game puts them. Under a roof, the outdoors is muffled, and indoors the sky's blue light gives way to a warmer bounce.
  Footsteps and impacts sound like what they land on. Sand, grass, dirt, mud and rock come from
  the painted terrain layers. Wooden floors, metal roofs, concrete and fences come from each
  model's own collision materials. Shallow water splashes and bodies thud. Walking, crouching
  and running each use the game's own step sets. Landings and slides sound like the ground too.
  Vehicles use the game's own sound sets:
  - Engines rev through the gears and follow the throttle, blending the game's RPM-banded loops.
  - Tyres roll on the surface beneath them and screech when sliding.
  - Ignition, switch-off, suspension landings, brakes, and smoking and burning engines all play.
  - Patrol vehicles in the distance use the game's lighter AI engines.
  Reloads, draws and takedowns play the sounds their clips carry, in time with the animation.
  Player foley and stings:
  - Bullets whizz past close by.
  - Being spotted plays the game's detection sting.
  - Levelling up and looting cash have their sounds.
  - Diving under splashes into a muffled underwater bed, and coming up after a long dive gasps for air.
  - Swimming strokes, wind rushing past in a fall or wingsuit glide, and the death sound all play.
- **Lights and effects**: the world's 2,200 placed lamps, lanterns, street lights and camp fires
  light up at dusk, with glows visible from across a valley. With the game's grade the lamps light
  as the game's light passes do: their colour times their intensity, full within their inner
  radius and falling as `a / d² + b` to nothing at their radius (`fInnerRadius`, `fRadius` of the
  game's `CDynamicLightComponent`), in the same units of light as the sun and moon, and flicker as
  the engine's light code has it: a wave at a rate set by `fFlickeringFrequency`, a slow cosine
  roughened by `fFlickeringNoise` times a few faster sines and cosines, a share of
  `fFlickeringAmplitude` of which is added to the intensity. The converter reads the wave's
  multipliers, which terms are sines, and its scales out of the engine's own code in the
  installed `FC3.dll` (`fc3tool flicker` prints them); without them lights burn steady.
  Camp fires light their logs and stones with the light their flame effect gives off in the game's
  particle library (a pool a metre and a half across, at the effect's bone); an effect's light, as
  the engine's particle code has it, is one light per particle its emitter puts out, each lasting
  its `LightDuration` and fading in and out over `LightFadeIn` and `LightFadeOut`, each its
  intensity times a share of its amplitude times the wave's size with a random phase of its own,
  so a fire's intensity of 50 comes to about 1; each goes out below the engine's cutoff (read from
  its code too). About three in four fires also have a broader light the designers placed beside
  them; fires dying down show their embers and smoke but give no light, as in the game. Muzzle
  flashes, impact dust and
  sparks, blood, explosions, splashes, smoke and fire use the game's own effect flipbooks, and bullet
  holes, blood and scorch marks stay on surfaces. The world's 4,500 placed effects of the kinds the
  game knows play near the player: spray and mist where the waterfalls land, low mist in the
  valleys, fireflies after dark, candle and torch flames, and camp fires burning or dying down to
  embers (the effects props play on themselves, at their bones). Standing in a waterfall's spray or
  coming up out of the sea leaves water on the lens (the game's droplet map), which dries off.
- **Fire**: molotovs, the flamethrower and explosions set grass alight; fire spreads downwind
  through grass and bushes, burns them away, scorches the ground and burns whatever stands in it.
  Rain damps it down.
- **Aiming**: each weapon aims with its own FOV, sight time, and slower walk and turn (pad or mouse)
  from the game's data; with a gamepad the game's aim assist for the difficulty follows a target
  under the sights, snaps the sights onto it as they come up, and bends shots toward it (an
  option turns it off).
- **Enemies' senses**: guards see through the game's view volumes for the player's posture and their
  state (a narrow one while unaware, wider as they grow alert), find bodies and spot live grenades
  in the game's times, and run clear of a grenade's blast.
- **Wildlife**: 28 kinds of animal from the game's models, graphic kits (their skin variants) and clips: deer, goats, boar,
  wild and kept pigs, tapirs, chickens, macaques, cassowaries, buffalo, wild dogs, thylacines, leopards,
  tigers, bears, komodo dragons, crocodiles at the shore and sharks in deep water, and the small and
  the swimming: rats, lizards, crabs, crows (which fly off and glide back down), giant tortoises
  (which hide in their shells), river fish, sea turtles, jellyfish, moray eels and manta rays, spawned where
  the game's wildlife regions put them (each region's own mix and distances, within the game's
  budget of groups) and moving at their archetypes' paces, with schools of the game's reef fish (yellow and red reef fish, emperor and French angelfish) cruising the reefs the game's wildlife regions stock with them and scattering from a diver. Each behaves by its personality in the game's
  data: it notices someone by sight (its sensory system), smell, a moving vehicle or sounds of
  each kind, stops and watches while aware, and then acts: prey keep off or bolt, boar and
  buffalo charge whoever comes too near or hurts them, and predators (cassowaries among them)
  hunt whoever is nearest, pirates and rebels included, striking in bursts with a pause to look
  between, and soldiers shoot back. Kills can be skinned. Animals are heard as well: each species
  calls now and then, gives its alarm as it bolts or charges, and cries out when hurt or
  killed, on its own sound events. Overhead, vultures and black eagles circle on
  thermals over the land and gulls wheel along the shore, on the game's bird models and flight
  cycles; a shot brings one tumbling down.
- **Villagers**: the island's locals go about the world's 55 villager zones, dressed from the
  game's generic villager kits (men and women) and moving on its civilian clips. They wander their
  patch and stop at the world's 50 villager spots nearby, on the game's own clips: hoeing a garden
  (hoe in hand, with its sounds), leaning on a wall, sitting on the ground, praying or going
  through a body's pockets. Plantations with no village by them get farmhands of their own. They run from gunfire and explosions and cower, and can be hurt. Zones inside an outpost the
  enemy holds stay empty until it's liberated.
- **Player**: first-person walk, sprint, crouch, slide (crouch at a sprint: low and fast, easing into
  a crouch, with each weapon's slide clips), jump and swim at the game's own speeds, jump height and
  gravity (the fast sprint tires after a few seconds into a slower one unless Marathon Man is learned),
  with collision against terrain and objects, plus fall damage. A sniper scope drifts with the breath; holding sprint holds the
  breath for a steady shot (five seconds, then a gasp and a pause).
- **Combat**: 29 of the game's weapons: handguns, SMGs, assault rifles, shotguns, sniper rifles,
  LMGs, the RPG, a grenade launcher, a flamethrower and the recurve bow (silent arrows that drop
  and stick; fire arrows that set alight where they land and explosive arrows, bought by the five
  and nocked with the reload key), with recoil, spread, reloading, aiming down sights and suppressed variants that
  barely alert anyone. Each weapon fires with its own sounds from the game (suppressed
  versions, empty clicks), and an automatic burst ends with its tail and echo. Shops sell the game's attachments for the guns you carry, as each weapon's shop list has them: suppressors,
  the sights (two reflex sights, optical, ACOG, illuminated iron sights; one fitted at a time, those bought kept to swap back on
  for nothing), the high-power and illuminated sniper scopes, the bow's barrel extension and larger magazines. Each mounts on the
  weapon's own attachment points and changes how it handles. Sniper rifles aim through a
  full-screen scope: the game's own scope mask round the view (the high-power and illuminated
  scopes' sharper one) with a duplex reticle, lit red in the illuminated scope. The game's screen overlays also
  put blood on the screen with hits and low health, an animal's claw marks across it when one strikes, and the
  dark closing in as the air runs out underwater. Weapons show their real metal and wood finishes and are held by first-person
  arms playing each weapon's own clips (breathing idle, sights, every shot, reloads, draw, walk and
  sprint, machete swings, throws); swimming, ladders, zip lines and the driver's seat get their own
  arm animations too. Also grenades and molotovs (in hand through the throw), explosions,
  explosive barrels, and segmented health that regenerates. C4 is thrown and sticks to whatever it
  hits, vehicles included, and a held key sets every charge off from the remote; proximity mines are
  laid underfoot and go off under an enemy, an animal or a vehicle. The DShK and M249 guns the world
  sets on tripods and bunker mounts can be manned (E): the view sits behind the gun with the hands on
  its grips, it turns as far as its mount allows, never needs reloading but overheats. Enemies in a
  fight run to a free gun nearby and swing it onto the player. Armed vehicles (the world's
  `_Turret` technicals, 4x4s and Zodiacs, plus road patrols and reinforcements) carry a DShK on
  their own mount (the technical's post, the 4x4's roll-bar ring, the Zodiac's ring): a crewman
  stands at it and fires on the move, and stays on it when the rest jump out; the player can take
  it over from the bed. A medkit is a syringe jabbed into the forearm. Out of medkits and badly
  hurt, the player patches himself up instead: he digs out the bullet, pulls out shrapnel, sets a
  finger or pats out flames, depending on what last hurt him. Searching bodies, picking plants and
  skinning animals play the game's own first-person clips with their props.
- **Enemies**: they spawn at the world's guard posts, are animated by the game's skinned character
  clips, notice the player by sight and gunfire, then search, fight and die. They find their way
  round buildings and fences over the game's own navigation meshes (see `docs/navigation.md`).
  In a fight, riflemen, snipers and rocket and firebomb troops run for low cover along the mesh's
  walls, then crouch behind it (reloading there) and pop up to fire. Shotgunners, heavies and
  flamers keep coming. Headshots are judged against each enemy's animated head, so a shot just
  over a crouched head misses. Each enemy and animal takes damage as the game's own counters say:
  any real head shot kills, two rifle rounds to the torso do, the armoured roles soak body rounds
  and fire, arrows hit hard, animals have their own health; weapons lose damage with range as
  the game's do. Explosions nearby shake the view (and the gamepad, with Vibration on) with the
  game's own curves, as do scripted shakes, crashes and hits. Get within arm's length of one in a fight and he swings his rifle
  butt (the game's own strikes), knocking you back. Enemies flinch when hit. Each death is one of the game's 101 death
  clips, picked by how the enemy was moving (standing, walking, running or crouched in cover),
  where it was hit (head, body or legs) and from which side. Pirates and privateers
  come in seven roles: assault troops, shotgunners who rush in, snipers whose scopes glint, armoured
  heavies with machine guns, rocket teams, firebomb throwers and armoured flamers whose tanks burst
  when they fall. Posts whose markers name a role get it; others are a weighted mix. Each enemy is
  dressed from the game's own outfit kits (a weighted part per slot, dyed cloth and matching skin
  tones), so no two look quite alike. Troops flush a hidden player out with grenades, and the HUD
  marks grenades that land nearby.
- **Outposts**: the island's 34 enemy outposts, found by their flag poles and reaching as far as
  their alarms; each flies the pirates' or privateers' flag until it is taken, then the rebels'. Guards run for the alarm
  boxes when they spot you (the siren brings reinforcements, who drive in by technical from the
  nearest road or open ground and pile out to fight); sabotage the alarms first, clear the
  garrison to liberate the outpost (the game's safe-house XP, with its outposts' script's bonus for
  staying undetected or, failing that, for no alarm raised). Liberated outposts are then held
  by rebels, get a shop, and become fast-travel points on the map; after a death you come to at the
  nearest one. Rebels (also at the world's own
  rebel posts) fight any pirates or privateers they see, and rebels drive the roads round a
  liberated outpost in place of pirate patrols. Shoot open an outpost's animal cage and a tiger
  (a bear in the south) bursts out and goes for the garrison. Search fallen enemies for cash, ammo and
  their gear. Progress is saved.
- **Stealth**: silent takedowns on unaware enemies, played out with the game's paired first-person
  and victim animations (from behind, face to face, or dropping from above onto his front, back or either side, from a ledge or
  off a zip line passing over him; armoured heavies need
  the skill). The chain takedown skill turns a takedown into a knife pulled from the victim and
  thrown at a second enemy in view. With the pistol takedown skill and two or more enemies in view
  (or one, without the knife throw), you pull the victim's pistol instead and, holding his body up
  in front, shoot up to three of them before letting him drop; the shots are loud. The double takedown skill drops onto two enemies standing
  together and takes both. With the grenade takedown skill, when two or more enemies stand
  a few metres ahead, you pull the pin on the victim's grenade and kick him into them. The
  takedown drag skill (E instead of F) takes him down and lets you drag the body out of sight,
  walking backwards with it, before letting go (E or F). With Death From Below, an unaware
  enemy at the edge of a ledge just above you (or of a dock, from the water) can be pulled off
  it. Grass and
  bushes as tall as you hide you: crouched and still in a thick patch, you're nearly invisible
  beyond a few metres. Also camera
  tagging (tagged enemies stay marked and show on the minimap), detection meters over enemies'
  heads, and thrown rocks that draw guards over to look. Tagging raises the game's camera
  (its draw clip and model in hand) before the view zooms in through it. Some guards walk patrols around their posts;
  a guard who finds a body searches the area and calls others over. Off guard, guards take up
  the world's 2,100 guard spots: they stand easy or watchful, lean on walls, smoke (rifle put
  away), look over cover and round corners, check their gear, sit, or take a leak, on the game's
  own clips. Anything that puts them on guard sends them back to the fight.
- **Progression**: XP for kills, takedowns, hunting and outposts; levels give skill points for 21
  skills in three branches (health, damage resistance, reload, recoil, stealth, swimming and more).
  Money from loot and outposts; craft carry upgrades (ammo pouch, grenade bag, syringe kit, weapon
  holster, wallet) from animal skins; buy weapons, attachments, paint jobs (the game's tribal,
  urban, tiger and desert patterns, on the weapons whose materials come in painted versions: the
  game's own shader blends the pattern by the weapon's mask), supplies (C4 and mines too, three of
  each carried) and the parachute and wingsuit by category at liberated outposts.
  The journal (Tab) shows it all.
- **Getting around**: jump at a ledge up to about head height to pull yourself up onto it, or vault
  a low fence or crate with a drop behind, with the game's climbing and vaulting clips. Ride the
  world's zip lines (humming faster as you pick up speed), climb its ladders, and see the power lines strung
  between poles. Climb the 18 radio towers and take them over to reveal the map around them and
  free a weapon in every shop. Shops sell a parachute and a wingsuit. Jump while falling from a
  height to open the wingsuit, then jump again to open the parachute. Both fly by the game's own
  wingsuit figures and are steered with the movement keys while the mouse looks about (as far as the
  suit's look limits). The wingsuit glides at 12 m/s and sinks 4 m/s (barely at all skimming the
  ground); W noses it down to dive (up to 60 m/s) and S brakes (it can't climb); A and D bank it into
  turns. Flying into the ground is fatal, but if you own the parachute it opens by itself a moment short
  of it. The parachute pulls hard as it opens, then drifts at 4.5 m/s (7.5 pushed on) and sinks
  6 m/s, banking gently into turns. Both use the game's own first-person clips:
  a long fall throws the arms out into a dive, and the wingsuit unfolds along them (the game's
  membrane, rigged to its own skeleton) and the canopy is pulled as each opens. The parachute
  shows its canopy overhead.
- **Collectibles**: the island's 120 relics, 20 lost letters and 16 memory cards, each worth cash and XP.
- **Plants and syringes**: the island's plants go into a satchel. Arrowroot becomes medkits; the
  others mix into the game's syringe recipes in the journal, each lasting as its recipe says: marking
  animals, enemies or plants nearby, extra damage to animals, keeping animals away, steadier aim,
  fireproofing, a long breath underwater, a burst of sprinting and swimming, invincibility, one-hit
  kills and extra health. Y picks the syringe on hand and holding it injects. Underwater you hold
  your breath, and drown when it runs out.
- **Vehicles**: the world's ~400 vehicle spawns: 4x4s, technicals, quad bikes, cars, buggies and
  trucks on raycast suspension (weight carried low at the axles), boats and jet skis that float, and
  hang gliders. The cars, trucks, buggies and quad bikes drive by their archetypes' own drivetrains (the
  converter's `vehicles/handling.txt`, in the units the engine's own field notes give): their mass, the
  engine's horsepower and revs through each gear of its gearbox (shifting at the game's km/h), cut off
  at the game's speed (the 4x4's 20 m/s, 72 km/h, as in the original), its air drag, steering lock from
  rest to top speed, and each wheel's grip, springs and dampers (standing on its tyres as modelled),
  whether the engine drives it (front- and rear-wheel-drive cars as well as four-wheel drive) and whether
  the handbrake holds it: Space pulls the handbrake, which locks the rear wheels and lets them slide.
  The chassis is pressed down by its downforce and extra gravity, falls heavier once no wheel touches,
  and only rights itself in the air if the game gives it a stabiliser. Boats run on their own motor (thrust per unit of mass, its revs
  coming up at the game's rate), rudder angles and water friction ahead and to the side. Hang gliders
  fly by the game's paraglider figures: a 2.2 s take-off run off the launch without controls, then the
  stick banks and pitches the wing at the game's rates and angles, the bank turns it, nose down gains
  speed (to 30 m/s) and nose up bleeds it until the nose drops back, the brake slows it to its floor and
  left alone it settles back to cruising at 10 m/s, always sinking (less near the ground, fast when
  broken), with gentler landing figures below 10 m. Getting off in flight drops you with its momentum
  while the empty glider glides on until it comes down. At the wheel your head turns only as far as the
  vehicle's own limits let it (an open 4x4 or quad bike looks far up but not down at your legs, closed
  cars barely up, the hang glider far down). Every vehicle's
  driving camera widens with speed by its own angles (the 4x4's 85 to 115 degrees by 55 km/h, as a share
  of your field of view). Wheels roll with the
  ground, the front ones and the steering wheel turn with the steering. Parked vehicles keep their brake on.
  From the hour each vehicle's light settings turn them on (17:45) to the one they go off (6:30), a vehicle
  being driven lights its lamps (the lit glass its light switch puts on), throws its one headlight beam ahead
  (from its model's `DynamicLight` node, its range and cone), and its tail lamps glow red, brighter as it brakes. Vehicles take damage (hulls shrug off rounds but
  not rockets, C4 or grenades; enemy fire into a car you're driving gets through to you too):
  they smoke, then catch fire and blow up a few seconds later, leaving a charred, smouldering
  wreck that can't be driven. Hold E at a damaged one (a tap still gets in) to patch it up with
  the welding torch, on the game's repair-tool clips; a burning one stops burning once it's
  back above the fire mark. Chase camera or driver's seat (V), run-over damage, and the game's engine, tyre and wind sounds. Enemy technicals patrol
  the world's 12 patrol routes with a driver and a gunner in the back; when they spot or hear you they
  pull up and pile out to fight, and the vehicle is yours to take. At the world's boat-patrol
  markers (bays and the C4/C7 rivers and lakes) a Zodiac with a gunner circles the open water,
  slowing to fight from the boat when alerted. Cars driven into deep water flood and stall.
- **Pickups**: ammo, health, weapons, loot and herbs at the world's pickup locations; pickups and
  unsearched bodies nearby catch the light with a passing glint. A fallen enemy drops the gun he
  carried by his feet (the pirates' AK-74M, the privateers' P416); walk over one of a kind you carry
  to take half a magazine of its rounds, or pick it up as any weapon. Dropped guns go after two
  minutes (`FC3_DROP_TEST=<weapon>` drops one in front of you, for tests).
- **Interface**: HUD with minimap (turns with the player; enemies, tagged targets and outposts; the game's
  reach: 40 m on foot, widening to 100 m in a vehicle at speed and on the glider),
  a full-screen world map (M: zoom, pan, waypoints, fast travel; explored areas saved) with a GPS route to the waypoint along the island's roads, also on the minimap, main and
  pause menus, the weapon wheel (hold next weapon: the world slows while you pick), an options screen (difficulty: Casual, Adventurer, Warrior or Infamous: how much a health bar holds, as the game's own table scales it, the regeneration's delay, and how fast enemies spot you; sensitivity, FOV, volume, view distance, shadows, window mode,
  rebindable keys), gamepad support, and a first-launch conversion screen.
- **Saving**: autosaves every minute, on pause and on quit (position, health, weapons and ammo,
  collected pickups, time of day). CONTINUE in the main menu resumes it.
- **Original shaders**: the game's own D3D9 shaders are translated to WGSL during conversion. The
  converter pairs each material with a vertex/pixel shader pair, checks the pair by running it on the
  CPU, and bakes the result into the cache (3,747 of 3,762 materials pair; the textures a material
  leaves out are bound as the engine binds them: its own stand-ins where the material's descriptor
  names one, else nothing, which reads black). World props,
  characters and the nearby terrain then render with those shaders, lit by Bevy, including their
  normal maps, layer masks and dyes, and each material's own gloss and reflectance. Surfaces the
  game paints with its fully transparent debug map are left out (grey stand-ins on 35 models
  before). F4 or `--standard-shaders` switches to plain materials.
  See `docs/shaders.md`.

## Campaign support

New games run the installed game's original single-player mission graphs, with mission layers,
named characters, objectives, checkpoint reloads, voiced dialogue and subtitles, music, cinematic
sequences, facial animation, Bink videos and quick-time events. The scripts read the player's live
inventory, money, health, difficulty and time of day. Tutorials display the current keyboard or
gamepad bindings; visible mission timers follow the story clock and pause with the game.

Campaign support is still in development. End-to-end completion of every mission and side activity
has not been verified, and specific gameplays other than `QTEGSF` (including minigames) still need
host implementations. The skill tree, crafting and weapon tuning also retain sandbox rules. Full
visual, gameplay and UX parity with the original is the target, not a completed claim. See
[`docs/story.md`](docs/story.md) for the current runtime and test tools.

Options can select a free-roam new game, adjust music volume and toggle subtitles.
`FC3_STORY=0/1` overrides the new-game mode for testing.

## Requirements

- An installed copy of Far Cry 3 (Steam or Ubisoft Connect).
- A recent stable Rust toolchain (developed with Rust 1.99).
- About 8 GB of free disk space for the converted cache. Conversion stops if it would leave less than
  5 GB free.

## Build and run

```sh
cargo run --release -p fc3_game
```

On first launch the game finds your install and converts it into the cache in the background,
which takes about a minute on a fast machine, then opens the main menu. After that it starts in
seconds.

You can also convert ahead of time:

```sh
cargo run --release -p fc3_convert          # incremental; --force rebuilds everything
```

### Locations and environment variables

| Variable    | Purpose                                                   | Default |
|-------------|-----------------------------------------------------------|---------|
| `FC3_DATA`  | Game install, or its `data_win32` folder                  | Standard Steam / Ubisoft paths on Windows and Linux |
| `FC3_CACHE` | Converted cache folder                                     | `%LOCALAPPDATA%\fc3-bevy\cache` (Windows), `~/.cache/fc3-bevy/cache` (Linux), `~/Library/Caches/fc3-bevy/cache` (macOS) |
| `FC3_SAVE`  | Save file (also lets test runs save, which they otherwise never do) | `save.txt` beside the cache folder |

The cache is derived from your copy of the game. Keep it private and do not share or redistribute it.

## Controls

| Input            | Action                          |
|------------------|---------------------------------|
| WASD / mouse     | Move / look                     |
| Shift / Ctrl     | Sprint (through a scope: hold breath) / crouch (at a sprint: slide) |
| Space            | Jump; at a ledge, climb or vault it; in water, swim up (C swims down); in the air, open the wingsuit or parachute |
| LMB / RMB        | Fire / aim                      |
| R                | Reload                          |
| 1–8, mouse wheel, Z / X | Switch weapon (hold X: the weapon wheel, the world slowed while you pick) |
| F                | Machete; takedown when close to an unaware enemy |
| G / Q            | Throw grenade / molotov         |
| J (tap / hold)   | Throw C4 / set off every charge |
| K                | Lay a proximity mine            |
| Middle mouse     | Throw a rock (guards come to look) |
| Y (tap / hold)   | Pick / inject a syringe         |
| H                | Use medkit (or patch up when out of them and badly hurt) |
| E                | Pick up, skin, disable alarm, shop, vehicles, zip lines, radio towers, mounted guns |
| B (hold)         | Camera: tag enemies             |
| M                | Map (right click: waypoint; click a liberated outpost: fast travel) |
| Tab              | Journal: skills, crafting, syringes, stats (also in the pause menu) |
| V                | Vehicle view (chase / seat)     |
| T                | Advance time of day             |
| F4               | Original shaders on/off         |
| F1               | Controls overlay                |
| Esc              | Pause (options are in the pause menu) |

Options also set the post effects (Low: none, Medium: SMAA anti-aliasing, High: and bloom, sun shafts, screen-space ambient occlusion as the game has, about 2.5 ms a frame, and the world's own depth of field, a slight softening of what lies far off, Ultra: finer SMAA and occlusion). Keys can be rebound in Options; settings are saved to `settings.txt` beside the cache. Gamepads
work out of the box: sticks move and look, RT fire, LT aim, A jump, B crouch, X reload / interact,
Y next weapon (hold: the weapon wheel, steered with the right stick), LB grenade, D-pad down molotov, D-pad left rock (hold: lay a mine), D-pad right C4 (hold: set
it off), RB tag, L3 sprint, R3 melee, D-pad up medkit,
View map, Menu pause. Menus, the shop and the journal are driven with the D-pad or left stick and A
(B goes back); on the map the left stick pans, the triggers zoom, X sets a waypoint and A fast-travels.
Walk into a ladder to climb it (forward / back; jump lets go); jump drops
off a zip line.

## Performance

On an RTX 5070 Ti at 1600x900 with High settings, the busiest outposts run at about 90 to 105 fps
(about 58 before the changes below) and open country above 100. What keeps it there:

- Props and near terrain on the original shaders cast their shadows through proxies that share one
  material, so the shadow cascades draw them in a few batches instead of one per translated material
  (that alone took an outpost from about 60 to 90 fps; see `docs/shaders.md`).
- Grass carries its ground tint per instance, so each grass type is one material and one batch.
- Small casters drop their shadows with distance (bushes at about 40 m, trees at a couple of hundred).
- Streaming work runs off the main thread or a slice per frame: the water depth textures and the
  navigation graph build in the background; colliders for new objects (nearest first), the
  spawning of vegetation and objects, the despawning of sectors left behind and, once play is under
  way, texture and mesh uploads (24 MB a frame) are spread over frames.
- A terrain sector loads with only its coarsest mesh, all the edge of view needs; the finer ones are
  built in the background as the viewer nears, a few a frame. Each sector crossed used to send a
  whole row's three levels of detail to the GPU at once (99 meshes, about 10 MB), and keeping every
  level for every sector took 460 MB of GPU memory more than now.
- Sounds decode to samples as they load, on the loading threads. Bevy's own audio source builds its
  decoder when a sound starts, on the main thread, and for Ogg Vorbis that cost 5 to 9 ms whenever a
  few started together (sound points coming into earshot, a burst of fire). Long sounds (over about
  12 MB decoded) stay compressed and decode as they play.
- Bevy's mesh crate is vendored with one fix (`vendor/README.md`): it wrote each vertex attribute
  into the GPU staging buffer one strided write at a time, at about 1 ms per megabyte; interleaving
  in ordinary memory first halved the render thread's time for a row of sectors' meshes (about
  500 meshes, 12 MB) from 13.5 to 7 ms.
- Streamed models and the terrain's finer levels of detail hand their meshes over within a budget
  of 3 MB a frame (`upload_budget.rs`), so a row of sectors' meshes reaches the GPU over a few
  frames rather than in one: on the driving-speed route the largest frame's worth fell from 11 to
  12 MB (5.5 ms of the render thread) to 6.5 MB (4.5 ms).
  Starting out, or arriving by fast travel, there is no limit until the ground underfoot is in.
  (Gathering each frame's meshes into one staging buffer, in a patched `bevy_render`, measured
  slower than Bevy's own buffer per mesh and was dropped.)

Shader pipelines compile in the background, so a new material can appear a moment late but does not
stall the frame. Streaming spawns at most 500 vegetation instances and one sector of objects a
frame (bursts of about a thousand entities had held the main thread up for 15 to 20 ms). Crossing
the island at driving speed (with High effects: the game's bloom, ambient occlusion and depth of
field) averages about 9 ms a frame on a quiet machine, the worst frame in most five-second spans
14 to 17 ms, with the odd one of 30 to 36 ms. The busiest view on the route, a village from the
air with 60,000 mesh instances and 25 to 40 animated people in it, holds 14 to 17 ms (60 to 74
fps), bound by the render thread's work spread across shadow passes, GPU-culling buffers, skins
and queue submission rather than any one cost. Profiled, the odd slow frames are spent in the render
schedule waiting on the GPU (5 to 8 ms of it recording passes), not in streaming; the longer
stalls measured so far came with other programs loading the machine, when systems that normally
take well under a millisecond (transform propagation, atmosphere uniforms, SMAA bind groups) each
showed 15 to 30 ms runs, so they are not yet pinned on this game.
`--tour` with `FC3_TOUR_SPEED=25` drives the route. `FC3_PERF`, `FC3_GPU_TIMING`, `FC3_MESH_DEBUG` and `--features profile` (below) measure all this.

## Debug and test flags

```
--pos x,y,z        start position in game coordinates (Z up)
--yaw/--pitch deg  start view direction
--noclip           fly freely (F2 toggles in game)
--underground      with --pos, start on the floor just below the given height (inside caves)
--screenshot file  capture once streaming settles, then exit (--menu keeps the menu up)
--walk secs        scripted movement test, logging position each second
--fire-test secs   scripted shooting test against a target
--npc-test anim    one posed enemy (idle|walk|run|aim|fire|reload|death); "lineup" poses every
                   faction and role; "fight:role[:privateer]" sends three of a role at the player
                   (assault|shotgun|sniper|heavy|rocket|molotov|flamer), logging the fight;
                   "distract" / "body" check guards reacting to a noise or a fallen comrade;
                   "skirmish" pits three rebels against three pirates; "crowd" puts three guards a few
                   metres beyond a fourth (grenade takedowns); "guards" just places two
--takedown-test    sneak up on the --npc-test enemy and take it down
--outpost-test     alert the nearest outpost, let the alarm sound, clear it, log each step
--animal-test clip a row of every animal species playing a clip (idle|walk|run|eat|die); "hunt"
                   releases one tiger 30 m out (try with --npc-test guards)
--drive-test       drive the nearest car (FC3_DRIVE_KIND=boat|glider for others), logging speed;
                   try at --pos -982,-205,107 (beside a technical), and it says why if there is none;
                   a glider launches, banks left, dives and pulls up (try --pos -1746.5,-589.3,351.5),
                   FC3_DRIVE_EXIT=secs gets out that far in and logs the fall;
                   FC3_DRIVE_SEAT=passenger sits beside the driver's seat instead, weapon in hand
--patrol-test      start road and boat patrols near the player (even close by), logging their progress
--open-map         open the full map once playing; --journal / --options likewise
--shop tab         open the shop once playing on a tab (0 supplies .. 7 bows, 8 attachments)
--weapon name      start holding a weapon (e.g. "Recurve Bow")
--tour secs        soak test: fly across the island logging frame time and entity counts
--novsync          uncapped frame rate
--continue         start from the save instead of the start position
--standard-shaders render props with plain materials instead of the original shaders
```

Environment switches for tests: `FC3_WEATHER=0..4` pins the sky (clear .. storm),
`FC3_FX_TEST=explosion|explosion-air|impacts|blood|splash|fire` repeats an effect where the camera
looks, `FC3_TRAVERSAL_TEST=zip|ladder|tower` rides, climbs or takes over the nearest one (`zip_takedown`, with `FC3_SKILLS=all`, puts an enemy under a zip line and drops onto him from it),
`FC3_PEACEFUL` keeps enemies away, `FC3_GOD` makes the player take no damage (hits are logged),
`FC3_KIT_DEBUG` logs how each enemy was dressed (`FC3_KIT_PART=text` dresses each slot that has one in the part whose model names the text), `FC3_SHAKE_TEST` sets off a harmless blast 6 m ahead every 3 s and logs the view's peak shake, `FC3_DAMAGE_DEBUG` logs each hit on an enemy or animal (body part, damage taken and what each part has left), `FC3_SKILLS=all` grants every skill,
`FC3_CLIMB_TEST=boxes` (with `--walk`) sets out a crate, a fence and a wall to climb and vault, `FC3_CONVOY_TEST` sends reinforcements to the nearest outpost (log with `--patrol-test`), `FC3_KILL_TEST=head|body|hit` shoots the `--npc-test` enemy (with `FC3_TD_SHOT` screenshots),
`FC3_FREEFALL=chute|wingsuit` drops the player from 120 m and opens it (with `FC3_TD_SHOT` screenshots; the wingsuit dives, banks left and opens the parachute at `FC3_FREEFALL_HOLD` seconds, 4.5),
`FC3_BIRD_TEST` looks at the nearest bird (`=close` rides behind one, with `--noclip`), `FC3_WRECK_TEST` logs nearby vehicles' damage (`=rocket` blasts the nearest one 25 s in, with `FC3_TD_SHOT` screenshots), `FC3_VILLAGER_TEST` logs the villagers nearby (`=panic` fires a shot among them 20 s in), `FC3_MELEE_TEST` (with `--npc-test fight:<role>` and `FC3_GOD`) keeps the player at arm's length from an enemy, `FC3_EFFECTS=Low|Medium|High|Ultra` overrides the post effects setting (`FC3_SSAO=0` leaves the ambient occlusion off, `FC3_DOF=0` the depth of field), `FC3_CONCEAL_TEST` logs how hidden the player is in the plants around them, `FC3_SLIDE_TEST` sprints then slides, logging speed and eye height, `FC3_REPAIR_TEST` (try at `--pos -978,-128,102` with `FC3_PEACEFUL`) knocks the nearest vehicle down to 40% and holds E beside it, logging the repair, `FC3_NEAR_DEBUG` logs the placed objects within 15 m (entity, model and material paths) 25 s in, `FC3_TAG_TEST` (with `--npc-test guards`) holds the tag key from 6 s in, `FC3_SPOT_TEST[=kind]` (with `--noclip` near an outpost) looks at the nearest posted guard, `FC3_SPOT_DEBUG` logs the guard spots taken, `FC3_SFX_DEBUG` logs each one-shot sound event and what it resolves to, `FC3_SURFACE_DEBUG` logs the surface under each footstep, `FC3_SURFACE_TEST` probes the ground on a 2 m grid around the player 20 s in and logs the surfaces found per model, `FC3_CLIP_SOUND_DEBUG` logs each clip a rig starts with its number of sound events, `FC3_WHEEL_TEST` holds the weapon wheel open 20 s in and picks the weapon to the right, `FC3_REGION_DEBUG` logs the sound region every two seconds, `FC3_DIFFICULTY=Casual|Adventurer|Warrior|Infamous` overrides the difficulty setting, `FC3_SKY_FILL=2` sets how much the sky's fill light is boosted by day (`FC3_SKY_PROBE_GAIN=0.7` scales the sky-colour light probe; with the game's grade both come from its presets unless set), `FC3_NO_HORIZON` turns off the distant land's horizon-map shadows, `FC3_NO_FOG` the world's haze in clear weather, `FC3_GPS_DEBUG` logs how the road network for the GPS hangs together and each route planned (`FC3_WAYPOINT=x,y` sets a waypoint at Dunia x, y from the start), `FC3_FISH_TEST` puts a school of reef fish just ahead of the player, `FC3_CAGE_TEST` shoots the nearest animal cage open 20 s in and logs the predators near it, `FC3_SHAFTS=strength,threshold` tunes the sun shafts (`0` turns them off) and `FC3_SHAFTS_DEBUG` logs where the sun is on screen and their strength, `FC3_PERF` logs frame times (average, 99th percentile, worst), entity counts and the distinct materials in view (each non-bindless one draws as a batch of its own) every five seconds wherever the player is, `FC3_GPU_TIMING` logs each render pass's CPU and GPU time every ten seconds (building with `--features profile` also logs the time each system takes per frame), `FC3_NO_VEG=all|grass|mesh|tree` leaves out vegetation, `FC3_SHADOW_PROXY=0` makes the original-shader props and terrain cast their own shadows instead of through shared-material proxies, `FC3_SHADOW_LOD=0` keeps small casters casting at any distance, `FC3_NO_VISTA`, `FC3_NO_FAR` and `FC3_NO_CUTOUT` leave out the far tree billboards, the far terrain and the original-shader cut-outs (which then use the standard material), `FC3_NO_BLEND` keeps blended overlays (dirt, blood, foam) on the standard material, `FC3_ORIG_RANGE=metres` sets how near props use the original shaders (150 m), `FC3_RESOLUTION=WxH` sizes the window, `FC3_UPLOAD_MB=megabytes` sets how much texture data goes to the GPU per frame (0: no limit; `FC3_MESH_UPLOAD_MB`, 3, the same for streamed models' and the terrain's mesh data), `FC3_GAME_BLOOM=0` uses Bevy's bloom instead of the game's, `FC3_NO_SSS` leaves out the light through leaves, `FC3_GRADE=0` keeps Bevy's AgX tone mapping (and the exposure set by eye for it, and Bevy's bloom) instead of the game's colour grade (`fc3tool grade` prints the game's tone curve as the converter derives it, `FC3_GRADE_DEBUG` with it each shader variant tried), `FC3_MESH_DEBUG` logs each frame's burst of new meshes (how many, how big, where from, how long Bevy's mesh allocator took) and each growth of its GPU buffers (`FC3_MESH_SLAB_MB=min,max` sets their sizes), `FC3_TOUR_SPEED=m/s` sets the `--tour` flight speed (60), `FC3_HITCH` logs every frame over 25 ms with where the main thread's time went (physics, transforms, visibility and the rest) and what streamed in with it, `FC3_PAINT=Tribal|Urban|Tiger|Desert` paints the starting weapon if it takes that paint, `FC3_ARROWS=fire|explosive` starts with ten of each special arrow and that kind nocked, `FC3_VEHICLE_DEBUG` logs each parked vehicle as it appears (and any still waiting for the ground under it to load), `FC3_TURRET_TEST` gets on the nearest mounted gun, swings it and fires until it overheats (logging the heat,
`FC3_TD_SHOT` screenshots), `FC3_GUNNER_TEST` sends a pirate onto the nearest gun to fire at the player
(`=stay` keeps the player at `--pos`), `FC3_ARMED_TEST` parks an armed technical (`FC3_ARMED_KIND=4x4|Boat` for others) ahead and gets on its gun,
`FC3_EXPLOSIVES` starts with three C4 and three mines, and `FC3_EXPLOSIVES_TEST` (with it and `--npc-test guards`)
lays a mine, throws a C4 at the guards, sets it off and walks a survivor onto the mine, logging their health
(`=pad` does it all through a virtual gamepad's D-pad),
`FC3_AIM` holds aim down the sights, `FC3_ATTACH=all` fits every weapon with all it can take (its first sight; `=reflexb`, `=acog` and so on fit that one only), `FC3_ACTION_TEST=syringe|manual[:bullet|explosion|melee|fire|drown]|body|plant|skin[:large]` plays a
first-person action (screenshots via `FC3_TD_SHOT` as below), `FC3_NPC_DIST=metres` brings the single
`--npc-test` enemy closer, `FC3_TAKEDOWN_MODE=above[_front|_back|_left|_right]|double|front|chain|grenade|pistol|drag|below` varies `--takedown-test` (`grenade` with `--npc-test crowd` and `FC3_SKILLS=all`; `pistol` with `--npc-test crowd` and `FC3_SKILLS=front_takedown,pistol_takedown`, since `FC3_SKILLS` also takes a list; `below` with `FC3_CLIMB_TEST=boxes`, which puts a guard on the course's wall; `below_water` swims out to the nearest deep water under a guard standing just above the surface) (with `FC3_TD_SHOT=0.5,1.2` it also captures `<screenshot>_tdN.png` that long after the strike), `FC3_FP_POSE=idle|iron|shot|reload|draw|sprint|walk`
(with `FC3_FP_TIME=seconds`) holds a first-person pose, `FC3_NAV_TEST` drives the menus with a virtual
gamepad, and `FC3_FIRE_DEBUG` logs shots and projectile hits.

## Layout

| Crate         | Contents |
|---------------|----------|
| `fc3_formats` | Readers for the game's formats: archives, binary objects, models, materials, textures, terrain, world sectors, vegetation, skeletons and animation, sound banks. Also the cache file formats. |
| `fc3_convert` | The one-time converter (library plus the `fc3convert` CLI). It is parallel, incremental, writes files atomically and checks free disk space. |
| `fc3_game`    | The Bevy game. |
| `fc3_tool`    | Exploration CLIs (`fc3tool`, `fc3anim`, `fc3audio`, `fc3veg`). |
| `fc3_domino`  | Embedded Lua 5.1 runtime for the original mission graphs. |
| `fc3_story`   | Mission management, native services and headless campaign state. |
| `fc3_bink`    | Bink 1 video and audio decoding. |

The `docs/` folder describes the reverse-engineered formats.
