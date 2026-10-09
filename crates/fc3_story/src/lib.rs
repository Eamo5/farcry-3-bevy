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

/// A story entity the host has in the world, as it is now.
#[derive(Debug, Clone, Copy, Default)]
pub struct Live {
    /// Dunia space.
    pub pos: [f32; 3],
    pub alive: bool,
    pub health: f32,
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
    /// Unlocked skills (lower-case names).
    pub skills: HashSet<String>,
    pub on_zipline: bool,
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
    errors: usize,
}

impl Story {
    /// Loads the scripts and story data from a cache.
    pub fn open(cache: &Path) -> Result<Story> {
        let pack = ScriptPack::from_bytes(&std::fs::read(cache.join(DOMINO_PACK_REL))?)?;
        let data = StoryData::load(cache)?;
        Story::new(pack, data)
    }

    pub fn new(pack: ScriptPack, data: StoryData) -> Result<Story> {
        let domino = Domino::new(Pack(pack)).map_err(|e| anyhow::anyhow!("{e}"))?;
        let core = Rc::new(RefCell::new(Core::new(Rc::new(data))));
        natives::register(&domino, &core).map_err(|e| anyhow::anyhow!("{e}"))?;
        domino.lua().load(include_str!("glue.lua")).set_name("glue").exec().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Story { domino, core, instances: HashMap::new(), loaded_scripts: HashSet::new(), errors: 0 })
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
    pub fn timers(&self) -> Vec<Timer> {
        self.core.borrow().media.timers.iter().filter(|(_, t)| t.shown).map(|(&id, t)| Timer {
            id, label: t.label.clone(), seconds: t.value,
            increasing: t.increasing, append_time: t.append_time,
        }).collect()
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
            // Stance changes are events on the player.
            let stance = view.player_state.stance;
            if c.view.player_state.stance != stance && c.time > 0.0 {
                let event = ["new_stance_stand", "new_stance_crouch", "new_stance_swim"][(stance as usize).min(2)];
                c.raise(&Listen::Entity(PLAYER, event.into()), vec![Arg::id(PLAYER)]);
            }
            c.view = view;
            for (id, was_loaded) in before {
                let loaded = c.is_loaded(id);
                if loaded != was_loaded {
                    c.raise(&if loaded { Listen::Spawn(id) } else { Listen::Remove(id) }, vec![]);
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
            // The world streamed in round a teleported player.
            if c.world_ready_at.is_some_and(|at| c.time >= at) {
                c.world_ready_at = None;
                c.raise(&Listen::Generic("DominoGameReady".into()), vec![]);
            }
            natives::world::update_triggers(&mut c);
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

    /// An event about an entity for the scripts listening on the groups it
    /// belongs to (`Die`, `CCounterEvent`, `CA_End`, `Alarm_Activated`,
    /// ...): callbacks get the event name, then `args`.
    pub fn group_event(&mut self, entity: u64, event: &str, args: Vec<Arg>) {
        {
            let mut c = self.core.borrow_mut();
            let groups: Vec<String> = c
                .listeners
                .values()
                .filter_map(|l| match &l.on {
                    Listen::Group(g, e) if e == event || e.is_empty() => Some(g.clone()),
                    _ => None,
                })
                .collect();
            let mut all = vec![Arg::Str(event.to_string())];
            all.extend(args);
            for g in groups {
                if c.groups(&values::Text(g.clone())).contains(&entity) {
                    c.raise_where(|l| matches!(l, Listen::Group(lg, e) if *lg == g && (e == event || e.is_empty())), all.clone());
                }
            }
        }
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

    /// The nearest usable mission trigger within reach, respecting the
    /// script's use/inside/look/angle restrictions and current placement.
    pub fn use_target(&self, player: [f32; 3], eye: [f32; 3], look: [f32; 3]) -> Option<UseTarget> {
        let c = self.core.borrow();
        let ids: std::collections::BTreeSet<u64> = c.listeners.values().filter_map(|l| match &l.on {
            Listen::Entity(id, event) if event == "Use" => Some(*id),
            _ => None,
        }).collect();
        ids.into_iter().filter_map(|id| {
            if !c.is_loaded(id) || !c.trigger_on(id) {
                return None;
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
            let text = prox.u32("locidDisplayInteractionString").and_then(|id| c.data.strings.get(id)).map(str::to_string)
                .or_else(|| prox.str("sUsageString").filter(|s| !s.is_empty())).unwrap_or_else(|| "Interact".into());
            Some(UseTarget { entity: id, position: e.pos, text })
        }).min_by(|a, b| {
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
        self.core.borrow_mut().dead.insert(id);
        self.group_event(id, "Die", vec![Arg::id(PLAYER), Arg::Num(0.0), Arg::id(id), Arg::Str(String::new())]);
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
        }
        self.pump();
    }

    /// A character finished its contextual action.
    pub fn action_finished(&mut self, pawn: u64) {
        {
            let mut c = self.core.borrow_mut();
            let Some(cur) = c.ca.current.remove(&pawn) else { return };
            c.action_event(cur.action, "CA_End", pawn);
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
        }
        self.pump();
    }

    /// The specific gameplays running (minigames, QTEs): (id, name).
    pub fn gameplays(&self) -> Vec<(u64, String)> {
        self.core.borrow().gameplays.iter().map(|(i, n)| (*i, n.clone())).collect()
    }

    /// A specific gameplay the host ran has ended.
    pub fn gameplay_ended(&mut self, id: u64) {
        self.core.borrow_mut().end_gameplay(id);
        self.pump();
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

    /// A sound scripts started has ended (`reason` as the engine gives it:
    /// 0 finished, 1 stopped).
    pub fn sound_finished(&mut self, id: u64, reason: f64) {
        let cb = self.core.borrow_mut().media.sounds.remove(&id);
        if let Some(cb) = cb {
            self.core.borrow_mut().call(cb, vec![Arg::Num(reason)]);
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

    /// The triggers scripts wait on now: (trigger, event, a point inside,
    /// enabled), for tools that drive the story without a world.
    pub fn watched_triggers(&self) -> Vec<(u64, String, [f32; 3], bool)> {
        let c = self.core.borrow();
        let mut out = Vec::new();
        for l in c.listeners.values() {
            let Listen::Entity(t, e) = &l.on else { continue };
            if !matches!(e.as_str(), "Enter" | "Use" | "EnterFOV") || !c.is_loaded(*t) {
                continue;
            }
            let Some(entry) = c.data.entities.get(t) else { continue };
            let point = match triggers::shape_of(entry) {
                Some(triggers::Shape::Prism { points, z_min, .. }) => {
                    let n = points.len() as f32;
                    let (x, y) = points.iter().fold((0.0, 0.0), |a, p| (a.0 + p[0] / n, a.1 + p[1] / n));
                    [x, y, if z_min > f32::MIN { z_min + 1.0 } else { entry.pos[2] }]
                }
                _ => entry.pos,
            };
            out.push((*t, e.clone(), point, c.trigger_on(*t)));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        out.dedup();
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

    /// Every layer whose entities are in the world now.
    pub fn present_layers(&self) -> Vec<u64> {
        self.core.borrow().layers.iter().filter(|(_, s)| s.present()).map(|(l, _)| *l).collect()
    }

    /// (name, state) of the main missions registered so far.
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

fn in_sectors(e: &data::Entry, sectors: Option<&HashSet<u32>>) -> bool {
    match (e.origin, sectors) {
        (data::Origin::Sector(id), Some(s)) => s.contains(&id),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_story() -> Story {
        Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), StoryData::default()).unwrap()
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
        story.update(0.1, view);
        assert!(story.core.borrow().contacts(100).is_empty());
        assert_eq!(story.core.borrow().contacts(200), vec![20]);
    }
}
