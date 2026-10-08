//! The runtime state behind the natives: the queue of work for the scripts,
//! callback registrations, timers and the world state the mission system
//! keeps (layers, missions, safehouses).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;

use mlua::Table;

use crate::data::StoryData;
use crate::missions::Missions;
use crate::values::Arg;
use crate::{Command, View};

/// A script method to call back: a box (instance or class table) and the
/// name of one of its class's methods.
#[derive(Clone)]
pub struct Callback {
    pub target: Table,
    pub method: String,
}

/// What a registration listens for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Listen {
    /// An event an entity raises (`Enter`, `Domino_DialogFinished`, ...).
    Entity(u64, String),
    /// A named global event (`DominoGameReady`, `PlayGame`, ...).
    Generic(String),
    /// A message on the bus.
    Message(String),
    /// An entity streaming in / out.
    Spawn(u64),
    Remove(u64),
    /// A mission's `Completed` / `Failed`.
    Mission(String, String),
    /// A group event (`Die`, `CCounterEvent`, ...), for every member.
    Group(String, String),
    /// A pawn entering a state-machine state.
    State(u64, String),
    /// An input signal on the player.
    Input(String),
    /// A debug console command.
    Console(String),
    /// A HUD text's events.
    HudText(u64, String),
}

pub struct Listener {
    pub on: Listen,
    pub callback: Callback,
}

/// A one-shot timer (`CDominoDelayManager`): created stopped, counted from
/// zero on "start"/"restart".
pub struct Delay {
    pub seconds: f64,
    pub elapsed: f64,
    pub running: bool,
    pub callback: Callback,
}

/// Work for the scripts, done outside any native call.
pub enum Action {
    /// Call a script method with arguments.
    Call(Callback, Vec<Arg>),
    /// Create a graph on an entity and call its entry points in order.
    Start { script: String, owner: u64, prefab: Option<u64>, inputs: Vec<(String, crate::data::InputValue)>, entries: Vec<&'static str> },
    /// Shut down the graph running on an entity.
    Stop(u64),
}

/// A mission layer's state, as scripts number them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LayerState {
    /// Removed.
    Disabled = 0,
    /// Precached, not spawned.
    Enabled = 1,
    /// Spawned.
    Active = 2,
    /// Shut down; what is spawned stays until it streams out.
    Deactivated = 3,
}

impl LayerState {
    pub fn from_code(n: i64) -> Option<LayerState> {
        Some(match n {
            0 => LayerState::Disabled,
            1 => LayerState::Enabled,
            2 => LayerState::Active,
            3 => LayerState::Deactivated,
            _ => return None,
        })
    }

    /// Whether the layer's entities exist in the world.
    pub fn present(self) -> bool {
        matches!(self, LayerState::Active | LayerState::Deactivated)
    }
}

pub struct Core {
    pub data: Rc<StoryData>,
    pub view: View,
    /// What the host should do.
    pub out: Vec<Command>,
    pub actions: VecDeque<Action>,
    next_id: u64,
    pub listeners: BTreeMap<u64, Listener>,
    pub delays: BTreeMap<u64, Delay>,
    pub ticks: BTreeMap<u64, Callback>,
    pub missions: Missions,
    pub layers: HashMap<u64, LayerState>,
    /// Block states (as layer state codes) set by scripts or missions.
    pub blocks: HashMap<u64, LayerState>,
    /// Graph instances running on entities (by owner).
    pub running: HashSet<u64>,
    /// Session flags (`MissionSetSessionGlobal`), kept across reloads.
    pub session: HashMap<String, bool>,
    pub safehouses_unlocked: HashSet<u64>,
    pub radio_towers_unlocked: HashSet<u64>,
    pub unlocked_tags: HashSet<String>,
    /// Entities removed by scripts.
    pub removed: HashSet<u64>,
    /// Lines for the log (warnings, traces).
    pub log: Vec<String>,
    /// Seconds since the scripts started.
    pub time: f64,
    pub game_ready: bool,
    pub play_game: bool,
}

