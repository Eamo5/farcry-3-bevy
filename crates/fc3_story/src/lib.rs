//! Far Cry 3's story: the mission system around the game's own Domino
//! graphs.
//!
//! [`Story`] runs the graphs (on `fc3_domino`) with the engine services they
//! call implemented here: callbacks and timers, the message bus, the mission
//! manager with its blocks and layers, safehouses, and the queries scripts
//! make of the world. It knows the world through the data the converter put
//! in the cache ([`data::StoryData`]) and a [`View`] the host refreshes each
//! frame; what the world should do comes back as [`Command`]s, and what
//! happens in it goes in as events ([`Story::entity_event`] and friends).
//! Nothing here depends on the renderer, so the same story runs headless
//! (`fc3tool story-run`) and in the game.

pub mod core;
pub mod data;
pub mod missions;
mod natives;
pub mod save;
pub mod triggers;
pub mod travel;
pub mod vehicles;
pub mod values;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use anyhow::Result;
use fc3_domino::{Domino, ScriptSource};
use fc3_formats::scripts::{ScriptPack, DOMINO_PACK_REL};
use mlua::Table;

pub use crate::core::LayerState;
use crate::core::{Action, Callback, Core, Listen};
use crate::data::{InputValue, StoryData};
pub use crate::values::Arg;

/// The entity id scripts know the player by.
pub const PLAYER: u64 = 1;

/// `VehicleDamageListener`'s damage states: destroyed, and mint (whole).
pub(crate) const VEHICLE_DESTROYED: f64 = 0.0;
pub(crate) const VEHICLE_MINT: f64 = 5.0;

/// The mark on ids the story makes for what it spawns (reinforcements,
/// archetypes placed by scripts). They count up from the start of each
/// session, so saves keep none of them.
pub const SPAWNED_IDS: u64 = 0x7FFE_0000_0000_0000;

/// Whether an id is one the story made this session.
pub fn spawned_id(id: u64) -> bool {
    id & SPAWNED_IDS == SPAWNED_IDS
}

/// What a companion was told to do (`CAPI_Companion`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Companion {
    /// Follow the player.
    Follow,
    /// Walk a path (a shape entity's points), waiting while the player is
    /// further off than `distance`; `Domino_CompanionLead` at its end.
    Lead { path: u64, distance: f32 },
    /// Stay where it is.
    Wait,
}

/// A story entity the host has in the world, as it is now.
#[derive(Debug, Clone, Copy, Default)]
pub struct Live {
    /// Dunia space.
    pub pos: [f32; 3],
    pub alive: bool,
    pub health: f32,
    /// Its health when whole (a vehicle's health listeners hear both).
    pub max_health: f32,
    /// AI state: 0 relaxed or cautious, 1 investigating, 2 alert, 3 in
    /// combat.
    pub ai_state: u8,
}

/// An inventory item as scripts ask about it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    /// The item's name (`FC3/Machete`, `Syringe_Heal`...).
    pub name: String,
    /// Weapon class or gadget kind.
    pub class: String,
    /// Category (`binoculars`, `assault`, ...).
    pub category: String,
    pub count: u32,
}

/// The player as scripts ask about them.
#[derive(Debug, Clone, Default)]
pub struct PlayerView {
    /// In a QTE (scripts keep their action maps through one).
    pub in_qte: bool,
    pub health: f32,
    pub max_health: f32,
    pub money: u32,
    pub medpacks: u32,
    /// 0 standing, 1 crouching, 2 swimming.
    pub stance: u8,
    pub vehicle: Option<u64>,
    pub seat: i32,
    pub seat_index: i32,
    pub vehicle_type: i32,
    /// Degrees (pitch, roll, yaw).
    pub look_angles: [f32; 3],
    pub weapon_slots: u32,
    pub weapons: Vec<Item>,
    pub gadgets: Vec<Item>,
    /// Carried loot counts, keyed by the item's localized name id (not CRC).
    pub loot: HashMap<u32, u32>,
    /// The name of what is in the player's hands.
    pub in_hands: Option<String>,
    /// The gadget raised in the player's hands (the camera), if any.
    pub gadget: Option<Item>,
    /// Unlocked skills (lower-case names).
    pub skills: HashSet<String>,
    pub on_zipline: bool,
    /// How near the enemies are to spotting the player (the detection
    /// meter): 0 unnoticed .. 1 spotted.
    pub stealth: f32,
}

/// What the scripts may read of the world, refreshed by the host.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub player_state: PlayerView,
    /// Entities the player has tagged.
    pub tagged: HashSet<u64>,
    /// 0 easy .. 3 hardest.
    pub difficulty: u8,
    /// Entities inside each trigger the host evaluates (by trigger id).
    pub contacts: HashMap<u64, Vec<u64>>,
    /// The player's position, Dunia space (Z up).
    pub player: [f32; 3],
    /// Heading in degrees (Dunia yaw).
    pub player_yaw: f32,
    /// Where the player's view looks (a unit vector, Dunia space) and its
    /// vertical field of view in degrees, for look-at triggers.
    pub look: [f32; 3],
    pub fov: f32,
    pub player_dead: bool,
    pub player_in_vehicle: bool,
    /// Story entities the host has spawned (NPCs, props), by id.
    pub spawned: HashMap<u64, Live>,
    /// Physical NPC occupants; script seat requests alone do not populate this.
    pub pawn_vehicles: HashMap<u64, vehicles::Occupant>,
    /// Hours (0..24).
    pub time_of_day: f32,
}

/// A mission timer the host should display, sampled from the story clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Timer {
    pub id: u64,
    pub label: String,
    pub seconds: f64,
    pub increasing: bool,
    pub append_time: bool,
}

/// An entity's world-space pose in Dunia coordinates (Z up, degrees).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub position: [f32; 3],
    pub angles: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseTarget {
    pub entity: u64,
    pub position: [f32; 3],
    pub text: String,
}

/// How near (m) a usable pawn is used, within how wide a look (the cosine
/// of half the angle), and its prompt (the game's "Interact").
const PAWN_USE_REACH: f32 = 2.5;
const PAWN_USE_COS: f32 = 0.6;
const PAWN_USE_TEXT: u32 = 89_343;

/// Something the world should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A mission layer's entities appear, go, or stay without respawning.
    Layer {
        layer: u64,
        state: LayerState,
    },
    /// A mission's (or block's) state changed.
    MissionState {
        name: String,
        state: String,
        block: Option<u64>,
    },
    /// The current objective.
    Objective {
        text: Option<String>,
        id: Option<u32>,
        play_complete: bool,
    },
    /// A mission name shown as it starts.
    MissionTitle {
        title: Option<String>,
        description: Option<String>,
    },
    MissionComplete {
        name: String,
        title: Option<String>,
        text: Option<String>,
        xp: u32,
        money: u32,
        screen: bool,
    },
    /// A failed mission; with `reload`, back to the last checkpoint.
    MissionFailed {
        text: Option<String>,
        reload: bool,
    },
    /// Save progress here.
    Checkpoint,
    /// Where the player respawns from now on (a spawn point).
    RespawnPoint(u64),
    /// Debug text on screen.
    Print(String),
    /// The loading screen may go (`SendPlayGame`).
    PlayGame,
    /// A script command to an entity's component (`CDoor_Enable`, ...).
    EntityEvent {
        entity: u64,
        event: String,
    },
    /// An entity removed by a script.
    Remove(u64),
    /// A script moved an entity to another entity's current pose. The pose
    /// is resolved when the native runs, before later commands can move it.
    Teleport {
        entity: u64,
        pose: Pose,
    },
    /// A safehouse or outpost unlocked / locked.
    SafeHouse {
        id: u64,
        unlocked: bool,
        silent: bool,
    },
    /// A message on the bus (for host systems that react to them).
    Message(String),
    /// A character says a line (voice by sound id, its subtitle, how long).
    Dialog {
        speaker: u64,
        sound: u32,
        target: Option<u64>,
        subtitle: Option<String>,
        seconds: f32,
    },
    /// A character stops talking.
    StopDialog {
        speaker: u64,
    },
    /// A character goes to a contextual action's spot and does it (report
    /// `Story::action_started` there, `Story::action_finished` when done),
    /// or stops (`None`).
    Action {
        pawn: u64,
        action: Option<u64>,
    },
    /// Anything else a script asked for that the host may act on: the
    /// native's name and its arguments as text.
    Native {
        name: String,
        args: Vec<String>,
    },
}

struct Pack(ScriptPack);

impl ScriptSource for Pack {
    fn script(&self, path: &str) -> Option<String> {
        self.0.get(path)
    }
}

/// The story's state and the Lua running its graphs.
pub struct Story {
    domino: Domino,
    core: Rc<RefCell<Core>>,
    /// Graph instances by the entity they run on.
    instances: HashMap<u64, Table>,
    /// Start-on-load graphs started (by entity).
    loaded_scripts: HashSet<u64>,
    /// Start-on-load graphs kept running when their entity's sector goes
    /// (a race's controller while the race drives far from it).
    held_scripts: HashSet<u64>,
    errors: usize,
}

impl Story {
    /// Loads the scripts and story data from a cache.
    pub fn open(cache: &Path) -> Result<Story> {
        let pack = ScriptPack::from_bytes(&std::fs::read(cache.join(DOMINO_PACK_REL))?)?;
        let data = StoryData::load(cache)?;
        let gameplays = std::fs::read(cache.join(fc3_formats::gsf::CACHE_REL)).unwrap_or_default();
        let standing = standing_layers(&pack, &data, &gameplays);
        let story = Story::new(pack, data)?;
        story.core.borrow_mut().standing = standing;
        Ok(story)
    }

    pub fn new(pack: ScriptPack, data: StoryData) -> Result<Story> {
        let domino = Domino::new(Pack(pack)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let core = Rc::new(RefCell::new(Core::new(Rc::new(data))));
        natives::register(&domino, &core).map_err(|e| anyhow::anyhow!("{e}"))?;
        domino.lua().load(include_str!("glue.lua")).set_name("glue").exec().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Story { domino, core, instances: HashMap::new(), loaded_scripts: HashSet::new(), held_scripts: HashSet::new(), errors: 0 })
    }

    pub fn data(&self) -> Rc<StoryData> {
        self.core.borrow().data.clone()
    }

    pub fn domino(&self) -> &Domino {
        &self.domino
    }

    /// The archetype behind an inventory entity id returned to a script.
    /// Commands also accept archetype names directly.
    pub fn item_name(&self, name_or_id: &str) -> String {
        name_or_id.parse::<u64>().ok().and_then(|id| self.core.borrow().item_ids.get(&id).cloned()).unwrap_or_else(|| name_or_id.to_string())
    }

    /// Visible named timers, in registration order. Hidden timers still run
    /// and deliver their callbacks, but take no space in the HUD.
    /// The HUD's objective timer and gauge come first (ids 0 and 1).
    pub fn timers(&self) -> Vec<Timer> {
        let c = self.core.borrow();
        let objective = [(0, &c.media.objective_timer), (1, &c.media.gage_timer)].into_iter().filter_map(|(id, t)| {
            let t = t.as_ref()?;
            let label = t.text.and_then(|s| c.data.strings.get(s)).unwrap_or_default().to_string();
            Some(Timer { id, label, seconds: t.left, increasing: false, append_time: true })
        });
        let named = c.media.timers.iter().filter(|(_, t)| t.shown).map(|(&id, t)| Timer {
            id, label: t.label.clone(), seconds: t.value,
            increasing: t.increasing, append_time: t.append_time,
        });
        objective.chain(named).collect()
    }

    /// Starts a new game: the blocks active from the start, then the master
    /// graph.
    pub fn boot(&mut self) {
        {
            let mut c = self.core.borrow_mut();
            let init = c.data.active_on_init.clone();
            for b in init {
                c.set_block(b, LayerState::Active);
            }
        }
        self.start_root();
    }

    /// Starts from a save: its world state first, then the master graph,
    /// whose missions take their saved states as they register.
    pub fn boot_saved(&mut self, save: &save::StorySave) {
        self.restore(save);
        self.start_root();
    }

    fn restore(&mut self, s: &save::StorySave) {
        {
            let mut c = self.core.borrow_mut();
            c.missions.saved.missions = s.missions.iter().map(|(k, st, ack)| (k.clone(), (missions::State::parse(st), *ack))).collect();
            for &(l, st) in &s.layers {
                if let Some(state) = LayerState::from_code(st as i64) {
                    c.set_layer(l, state);
                }
            }
            for &(b, st) in &s.blocks {
                if let Some(state) = LayerState::from_code(st as i64) {
                    c.blocks.insert(b, state);
                }
            }
            c.safehouses_unlocked = s.safehouses.iter().copied().collect();
            c.radio_towers_unlocked = s.radio_towers.iter().copied().collect();
            c.unlocked_tags = s.tags.iter().cloned().collect();
            c.removed = s.removed.iter().copied().collect();
            c.dead = s.dead.iter().copied().collect();
            c.missions.chapter = s.chapter;
            c.respawn = s.respawn;
        }
        let lua = self.domino.lua();
        if let Ok(globals) = lua.globals().get::<Table>("PersistentGlobals") {
            if let Err(e) = save::load_table(lua, &globals, &s.persistent) {
                self.core.borrow_mut().warn(format!("restoring PersistentGlobals: {e}"));
            }
        }
    }

    /// The story's progress, for a save.
    pub fn save(&self) -> save::StorySave {
        let c = self.core.borrow();
        let mut missions: Vec<(String, String, bool)> = c
            .missions
            .by_id
            .values()
            .map(|m| {
                let key = match &m.block {
                    Some(b) => b.block.to_string(),
                    None => m.name.clone(),
                };
                (key, m.state.name().to_string(), m.acknowledged)
            })
            .collect();
        // What an earlier save knew that hasn't registered this time.
        for (k, (st, ack)) in &c.missions.saved.missions {
            if !missions.iter().any(|m| &m.0 == k) {
                missions.push((k.clone(), st.name().to_string(), *ack));
            }
        }
        missions.sort();
        let mut layers: Vec<(u64, u8)> = c.layers.iter().map(|(l, s)| (*l, *s as u8)).collect();
        layers.sort_unstable();
        let mut blocks: Vec<(u64, u8)> = c.blocks.iter().map(|(b, s)| (*b, *s as u8)).collect();
        blocks.sort_unstable();
        let sorted = |set: &HashSet<u64>| {
            let mut v: Vec<u64> = set.iter().copied().collect();
            v.sort_unstable();
            v
        };
        let mut tags: Vec<String> = c.unlocked_tags.iter().cloned().collect();
        tags.sort();
        let persistent = self.domino.lua().globals().get::<Table>("PersistentGlobals").map(|t| save::dump_table(&t)).unwrap_or_default();
        save::StorySave {
            missions,
            layers,
            blocks,
            safehouses: sorted(&c.safehouses_unlocked),
            radio_towers: sorted(&c.radio_towers_unlocked),
            tags,
            removed: sorted(&c.removed),
            dead: sorted(&c.dead),
            chapter: c.missions.chapter,
            respawn: c.respawn,
            persistent,
        }
    }

    /// Where the player comes back after a reload (a spawn point), if a
    /// checkpoint set one.
    pub fn respawn_point(&self) -> Option<u64> {
        self.core.borrow().respawn
    }

    fn start_root(&mut self) {
        {
            let mut c = self.core.borrow_mut();
            match c.data.root {
                Some(root) => {
                    c.start_entity_script(root);
                }
                None => c.warn("no entity runs the master graph"),
            }
        }
        self.pump();
        let mut c = self.core.borrow_mut();
        c.game_ready = true;
        c.raise(&Listen::Generic("DominoGameReady".into()), vec![]);
        drop(c);
        self.pump();
    }

