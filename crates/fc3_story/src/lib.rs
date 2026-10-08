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
}

/// What the scripts may read of the world, refreshed by the host.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// The player's position, Dunia space (Z up).
    pub player: [f32; 3],
    /// Heading in degrees (Dunia yaw).
    pub player_yaw: f32,
    pub player_dead: bool,
    pub player_in_vehicle: bool,
    /// Story entities the host has spawned (NPCs, props), by id.
    pub spawned: HashMap<u64, Live>,
    /// Hours (0..24).
    pub time_of_day: f32,
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
    /// A safehouse or outpost unlocked / locked.
    SafeHouse {
        id: u64,
        unlocked: bool,
        silent: bool,
    },
    /// A message on the bus (for host systems that react to them).
    Message(String),
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

    /// Starts a new game: the blocks active from the start, then the master
    /// graph.
    pub fn boot(&mut self) {
        {
            let mut c = self.core.borrow_mut();
            let init = c.data.active_on_init.clone();
            for b in init {
                c.set_block(b, LayerState::Active);
            }
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
        {
            let mut c = self.core.borrow_mut();
            c.view = view;
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
            natives::world::update_triggers(&mut c);
        }
        self.pump();
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