impl Core {
    pub fn new(data: Rc<StoryData>) -> Core {
        Core {
            data,
            view: View::default(),
            out: Vec::new(),
            actions: VecDeque::new(),
            next_id: 1,
            listeners: BTreeMap::new(),
            delays: BTreeMap::new(),
            ticks: BTreeMap::new(),
            missions: Missions::default(),
            layers: HashMap::new(),
            blocks: HashMap::new(),
            running: HashSet::new(),
            session: HashMap::new(),
            safehouses_unlocked: HashSet::new(),
            radio_towers_unlocked: HashSet::new(),
            unlocked_tags: HashSet::new(),
            removed: HashSet::new(),
            log: Vec::new(),
            time: 0.0,
            game_ready: false,
            play_game: false,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn listen(&mut self, on: Listen, callback: Callback) -> u64 {
        let id = self.next_id();
        self.listeners.insert(id, Listener { on, callback });
        id
    }

    pub fn unlisten(&mut self, id: u64) {
        self.listeners.remove(&id);
    }

    /// Queues the callbacks listening for something.
    pub fn raise(&mut self, on: &Listen, args: Vec<Arg>) -> usize {
        let hits: Vec<Callback> = self.listeners.values().filter(|l| &l.on == on).map(|l| l.callback.clone()).collect();
        let n = hits.len();
        for cb in hits {
            self.actions.push_back(Action::Call(cb, args.clone()));
        }
        n
    }

    /// Queues the callbacks of every listener matching a test.
    pub fn raise_where(&mut self, test: impl Fn(&Listen) -> bool, args: Vec<Arg>) -> usize {
        let hits: Vec<Callback> = self.listeners.values().filter(|l| test(&l.on)).map(|l| l.callback.clone()).collect();
        let n = hits.len();
        for cb in hits {
            self.actions.push_back(Action::Call(cb, args.clone()));
        }
        n
    }

    pub fn call(&mut self, callback: Callback, args: Vec<Arg>) {
        self.actions.push_back(Action::Call(callback, args));
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.log.push(text.into());
    }

    pub fn layer_state(&self, layer: u64) -> LayerState {
        if layer == fc3_formats::world::DEFAULT_MISSION_LAYER {
            return LayerState::Active;
        }
        self.layers.get(&layer).copied().unwrap_or(LayerState::Disabled)
    }

    /// Sets a layer's state, telling the host and raising spawn/remove
    /// callbacks for the entities scripts wait on.
    pub fn set_layer(&mut self, layer: u64, state: LayerState) {
        let before = self.layer_state(layer);
        if before == state {
            return;
        }
        self.layers.insert(layer, state);
        self.out.push(Command::Layer { layer, state });
        if before.present() != state.present() {
            let watched: Vec<u64> = self
                .listeners
                .values()
                .filter_map(|l| match l.on {
                    Listen::Spawn(e) | Listen::Remove(e) => Some(e),
                    _ => None,
                })
                .filter(|e| self.data.layer_of(*e) == Some(layer))
                .collect();
            for e in watched {
                let on = if state.present() { Listen::Spawn(e) } else { Listen::Remove(e) };
                self.raise(&on, vec![]);
            }
        }
    }

    /// Whether an entity exists in the world now (its layer is present and
    /// no script removed it).
    pub fn is_loaded(&self, id: u64) -> bool {
        if id == crate::PLAYER {
            return true;
        }
        if self.removed.contains(&id) {
            return false;
        }
        if self.view.spawned.contains_key(&id) {
            return true;
        }
        match self.data.layer_of(id) {
            Some(layer) => self.layer_state(layer).present(),
            None => false,
        }
    }

    /// Where an entity is now (the host's live position, else its placement).
    pub fn position(&self, id: u64) -> Option<[f32; 3]> {
        if id == crate::PLAYER {
            return Some(self.view.player);
        }
        self.view.spawned.get(&id).map(|l| l.pos).or_else(|| self.data.position(id))
    }

    /// Sets a block's state (0 disable, 1 enable, 2 activate, 3 deactivate)
    /// and with it its layers; activating starts the block's zone script.
    pub fn set_block(&mut self, block: u64, state: LayerState) {
        let Some(b) = self.data.blocks.get(&block).cloned() else {
            self.warn(format!("unknown mission block {block}"));
            return;
        };
        self.blocks.insert(block, state);
        for l in &b.layers {
            let Some(layer) = self.data.layers.get(l).cloned() else { continue };
            match state {
                LayerState::Disabled => self.set_layer(layer.id, LayerState::Disabled),
                LayerState::Enabled => {
                    if layer.on_activate && !self.layer_state(layer.id).present() {
                        self.set_layer(layer.id, LayerState::Enabled);
                    }
                }
                LayerState::Active => {
                    if layer.on_activate {
                        self.set_layer(layer.id, LayerState::Active);
                    }
                }
                LayerState::Deactivated => {
                    if layer.disable_on_deactivate {
                        self.set_layer(layer.id, LayerState::Disabled);
                    } else if self.layer_state(layer.id) == LayerState::Active {
                        self.set_layer(layer.id, LayerState::Deactivated);
                    }
                }
            }
        }
        if let Some(zone) = b.zone {
            if state == LayerState::Active {
                self.start_entity_script(zone);
            } else if self.running.contains(&zone) {
                self.actions.push_back(Action::Stop(zone));
            }
        }
    }

    /// Starts the graph of an entity's Domino component (once).
    pub fn start_entity_script(&mut self, entity: u64) -> bool {
        if self.running.contains(&entity) {
            return false;
        }
        let Some(e) = self.data.entities.get(&entity) else { return false };
        let Some(c) = e.component("CDominoComponent") else { return false };
        let Some(script) = crate::data::script_path(c) else { return false };
        let prefab = c.u64("hidParentPrefabId").filter(|&p| p != u64::MAX && p != 0);
        let inputs = crate::data::script_inputs(c);
        self.running.insert(entity);
        self.actions.push_back(Action::Start { script, owner: entity, prefab, inputs, entries: vec!["In"] });
        true
    }

    /// The host gets a command.
    pub fn emit(&mut self, c: Command) {
        self.out.push(c);
    }
}