    /// Advances the story by `dt` seconds with the world as `view` shows it;
    /// returns what the world should do.
    pub fn update(&mut self, dt: f32, view: View) -> Vec<Command> {
        let previous_occupants = vehicles::occupants(&self.core.borrow().view);
        let gadget_change = {
            let was = &self.core.borrow().view.player_state.gadget;
            (*was != view.player_state.gadget).then(|| (was.clone(), view.player_state.gadget.clone()))
        };
        {
            let mut c = self.core.borrow_mut();
            // Inventory entities have no world placement. Their availability
            // changes when the host reports a weapon/gadget was actually
            // given or removed, including subscriptions made before it arrived.
            let watched: HashSet<u64> = c.listeners.values().filter_map(|l| match l.on {
                Listen::Spawn(id) | Listen::Remove(id) if c.item_ids.contains_key(&id) => Some(id),
                _ => None,
            }).collect();
            let before: Vec<(u64, bool)> = watched.into_iter().map(|id| (id, c.is_loaded(id))).collect();
            let vehicles_before = c.watched_vehicles();
            // Stance changes are events on the player.
            let stance = view.player_state.stance;
            if c.view.player_state.stance != stance && c.time > 0.0 {
                let event = ["new_stance_stand", "new_stance_crouch", "new_stance_swim"][(stance as usize).min(2)];
                c.raise(&Listen::Entity(PLAYER, event.into()), vec![Arg::id(PLAYER)]);
            }
            // So are the detection meter's (`CStealthIndicatorEvent`, with its
            // new value, to hundredths).
            let meter = |v: f32| (v.clamp(0.0, 1.0) as f64 * 100.0).round() / 100.0;
            let stealth = meter(view.player_state.stealth);
            if meter(c.view.player_state.stealth) != stealth && c.time > 0.0 {
                c.raise(&Listen::Entity(PLAYER, "CStealthIndicatorEvent".into()), vec![Arg::Num(stealth)]);
            }
            // A watched vehicle's health changing (`OnVehicleHealthChanged`:
            // its health and its full health; JAS_130 B50's pirates bail out
            // of a technical shot down to 900). Told in its own data's terms
            // where it has them (a technical's 1000): the host's own figures
            // for how much a vehicle takes aren't the game's.
            let health_watched: std::collections::BTreeSet<u64> = c
                .listeners
                .values()
                .filter_map(|l| match &l.on {
                    Listen::Entity(id, e) if e == "OnVehicleHealthChanged" => Some(*id),
                    _ => None,
                })
                .collect();
            for id in health_watched {
                let (Some(was), Some(now)) = (c.view.spawned.get(&id), view.spawned.get(&id)) else { continue };
                if (was.health - now.health).abs() > 1e-3 {
                    let (health, full) = match c.data.entities.get(&id).and_then(vehicles::max_health) {
                        Some(full) if now.max_health > 0.0 => (now.health / now.max_health * full, full),
                        _ => (now.health, now.max_health.max(now.health)),
                    };
                    let args = vec![Arg::Num(health as f64), Arg::Num(full as f64)];
                    c.raise(&Listen::Entity(id, "OnVehicleHealthChanged".into()), args);
                }
            }
            c.view = view;
            for (id, was_loaded) in before {
                let loaded = c.is_loaded(id);
                if loaded != was_loaded {
                    c.raise(&if loaded { Listen::Spawn(id) } else { Listen::Remove(id) }, vec![]);
                }
            }
            c.group_state_events();
            // A watched vehicle wrecked: its damage state changes to destroyed.
            for (id, was_dead) in vehicles_before {
                if !was_dead && c.is_dead(id) {
                    c.raise(&Listen::Entity(id, "OnStateChange".into()), vec![Arg::Num(VEHICLE_DESTROYED)]);
                }
            }
            c.time += dt as f64;
            // Timers.
            let mut due = Vec::new();
            for (id, d) in c.delays.iter_mut() {
                if d.running {
                    d.elapsed += dt as f64;
                    if d.elapsed >= d.seconds {
                        d.running = false;
                        due.push((*id, d.callback.clone()));
                    }
                }
            }
            for (_, cb) in due {
                c.call(cb, vec![]);
            }
            let ticks: Vec<Callback> = c.ticks.values().cloned().collect();
            for cb in ticks {
                c.call(cb, vec![Arg::Num(dt as f64)]);
            }
            c.update_media(dt as f64);
            c.update_dialogs(dt as f64);
            c.update_moves(dt as f64);
            c.cooled_actions();
            c.offer_enabled_actions();
            // The world streamed in round a teleported player.
            if c.world_ready_at.is_some_and(|at| c.time >= at) {
                c.world_ready_at = None;
                c.raise(&Listen::Generic("DominoGameReady".into()), vec![]);
            }
            // The player's pawn switched (`CAPI_Player.SwitchPlayerPawn`).
            if c.pawn_ready_at.is_some_and(|at| c.time >= at) {
                c.pawn_ready_at = None;
                c.raise(&Listen::Generic("LocalPlayerRespawnedReady".into()), vec![]);
            }
            natives::world::update_triggers(&mut c);
        }
        // A gadget raised or put away: the player's groups hear it drawn or
        // holstered, with its name and category ids.
        if let Some((old, new)) = gadget_change {
            let ids = |g: &Item| {
                let id = |s: &str| Arg::Num(fc3_formats::hash::crc32(s.to_ascii_lowercase().as_bytes()) as f64);
                vec![Arg::id(PLAYER), id(&g.name), id(&g.category)]
            };
            if let Some(g) = old {
                self.group_event(PLAYER, "CGadgetHolsteredEvent", ids(&g));
            }
            if let Some(g) = new {
                self.group_event(PLAYER, "CGadgetDrawEvent", ids(&g));
            }
        }
        self.pump();
        self.vehicle_changes(previous_occupants);
        self.take_commands()
    }

    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.core.borrow_mut().out)
    }

    /// Log lines from the runtime and the scripts since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut self.core.borrow_mut().log);
        out.extend(self.domino.take_log());
        out
    }

    /// Natives scripts called that aren't provided.
    pub fn take_unknown_calls(&self) -> Vec<String> {
        self.domino.take_unknown_calls()
    }

    /// An event an entity raises (`Enter`, `Die`, `Domino_DialogFinished`,
    /// ...), for the scripts listening on it.
    pub fn entity_event(&mut self, entity: u64, event: &str, args: Vec<Arg>) {
        self.core.borrow_mut().raise(&Listen::Entity(entity, event.to_string()), args);
        self.pump();
    }

    /// An alarm box changed state (1 set off, 0 quiet again, 2 switched off,
    /// 3 destroyed) by `user` (the guard or the player): the groups it is in
    /// hear `Alarm_Activated` / `..Deactivated` / `..Disabled` /
    /// `..Destroyed` with the user and the alarm, and
    /// `CAPI_Entity.GetAlarmState` tells it from now on.
    pub fn alarm_event(&mut self, alarm: u64, state: u8, user: u64) {
        self.core.borrow_mut().alarm_event(alarm, state, user);
        self.pump();
    }

    /// An event about an entity for the scripts listening on the groups it
    /// belongs to (`Die`, `CCounterEvent`, `CA_End`, `Alarm_Activated`,
    /// ...): callbacks get the event name, then `args`.
    pub fn group_event(&mut self, entity: u64, event: &str, args: Vec<Arg>) {
        self.core.borrow_mut().group_event(entity, event, args);
        self.pump();
    }

    /// The world sectors of the mission zones being played now (the
    /// world's own encounters keep out of them).
    pub fn active_zone_sectors(&self) -> HashSet<u32> {
        let c = self.core.borrow();
        c.missions
            .by_id
            .values()
            .filter(|m| m.state == missions::State::Running)
            .filter_map(|m| m.block.as_ref())
            .filter_map(|b| c.data.blocks.get(&b.block))
            .flat_map(|b| b.zone_sectors.iter().copied())
            .collect()
    }

    /// The objective markers shown now, where they are (Dunia space): (marker
    /// entity, position).
    pub fn objective_markers(&self) -> Vec<(u64, [f32; 3])> {
        let c = self.core.borrow();
        c.objective_markers.keys().filter_map(|&m| c.position(m).map(|p| (m, p))).collect()
    }

    /// The members of a group as scripts write it (`#hash`, or one id).
    pub fn group_members(&self, group: &str) -> Vec<u64> {
        self.core.borrow().groups(&values::Text(group.to_string()))
    }

    /// Whether an entity is in the world as the story sees it (its layer
    /// present, not removed by a script).
    pub fn is_loaded(&self, id: u64) -> bool {
        self.core.borrow().is_loaded(id)
    }

    /// A mission block by its name (case aside): its id and its zone (the
    /// entity whose script the block runs).
    pub fn block_named(&self, name: &str) -> Option<(u64, Option<u64>)> {
        let c = self.core.borrow();
        c.data.blocks.values().find(|b| b.name.eq_ignore_ascii_case(name)).map(|b| (b.id, b.zone))
    }

    /// Sets a mission block's state from the host (a trial's own block, as
    /// the engine did when a trial began and ended): its layers with it;
    /// activating it starts its zone's script, putting it away stops it.
    pub fn set_block_state(&mut self, block: u64, state: LayerState) {
        self.core.borrow_mut().set_block(block, state);
        self.pump();
    }

    /// Keeps an entity's start-on-load graph running while its sector is out
    /// of the world (or lets it go with it again): a race's controller, its
    /// gameplay's owner, while the race drives far from it, as the engine
    /// kept a gameplay going whoever started it.
    pub fn hold_script(&mut self, owner: u64, hold: bool) {
        if hold {
            self.held_scripts.insert(owner);
        } else {
            self.held_scripts.remove(&owner);
        }
    }

    /// The pawns scripts have made usable, where they are, and whether a
    /// `Use` is listened for on them.
    pub fn usable_pawns(&self) -> Vec<(u64, Option<[f32; 3]>, bool)> {
        let c = self.core.borrow();
        c.usable_pawns
            .keys()
            .map(|&id| (id, c.position(id), c.listeners.values().any(|l| matches!(&l.on, Listen::Entity(e, ev) if *e == id && ev == "Use"))))
            .collect()
    }

    /// The nearest usable mission trigger within reach, respecting the
    /// script's use/inside/look/angle restrictions and current placement.
    pub fn use_target(&self, player: [f32; 3], eye: [f32; 3], look: [f32; 3]) -> Option<UseTarget> {
        let c = self.core.borrow();
        let ids: std::collections::BTreeSet<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(id, event) if event == "Use" => Some(*id),
                _ => None,
            })
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                // A pawn made usable: used standing by it (and facing it,
                // unless the script lets it be used any way round).
                if let Some(&any_way) = c.usable_pawns.get(&id) {
                    if !c.is_loaded(id) || c.dead.contains(&id) {
                        return None;
                    }
                    let at = c.position(id)?;
                    let (dx, dy) = (at[0] - player[0], at[1] - player[1]);
                    let flat = dx.hypot(dy);
                    let facing = any_way || flat < 0.5 || (dx * look[0] + dy * look[1]) / (flat * look[0].hypot(look[1]).max(1e-3)) > PAWN_USE_COS;
                    let near = flat < PAWN_USE_REACH && (at[2] - player[2]).abs() < 2.0;
                    let text = c.data.strings.get(PAWN_USE_TEXT).map_or_else(|| "Interact".to_string(), str::to_string);
                    return (near && facing).then_some(UseTarget { entity: id, position: at, text });
                }
                if !c.is_loaded(id) || !c.trigger_on(id) {
                    return None;
                }
                // Spawned from an archetype (a bomb's spot): used standing by it.
                if let Some(archetype) = c.spawned.get(&id).and_then(|name| c.data.archetype_named(name)) {
                    let at = c.position(id)?;
                    let near = (at[0] - player[0]).hypot(at[1] - player[1]) < 1.6 && (at[2] - player[2]).abs() < 2.0;
                    let text = ["CUsableComponent", "CPickupMissionItem", "CTriggerComponent"]
                        .iter()
                        .filter_map(|c| archetype.component(c))
                        .find_map(|comp| comp.u32("locidDisplayString").filter(|&id| id != 0 && id != u32::MAX))
                        .and_then(|loc| c.data.strings.get(loc))
                        .map_or_else(|| "Interact".to_string(), str::to_string);
                    return near.then_some(UseTarget { entity: id, position: at, text });
                }
                let mut e = c.data.entities.get(&id)?.clone();
                if let Some(pose) = c.pose(id) {
                    e.pos = pose.position;
                    e.angles = pose.angles;
                }
                if !triggers::can_use(&e, c.use_rules.get(&id).copied().unwrap_or_default(), player, eye, look) {
                    return None;
                }
                let prox = e.component("CProximityTriggerComponent")?;
                let text = prox
                    .u32("locidDisplayInteractionString")
                    .and_then(|id| c.data.strings.get(id))
                    .map(str::to_string)
                    .or_else(|| prox.str("sUsageString").filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| "Interact".into());
                Some(UseTarget { entity: id, position: e.pos, text })
            })
            .min_by(|a, b| {
                let distance = |p: [f32; 3]| (0..3).map(|i| (p[i] - player[i]).powi(2)).sum::<f32>();
                distance(a.position).total_cmp(&distance(b.position))
            })
    }

    /// The entities scripts have removed (saves keep them).
    pub fn removed(&self) -> HashSet<u64> {
        self.core.borrow().removed.clone()
    }

    /// A character died (killed by the player unless `by` says otherwise):
    /// the `Die` event for every group it is in, and it counts as dead from
    /// now on.
    pub fn killed(&mut self, id: u64) {
        if !self.core.borrow_mut().dead.insert(id) { return; }
        {
            let mut c = self.core.borrow_mut();
            // (A scene's lead killed at it: its other part stops too.)
            if let Some(action) = c.ca.current.get(&id).map(|cur| cur.action) {
                c.release_sync_partners(action, true);
            }
            let groups: std::collections::BTreeSet<String> = c.listeners.values().filter_map(|l| match &l.on {
                Listen::Group(g, event) if event == "CGroupSizeEvent" || event == "CGroupCombatEvent" => Some(g.clone()),
                _ => None,
            }).collect();
            for group in groups {
                let members = c.groups(&values::Text(group.clone()));
                if !members.contains(&id) { continue; }
                let count = members.iter().filter(|&&e| c.is_loaded(e) && !c.dead.contains(&e)).count();
                c.raise(&Listen::Group(group.clone(), "CGroupSizeEvent".into()), vec![
                    Arg::Str("CGroupSizeEvent".into()), Arg::Str("MemberRemoved".into()), Arg::Num(count as f64),
                ]);
                let dead = members.iter().filter(|&&e| c.is_loaded(e) && c.is_dead(e)).count();
                if dead == 1 {
                    c.raise(&Listen::Group(group.clone(), "CGroupCombatEvent".into()), vec![
                        Arg::Str("CGroupCombatEvent".into()), Arg::Str("DeadStart".into()), Arg::Num(dead as f64),
                    ]);
                }
                c.raise(&Listen::Group(group, "CGroupCombatEvent".into()), vec![
                    Arg::Str("CGroupCombatEvent".into()), Arg::Str("DeadMemberIn".into()), Arg::Num(dead as f64),
                ]);
            }
        }
        self.group_event(id, "Die", vec![Arg::id(PLAYER), Arg::Num(0.0), Arg::id(id), Arg::Str(String::new())]);
    }

    /// A pawn's health went from `old` to `new` (hurt by `by`): the groups it
    /// is in hear `CCounterEvent` on its health counter, as `HealthListener`
    /// boxes read it (counter, old, new, instigator, source type, victim,
    /// emitter: `BulletImpact` and the gun, `arrow`...). A round the player
    /// fired with no emitter given came from the weapon in their hands (a
    /// hunt's weapon check: `GetWeaponClassFromName`).
    pub fn health_changed(&mut self, pawn: u64, old: f32, new: f32, by: u64, source: &str, emitter: Option<&str>) {
        if old == new {
            return;
        }
        let counter = fc3_formats::hash::crc32(b"health") as f64;
        let emitter = emitter.map(str::to_string).unwrap_or_else(|| {
            let c = self.core.borrow();
            (by == PLAYER && source == "BulletImpact").then(|| c.view.player_state.in_hands.clone()).flatten().unwrap_or_default()
        });
        self.group_event(
            pawn,
            "CCounterEvent",
            vec![Arg::Num(counter), Arg::Num(old as f64), Arg::Num(new as f64), Arg::id(by), Arg::Str(source.to_string()), Arg::id(pawn), Arg::Str(emitter)],
        );
    }

    /// A character reached its contextual action and began it.
    pub fn action_started(&mut self, pawn: u64) {
        {
            let mut c = self.core.borrow_mut();
            let Some(cur) = c.ca.current.get_mut(&pawn) else { return };
            if cur.started {
                return;
            }
            cur.started = true;
            let action = cur.action;
            c.action_event(action, "CA_Start", pawn);
            // (Its markups, for the scripts listening for them: when its
            // sequence has them, else a while in.)
            let text = action.to_string();
            let markups: Vec<(f64, String)> =
                c.data.action_markups.get(&action).cloned().unwrap_or_else(|| natives::ai::MARKUP_SECS.iter().map(|(s, e)| (*s, e.to_string())).collect());
            for (secs, event) in markups {
                if c.listeners.values().any(|l| matches!(&l.on, Listen::Group(g, e) if *g == text && *e == event)) {
                    c.action_markups.push((secs, action, pawn, event));
                }
            }
        }
        self.pump();
    }

    /// A character finished its contextual action.
    pub fn action_finished(&mut self, pawn: u64) {
        {
            let mut c = self.core.borrow_mut();
            let Some(cur) = c.ca.current.remove(&pawn) else { return };
            c.action_event(cur.action, "CA_End", pawn);
            c.release_sync_partners(cur.action, false);
            c.take_up_action(pawn, cur.action);
        }
        self.pump();
    }

    /// A character's AI broke off its contextual action (it heard or saw
    /// something).
    pub fn action_interrupted(&mut self, pawn: u64) {
        {
            let mut c = self.core.borrow_mut();
            let Some(cur) = c.ca.current.remove(&pawn) else { return };
            c.action_event(cur.action, "CA_Interrupt", pawn);
            c.release_sync_partners(cur.action, true);
        }
        self.pump();
    }

    /// The specific gameplays running (minigames, QTEs): (id, name).
    pub fn gameplays(&self) -> Vec<(u64, String)> {
        self.core.borrow().gameplays.iter().map(|(i, n)| (*i, n.clone())).collect()
    }

    /// A tree the host is growing (`SetRealTreeGrowthSpeed`'s id) has
    /// reached its size.
    pub fn growth_finished(&mut self, id: u64) {
        {
            let mut c = self.core.borrow_mut();
            if let Some(Some(cb)) = c.growths.remove(&id) {
                c.call(cb, vec![]);
            }
        }
        self.pump();
    }

    /// A specific gameplay the host ran has ended.
    /// A specific gameplay plays a QTE move (the host's QTE runner's).
    pub fn is_qte_gameplay(&self, id: u64) -> bool {
        self.core.borrow().qte_gameplays.contains(&id)
    }

    pub fn gameplay_ended(&mut self, id: u64) {
        self.core.borrow_mut().end_gameplay(id);
        self.pump();
    }

    /// Whether a character is saying a scripted line.
    pub fn is_speaking(&self, id: u64) -> bool {
        self.core.borrow().dialogs.speaking.contains_key(&id)
    }

    /// Whether a character has died (or was removed).
    pub fn is_dead(&self, id: u64) -> bool {
        self.core.borrow().is_dead(id)
    }

    /// A global event (`PlayGame`, UI page events, ...).
    pub fn generic_event(&mut self, event: &str, args: Vec<Arg>) {
        self.core.borrow_mut().raise(&Listen::Generic(event.to_string()), args);
        self.pump();
    }

    /// A message on the bus, as if a script broadcast it.
    pub fn message(&mut self, message: &str) {
        self.core.borrow_mut().raise(&Listen::Message(message.to_string()), vec![]);
        self.pump();
    }

    /// The weapon a running hunt asks its quarry be killed with (its animal
    /// quest box's `WeaponClass`: `bow`, `shotgun`, `machete`...), if one is.
    pub fn hunt_weapon(&self) -> Option<String> {
        let c = self.core.borrow();
        // (Its listeners are its own boxes': up their graphs to it.)
        c.listeners.values().find_map(|l| {
            let mut table = l.callback.target.clone();
            for _ in 0..16 {
                let path = table.get::<mlua::Table>("_type").and_then(|ty| ty.get::<String>("_path")).unwrap_or_default();
                if path.to_ascii_lowercase().contains("animalsidequestb20lib") {
                    return table.get::<String>("WeaponClass").ok().filter(|w| !w.is_empty());
                }
                let parent = table.get::<mlua::Table>("_graph").ok()?;
                if parent == table {
                    return None;
                }
                table = parent;
            }
            None
        })
    }

    /// The safe houses the player may hold (outposts' and towns'): their
    /// ids, places (Dunia) and whether each is the player's.
    pub fn safe_houses(&self) -> Vec<(u64, [f32; 3], bool)> {
        let c = self.core.borrow();
        let mut out: Vec<(u64, [f32; 3], bool)> = c
            .data
            .entities
            .values()
            .filter(|e| e.component("CSafeHouseComponent").is_some())
            .map(|e| (e.id, e.pos, c.safehouses_unlocked.contains(&e.id)))
            .collect();
        out.sort_by_key(|s| s.0);
        out
    }

    pub fn safe_house_unlocked(&self, id: u64) -> bool {
        self.core.borrow().safehouses_unlocked.contains(&id)
    }

    /// The loading screen's movie a mission asks for instead of the usual
    /// one (`SetFlashLoadingScreen`), if any.
    pub fn loading_screen(&self) -> Option<String> {
        Some(self.core.borrow().media.loading_screen.clone()).filter(|m| !m.is_empty())
    }

    /// The water levels scripts have set or are moving, by sector id, as
    /// they stand. (Single-player has the one world, whatever name a box
    /// gives it: `SetWaterLevel`'s default is `world2`, the modifier's
    /// `fc3_main`.)
    pub fn water_levels(&self) -> Vec<(u32, f32)> {
        let c = self.core.borrow();
        let mut out: Vec<(u32, f32)> = c.media.water.iter().map(|(s, l)| (*s, *l as f32)).collect();
        out.sort_by_key(|w| w.0);
        out
    }

    /// A safe house the player has taken (an outpost liberated), as the
    /// engine unlocks one: the state scripts read, and their listeners told
    /// (an outpost's flag controller; the outposts manager, which opens the
    /// country's activities).
    pub fn unlock_safe_house(&mut self, id: u64) {
        let fresh = {
            let mut c = self.core.borrow_mut();
            let fresh = c.safehouses_unlocked.insert(id);
            if fresh {
                c.emit(Command::SafeHouse { id, unlocked: true, silent: false });
                c.raise(&Listen::Entity(id, "SafeHouseUnlockedEvent".into()), vec![]);
            }
            fresh
        };
        if fresh {
            self.pump();
        }
    }

    /// A radio tower the player has taken (its panel used), as the engine
    /// reports one: the state scripts read (`IsRadioTowerUnlocked`), then the
    /// panel's start and finish on the player and on the panel (the radio
    /// towers manager then tells the region its tower is unlocked, which
    /// opens its supply runs).
    pub fn take_radio_tower(&mut self, panel: u64) {
        if !self.core.borrow_mut().radio_towers_unlocked.insert(panel) {
            return;
        }
        for event in ["CRadioTowerStartedEvent", "CRadioTowerFinishedEvent"] {
            for on in [PLAYER, panel] {
                self.entity_event(on, event, vec![Arg::id(panel)]);
            }
        }
    }

    pub fn radio_tower_unlocked(&self, panel: u64) -> bool {
        self.core.borrow().radio_towers_unlocked.contains(&panel)
    }

    /// A cinematic sequence's progress, as the host plays it: `Started`,
    /// `Finished`, `FinishedLoop`, `Paused`, `Stopped`, `Skipped`, or
    /// `SyncPointReached` (with the sync point's name).
    pub fn sequence_event(&mut self, scene: &str, file: &str, event: &str, sync_point: Option<&str>) {
        let name = format!("{}{event}", natives::media::sequence_name(scene, file));
        {
            let mut c = self.core.borrow_mut();
            if let Some(sp) = sync_point {
                c.media.sync_points.insert((scene.to_string(), file.to_ascii_lowercase()), sp.to_string());
            }
            // (Scripts name sequences in whatever case they were written.)
            c.raise_where(|l| matches!(l, Listen::Entity(_, e) if e.eq_ignore_ascii_case(&name)), vec![Arg::id(PLAYER), Arg::Str(name.clone())]);
        }
        self.pump();
    }

    /// A video started (`ended = false`) or ended: the events Bink
    /// listeners wait for (on the player, with the video's path id), and
    /// the end of a fullscreen video.
    pub fn video_event(&mut self, path: &str, ended: bool, fullscreen: bool) {
        {
            let mut c = self.core.borrow_mut();
            let id = fc3_domino::path_id(path);
            c.raise(&Listen::Entity(PLAYER, "CTabletCallEvent".into()), vec![Arg::Num(if ended { 2.0 } else { 1.0 }), Arg::Str(String::new()), Arg::Str(id)]);
            if ended && fullscreen {
                c.raise(&Listen::Generic("StopVideo".into()), vec![]);
            }
        }
        self.pump();
    }

    /// The player gave an input the scripts may listen for (`jump`,
    /// `throw_rock`, `show_map`, ...).
    pub fn input_signal(&mut self, signal: &str) {
        let n = self.core.borrow_mut().raise(
            &Listen::Input(signal.to_string()),
            vec![Arg::id(PLAYER), Arg::Nil, Arg::Num(0.0), Arg::Num(0.0), Arg::Num(0.0), Arg::Num(0.0), Arg::Str(signal.to_string())],
        );
        if n > 0 {
            self.pump();
        }
    }

    /// A sound scripts started has ended: its box hears "finished" (the
    /// reason `SoundModifier` waits for; "started" went when it began).
    pub fn sound_finished(&mut self, id: u64) {
        let cb = self.core.borrow_mut().media.sounds.remove(&id);
        if let Some(cb) = cb {
            self.core.borrow_mut().call(cb, vec![Arg::Str("finished".into())]);
            self.pump();
        }
    }

    /// The player closed a message or tutorial box, accepting or cancelling.
    pub fn box_answered(&mut self, id: u64, accept: bool) {
        let cb = self.core.borrow_mut().media.boxes.remove(&id);
        if let Some(cb) = cb {
            self.core.borrow_mut().call(cb, vec![Arg::Str(if accept { "ACCEPT" } else { "CANCEL" }.into())]);
            self.pump();
        }
    }

    /// Runs a debug console command scripts registered (cheats like
    /// `PlayChapter`).
    pub fn console(&mut self, command: &str, parameter: &str) -> bool {
        let n = self.core.borrow_mut().raise(&Listen::Console(command.to_ascii_lowercase()), vec![Arg::Str(parameter.to_string())]);
        self.pump();
        n > 0
    }

    /// The registered console commands.
    pub fn console_commands(&self) -> Vec<String> {
        self.core
            .borrow()
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Console(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether a live proximity listener explicitly selects this pawn.
    pub fn trigger_selects(&self, trigger: u64, pawn: u64) -> bool {
        self.core.borrow().selected_trigger_pawns(trigger).contains(&pawn)
    }

    /// The triggers scripts wait on now: (trigger, event, a point inside,
    /// enabled), for tools that drive the story without a world.
    pub fn watched_triggers(&self) -> Vec<(u64, String, [f32; 3], bool)> {
        self.watched_triggers_in_graph(None)
    }

    /// Automation can select a graph family without entering unrelated world
    /// effects or optional missions. Nested library boxes retain their graph
    /// ancestry, so their triggers remain eligible for the owning mission.
    pub fn watched_triggers_in_graph(&self, graph: Option<&str>) -> Vec<(u64, String, [f32; 3], bool)> {
        self.watched_triggers_where(graph, |_, _| true, false)
    }

    /// The missions running that hold the player, by their names' stem
    /// (`CIT_020`): main graphs the player has taken (acknowledged), not a
    /// giver's brief (which runs while its mission waits to be taken), nor
    /// one running unacknowledged (the DLC's, from the start). (Not the
    /// exclusive ones: those are the quests, HUR_020; the story's own,
    /// CIT_020, are not.)
    /// The messages the running missions' scripts wait for (their enabled
    /// `MessageListener`s), by name: what a minigame broadcasts as its story
    /// goes on (HOY_040's poker table, `JAS220_B10C_01`...).
    pub fn mission_messages(&self) -> Vec<String> {
        let missions = self.missions_running();
        let c = self.core.borrow();
        let mut out: Vec<String> = c
            .listeners
            .values()
            .filter_map(|l| {
                let Listen::Message(m) = &l.on else { return None };
                of_missions(&c, &l.callback.target, &missions).then(|| m.clone())
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn missions_running(&self) -> Vec<String> {
        let c = self.core.borrow();
        let mut out: Vec<String> = c
            .missions
            .by_id
            .values()
            .filter(|m| !m.giver && m.acknowledged && m.block.is_none() && m.state == missions::State::Running)
            .map(|m| m.name.trim_end_matches("_MAIN").to_string())
            .collect();
        out.sort();
        out
    }

    /// The triggers of these missions' graphs (by their names' stem) that
    /// scripts wait on, as `watched_triggers_in_graph` lists them but in
    /// the order the scripts armed them: the way their designers laid
    /// them. (DOC_020 B10E arms the mushroom's reaction, then the cave's
    /// way on, which stops the reaction's trip in case it still runs; the
    /// way on stepped into first, the trip, begun after, was never
    /// stopped and the block never completed.)
    pub fn watched_triggers_of(&self, graph: Option<&str>, missions: &[String]) -> Vec<(u64, String, [f32; 3], bool)> {
        self.watched_triggers_where(graph, |c, target| of_missions(c, target, missions), true)
    }

    /// The triggers scripts wait on that belong to no mission (a cave's
    /// exits, a base script's), as `watched_triggers_in_graph` lists them:
    /// not another mission's start (a quest giver's brief runs too) while
    /// one of the story's own holds the player.
    pub fn watched_triggers_unowned(&self, graph: Option<&str>) -> Vec<(u64, String, [f32; 3], bool)> {
        self.watched_triggers_where(graph, |c, target| mission_of(c, target).is_none(), false)
    }

    /// (`armed`: in the order they were first listened to, else by id.)
    fn watched_triggers_where(&self, graph: Option<&str>, keep: impl Fn(&Core, &Table) -> bool, armed: bool) -> Vec<(u64, String, [f32; 3], bool)> {
        let c = self.core.borrow();
        let graph = graph.map(str::to_ascii_lowercase);
        let mut out = Vec::new();
        for (&key, l) in &c.listeners {
            let Listen::Entity(t, e) = &l.on else { continue };
            if !matches!(e.as_str(), "Enter" | "Use" | "EnterFOV") || !c.is_loaded(*t) {
                continue;
            }
            // (Not a mission's area, entered again to stop its leaving
            // being warned of: JAS_220's spans the north-west, and going to
            // its middle left the fight at Hoyt's compound behind.)
            if in_graph(&l.callback.target, "exitzonewarning") {
                continue;
            }
            // A spot a script spawned to be used (a bomb's): where it stands.
            if e == "Use" && c.spawned.contains_key(t) && !c.data.entities.contains_key(t) {
                if graph.as_deref().is_some_and(|needle| !in_graph(&l.callback.target, needle)) || !keep(&c, &l.callback.target) {
                    continue;
                }
                if let Some(at) = c.position(*t) {
                    out.push((key, (*t, e.clone(), at, c.trigger_on(*t))));
                }
                continue;
            }
            if graph.as_deref().is_some_and(|needle| !in_graph(&l.callback.target, needle) || of_finished_mission(&c, &l.callback.target)) {
                continue;
            }
            if !keep(&c, &l.callback.target) {
                continue;
            }
            // One whose box counts only other characters (a companion's
            // destination) is not for the player to step into.
            if let Ok(filter) = l.callback.target.get::<crate::values::Text>("colliderFilterId") {
                if !filter.0.is_empty() && !c.groups(&filter).contains(&PLAYER) {
                    continue;
                }
            }
            let Some(entry) = c.data.entities.get(t) else { continue };
            let point = match triggers::shape_of(entry) {
                Some(triggers::Shape::Prism { points, z_min, .. }) => {
                    let n = points.len() as f32;
                    let (x, y) = points.iter().fold((0.0, 0.0), |a, p| (a.0 + p[0] / n, a.1 + p[1] / n));
                    [x, y, if z_min > f32::MIN { z_min + 1.0 } else { entry.pos[2] }]
                }
                // (One with no trigger shape of its own that moves, a
                // vehicle to use: where it is now. SAM_015's 4x4, driven
                // across the depot, waits for its turret to be manned.)
                None => c.view.spawned.get(t).map_or(entry.pos, |l| l.pos),
                _ => entry.pos,
            };
            out.push((key, (*t, e.clone(), point, c.trigger_on(*t))));
        }
        out.sort_by(|a, b| a.1 .0.cmp(&b.1 .0).then(a.1 .1.cmp(&b.1 .1)).then(a.0.cmp(&b.0)));
        out.dedup_by(|a, b| a.1 .0 == b.1 .0 && a.1 .1 == b.1 .1);
        if armed {
            out.sort_by_key(|w| w.0);
        }
        out.into_iter().map(|w| w.1).collect()
    }

    /// The places scripts wait for the player to come near
    /// (`ProximityRadiusListener` boxes polling their distance with a plugged
    /// `Near`), optionally only in graphs whose path contains `graph`: the
    /// place's entity, the radius and where it is now (Dunia).
    /// The distance checks running (`CheckDistance` delays), for working out
    /// stalls: each one's `Entities`, `id2`, near radius, whether its `Near`
    /// is plugged, and its graph.
    pub fn proximity_debug(&self) -> Vec<String> {
        let c = self.core.borrow();
        let dummy = self.domino.lua().globals().get::<mlua::Function>("DummyFunction").ok();
        let text = |t: &Table, k: &str| t.get::<mlua::Value>(k).ok().map(|v| format!("{v:?}")).unwrap_or_default();
        c.delays
            .values()
            .filter(|d| d.running && d.callback.method == "CheckDistance")
            .map(|d| {
                let t = &d.callback.target;
                let near = t.get::<mlua::Function>("Near").ok();
                let path = t.get::<Table>("_graph").and_then(|g| g.get::<Table>("_type")).and_then(|ty| ty.get::<String>("_path")).unwrap_or_default();
                format!(
                    "Entities {} id2 {} near {} plugged {} in {path}",
                    text(t, "Entities"),
                    text(t, "id2"),
                    text(t, "nearZone"),
                    near.is_some() && near != dummy
                )
            })
            .collect()
    }

    pub fn watched_proximity_in_graph(&self, graph: Option<&str>) -> Vec<(u64, f32, [f32; 3])> {
        self.watched_proximity_of(PLAYER, graph)
    }

    /// The same for the vehicle the player is in: places a script waits for
    /// it to come near (a supply run's flares and drop point).
    pub fn watched_vehicle_proximity(&self, graph: Option<&str>) -> Vec<(u64, f32, [f32; 3])> {
        let vehicle = self.core.borrow().vehicle_occupant(PLAYER).map(|o| o.vehicle);
        vehicle.map(|v| self.watched_proximity_of(v, graph)).unwrap_or_default()
    }

    fn watched_proximity_of(&self, of: u64, graph: Option<&str>) -> Vec<(u64, f32, [f32; 3])> {
        let c = self.core.borrow();
        let graph = graph.map(str::to_ascii_lowercase);
        let dummy = self.domino.lua().globals().get::<mlua::Function>("DummyFunction").ok();
        let mut out = Vec::new();
        for d in c.delays.values().filter(|d| d.running && d.callback.method == "CheckDistance") {
            let t = &d.callback.target;
            if graph.as_deref().is_some_and(|needle| !in_graph(t, needle) || of_finished_mission(&c, t)) {
                continue;
            }
            let near = t.get::<mlua::Function>("Near").ok();
            if near.is_none() || near == dummy {
                continue;
            }
            // (The player among the `Entities` and the place as `id2`, or the
            // other way round: JAS_220's way to the airport measures from its
            // places to the player.)
            let (entities, id2) = (t.get::<values::Text>("Entities").ok(), t.get::<values::Text>("id2").ok());
            let place = if entities.as_ref().is_some_and(|e| c.groups(e).contains(&of)) {
                id2.and_then(|x| x.id())
            } else if id2.and_then(|x| x.id()) == Some(of) {
                entities.and_then(|e| e.id())
            } else {
                None
            };
            let Some(place) = place else { continue };
            let radius = t.get::<f64>("nearZone").ok().unwrap_or(2.0) as f32;
            if let Some(at) = c.position(place) {
                out.push((place, radius, at));
            }
        }
        // (The nearest asked for, where several listen around one place.)
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        out.dedup_by_key(|w| w.0);
        out
    }

    /// A spot on the ground inside a region trigger, given the ground's height
    /// at a point (its centre may lie above the region's heights, as on a
    /// hill a region rings): the centre, else points between it and the
    /// corners and edges.
    pub fn trigger_ground_point(&self, id: u64, ground: impl Fn(f32, f32) -> Option<f32>) -> Option<[f32; 3]> {
        let c = self.core.borrow();
        let shape = c.data.entities.get(&id).and_then(triggers::shape_of)?;
        let triggers::Shape::Prism { points, .. } = &shape else { return None };
        let n = points.len() as f32;
        let mid = points.iter().fold([0.0, 0.0], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
        let edges = (0..points.len()).map(|i| {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
        });
        let rims: Vec<[f32; 2]> = points.iter().copied().chain(edges).collect();
        let lerp = |p: [f32; 2], t: f32| [mid[0] + (p[0] - mid[0]) * t, mid[1] + (p[1] - mid[1]) * t];
        std::iter::once(mid)
            .chain([0.5, 0.75, 0.9].into_iter().flat_map(|t| rims.iter().map(move |p| lerp(*p, t))))
            .filter_map(|[x, y]| ground(x, y).map(|z| [x, y, z + 0.2]))
            .find(|p| shape.contains(*p) && shape.contains([p[0], p[1], p[2] + triggers::BODY_HALF_HEIGHT]))
    }

    /// Why a trigger does or doesn't see the player (for working out
    /// stalls): loaded, on, its shape, and whether that holds the player.
    pub fn trigger_debug(&self, id: u64) -> String {
        let c = self.core.borrow();
        let shape = c.data.entities.get(&id).and_then(triggers::shape_of);
        let inside = shape.as_ref().map(|s| s.contains(c.view.player));
        format!(
            "loaded {} on {} shape {:?} player {:?} inside {:?} contacts {:?}",
            c.is_loaded(id),
            c.trigger_on(id),
            shape,
            c.view.player,
            inside,
            c.contacts(id)
        )
    }

    /// The contextual action a character is on, if any.
    pub fn current_action(&self, pawn: u64) -> Option<u64> {
        self.core.borrow().ca.current.get(&pawn).map(|c| c.action)
    }

    /// Contextual actions scripts wait to see interrupted (for tools that
    /// drive the story: a distraction is due).
    pub fn awaited_interrupts(&self) -> Vec<u64> {
        let c = self.core.borrow();
        c.listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(a, e) if e == "CA_Interrupt" => a.parse().ok(),
                _ => None,
            })
            .collect()
    }

    /// Where an entity is now and how it's turned, as the scripts see it
    /// (the host's live position, a script's placement, or its own).
    pub fn pose(&self, id: u64) -> Option<Pose> {
        self.core.borrow().pose(id)
    }

    /// The things scripts wait to see destroyed (`CDestroyEvent`), and where
    /// they are.
    pub fn watched_destructibles(&self) -> Vec<(u64, [f32; 3])> {
        let c = self.core.borrow();
        let mut ids: Vec<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(id, e) if e == "CDestroyEvent" && !c.dead.contains(id) => Some(*id),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().filter(|&id| c.is_loaded(id)).filter_map(|id| Some((id, c.position(id)?))).collect()
    }

    /// Something was destroyed (a blast, a script's word): the scripts
    /// waiting on it hear `CDestroyEvent` (its final state), once, and those
    /// waiting on a prop broken `StaticBreakable_Break` (JAS_130's melee
    /// tutorial's hotel door).
    pub fn destroyed(&mut self, id: u64) {
        let n = {
            let mut c = self.core.borrow_mut();
            if !c.dead.insert(id) {
                return;
            }
            c.raise(&Listen::Entity(id, "CDestroyEvent".into()), vec![Arg::Num(1.0)])
                + c.raise(&Listen::Entity(id, "StaticBreakable_Break".into()), vec![Arg::id(id)])
        };
        if n > 0 {
            self.pump();
        }
    }

    /// Whether a script listens for a group event (`Alarm_Disabled`, ...).
    pub fn awaits_group_event(&self, event: &str) -> bool {
        self.core.borrow().listeners.values().any(|l| matches!(&l.on, Listen::Group(_, e) if e == event))
    }

    /// Alarms a script waits to see switched off (`Alarm_Disabled` on their
    /// group) that are still on: not yet disabled or destroyed.
    pub fn alarms_to_disable(&self) -> Vec<u64> {
        let c = self.core.borrow();
        let groups: std::collections::BTreeSet<String> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, e) if e == "Alarm_Disabled" => Some(g.clone()),
                _ => None,
            })
            .collect();
        let mut out: Vec<u64> = groups
            .iter()
            .flat_map(|g| c.groups(&values::Text(g.clone())))
            .filter(|a| !c.alarms.get(a).is_some_and(|(state, _)| *state >= 2) && !c.removed.contains(a))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The living members of the groups scripts count (`CGroupSizeEvent`
    /// listeners: "kill them all"; or `CCounterEvent` on each one, CIT_020
    /// B30's two privateers at the convoy), for tools that drive the story.
    /// The members of the groups blocks count down by their size (a wave,
    /// the guards to kill), not those a health listener only watches
    /// (`CCounterEvent` on one character: JAS_200's visitor, to be
    /// photographed, not killed).
    pub fn size_counted_members(&self) -> std::collections::HashSet<u64> {
        let c = self.core.borrow();
        let groups: std::collections::BTreeSet<_> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, event) if event == "CGroupSizeEvent" => Some(g.clone()),
                _ => None,
            })
            .collect();
        groups.into_iter().flat_map(|g| c.groups(&values::Text(g))).filter(|&id| c.is_loaded(id) && !c.dead.contains(&id)).collect()
    }

    /// The characters a wanted poster's death check watches (its target, to
    /// be killed with a blade or a takedown: `Lib_SideQuest.WantedSQDeathCheck`
    /// takes only `WeaponMeleeAttack`).
    pub fn wanted_dead(&self) -> std::collections::HashSet<u64> {
        let c = self.core.borrow();
        let groups: std::collections::BTreeSet<_> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, event) if (event == "Die" || event == "CCounterEvent") && in_graph(&l.callback.target, "wantedsqdeathcheck") => {
                    Some(g.clone())
                }
                _ => None,
            })
            .collect();
        groups.into_iter().flat_map(|g| c.groups(&values::Text(g))).filter(|&id| c.is_loaded(id) && !c.dead.contains(&id)).collect()
    }

    pub fn counted_members(&self) -> std::collections::HashSet<u64> {
        let c = self.core.borrow();
        let groups: std::collections::BTreeSet<_> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, event) if event == "CGroupSizeEvent" || event == "CCounterEvent" => Some(g.clone()),
                _ => None,
            })
            .collect();
        groups.into_iter().flat_map(|g| c.groups(&values::Text(g))).filter(|&id| c.is_loaded(id) && !c.dead.contains(&id)).collect()
    }

    /// The groups blocks count down, each with its members still alive
    /// (whether in the host's world, and where): for working out why a
    /// count never reaches its end.
    pub fn counted_report(&self) -> Vec<String> {
        let c = self.core.borrow();
        let groups: std::collections::BTreeSet<_> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, event) if event == "CGroupSizeEvent" || event == "CCounterEvent" => Some(g.clone()),
                _ => None,
            })
            .collect();
        groups
            .into_iter()
            .map(|g| {
                let alive: Vec<String> = c
                    .groups(&values::Text(g.clone()))
                    .into_iter()
                    .filter(|id| !c.dead.contains(id))
                    .map(|id| format!("{id} {} {:?}", if c.is_loaded(id) { "in" } else { "out" }, c.position(id).map(|p| p.map(|v| v.round()))))
                    .collect();
                format!("{g}: {} alive [{}]", alive.len(), alive.join(", "))
            })
            .collect()
    }

    /// What scripts wait on now, one line each (what, and the box method
    /// it calls), with running delays: for working out where a story stalls.
    pub fn waiting_on(&self) -> Vec<String> {
        let c = self.core.borrow();
        // The graph a box belongs to (else the box), by its script's name.
        let graph = |t: &Table| -> String {
            let path = |t: &Table| -> String {
                let p = t.get::<Table>("_type").and_then(|ty| ty.get::<String>("_path")).unwrap_or_default();
                p.rsplit(['/', '\\']).next().unwrap_or_default().to_string()
            };
            t.get::<Table>("_graph").ok().map(|g| path(&g)).filter(|n| !n.is_empty()).unwrap_or_else(|| path(t))
        };
        let mut out: Vec<String> = c.listeners.values().map(|l| format!("{:?} -> {} {}", l.on, graph(&l.callback.target), l.callback.method)).collect();
        let groups: std::collections::BTreeSet<_> = c.listeners.values().filter_map(|l| match &l.on {
            Listen::Group(g, event) if event == "CGroupSizeEvent" => Some(g.clone()),
            _ => None,
        }).collect();
        for group in groups {
            let members: Vec<_> = c.groups(&values::Text(group.clone())).into_iter().map(|id|
                (id, c.data.name_of(id).unwrap_or_default(), c.is_loaded(id), c.dead.contains(&id))).collect();
            out.push(format!("group {group} members (id, name, loaded, dead): {members:?}"));
        }
        out.extend(c.delays.values().filter(|d| d.running).map(|d| format!("delay {:.1}/{:.1}s -> {}", d.elapsed, d.seconds, d.callback.method)));
        out.sort();
        out
    }

    /// The input signals scripts wait on now (for tools that drive the
    /// story without a player).
    pub fn watched_signals(&self) -> Vec<String> {
        let c = self.core.borrow();
        let mut out: Vec<String> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Input(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// A pickup was picked up: scripts waiting on it (a `PickupListener` on
    /// its id or its group) hear `DominoCallbackPickupPicked` with its id.
    pub fn pickup_picked(&mut self, pickup: u64) {
        // (A collectible's is told on the player too, `CCollectibleCollectedEvent`:
        // a POI's relic found, its layer switched for good.)
        let collectible = self.core.borrow().data.entities.get(&pickup).is_some_and(|e| e.component("CPickupCollectible").is_some());
        if collectible {
            self.core.borrow_mut().raise(&Listen::Entity(PLAYER, "CCollectibleCollectedEvent".into()), vec![Arg::id(pickup)]);
        }
        self.group_event(pickup, "DominoCallbackPickupPicked", vec![Arg::id(pickup)]);
    }

    /// The characters (or bodies) scripts wait to see through the camera
    /// (an `EntityTagListener`'s `Entity_Discovered`: a VIP's corpse to
    /// photograph; its `Entity_Tagged`: a captive to pick out).
    pub fn awaited_discoveries(&self) -> Vec<u64> {
        let c = self.core.borrow();
        let mut out: Vec<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(id, e) if e == "Entity_Discovered" || e == "Entity_Tagged" => Some(*id),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The entities scripts wait on for an event of this name (a burnable
    /// region's `FireStarted`).
    pub fn awaited_entity_events(&self, event: &str) -> Vec<u64> {
        let c = self.core.borrow();
        let mut out: Vec<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(id, e) if e == event => Some(*id),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The doors scripts wait to be pushed or pulled open (a `Door` box's
    /// `Push` / `Pull`: a hut to leave by).
    pub fn awaited_doors(&self) -> Vec<u64> {
        let c = self.core.borrow();
        let mut out: Vec<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(id, e) if e == "Push" || e == "Pull" => Some(*id),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// What each companion was last told to do.
    pub fn companions(&self) -> Vec<(u64, Companion)> {
        let mut out: Vec<(u64, Companion)> = self.core.borrow().companions.iter().map(|(&id, &c)| (id, c)).collect();
        out.sort_by_key(|c| c.0);
        out
    }

    /// A leading companion reached its path's end (`Domino_CompanionLead`'s
    /// `EndOfPathReached`); it waits there.
    pub fn companion_led(&mut self, companion: u64) {
        {
            let mut c = self.core.borrow_mut();
            c.companions.insert(companion, Companion::Wait);
            c.raise(&Listen::Entity(companion, "Domino_CompanionLead".into()), vec![Arg::id(companion), Arg::Str("EndOfPathReached".into())]);
        }
        self.pump();
    }

    /// Where a look-at trigger is now (one placed on a character rides with
    /// it).
    pub fn look_at_point(&self, trigger: u64) -> Option<[f32; 3]> {
        self.core.borrow_mut().look_at_point(trigger)
    }

    /// The characters to keep in sight: look-at triggers a script watches
    /// the player's view leave (a tail's), switched on and loaded, with where
    /// each is now and whether the player sees it.
    pub fn tail_targets(&self) -> Vec<(u64, [f32; 3], bool)> {
        let mut c = self.core.borrow_mut();
        let mut ids: Vec<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(t, e) if e == "LeaveFOV" => Some(*t),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|&t| c.trigger_on(t) && c.is_loaded(t));
        ids.into_iter().filter_map(|t| Some((t, c.look_at_point(t)?, c.looked_at.contains(&t)))).collect()
    }

    /// The pickups scripts wait to be picked up (their `PickupListener`s'
    /// ids, groups resolved).
    pub fn awaited_pickups(&self) -> Vec<u64> {
        let c = self.core.borrow();
        let mut out: Vec<u64> = c
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, e) if e == "DominoCallbackPickupPicked" => Some(c.groups(&values::Text(g.clone()))),
                _ => None,
            })
            .flatten()
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Filtered ingredient ids awaited by the player's loot listeners. Test
    /// drivers use these to gather real items, without guessing mission counts
    /// or sending a loot event for an item the player never collected.
    pub fn watched_loot_filters(&self) -> Vec<u32> {
        let c = self.core.borrow();
        let mut out: Vec<u32> = c.listeners.values().filter_map(|l| {
            if !matches!(&l.on, Listen::Entity(id, event) if *id == PLAYER && event == "CPawnLootItemEvent") {
                return None;
            }
            let filter = l.callback.target.get::<Table>("filterItemID").ok()?;
            if filter.get::<String>("section").ok()?.as_str() != "Name" { return None }
            filter.get::<values::Text>("id").ok()?.0.parse().ok()
        }).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Active player-only regions with exit listeners. Automated gathering
    /// keeps targets inside these authored boundaries instead of disabling
    /// their legitimate mission-failure callbacks.
    /// The regions running missions' scripts wait for the player to leave
    /// (enabled `Leave` listeners on triggers that are on): their ids and
    /// shapes (JAS_220's start: out of Hoyt's compound).
    pub fn watched_leaves_of(&self, missions: &[String]) -> Vec<(u64, triggers::Shape)> {
        let c = self.core.borrow();
        let mut out: Vec<(u64, triggers::Shape)> = c
            .listeners
            .values()
            .filter_map(|l| {
                let Listen::Entity(id, event) = &l.on else { return None };
                // (Not a mission's boundary, its leaving warned of and then
                // failed: JAS_220's area, left, fails the search for Riley.)
                if event != "Leave" || !c.is_loaded(*id) || !c.trigger_on(*id) || in_graph(&l.callback.target, "exitzonewarning") {
                    return None;
                }
                of_missions(&c, &l.callback.target, &missions).then_some(())?;
                Some((*id, triggers::shape_of(c.data.entities.get(id)?)?))
            })
            .collect();
        out.sort_by_key(|o| o.0);
        out.dedup_by_key(|o| o.0);
        out
    }

    pub fn watched_exit_regions(&self) -> Vec<triggers::Shape> {
        let c = self.core.borrow();
        let mut ids: Vec<_> = c.listeners.values().filter_map(|l| {
            let Listen::Entity(id, event) = &l.on else { return None };
            (event == "Leave" && c.is_loaded(*id) && c.trigger_on(*id)).then_some(*id)
        }).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().filter_map(|id| {
            let entry = c.data.entities.get(&id)?;
            let region = entry.component("CTrigger2DComponent")?;
            region.str_h(0xB3C7_40E6).filter(|s| s.eq_ignore_ascii_case("PlayerOnly"))?;
            triggers::shape_of(entry)
        }).collect()
    }

    pub fn layer_state(&self, layer: u64) -> LayerState {
        self.core.borrow().layer_state(layer)
    }

    /// A gameplay's own layer brought in or taken out, as a block's is (a
    /// shooting gallery's difficulty: its birds' boxes).
    pub fn set_layer_state(&mut self, layer: u64, state: LayerState) {
        self.core.borrow_mut().set_layer(layer, state);
        self.pump();
    }

    /// Every layer whose entities are in the world now.
    pub fn present_layers(&self) -> Vec<u64> {
        self.core.borrow().layers.iter().filter(|(_, s)| s.present()).map(|(l, _)| *l).collect()
    }

    /// (name, state) of the main missions registered so far.
    /// A mission's type as its script registered it (`mission`,
    /// `behaviourquest`, `speedquest`, `wantedquest`, `huntingquest`,
    /// `optional`, `ulcmission`), and whether it was a giver's (a
    /// briefing), by its main graph's name.
    pub fn mission_type(&self, name: &str) -> Option<(String, bool)> {
        let c = self.core.borrow();
        c.missions.by_id.values().find(|m| m.block.is_none() && m.name.eq_ignore_ascii_case(name)).map(|m| (m.type_name.clone(), m.giver))
    }

    pub fn missions(&self) -> Vec<(String, String)> {
        let c = self.core.borrow();
        let mut v: Vec<(String, String)> =
            c.missions.by_id.values().filter(|m| m.block.is_none()).map(|m| (m.name.clone(), m.state.name().to_string())).collect();
        v.sort();
        v
    }

    /// (block name, state) of the blocks registered so far.
    pub fn blocks(&self) -> Vec<(String, String)> {
        let c = self.core.borrow();
        let mut v: Vec<(String, String)> =
            c.missions.by_id.values().filter(|m| m.block.is_some()).map(|m| (m.name.clone(), m.state.name().to_string())).collect();
        v.sort();
        v
    }

    /// Script errors so far.
    pub fn errors(&self) -> usize {
        self.errors
    }

    /// Runs queued work until there is none (a bounded number of steps, so
    /// a script loop can't hang the frame).
    fn pump(&mut self) {
        for _ in 0..200_000 {
            let next = self.core.borrow_mut().actions.pop_front();
            let Some(action) = next else { return };
            {
                let mut c = self.core.borrow_mut();
                c.step_listeners.clear();
                c.told_loaded.clear();
            }
            match action {
                Action::Call(cb, args) => {
                    if let Err(e) = self.domino.call_with(&cb.target, &cb.method, mlua::MultiValue::from_iter(args.into_iter().map(|a| self.arg(a)))) {
                        self.error(format!("{}: {e}", cb.method));
                    }
                }
                Action::Start { script, owner, prefab, inputs, entries } => self.start(&script, owner, prefab, inputs, &entries),
                Action::Stop(owner) => self.stop(owner),
            }
        }
        self.core.borrow_mut().warn("script work still queued after 200000 steps (a loop?)");
    }

    fn arg(&self, a: Arg) -> mlua::Value {
        mlua::IntoLua::into_lua(a, self.domino.lua()).unwrap_or(mlua::Value::Nil)
    }

    fn error(&mut self, text: String) {
        self.errors += 1;
        self.core.borrow_mut().warn(format!("script error: {text}"));
    }

    fn start(&mut self, script: &str, owner: u64, prefab: Option<u64>, inputs: Vec<(String, InputValue)>, entries: &[&str]) {
        let lua = self.domino.lua();
        let table = match lua.create_table() {
            Ok(t) => t,
            Err(e) => return self.error(format!("{script}: {e}")),
        };
        for (k, v) in inputs {
            let _ = match v {
                InputValue::Number(n) => table.set(k, n),
                InputValue::Text(s) => table.set(k, s),
                InputValue::Bool(b) => table.set(k, b),
            };
        }
        let owner_s = owner.to_string();
        let prefab_s = prefab.map(|p| p.to_string()).unwrap_or_else(|| fc3_domino::INVALID_ID.to_string());
        let instance = match self.domino.create_on(script, Some(&owner_s), Some(&prefab_s), Some(table)) {
            Ok(i) => i,
            Err(e) => {
                self.core.borrow_mut().running.remove(&owner);
                return self.error(format!("creating {script}: {e}"));
            }
        };
        self.instances.insert(owner, instance.clone());
        for entry in entries {
            if let Err(e) = self.domino.call(&instance, entry) {
                self.error(format!("{script} {entry}: {e}"));
            }
        }
    }

    fn stop(&mut self, owner: u64) {
        self.core.borrow_mut().running.remove(&owner);
        self.loaded_scripts.remove(&owner);
        if let Some(instance) = self.instances.remove(&owner) {
            if let Err(e) = self.domino.call(&instance, "ShutDown") {
                self.error(format!("ShutDown on {owner}: {e}"));
            }
        }
    }

    /// Starts the graphs of start-on-load entities that are in the world
    /// (`sectors`: the world sectors loaded, or all when none), and stops
    /// those whose entities went.
    pub fn stream_scripts(&mut self, sectors: Option<&HashSet<u32>>) {
        let (starts, stops) = {
            let c = self.core.borrow();
            let mut starts = Vec::new();
            for e in c.data.entities.values() {
                let Some(d) = e.component("CDominoComponent") else { continue };
                if !d.bool("hidStartOnLoad").unwrap_or(false) || d.bool("hidStartByOwner").unwrap_or(false) {
                    continue;
                }
                if self.loaded_scripts.contains(&e.id) || c.running.contains(&e.id) {
                    continue;
                }
                if c.is_loaded(e.id) && in_sectors(e, sectors) {
                    starts.push(e.id);
                }
            }
            let stops: Vec<u64> = self
                .loaded_scripts
                .iter()
                .copied()
                .filter(|id| !self.held_scripts.contains(id))
                .filter(|id| !c.is_loaded(*id) || c.data.entities.get(id).is_some_and(|e| !in_sectors(e, sectors)))
                .collect();
            (starts, stops)
        };
        for id in stops {
            self.stop(id);
        }
        for id in starts {
            if self.core.borrow_mut().start_entity_script(id) {
                self.loaded_scripts.insert(id);
            }
        }
        self.pump();
    }
}

/// The world's own layers, by block: a base block's main layer
/// (`..._main`) that doesn't come with its block and that no script and no
/// gameplay names (by id or by name), as a bar's people and its poker table
/// (`c3_poker`'s `C3_Poker_main`). Nothing in the data brings them in, yet
/// the bars' tables were there to play once their country's activities
/// opened: here such a layer comes in with any other of its block's layers
/// (the poker block's marker layer, opened with its country). Those a
/// script or a gameplay switches (an outpost's, a shooting gallery's base)
/// are left to it, and those whose block nothing else brings in (a region's
/// variants) stay out.
pub fn standing_layers(pack: &ScriptPack, data: &StoryData, gameplays: &[u8]) -> HashMap<u64, Vec<u64>> {
    // What the scripts name: long numbers (ids) and quoted strings.
    let mut ids: HashSet<u64> = HashSet::new();
    let mut names: HashSet<String> = HashSet::new();
    for path in pack.paths() {
        let Some(text) = pack.get(path) else { continue };
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i].is_ascii_digit() {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i - start >= 12 {
                    if let Ok(n) = text[start..i].parse::<u64>() {
                        ids.insert(n);
                    }
                }
            } else if bytes[i] == b'"' {
                let start = i + 1;
                i = start;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                    i += 1;
                }
                if i - start > 4 && i - start < 128 {
                    names.insert(text[start..i].to_ascii_lowercase());
                }
                i += 1;
            } else {
                i += 1;
            }
        }
    }
    let in_gameplays = |name: &str| !name.is_empty() && gameplays.windows(name.len()).any(|w| w.eq_ignore_ascii_case(name.as_bytes()));
    let mut out: HashMap<u64, Vec<u64>> = HashMap::new();
    for l in data.layers.values() {
        let lower = l.name.to_ascii_lowercase();
        if l.on_activate || !lower.ends_with("_main") || !data.blocks.get(&l.block).is_some_and(|b| b.base) {
            continue;
        }
        if ids.contains(&l.id) || names.contains(&lower) || in_gameplays(&l.name) {
            continue;
        }
        out.entry(l.block).or_default().push(l.id);
    }
    out
}

fn in_sectors(e: &data::Entry, sectors: Option<&HashSet<u32>>) -> bool {
    match (e.origin, sectors) {
        (data::Origin::Sector(id), Some(s)) => s.contains(&id),
        _ => true,
    }
}

/// Whether a box belongs to a mission that has completed or failed (its
/// graph's file names it: `BAM_055_MAIN.SHA_Fire_DangerZone.lua`): a main
/// graph stays after its mission ends, and its sub-graphs' listeners with
/// it, left over.
fn of_finished_mission(c: &Core, target: &Table) -> bool {
    mission_of(c, target).is_some_and(|(_, state)| matches!(state, missions::State::Completed | missions::State::Failed))
}

/// Whether a box's graph belongs to one of these missions (by their names'
/// stem): its own mission, or for a block's graph the mission that
/// registered the block (JAS_030's blocks are JAS_020_MAIN's).
fn of_missions(c: &Core, target: &Table, missions: &[String]) -> bool {
    let Some((name, _)) = mission_of(c, target) else { return false };
    let named = |n: &str| missions.iter().any(|m| n.starts_with(m.as_str()));
    named(&name)
        || c.missions
            .by_id
            .values()
            .find(|m| m.name == name)
            .and_then(|m| m.block.as_ref()?.parent.as_ref())
            .and_then(|parent| c.missions.by_id.get(parent))
            .is_some_and(|parent| named(&parent.name))
}

/// The mission a box's graph belongs to, and its state: the first graph up
/// from it whose file names a mission (`CIT_020_B20.Sub.lua`).
fn mission_of(c: &Core, target: &Table) -> Option<(String, missions::State)> {
    let mut table = target.clone();
    for _ in 0..16 {
        if let Ok(path) = table.get::<Table>("_type").and_then(|ty| ty.get::<String>("_path")) {
            let file = path.rsplit(['/', '\\']).next().unwrap_or(&path);
            let name = file.split('.').next().unwrap_or(file).to_ascii_uppercase();
            if let Some(m) = c.missions.by_id.values().find(|m| m.name == name) {
                return Some((m.name.clone(), m.state));
            }
        }
        let Ok(parent) = table.get::<Table>("_graph") else { break };
        if parent == table {
            break;
        }
        table = parent;
    }
    None
}

/// Whether a box belongs to a graph whose script path contains `needle`
/// (lower case; any of several split by `|`): the box's own, or one it is
/// nested in.
fn in_graph(target: &Table, needle: &str) -> bool {
    let mut table = target.clone();
    for _ in 0..16 {
        if table
            .get::<Table>("_type")
            .and_then(|ty| ty.get::<String>("_path"))
            .is_ok_and(|path| needle.split('|').any(|n| !n.is_empty() && path.to_ascii_lowercase().contains(n)))
        {
            return true;
        }
        let Ok(parent) = table.get::<Table>("_graph") else { break };
        if parent == table {
            break;
        }
        table = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_story() -> Story {
        Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), StoryData::default()).unwrap()
    }

    #[test]
    fn a_mission_puts_its_own_loading_screen_up() {
        let story = empty_story();
        let lua = story.domino().lua().clone();
        assert_eq!(story.loading_screen(), None);
        lua.load(r#"SetFlashLoadingScreen("UI\\Singleplayer\\Menu\\Loading\\Gamemodes\\Singleplayer_white.feu")"#).exec().unwrap();
        assert_eq!(story.loading_screen().as_deref(), Some(r"UI\Singleplayer\Menu\Loading\Gamemodes\Singleplayer_white.feu"));
        assert_eq!(lua.load("return string.lower(GetFlashOverrideLoadingScreen())").eval::<String>().unwrap(), r"ui\singleplayer\menu\loading\gamemodes\singleplayer_white.feu");
        lua.load(r#"SetFlashLoadingScreen("")"#).exec().unwrap();
        assert_eq!(story.loading_screen(), None);
    }

    #[test]
    fn objective_timers_count_down_on_the_hud_until_hidden() {
        let mut story = empty_story();
        let lua = story.domino().lua().clone();
        lua.load("ShowObjectiveTimer(90, false); ShowObjectiveGageTimer(30, -1, false); PauseObjectiveGageTimer(true)").exec().unwrap();
        story.update(1.5, View::default());
        let t = story.timers();
        assert_eq!(t.iter().map(|t| t.id).collect::<Vec<_>>(), [0, 1]);
        assert!((t[0].seconds - 88.5).abs() < 1e-6 && !t[0].increasing && t[0].append_time);
        assert_eq!(t[1].seconds, 30.0);
        lua.load("PauseObjectiveGageTimer(false); ShowObjectiveTimer(-1, false)").exec().unwrap();
        story.update(1.0, View::default());
        let t = story.timers();
        assert_eq!(t.len(), 1);
        assert_eq!((t[0].id, t[0].seconds), (1, 29.0));
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn water_rises_from_the_worlds_level_and_calls_back_when_there() {
        let mut data = StoryData::default();
        data.water.insert(1428, 201.673);
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        let lua = story.domino().lua().clone();
        let level = |s: &str| lua.load(format!("return CDominoWaterLevelManager_GetInstance():GetWaterLevel('fc3_main', {s})")).eval::<f64>().unwrap();
        // The world's own level until a script moves it.
        assert!((level("1428") - 201.673).abs() < 1e-4);
        assert_eq!(level("7"), 0.0);
        lua.load(
            "box = { Done = function(self) self.done = (self.done or 0) + 1 end }
             box._type = box
             fill = CDominoWaterLevelManager_GetInstance():FillWater('fc3_main', 3678, 2, 139, box, 'Done')
             down = CDominoWaterLevelManager_GetInstance():DischargeWater('world2', 1428, 0.1, 201.0, box, 'Done')
             CDominoWaterLevelManager_GetInstance():SetWaterLevel('fc3_main', 3678, 135)",
        )
        .exec()
        .unwrap();
        story.update(1.0, View::default());
        assert!((level("3678") - 137.0).abs() < 1e-4);
        assert!((level("1428") - 201.573).abs() < 1e-4);
        story.update(1.0, View::default());
        assert!((level("3678") - 139.0).abs() < 1e-4);
        assert_eq!(lua.load("return box.done").eval::<u32>().unwrap(), 1);
        // A fill stopped stays where it stopped and calls nobody.
        lua.load("CDominoWaterLevelManager_GetInstance():StopChangingWaterLevel(down)").exec().unwrap();
        story.update(5.0, View::default());
        assert!((level("1428") - 201.473).abs() < 1e-4, "{}", level("1428"));
        assert_eq!(lua.load("return box.done").eval::<u32>().unwrap(), 1);
        let levels = story.water_levels();
        assert_eq!(levels.iter().map(|l| l.0).collect::<Vec<_>>(), [1428, 3678]);
        assert!((levels[0].1 - 201.473).abs() < 1e-3 && (levels[1].1 - 139.0).abs() < 1e-3);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_flag_controller_starts_once_per_sector_residency() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let mut story = Story::open(&cache).unwrap();
        let flag = 2078504254200370807;
        assert_eq!(story.domino().lua().load("return CAPI_Entity.GetEntityInPrefab('2078504254200370807', 'FlagCinematicSceneEntity')")
            .eval::<String>().unwrap(), "2078504268207249058");
        assert_eq!(story.data().entities[&2078504268207249058].pos, [-110.35325, 2271.5603, 152.863]);
        story.domino().lua().load("LockSafeHouse('2075834596452213647')").exec().unwrap();
        story.stream_scripts(Some(&HashSet::new()));
        assert!(!story.loaded_scripts.contains(&flag));
        let sectors = HashSet::from([18478]);
        story.stream_scripts(Some(&sectors));
        assert!(story.loaded_scripts.contains(&flag));
        let first = story.instances[&flag].clone();
        assert_eq!(first.get::<String>("SafeHouse").unwrap(), "2075834596452213647");
        story.stream_scripts(Some(&sectors));
        assert_eq!(story.instances[&flag], first);
        story.stream_scripts(Some(&HashSet::new()));
        assert!(!story.instances.contains_key(&flag));
        story.stream_scripts(Some(&sectors));
        assert!(story.instances.contains_key(&flag));
        assert_ne!(story.instances[&flag], first);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_group_size_listener_observes_each_death_once_and_queries_remaining_members() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let mut data = StoryData::default();
        data.groups.insert(0x12345678, vec![10, 11]);
        let mut story = Story::new(pack, data).unwrap();
        let mut view = View::default();
        for id in [10, 11, 12] { view.spawned.insert(id, Live { alive: true, ..Default::default() }); }
        story.update(0.0, view);
        let mut listeners = Vec::new();
        for _ in 0..2 {
            let listener = story.domino().create("Domino/System/GroupSizeListener_v4.lua").unwrap();
            let lua = story.domino().lua();
            listener.set("group", "#12345678").unwrap();
            listener.set("autoDisable", false).unwrap();
            for output in ["Enabled", "Disabled", "MemberAdded"] {
                listener.set(output, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
            }
            listener.set("MemberRemoved", lua.load(r#"return function(self)
                assert(self.memberCount == #GetEntitiesInGroup(self.group))
                self.received = (self.received or 0) + 1
            end"#).eval::<mlua::Function>().unwrap()).unwrap();
            story.domino().call(&listener, "Enable").unwrap();
            assert_eq!(listener.get::<u32>("memberCount").unwrap(), 2);
            listeners.push(listener);
        }
        story.killed(10);
        story.killed(10); // Duplicate reports cannot advance counters again.
        story.killed(12); // A death outside this group cannot notify it.
        for listener in &listeners {
            assert_eq!(listener.get::<u32>("received").unwrap(), 1);
            assert_eq!(listener.get::<u32>("memberCount").unwrap(), 1);
        }
        story.killed(11);
        for listener in &listeners {
            assert_eq!(listener.get::<u32>("received").unwrap(), 2);
            assert_eq!(listener.get::<u32>("memberCount").unwrap(), 0);
        }
        assert_eq!(story.errors(), 0);
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_dead_state_listener_receives_singleton_death_without_duplicate_delivery() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let mut story = Story::new(pack, StoryData::default()).unwrap();
        let mut view = View::default();
        view.spawned.insert(10, Live { alive: true, ..Default::default() });
        story.update(0.0, view);
        let listener = story.domino().create("Domino/System/AI/StateListener.lua").unwrap();
        let lua = story.domino().lua();
        listener.set("group", "10").unwrap();
        listener.set("autoDisable", false).unwrap();
        for output in ["Enabled", "Disabled", "StateStop"] {
            listener.set(output, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        }
        for (output, field) in [("StateStart", "starts"), ("MemberInState", "members")] {
            listener.set(output, lua.load(format!(r#"return function(self)
                assert(self.memberCountInState == 1)
                assert(GetGroupInStateMemberCount(self.group, 4) == 1)
                self.{field} = (self.{field} or 0) + 1
            end"#)).eval::<mlua::Function>().unwrap()).unwrap();
        }
        story.domino().call(&listener, "Dead").unwrap();
        story.killed(10);
        story.killed(10);
        assert_eq!(listener.get::<u32>("starts").unwrap(), 1);
        assert_eq!(listener.get::<u32>("members").unwrap(), 1);
        assert_eq!(story.errors(), 0);
    }

    /// A shot told on the player's groups reaches the game's own
    /// `PawnWeaponFiredListener` filtering on its class (WIL_020's
    /// flamethrower barks), with the rounds left; another class is passed by.
    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_weapon_fired_listener_hears_the_players_shots_by_class() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = ScriptPack::from_bytes(&std::fs::read(cache.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let mut story = Story::new(pack, StoryData::default()).unwrap();
        story.update(0.0, View::default());
        let listener = story.domino().create("Domino/System/PawnWeaponFiredListener.lua").unwrap();
        let lua = story.domino().lua();
        listener.set("classFilter", "FlameThrower").unwrap();
        listener.set("autoDisable", false).unwrap();
        for output in ["Enabled", "Disabled"] {
            listener.set(output, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        }
        listener.set("OnFired", lua.load("return function(self) self.heard = (self.heard or 0) + 1 end").eval::<mlua::Function>().unwrap()).unwrap();
        story.domino().call(&listener, "Enable").unwrap();
        let shot = |weapon: &str, class: &str, ammo: f64| {
            let class = fc3_formats::hash::crc32(class.to_ascii_lowercase().as_bytes()) as f64;
            vec![Arg::id(PLAYER), Arg::Str(weapon.into()), Arg::Num(class), Arg::Num(ammo)]
        };
        story.group_event(PLAYER, "CFCXWeaponShootEvent", shot("AK47", "Assault", 29.0));
        story.group_event(PLAYER, "CFCXWeaponShootEvent", shot("Flamethrower", "FlameThrower", 45.0));
        assert_eq!(listener.get::<u32>("heard").unwrap(), 1);
        assert_eq!(listener.get::<f64>("remainingAmmo").unwrap(), 45.0);
        assert_eq!(story.errors(), 0);
    }

    /// The camera raised: a gadget listener filtering on its category
    /// hears it drawn, and put away, holstered.
    #[test]
    fn raising_the_camera_is_heard_as_a_gadget_drawn() {
        let mut story = empty_story();
        let mut view = View::default();
        story.update(0.0, view.clone());
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = {}
            local box = { _type = {
                Drawn = function(self, event, wielder, name, category)
                    if category == GetNoCaseStringID("binoculars") then heard[#heard + 1] = "drawn" end
                end,
                Holstered = function(self, event) heard[#heard + 1] = "holstered" end,
            } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent(GetLocalPlayerId(), "CGadgetDrawEvent", box, "Drawn")
            CEntityGroupManager_GetInstance():RegisterScriptEvent(GetLocalPlayerId(), "CGadgetHolsteredEvent", box, "Holstered")
        "##,
            )
            .exec()
            .unwrap();
        view.player_state.gadget = Some(Item { name: "gadgets:Equipped/Tablet".into(), category: "binoculars".into(), ..Default::default() });
        story.update(0.1, view.clone());
        story.update(0.1, view.clone());
        view.player_state.gadget = None;
        story.update(0.1, view);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["drawn", "holstered"]);
        assert_eq!(story.errors(), 0);
    }

    /// A vehicle a mission watches reads mint while it stands, and its
    /// listener hears it destroyed once the host has it wrecked.
    #[test]
    fn a_watched_vehicle_reads_mint_until_it_is_wrecked() {
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), StoryData::default()).unwrap();
        let mut view = View::default();
        view.spawned.insert(40, Live { alive: true, ..Default::default() });
        story.update(0.0, view.clone());
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = {}
            local box = { _type = { Changed = function(self, state) heard[#heard + 1] = state end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("40", box, "Changed", "OnStateChange")
        "##,
            )
            .exec()
            .unwrap();
        let state: f64 = story.domino().lua().load("return CAPI_Vehicle.GetVehicleEngineState('40')").eval().unwrap();
        assert_eq!(state, 5.0);
        view.spawned.insert(40, Live { alive: false, ..Default::default() });
        story.update(0.1, view.clone());
        story.update(0.1, view);
        let heard: Vec<f64> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, [0.0]);
        let state: f64 = story.domino().lua().load("return CAPI_Vehicle.GetVehicleEngineState('40')").eval().unwrap();
        assert_eq!(state, 0.0);
    }

    /// A safe house the host takes is unlocked as a script would have it:
    /// the state scripts read, and their listeners told, once.
    #[test]
    fn a_safe_house_taken_is_unlocked_and_its_listeners_told_once() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = 0
            local box = { _type = { OnUnlocked = function(self) heard = heard + 1 end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("77", box, "OnUnlocked", "SafeHouseUnlockedEvent")
        "##,
            )
            .exec()
            .unwrap();
        story.unlock_safe_house(77);
        story.unlock_safe_house(77);
        let (unlocked, heard): (bool, i32) = story.domino().lua().load(r#"return IsSafeHouseUnlocked("77"), heard"#).eval().unwrap();
        assert!(unlocked);
        assert_eq!(heard, 1);
    }

    /// A radio tower the host takes reads unlocked, and the player's
    /// listeners hear its panel finished, once.
    #[test]
    fn a_radio_tower_taken_is_unlocked_and_heard_once() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = {}
            local box = { _type = { OnFinished = function(self, panel) heard[#heard + 1] = tostring(panel) end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback(GetLocalPlayerId(), box, "OnFinished", "CRadioTowerFinishedEvent")
        "##,
            )
            .exec()
            .unwrap();
        story.take_radio_tower(88);
        story.take_radio_tower(88);
        let unlocked: bool = story.domino().lua().load(r#"return IsRadioTowerUnlocked("88")"#).eval().unwrap();
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert!(unlocked && story.radio_tower_unlocked(88));
        assert_eq!(heard, ["88"]);
    }

    /// An alarm box switched off: the boxes listening on its group hear who
    /// and which, and its state reads disabled from then on.
    #[test]
    fn a_disabled_alarm_tells_its_group_and_reads_disabled() {
        let mut data = StoryData::default();
        data.groups.insert(0x3E17_E227, vec![77]);
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story.domino().lua().load(r##"
            heard = {}
            local box = { _type = { Got = function(self, event, user, alarm) heard[#heard + 1] = event .. " " .. tostring(user) .. " " .. tostring(alarm) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("#3E17E227", "Alarm_Disabled", box, "Got")
        "##).exec().unwrap();
        story.update(0.0, View::default());
        story.alarm_event(77, 2, PLAYER);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["Alarm_Disabled 1 77"]);
        let state: f64 = story.domino().lua().load("return CAPI_Entity.GetAlarmState('77')").eval().unwrap();
        assert_eq!(state, 2.0);
    }

    /// A script switching alarms (`AlarmModifier`): each box of the group
    /// that changes takes the state, its listeners hear it once, and the
    /// host is told to sound or silence it.
    #[test]
    fn a_script_sets_off_its_alarms() {
        let mut data = StoryData::default();
        data.groups.insert(0xB196_55C7, vec![77, 78]);
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        let lua = story.domino().lua().clone();
        lua.load(r##"
            heard = {}
            local box = { _type = { Got = function(self, event, user, alarm) heard[#heard + 1] = event .. " " .. tostring(alarm) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("#B19655C7", "", box, "Got")
        "##).exec().unwrap();
        story.update(0.0, View::default());
        story.take_commands();
        lua.load(r##"SetAlarmState("#B19655C7", 1); SetAlarmState("#B19655C7", 1)"##).exec().unwrap();
        let commands = story.take_commands();
        story.update(0.0, View::default());
        let mut heard: Vec<String> = lua.load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        heard.sort();
        assert_eq!(heard, ["Alarm_Activated 77", "Alarm_Activated 78"]);
        let told: Vec<Vec<String>> = commands
            .into_iter()
            .filter_map(|c| match c {
                Command::Native { name, args } if name == "SetAlarmState" => Some(args),
                _ => None,
            })
            .collect();
        assert_eq!(told, [vec!["77".to_string(), "1".to_string()], vec!["78".to_string(), "1".to_string()]]);
        let state: f64 = lua.load("return CAPI_Entity.GetAlarmState('78')").eval().unwrap();
        assert_eq!(state, 1.0);
        assert_eq!(story.errors(), 0);
    }

    /// A move a script plays on the player runs a while (not cut short
    /// meanwhile), brings the markup registered with it halfway, and a shut
    /// down state machine stands stunned.
    #[test]
    fn a_played_move_brings_its_markup_and_ends() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local box = { _type = { Got = function(self) heard[#heard + 1] = "markup" end, Stun = function(self) heard[#heard + 1] = "stun" end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("1", box, "Got", "SomeMarkup")
            CAPI_Pawn.PlayAnimation("1", "123", 1, true)
            playing = CAPI_Pawn.GetCurrentMoveName("1", 1)
            cut_at_once = CAPI_Pawn.IsAnimationCutable("1", "123", 1)
            CGOStateMachineScriptCallbackSystem_GetInstance():RegisterStateCallback("1", box, "Stun", "::Main Avatar/Common/Stun")
            CAPI_Pawn.ShutdownGOSM("1")
        "#,
            )
            .exec()
            .unwrap();
        let lua = story.domino().lua();
        assert_eq!(lua.globals().get::<String>("playing").unwrap(), "123");
        assert!(!lua.globals().get::<bool>("cut_at_once").unwrap());
        story.update(core::MOVE_SECS as f32 * 0.6, View::default());
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["stun", "markup"]);
        story.update(core::MOVE_SECS as f32, View::default());
        let cut: bool = story.domino().lua().load(r#"return CAPI_Pawn.IsAnimationCutable("1", "123", 1)"#).eval().unwrap();
        assert!(cut);
    }

    /// A proximity listener polling for the player with its `Near` plugged
    /// shows up with its place and radius; one with `Near` unplugged doesn't.
    #[test]
    fn proximity_listeners_waiting_on_the_player_are_listed() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            local function listen(near)
                local box = { Near = near, Entities = "1", id2 = "1", nearZone = 4 }
                local timer = CDominoDelayManager_GetInstance():CreateDelay(1, box, "CheckDistance")
                CDominoDelayManager_GetInstance():SendCommand(timer, "start")
            end
            listen(function() end)
            listen(DummyFunction)
        "#,
            )
            .exec()
            .unwrap();
        story.update(0.0, View { player: [5.0, 6.0, 7.0], ..View::default() });
        assert_eq!(story.watched_proximity_in_graph(None), vec![(PLAYER, 4.0, [5.0, 6.0, 7.0])]);
        assert!(story.watched_proximity_in_graph(Some("some_other_graph")).is_empty());
    }

    /// A completed mission's main graph stays, its sub-graphs' listeners with
    /// it: the autopilot leaves those alone, and goes on with a running one's.
    #[test]
    fn listeners_of_a_finished_mission_are_left_to_it() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            local function listen(path)
                local graph = { _type = { _path = path } }
                local box = { Near = function() end, Entities = "1", id2 = "1", nearZone = 4, _graph = graph }
                local timer = CDominoDelayManager_GetInstance():CreateDelay(1, box, "CheckDistance")
                CDominoDelayManager_GetInstance():SendCommand(timer, "start")
            end
            listen("Domino/user/fc3/FC3_MainMissions/OLD_010/OLD_010_MAIN.DangerZone.lua")
        "#,
            )
            .exec()
            .unwrap();
        story.update(0.0, View { player: [5.0, 6.0, 7.0], ..View::default() });
        let mission = |state| missions::Mission {
            id: "1".into(),
            script: String::new(),
            name: "OLD_010_MAIN".into(),
            giver: false,
            type_name: String::new(),
            objective: None,
            spawn: None,
            exclusive: false,
            title_screen: false,
            end_screen: false,
            state,
            acknowledged: true,
            owner: 7,
            block: None,
            current_objective: None,
            fast_travel: false,
        };
        story.core.borrow_mut().missions.by_id.insert("1".into(), mission(missions::State::Running));
        assert_eq!(story.watched_proximity_in_graph(Some("fc3_mainmissions")).len(), 1);
        story.core.borrow_mut().missions.by_id.insert("1".into(), mission(missions::State::Completed));
        assert!(story.watched_proximity_in_graph(Some("fc3_mainmissions")).is_empty());
        // (Not for other uses: the world still hears it.)
        assert_eq!(story.watched_proximity_in_graph(None).len(), 1);
    }

    /// A pawn's health change reaches `HealthListener`-style callbacks on its
    /// groups: hurt, then killed.
    #[test]
    fn health_changes_reach_counter_listeners() {
        let mut data = StoryData::default();
        data.groups.insert(0x0BAD_F00D, vec![42]);
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = {}
            local health = GetNoCaseStringID("health")
            local box = { _type = { Got = function(self, event, counter, old, new, by, source, victim, emitter)
                if counter == health and source ~= nil then heard[#heard + 1] = string.format("%s %d->%d by %s: %s %s", event, old, new, tostring(by), source, emitter) end
            end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("#0BADF00D", "CCounterEvent", box, "Got")
        "##,
            )
            .exec()
            .unwrap();
        let mut view = View::default();
        view.player_state.in_hands = Some("weapons:FC3/AK47".into());
        story.update(0.0, view);
        story.health_changed(42, 100.0, 40.0, PLAYER, "BulletImpact", None);
        story.health_changed(42, 40.0, 0.0, PLAYER, "BulletImpact", Some("arrow"));
        story.health_changed(42, 0.0, 0.0, PLAYER, "Generic", None);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["CCounterEvent 100->40 by 1: BulletImpact weapons:FC3/AK47", "CCounterEvent 40->0 by 1: BulletImpact arrow"]);
    }

    /// A `PickupListener` on a pickup's id hears it picked up, with its id;
    /// until then the pickup is awaited.
    #[test]
    fn a_picked_up_pickup_reaches_its_listener() {
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), StoryData::default()).unwrap();
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = {}
            local box = { _type = { Got = function(self, event, id) heard[#heard + 1] = event .. " " .. tostring(id) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("77", "DominoCallbackPickupPicked", box, "Got")
        "##,
            )
            .exec()
            .unwrap();
        story.update(0.0, View::default());
        assert_eq!(story.awaited_pickups(), vec![77]);
        story.pickup_picked(76);
        story.pickup_picked(77);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["DominoCallbackPickupPicked 77"]);
    }

    /// An action point switched off sends the character on it away: its end
    /// comes a moment later, to a listener the script registers just after
    /// switching it off (as `NarrativeStopSTP` does).
    #[test]
    fn a_switched_off_action_ends_once_its_user_is_up() {
        let mut story = empty_story();
        // (The character is in the world: told to one who isn't, nothing
        // happens.)
        let mut view = View::default();
        view.spawned.insert(5, Live { alive: true, ..Default::default() });
        story.update(0.0, view.clone());
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            SetAIFactEntityId("5", "DominoCAOverride", "900")
            ToggleContextualActionState("900", false)
            local box = { _type = { Ended = function(self, event, user) heard[#heard + 1] = event .. " " .. tostring(user) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("900", "CA_End", box, "Ended")
        "#,
            )
            .exec()
            .unwrap();
        let heard = |story: &Story| -> Vec<String> {
            story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect()
        };
        story.update(0.0, view.clone());
        assert!(heard(&story).is_empty());
        story.update(natives::ai::LEAVE_SECS as f32 + 0.1, view);
        assert_eq!(heard(&story), ["CA_End 5"]);
        assert_eq!(story.errors(), 0);
    }

    /// A group's members going into combat: its combat listeners hear the
    /// state start with the first, each member coming in, and its stop when
    /// none is left in it.
    #[test]
    fn a_group_hears_its_members_go_into_combat() {
        let mut data = StoryData::default();
        data.groups.insert(0xABC, vec![10, 11]);
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        let live = |state: u8| Live { alive: true, ai_state: state, ..Default::default() };
        let view = |a: u8, b: u8| {
            let mut v = View::default();
            v.spawned.insert(10, live(a));
            v.spawned.insert(11, live(b));
            v
        };
        story.update(0.0, view(0, 0));
        story
            .domino()
            .lua()
            .load(
                r##"
            heard = {}
            local box = { _type = { Got = function(self, event, what, count) heard[#heard + 1] = what .. " " .. tostring(count) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("#00000ABC", "CGroupCombatEvent", box, "Got")
        "##,
            )
            .exec()
            .unwrap();
        let heard = |story: &Story| -> Vec<String> {
            story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect()
        };
        story.update(0.1, view(0, 0));
        assert!(heard(&story).is_empty());
        story.update(0.1, view(3, 0));
        assert_eq!(heard(&story), ["CombatStart 1", "CombatMemberIn 1"]);
        story.update(0.1, view(3, 3));
        assert_eq!(heard(&story)[2..], ["IdleStop 0", "CombatMemberIn 2"]);
        story.update(0.1, view(0, 0));
        assert!(heard(&story).iter().any(|h| h == "CombatStop 0"));
        assert_eq!(story.errors(), 0);
    }

    /// A contextual action begun: the scripts listening for its markups hear
    /// them a while in, the first before the second.
    #[test]
    fn a_started_action_reaches_its_markups() {
        let mut story = empty_story();
        let mut view = View::default();
        view.spawned.insert(5, Live { alive: true, ..Default::default() });
        story.update(0.0, view.clone());
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            SetAIFactEntityId("5", "DominoCAOverride", "900")
            local box = { _type = { Got = function(self, event, user) heard[#heard + 1] = event .. " " .. tostring(user) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("900", "CA_Markup1", box, "Got")
            CEntityGroupManager_GetInstance():RegisterScriptEvent("900", "CA_Markup2", box, "Got")
        "#,
            )
            .exec()
            .unwrap();
        let heard = |story: &Story| -> Vec<String> {
            story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect()
        };
        story.action_started(5);
        story.update(1.0, view.clone());
        assert!(heard(&story).is_empty());
        story.update(1.1, view.clone());
        assert_eq!(heard(&story), ["CA_Markup1 5"]);
        story.update(2.0, view);
        assert_eq!(heard(&story), ["CA_Markup1 5", "CA_Markup2 5"]);
        assert_eq!(story.errors(), 0);
    }

    /// A sound a box plays says "started", then "finished" when the host
    /// reports its end, by name, as `SoundModifier` compares them.
    #[test]
    fn a_played_sound_reports_started_then_finished() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local target = { _type = { Callback_Sound = function(self, reason) heard[#heard + 1] = reason end } }
            sound = CDominoSoundManager_GetInstance():PlaySound("1", "0x001b83ba", 20, target, "Callback_Sound")
        "#,
            )
            .exec()
            .unwrap();
        story.update(0.0, View::default());
        let lua = story.domino().lua();
        let id: u64 = lua.globals().get::<String>("sound").unwrap().parse().unwrap();
        story.sound_finished(id);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["started", "finished"]);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn watched_loot_filters_track_only_live_player_ingredient_subscriptions() {
        let story = empty_story();
        story.domino().lua().load(r#"
            local callbacks = CScriptCallbackSystem_GetInstance()
            subscriptions = {}
            local function watch(entity, event, section, id)
                local target = { filterItemID = { section = section, id = id } }
                target._type = { Got = function() end }
                return callbacks:RegisterEventCallback(entity, target, "Got", event)
            end
            subscriptions[1] = watch("1", "CPawnLootItemEvent", "Name", "126007")
            subscriptions[2] = watch("1", "CPawnLootItemEvent", "Name", 104580)
            subscriptions[3] = watch("1", "CPawnLootItemEvent", "Name", "126007")
            watch("2", "CPawnLootItemEvent", "Name", "104581")
            watch("1", "CPawnBuyItemEvent", "Name", "142502")
            watch("1", "CPawnLootItemEvent", "Other", "142503")
            watch("1", "CPawnLootItemEvent", "Name", "not-an-id")
            watch("1", "CPawnLootItemEvent", nil, nil)
        "#).exec().unwrap();
        assert_eq!(story.watched_loot_filters(), vec![104580, 126007]);
        story.domino().lua().load(r#"
            local callbacks = CScriptCallbackSystem_GetInstance()
            callbacks:RemoveCallback("1", subscriptions[1])
            callbacks:RemoveCallback("1", subscriptions[3])
        "#).exec().unwrap();
        assert_eq!(story.watched_loot_filters(), vec![104580]);
        story.domino().lua().load(r#"
            CScriptCallbackSystem_GetInstance():RemoveCallback("1", subscriptions[2])
        "#).exec().unwrap();
        assert!(story.watched_loot_filters().is_empty());
    }

    #[test]
    fn native_player_queries_follow_host_updates() {
        let mut story = empty_story();
        let mut view = View { difficulty: 2, time_of_day: 18.5, ..Default::default() };
        view.player_state.money = 425;
        view.player_state.loot.insert(126007, 2);
        view.player_state.medpacks = 3;
        view.player_state.weapon_slots = 4;
        view.player_state.weapons.push(Item { name: "weapons:FC3/Kimber".into(), category: "handgun".into(), count: 1, ..Default::default() });
        view.player_state.in_hands = Some("weapons:FC3/Kimber".into());
        story.update(0.0, view.clone());
        let lua = story.domino().lua();
        let values: (u32, u32, u32, u32, u32) = lua.load(r#"
            return CAPI_Player.GetPlayerMoney("1"), CAPI_Player.GetPlayerMedPackCount("1"),
                CAPI_Player.GetNumWeaponSlotsUnlocked("1"), GetDifficulty(),
                CAPI_Pawn.CheckPawnWeaponInventoryByName("1", "Kimber")
        "#).eval().unwrap();
        assert_eq!(values, (425, 3, 4, 2, 1));
        let loot: (u32, u32, u32) = lua.load(r#"
            return CAPI_Pawn.GetLootQuantity("1", "126007"),
                CAPI_Pawn.GetLootQuantity("1", "104580"),
                CAPI_Pawn.GetLootQuantity("1", GetHashKey("weapons:fc3/kimber"))
        "#).eval().unwrap();
        assert_eq!(loot, (2, 0, 0));
        let clock: Vec<u32> = lua.load("return GetTimeOfDay()").eval().unwrap();
        assert_eq!(clock, vec![18, 30, 0]);
        view.player_state.in_hands = None;
        view.player_state.loot.clear();
        story.update(0.0, view);
        assert_eq!(story.domino().lua().load(r#"return CAPI_Pawn.CheckPawnWeaponInventoryByName("1", "Kimber")"#).eval::<u32>().unwrap(), 2);
        assert_eq!(story.domino().lua().load(r#"return CAPI_Pawn.GetLootQuantity("1", "126007")"#).eval::<u32>().unwrap(), 0);
    }

    #[test]
    fn spawned_inventory_ids_resolve_for_host_commands() {
        let story = empty_story();
        let id: String = story.domino().lua().load(r#"return CAPI_Pawn.SpawnWeapon("1", "gadgets:FC3/Rock")"#).eval().unwrap();
        assert_eq!(story.item_name(&id), "gadgets:FC3/Rock");
        assert_eq!(story.item_name("weapons:FC3/Kimber"), "weapons:FC3/Kimber");
    }

    #[test]
    fn a_tree_growth_calls_back_once_the_host_says_it_is_grown() {
        let mut story = empty_story();
        let lua = story.domino().lua();
        lua.load(
            r#"
            Grown = 0
            Box = { _type = { OnGrowthCompleted = function(self) Grown = Grown + 1 end } }
            First = CAPI_Entity.SetRealTreeGrowthSpeed("7", 1, 0.5, Box, "OnGrowthCompleted")
            Second = CAPI_Entity.SetRealTreeGrowthSpeed("8", 0, -0.2, Box, "OnGrowthCompleted")
        "#,
        )
        .exec()
        .unwrap();
        let commands = story.update(0.1, View::default());
        let grown = |story: &Story| story.domino().lua().globals().get::<i64>("Grown").unwrap();
        assert_eq!(grown(&story), 0, "not before the host has grown it");
        let ids: Vec<u64> = commands
            .iter()
            .filter_map(|c| match c {
                Command::Native { name, args } if name == "CAPI_Entity.SetRealTreeGrowthSpeed" => args.get(3)?.parse().ok(),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].to_string(), story.domino().lua().globals().get::<String>("First").unwrap());
        // A stopped growth never calls back; a finished one, once.
        story.domino().lua().load("CAPI_Entity.StopRealTreeGrowth('8', Second)").exec().unwrap();
        story.growth_finished(ids[1]);
        story.growth_finished(ids[0]);
        story.growth_finished(ids[0]);
        assert_eq!(grown(&story), 1);
    }

    #[test]
    fn named_timers_pause_with_story_time_and_unregister() {
        let mut story = empty_story();
        let id: u64 = story.domino().lua().load(r#"return RegisterNamedTimerLocalized("Escape", 65, false, true, "", "", true)"#).eval().unwrap();
        story.domino().lua().load(r#"RegisterNamedTimerLocalized("Hidden", 20, true, false, "", "", true)"#).exec().unwrap();
        story.update(1.5, View::default());
        assert_eq!(story.timers(), vec![Timer { id, label: "Escape".into(), seconds: 63.5, increasing: false, append_time: true }]);
        story.update(0.0, View::default());
        assert_eq!(story.timers()[0].seconds, 63.5);
        story.domino().lua().load(format!("UnregisterNamedTimer({id})")).exec().unwrap();
        assert!(story.timers().is_empty());
    }

    #[test]
    fn menu_callbacks_see_refreshed_inventory_without_advancing_paused_timers() {
        let mut story = empty_story();
        story.domino().lua().load(r#"
            RegisterNamedTimerLocalized("Escape", 65, false, true, "", "", true)
            menu = { crafting = 0, exit = 0 }
            menu._type = {
                Crafting = function(self)
                    assert(CAPI_Player.GetPlayerMedPackCount("1") == 1)
                    assert(CAPI_Pawn.GetLootQuantity("1", 104580) == 1)
                    self.crafting = self.crafting + 1
                end,
                Exit = function(self)
                    assert(CAPI_Player.GetPlayerMedPackCount("1") == 2)
                    assert(CAPI_Pawn.GetLootQuantity("1", 104580) == 0)
                    self.exit = self.exit + 1
                end
            }
            local callbacks = CScriptCallbackSystem_GetInstance()
            callbacks:RegisterGenericCallback(menu, "Crafting", "CDominoCraftingPageOpenedEvent")
            callbacks:RegisterGenericCallback(menu, "Exit", "CDominoExitPauseMenuEvent")
        "#).exec().unwrap();
        story.update(1.5, View::default());
        let mut view = View::default();
        view.player_state.medpacks = 1;
        view.player_state.loot.insert(104580, 1);
        story.update(0.0, view.clone());
        story.generic_event("CDominoCraftingPageOpenedEvent", vec![]);
        for _ in 0..10 { story.update(0.0, view.clone()); }
        assert_eq!(story.timers()[0].seconds, 63.5);
        view.player_state.medpacks = 2;
        view.player_state.loot.clear();
        story.update(0.0, view);
        story.generic_event("CDominoExitPauseMenuEvent", vec![]);
        let menu = story.domino().lua().globals().get::<Table>("menu").unwrap();
        assert_eq!(menu.get::<u32>("crafting").unwrap(), 1);
        assert_eq!(menu.get::<u32>("exit").unwrap(), 1);
        assert_eq!(story.timers()[0].seconds, 63.5);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn queued_tutorial_is_a_host_modal() {
        let mut story = empty_story();
        story.domino().lua().load(r#"PushTutorialBox("tutorial.feu", 1, 2, 3, true)"#).exec().unwrap();
        let commands = story.take_commands();
        let Command::Native { name, args } = &commands[0] else { panic!("expected a tutorial command") };
        assert_eq!(name, "ShowTutorialBox");
        assert!(args[0].parse::<u64>().is_ok());
        assert_eq!(&args[3..], &["false", "", "true"]);
        story.box_answered(args[0].parse().unwrap(), true);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn checklist_commands_preserve_row_identity_and_show_hide_state() {
        let mut story = empty_story();
        story.domino().lua().load(r#"
            SetDynamicCheckListObjective(true, "166310", 2)
            SetDynamicCheckListObjective(false, "166310", -1)
        "#).exec().unwrap();
        assert_eq!(story.take_commands(), vec![
            Command::Native { name: "ChecklistObjective".into(), args: vec!["true".into(), "166310".into(), "".into()] },
            Command::Native { name: "ChecklistObjective".into(), args: vec!["false".into(), "166310".into(), "".into()] },
        ]);
    }

    #[test]
    fn inventory_spawn_and_remove_callbacks_follow_host_availability() {
        let mut story = empty_story();
        let id: String = story.domino().lua().load(r#"
            counts = { spawned = 0, removed = 0 }
            counts._type = {
                Spawn = function(self) self.spawned = self.spawned + 1 end,
                Remove = function(self) self.removed = self.removed + 1 end,
            }
            local id = CAPI_Pawn.SpawnWeapon("1", "gadgets:FC3/Rock")
            local callbacks = CScriptCallbackSystem_GetInstance()
            callbacks:RegisterOnSpawnCallback(id, counts, "Spawn")
            callbacks:RegisterOnRemoveCallback(id, counts, "Remove")
            return id
        "#).eval().unwrap();
        story.update(0.1, View::default());
        let counts: Table = story.domino().lua().globals().get("counts").unwrap();
        assert_eq!(counts.get::<u32>("spawned").unwrap(), 0);
        let mut view = View::default();
        view.player_state.gadgets.push(Item { name: "gadgets:FC3/Rock".into(), count: 1, ..Default::default() });
        story.update(0.1, view.clone());
        story.update(0.1, view.clone());
        assert_eq!(counts.get::<u32>("spawned").unwrap(), 1);
        assert!(story.core.borrow().is_loaded(id.parse().unwrap()));
        story.update(0.1, View::default());
        assert_eq!(counts.get::<u32>("removed").unwrap(), 1);
        story.update(0.1, view);
        assert_eq!(counts.get::<u32>("spawned").unwrap(), 2);
    }

    #[test]
    fn spawn_callback_registered_after_arrival_is_deferred_and_delivered() {
        let mut story = empty_story();
        story.domino().lua().load(r#"
            target = { count = 0 }
            target._type = { Spawn = function(self)
                assert(self.subscription ~= nil)
                self.count = self.count + 1
            end }
            target.subscription = CScriptCallbackSystem_GetInstance():RegisterOnSpawnCallback("1", target, "Spawn")
        "#).exec().unwrap();
        let target: Table = story.domino().lua().globals().get("target").unwrap();
        assert_eq!(target.get::<u32>("count").unwrap(), 0);
        story.update(0.0, View::default());
        assert_eq!(target.get::<u32>("count").unwrap(), 1);
        assert_eq!(story.errors(), 0);
    }

    /// A synchronised scene's lead broken off (alerted) takes the scene's
    /// other part's user off it too (`CA_Interrupt`, and the host told).
    #[test]
    fn a_scene_lead_broken_off_frees_its_partner() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let mut data = StoryData::default();
        for (id, name) in [(900u64, "Relax/SyncAnimLeadActor.SyncAnimLeadActor_Test_1"), (901, "Relax/SyncAnimExtraActor.SyncAnimExtraActor_Test_1")] {
            data.entities.insert(
                id,
                data::Entry {
                    id,
                    name: name.into(),
                    class: "CEntity".into(),
                    pos: [0.0; 3],
                    angles: [0.0; 3],
                    layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
                    origin: data::Origin::Global,
                    components: Arc::new(Object { name_hash: name_hash("Components"), fields: vec![], children: vec![] }),
                    fields: Arc::new(Object { name_hash: name_hash("Entity"), fields: vec![], children: vec![] }),
                },
            );
        }
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        let mut view = View::default();
        view.spawned.insert(5, Live { alive: true, ..Default::default() });
        view.spawned.insert(6, Live { alive: true, ..Default::default() });
        story.update(0.0, view.clone());
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            SetAIFactEntityId("5", "DominoCAOverride", "900")
            SetAIFactEntityId("6", "DominoCAOverride", "901")
            local box = { _type = { Got = function(self, event, user) heard[#heard + 1] = event .. " " .. tostring(user) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("901", "CA_Interrupt", box, "Got")
        "#,
            )
            .exec()
            .unwrap();
        story.update(0.1, view.clone());
        story.take_commands();
        story.action_interrupted(5);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["CA_Interrupt 6"]);
        assert!(story.take_commands().contains(&Command::Action { pawn: 6, action: None }));
        assert_eq!(story.errors(), 0);
    }

    /// A script that finds an entity already there and deals with that
    /// itself before subscribing to its spawns (the entity status listener)
    /// hears of it once: the subscription isn't called at once as well.
    #[test]
    fn a_spawn_subscription_after_finding_the_entity_there_waits_for_a_spawn() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            target = { count = 0 }
            target._type = { Spawn = function(self) self.count = self.count + 1 end }
            if CAPI_Entity.IsEntityLoaded("1") then target.count = target.count + 1 end
            target.subscription = CScriptCallbackSystem_GetInstance():RegisterOnSpawnCallback("1", target, "Spawn")
        "#,
            )
            .exec()
            .unwrap();
        story.update(0.0, View::default());
        let target: Table = story.domino().lua().globals().get("target").unwrap();
        assert_eq!(target.get::<u32>("count").unwrap(), 1);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn entity_teleport_resolves_live_destination_and_updates_queries_immediately() {
        let mut story = empty_story();
        let mut view = View::default();
        view.spawned.insert(10, Live { pos: [1.0, 2.0, 3.0], alive: true, ..Default::default() });
        view.spawned.insert(20, Live { pos: [40.0, 50.0, 60.0], alive: true, ..Default::default() });
        story.core.borrow_mut().poses.insert(20, Pose { position: [0.0; 3], angles: [0.0, 0.0, 90.0] });
        story.update(0.0, view.clone());
        let values: (Vec<f32>, Vec<f32>) = story.domino().lua().load(r#"
            CAPI_Entity.TeleportEntity("10", "20", false, false)
            return CAPI_Entity.GetEntityPositionVec3("10"), CAPI_Entity.GetEntityAngles("10")
        "#).eval().unwrap();
        assert_eq!(values, (vec![40.0, 50.0, 60.0], vec![0.0, 0.0, 90.0]));
        assert_eq!(story.take_commands(), vec![Command::Teleport {
            entity: 10, pose: Pose { position: [40.0, 50.0, 60.0], angles: [0.0, 0.0, 90.0] },
        }]);
        // Once the host moves the actor, queries follow its live movement.
        view.spawned.get_mut(&10).unwrap().pos = [41.0, 50.0, 60.0];
        story.update(0.0, view);
        assert_eq!(story.core.borrow().position(10), Some([41.0, 50.0, 60.0]));
    }

    #[test]
    fn teleport_before_streaming_and_chained_teleports_keep_the_resolved_pose() {
        let mut story = empty_story();
        story.update(0.0, View { player: [100.0, 200.0, 12.0], player_yaw: 45.0, ..Default::default() });
        story.domino().lua().load(r#"
            CAPI_Entity.TeleportEntity("10", "1", false, false)
            CAPI_Entity.TeleportEntity("20", "10", false, false)
        "#).exec().unwrap();
        let pose = Pose { position: [100.0, 200.0, 12.0], angles: [0.0, 0.0, 45.0] };
        assert_eq!(story.take_commands(), vec![Command::Teleport { entity: 10, pose }, Command::Teleport { entity: 20, pose }]);
        assert_eq!(story.core.borrow().pose(20), Some(pose));
        story.domino().lua().load(r#"CAPI_Entity.TeleportEntity("30", "invalid", false, false)"#).exec().unwrap();
        assert!(story.take_commands().is_empty());
    }

    /// A companion told to lead is reported leading along its path; at its
    /// end the box listening hears `EndOfPathReached` and it waits.
    #[test]
    fn a_companion_leads_and_reports_its_paths_end() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local box = { _type = { OnDominoEvent = function(self, entity, event) heard[#heard + 1] = tostring(entity) .. " " .. event end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("40", box, "OnDominoEvent", "Domino_CompanionLead")
            ok = CAPI_Companion.CompanionSetLead("40", "41", 12)
        "#,
            )
            .exec()
            .unwrap();
        assert!(story.domino().lua().globals().get::<bool>("ok").unwrap());
        assert_eq!(story.companions(), [(40, Companion::Lead { path: 41, distance: 12.0 })]);
        story.companion_led(40);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["40 EndOfPathReached"]);
        assert_eq!(story.companions(), [(40, Companion::Wait)]);
    }

    /// An action point switched on is taken up by a free member of its
    /// group close enough; one too far off, or of another group, is left be.
    #[test]
    fn a_switched_on_action_is_taken_up_by_its_group() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let object = |name: &str, fields: Vec<(&str, Vec<u8>)>, children| {
            Arc::new(Object { name_hash: name_hash(name), fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(), children })
        };
        let entry = |id: u64, pos: [f32; 3], component: Arc<Object>| data::Entry {
            id,
            name: format!("e{id}"),
            class: "CEntity".into(),
            pos,
            angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: data::Origin::Global,
            components: object("Components", vec![], vec![component]),
            fields: object("Entity", vec![], vec![]),
        };
        let point = |group: u32| {
            object(
                "CAIContextualActionComponent",
                vec![("GroupFilter", group.to_le_bytes().to_vec())],
                vec![object("Idle", vec![("Radius", 5f32.to_le_bytes().to_vec())], vec![])],
            )
        };
        let mut data = StoryData::default();
        data.entities.insert(10, entry(10, [0.0, 0.0, 0.0], object("CPawn", vec![], vec![])));
        data.entities.insert(11, entry(11, [40.0, 0.0, 0.0], object("CPawn", vec![], vec![])));
        data.groups.insert(0xABC, vec![10, 11]);
        data.entities.insert(20, entry(20, [1.0, 1.0, 0.0], point(0xABC)));
        data.entities.insert(21, entry(21, [1.0, -1.0, 0.0], point(0xDEF)));
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story.domino().lua().load(r#"ToggleContextualActionState("21", true) ToggleContextualActionState("20", true)"#).exec().unwrap();
        let commands = story.update(0.0, View::default());
        let given: Vec<(u64, Option<u64>)> =
            commands.iter().filter_map(|c| if let Command::Action { pawn, action } = c { Some((*pawn, *action)) } else { None }).collect();
        assert_eq!(given, [(10, Some(20))]);
    }

    /// A member of its group who comes within reach after a point was
    /// switched on takes it up then (a guard arriving by truck).
    #[test]
    fn a_switched_on_action_waits_for_its_group_to_come() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let object = |name: &str, fields: Vec<(&str, Vec<u8>)>, children| {
            Arc::new(Object { name_hash: name_hash(name), fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(), children })
        };
        let entry = |id: u64, pos: [f32; 3], component: Arc<Object>| data::Entry {
            id,
            name: format!("e{id}"),
            class: "CEntity".into(),
            pos,
            angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: data::Origin::Global,
            components: object("Components", vec![], vec![component]),
            fields: object("Entity", vec![], vec![]),
        };
        let point = object(
            "CAIContextualActionComponent",
            vec![("GroupFilter", 0xABCu32.to_le_bytes().to_vec())],
            vec![object("Idle", vec![("Radius", 5f32.to_le_bytes().to_vec())], vec![])],
        );
        let mut data = StoryData::default();
        data.entities.insert(11, entry(11, [40.0, 0.0, 0.0], object("CPawn", vec![], vec![])));
        data.groups.insert(0xABC, vec![11]);
        data.entities.insert(20, entry(20, [0.0, 0.0, 0.0], point));
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story.domino().lua().load(r#"ToggleContextualActionState("20", true)"#).exec().unwrap();
        let given = |commands: Vec<Command>| -> Vec<(u64, Option<u64>)> {
            commands.iter().filter_map(|c| if let Command::Action { pawn, action } = c { Some((*pawn, *action)) } else { None }).collect()
        };
        assert!(given(story.update(0.5, View::default())).is_empty());
        let mut view = View::default();
        view.spawned.insert(11, Live { pos: [1.0, 0.0, 0.0], alive: true, ..Default::default() });
        assert_eq!(given(story.update(1.0, view)), [(11, Some(20))]);
    }

    /// The detection meter's changes reach the scripts listening on the
    /// player (`StealthIndicatorListener`), to hundredths, once each.
    #[test]
    fn the_detection_meter_is_told_as_it_changes() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local box = { _type = { OnValue = function(self, value) heard[#heard + 1] = value end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback(GetLocalPlayerId(), box, "OnValue", "CStealthIndicatorEvent")
        "#,
            )
            .exec()
            .unwrap();
        let mut view = View::default();
        story.update(0.1, view.clone());
        for value in [0.304, 0.3, 0.85, 1.2, 1.0] {
            view.player_state.stealth = value;
            story.update(0.1, view.clone());
        }
        let heard: Vec<f64> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, [0.3, 0.85, 1.0]);
    }

    /// A point ended is available again once its cooldown has run out (none
    /// set: at the next update), told once to those listening.
    #[test]
    fn an_ended_action_is_available_again_after_its_cooldown() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local box = { _type = { Got = function(self, event, user) heard[#heard + 1] = event .. " " .. tostring(user) end } }
            CEntityGroupManager_GetInstance():RegisterScriptEvent("60", "CA_Available", box, "Got")
        "#,
            )
            .exec()
            .unwrap();
        story.update(0.1, View::default());
        story.core.borrow_mut().action_event(60, "CA_End", 7);
        story.update(0.1, View::default());
        story.update(0.1, View::default());
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["CA_Available 7"]);
    }

    /// A prop broken is told to its `StaticBreakableListener`, once.
    #[test]
    fn a_broken_prop_is_told_once() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            broken = 0
            local box = { _type = { OnBreak = function(self) broken = broken + 1 end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("80", box, "OnBreak", "StaticBreakable_Break")
        "#,
            )
            .exec()
            .unwrap();
        story.destroyed(80);
        story.destroyed(80);
        assert_eq!(story.domino().lua().globals().get::<u32>("broken").unwrap(), 1);
    }

    /// A watched vehicle's health changing is told with its health and full
    /// health (`VehicleHealthListener_v2`); unchanged, it isn't.
    #[test]
    fn a_watched_vehicles_health_changes_are_told() {
        let mut story = empty_story();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local box = { _type = { OnHealth = function(self, health, full) heard[#heard + 1] = health .. "/" .. full end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("70", box, "OnHealth", "OnVehicleHealthChanged")
        "#,
            )
            .exec()
            .unwrap();
        let mut view = View::default();
        for health in [1000.0, 1000.0, 850.0, 850.0, 0.0] {
            view.spawned.insert(70, Live { alive: health > 0.0, health, max_health: 1000.0, ..Default::default() });
            story.update(0.1, view.clone());
        }
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["850/1000", "0/1000"]);
    }

    /// A collectible picked up is told on the player with its id (the
    /// `CollectibleListener` a POI waits on its relic with); another pickup
    /// isn't.
    #[test]
    fn a_collectible_picked_up_is_told_on_the_player() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let object = |name: &str, children| Arc::new(Object { name_hash: name_hash(name), fields: Default::default(), children });
        let entry = |id: u64, component: &str| data::Entry {
            id,
            name: format!("e{id}"),
            class: "CEntity".into(),
            pos: [0.0; 3],
            angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: data::Origin::Global,
            components: object("Components", vec![object(component, vec![])]),
            fields: object("Entity", vec![]),
        };
        let mut data = StoryData::default();
        data.entities.insert(50, entry(50, "CPickupCollectible"));
        data.entities.insert(51, entry(51, "CPickupAmmo"));
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story
            .domino()
            .lua()
            .load(
                r#"
            heard = {}
            local box = { _type = { OnCollected = function(self, id) heard[#heard + 1] = id end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback(GetLocalPlayerId(), box, "OnCollected", "CCollectibleCollectedEvent")
        "#,
            )
            .exec()
            .unwrap();
        story.pickup_picked(51);
        story.pickup_picked(50);
        let heard: Vec<String> = story.domino().lua().load("return heard").eval::<mlua::Table>().unwrap().sequence_values().map(Result::unwrap).collect();
        assert_eq!(heard, ["50"]);
    }

    /// A character placed under its scripts' control takes up by itself only
    /// its own post (where it stands and is placed), not another point it
    /// stands by, until its scripts let it go.
    #[test]
    fn a_held_character_takes_up_only_its_own_post() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let object = |name: &str, fields: Vec<(&str, Vec<u8>)>, children| {
            Arc::new(Object { name_hash: name_hash(name), fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(), children })
        };
        let entry = |id: u64, pos: [f32; 3], component: Arc<Object>| data::Entry {
            id,
            name: format!("e{id}"),
            class: "CEntity".into(),
            pos,
            angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: data::Origin::Global,
            components: object("Components", vec![], vec![component]),
            fields: object("Entity", vec![], vec![]),
        };
        let held = || {
            let agent = object("CPawnAgent", vec![("bStartOnScriptControl", vec![1])], vec![]);
            object("CFCXAIComponent", vec![], vec![object("AIObject", vec![], vec![agent])])
        };
        let point = || {
            object(
                "CAIContextualActionComponent",
                vec![("GroupFilter", 0xABCu32.to_le_bytes().to_vec())],
                vec![object("Idle", vec![("Radius", 5f32.to_le_bytes().to_vec())], vec![])],
            )
        };
        let mut data = StoryData::default();
        data.entities.insert(10, entry(10, [0.0, 0.0, 0.0], held()));
        data.entities.insert(11, entry(11, [40.0, 0.0, 0.0], held()));
        data.groups.insert(0xABC, vec![10, 11]);
        data.entities.insert(20, entry(20, [0.5, 0.0, 0.0], point()));
        data.entities.insert(21, entry(21, [30.0, 0.0, 0.0], point()));
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story.domino().lua().load(r#"ToggleContextualActionState("20", true) ToggleContextualActionState("21", true)"#).exec().unwrap();
        let given = |commands: Vec<Command>| -> Vec<(u64, Option<u64>)> {
            commands.iter().filter_map(|c| if let Command::Action { pawn, action } = c { Some((*pawn, *action)) } else { None }).collect()
        };
        let mut view = View::default();
        view.spawned.insert(10, Live { pos: [0.0, 0.0, 0.0], alive: true, ..Default::default() });
        view.spawned.insert(11, Live { pos: [30.5, 0.0, 0.0], alive: true, ..Default::default() });
        assert_eq!(given(story.update(0.0, view.clone())), [(10, Some(20))]);
        story.domino().lua().load(r#"CAPI_AI.AcquireControl("11", false)"#).exec().unwrap();
        assert_eq!(given(story.update(1.5, view)), [(11, Some(21))]);
    }

    /// A look-at trigger placed on a character where it spawns rides with
    /// it: a script keeping the character in sight sees it where the
    /// character has walked to, and the player looking there sees it.
    #[test]
    fn a_look_at_trigger_on_a_character_follows_it() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let object = |name: &str, fields: Vec<(&str, Vec<u8>)>, children| {
            Arc::new(Object { name_hash: name_hash(name), fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(), children })
        };
        let entry = |id: u64, pos: [f32; 3], component: Arc<Object>| data::Entry {
            id,
            name: format!("e{id}"),
            class: "CEntity".into(),
            pos,
            angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: data::Origin::Global,
            components: object("Components", vec![], vec![component]),
            fields: object("Entity", vec![], vec![]),
        };
        let look = object(
            "CLookAtTriggerComponent",
            vec![("bEnabled", vec![1]), ("fRange", 40f32.to_le_bytes().to_vec()), ("fFOVPercentage", 1f32.to_le_bytes().to_vec())],
            vec![],
        );
        let mut data = StoryData::default();
        data.entities.insert(100, entry(100, [10.0, 10.0, 0.0], object("CPawn", vec![], vec![])));
        data.entities.insert(200, entry(200, [10.3, 10.2, 1.6], look));
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story
            .domino()
            .lua()
            .load(
                r#"
            local box = { _type = { Left = function(self) end, Came = function(self) end } }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("200", box, "Came", "EnterFOV")
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("200", box, "Left", "LeaveFOV")
        "#,
            )
            .exec()
            .unwrap();
        // He has walked 20 m east; the player stands 10 m behind, looking at him.
        let mut view = View { player: [20.0, 10.0, 0.0], look: [1.0, 0.0, 0.0], fov: 60.0, ..View::default() };
        view.spawned.insert(100, Live { pos: [30.0, 10.0, 0.0], alive: true, health: 1.0, max_health: 1.0, ai_state: 0 });
        story.update(0.1, view);
        let tails = story.tail_targets();
        assert_eq!(tails.len(), 1);
        let (id, at, seen) = tails[0];
        assert_eq!(id, 200);
        assert!((at[0] - 30.3).abs() < 1e-4 && (at[1] - 10.2).abs() < 1e-4 && (at[2] - 1.6).abs() < 1e-4, "{at:?}");
        assert!(seen);
    }

    #[test]
    fn mission_use_target_respects_enable_use_and_view_restrictions() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let object = |name: &str, fields: Vec<(&str, Vec<u8>)>, children| Arc::new(Object {
            name_hash: name_hash(name), fields: fields.into_iter().map(|(n, v)| (name_hash(n), v)).collect(), children,
        });
        let restrictions = object("UseRestrictions", vec![("bIsInsideToUse", vec![0]), ("bLookAtCheck", vec![1]), ("bAngleCheck", vec![1])], vec![]);
        let prox = object("CProximityTriggerComponent", vec![
            ("bEnabled", vec![0]), ("bUsable", vec![0]),
            ("vectorSize", [2.0f32; 3].into_iter().flat_map(f32::to_le_bytes).collect()),
            ("sUsageString", b"Use test item\0".to_vec()),
        ], vec![restrictions]);
        let mut data = StoryData::default();
        data.entities.insert(100, data::Entry {
            id: 100, name: "Test trigger".into(), class: "CEntity".into(), pos: [0.0, 0.0, 1.0], angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER, origin: data::Origin::Global,
            components: object("Components", vec![], vec![prox]), fields: object("Entity", vec![], vec![]),
        });
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story.domino().lua().load(r#"
            target = { uses = 0 }
            target._type = { Use = function(self, user) assert(user == "1"); self.uses = self.uses + 1 end }
            CScriptCallbackSystem_GetInstance():RegisterEventCallback("100", target, "Use", "Use")
        "#).exec().unwrap();
        let (player, eye, forward) = ([0.0, -1.8, 1.0], [0.0, -1.8, 1.75], [0.0, 1.0, -0.4]);
        assert!(story.use_target(player, eye, forward).is_none());
        story.domino().lua().load(r#"
            SendRegisteredEventToEntity("100", "CBaseTriggerComponent_SetEnabled", {})
            SendRegisteredEventToEntity("100", "CProximityTriggerComponent_SetAsUsable", {})
        "#).exec().unwrap();
        let target = story.use_target(player, eye, forward).unwrap();
        assert_eq!((target.entity, target.text.as_str()), (100, "Use test item"));
        assert!(story.use_target([0.0, -8.0, 1.0], eye, forward).is_none());
        assert!(story.use_target(player, eye, [0.0, -1.0, 0.0]).is_none());
        story.domino().lua().load(r#"
            SendRegisteredEventToEntity("100", "CProximityTriggerComponent_SetNotCheckLookAt", {})
            SendRegisteredEventToEntity("100", "CProximityTriggerComponent_SetNotCheckAngle", {})
        "#).exec().unwrap();
        assert!(story.use_target(player, eye, [0.0, -1.0, 0.0]).is_some());
        story.entity_event(target.entity, "Use", vec![Arg::id(PLAYER)]);
        let listener: Table = story.domino().lua().globals().get("target").unwrap();
        assert_eq!(listener.get::<u32>("uses").unwrap(), 1);
        story.domino().lua().load(r#"SendRegisteredEventToEntity("100", "CProximityTriggerComponent_SetAsUnusable", {})"#).exec().unwrap();
        assert!(story.use_target(player, eye, forward).is_none());
    }

    #[test]
    fn player_only_boundary_events_ignore_npcs_moving_or_streaming_out() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let region = Arc::new(Object {
            name_hash: name_hash("CTrigger2DComponent"),
            fields: vec![(0xB3C7_40E6, b"PlayerOnly\0".to_vec())], children: vec![],
        });
        let points = [[-5.0f32, -5.0, 0.0], [5.0, -5.0, 0.0], [5.0, 5.0, 0.0], [-5.0, 5.0, 0.0]];
        let mut shape = 4u32.to_le_bytes().to_vec();
        shape.extend(points.into_iter().flatten().flat_map(f32::to_le_bytes));
        let mut data = StoryData::default();
        data.entities.insert(100, data::Entry {
            id: 100, name: "Player boundary".into(), class: "CBasicRegionEntity".into(),
            pos: [0.0; 3], angles: [0.0; 3], layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            origin: data::Origin::Global,
            components: Arc::new(Object { children: vec![region], ..Default::default() }),
            fields: Arc::new(Object { fields: vec![
                (name_hash("hidShapePoints"), shape), (name_hash("fHeight"), (-1.0f32).to_le_bytes().to_vec()),
            ], ..Default::default() }),
        });
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        story.domino().lua().load(r#"
            boundary = { entered = 0, left = 0 }
            boundary._type = {
                Enter = function(self, id) assert(id == "1"); self.entered = self.entered + 1 end,
                Leave = function(self, id) assert(id == "1"); self.left = self.left + 1 end,
            }
            local callbacks = CScriptCallbackSystem_GetInstance()
            callbacks:RegisterEventCallback("100", boundary, "Enter", "Enter")
            callbacks:RegisterEventCallback("100", boundary, "Leave", "Leave")
        "#).exec().unwrap();
        let mut view = View::default();
        view.spawned.insert(20, Live { pos: [0.0; 3], alive: true, ..Default::default() });
        view.contacts.insert(100, vec![20]);
        story.update(0.1, view.clone());
        assert_eq!(story.core.borrow().contacts(100), vec![PLAYER]);
        let regions = story.watched_exit_regions();
        assert_eq!(regions.len(), 1);
        assert!(regions[0].contains([0.0; 3]));
        assert!(!regions[0].contains([50.0, 0.0, 0.0]));
        view.spawned.get_mut(&20).unwrap().pos = [50.0, 0.0, 0.0];
        view.contacts.clear();
        story.update(0.1, view.clone());
        view.spawned.clear();
        story.update(0.1, view.clone());
        let boundary: Table = story.domino().lua().globals().get("boundary").unwrap();
        assert_eq!((boundary.get::<u32>("entered").unwrap(), boundary.get::<u32>("left").unwrap()), (1, 0));
        view.player = [50.0, 0.0, 0.0];
        story.update(0.1, view);
        assert_eq!(boundary.get::<u32>("left").unwrap(), 1);
        assert_eq!(story.errors(), 0);
        story.domino().lua().load(r#"SendRegisteredEventToEntity("100", "CBaseTriggerComponent_SetDisabled", {})"#).exec().unwrap();
        assert!(story.watched_exit_regions().is_empty());
    }

    #[test]
    fn proximity_contacts_respect_the_authored_nonplayer_filter() {
        use fc3_formats::{fcb::Object, hash::name_hash};
        use std::sync::Arc;
        let mut data = StoryData::default();
        for (id, allow) in [(100, false), (200, true)] {
            let prox = Arc::new(Object {
                name_hash: name_hash("CProximityTriggerComponent"), fields: vec![
                    (name_hash("bAllowTriggerByNonPlayers"), vec![u8::from(allow)]),
                    (name_hash("vectorSize"), [10.0f32; 3].into_iter().flat_map(f32::to_le_bytes).collect()),
                ], children: vec![],
            });
            data.entities.insert(id, data::Entry {
                id, name: "Test proximity".into(), class: "CEntity".into(), pos: [0.0; 3], angles: [0.0; 3],
                layer: fc3_formats::world::DEFAULT_MISSION_LAYER, origin: data::Origin::Global,
                components: Arc::new(Object { children: vec![prox], ..Default::default() }), fields: Arc::new(Object::default()),
            });
        }
        let mut story = Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap();
        let mut view = View::default();
        view.spawned.insert(20, Live { pos: [0.0; 3], alive: true, ..Default::default() });
        view.contacts.insert(100, vec![20]);
        story.update(0.1, view.clone());
        assert_eq!(story.core.borrow().contacts(100), vec![PLAYER]);
        assert_eq!(story.core.borrow().contacts(200), vec![PLAYER, 20]);
        view.player = [50.0, 0.0, 0.0];
        story.update(0.1, view.clone());
        assert!(story.core.borrow().contacts(100).is_empty());
        assert_eq!(story.core.borrow().contacts(200), vec![20]);
        let target = story.domino().lua().create_table().unwrap();
        target.set("colliderFilterId", "20").unwrap();
        story.core.borrow_mut().listeners.insert(999, crate::core::Listener {
            on: crate::core::Listen::Entity(100, "Enter".into()),
            callback: crate::core::Callback { target, method: "Enter".into() },
        });
        // Named mission pawns still require a physical overlap. The listener
        // does not permit unrelated NPCs or affect other trigger volumes.
        assert!(story.trigger_selects(100, 20));
        assert!(!story.trigger_selects(100, 30));
        assert!(!story.trigger_selects(200, 20));
        view.contacts.clear();
        view.spawned.insert(30, Live { pos: [0.0; 3], alive: true, ..Default::default() });
        story.core.borrow_mut().view = view.clone();
        assert_eq!(story.core.borrow().contacts(100), vec![20]);
        view.spawned.get_mut(&20).unwrap().pos = [50.0, 0.0, 0.0];
        story.core.borrow_mut().view = view.clone();
        assert!(story.core.borrow().contacts(100).is_empty());
        view.spawned.get_mut(&20).unwrap().pos = [0.0; 3];
        story.core.borrow_mut().view = view;
        story.core.borrow_mut().listeners.remove(&999);
        assert!(!story.trigger_selects(100, 20));
        assert!(story.core.borrow().contacts(100).is_empty());
    }
}
