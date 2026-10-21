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

/// How long a move a script plays on a pawn runs (the move tables' own
/// lengths aren't read).
pub const MOVE_SECS: f64 = 1.5;

/// Whether an event a script listens for on a pawn can be one of a move's
/// markups: not one the engine raises itself (dialogue, triggers, doors,
/// `C...Event`s).
fn is_markup(event: &str) -> bool {
    let engine_event = event.starts_with("Domino_")
        || (event.starts_with('C') && event.chars().nth(1).is_some_and(|c| c.is_ascii_uppercase()))
        || matches!(event, "Use" | "Enter" | "Leave" | "Die" | "Push" | "Pull" | "Closed" | "EnterFOV" | "LeaveFOV");
    !engine_event
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
    /// Base blocks' own layers, by block (`standing_layers`).
    pub standing: HashMap<u64, Vec<u64>>,
    pub view: View,
    /// What the host should do.
    pub out: Vec<Command>,
    pub actions: VecDeque<Action>,
    next_id: u64,
    pub listeners: BTreeMap<u64, Listener>,
    pub delays: BTreeMap<u64, Delay>,
    /// Alarm boxes that have changed state: (state, who), the state as
    /// `CAPI_Entity.GetAlarmState` gives it (0 idle, 1 active, 2 disabled,
    /// 3 destroyed).
    pub alarms: HashMap<u64, (u8, u64)>,
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
    pub media: crate::natives::media::Media,
    /// Stand-in ids for inventory items scripts hold.
    pub item_ids: HashMap<u64, String>,
    /// Entities scripts spawned (archetype by id).
    pub spawned: HashMap<u64, String>,
    /// The move each pawn plays at a script's word (`CAPI_Pawn.PlayAnimation`)
    /// and how long it has left.
    pub moves: HashMap<u64, (String, f64)>,
    /// Markup events due on pawns (seconds left, pawn, event).
    pub markups: Vec<(f64, u64, String)>,
    /// Contextual actions' markups due (seconds left, action, user, event).
    pub action_markups: Vec<(f64, u64, u64, String)>,
    /// How many of each watched group's live members were in each AI state
    /// (relaxed, investigating, alerted, in combat) when last looked.
    pub group_states: HashMap<String, [usize; 4]>,
    /// Characters scripts gave a battlefield (`SetBattlefields`): fighting
    /// there, whatever they are doing.
    pub battlefields: HashMap<u64, u64>,
    /// Characters getting up from a contextual action switched off under
    /// them (seconds left, action, pawn): its end comes when they're up.
    pub leaving: Vec<(f64, u64, u64)>,
    /// Listeners registered during the script step being run.
    pub step_listeners: Vec<u64>,
    /// Entities the script step being run was told are loaded
    /// (`IsEntityLoaded`).
    pub told_loaded: HashSet<u64>,
    /// Group membership scripts changed: (group text, entity) added / removed.
    pub group_added: HashSet<(String, u64)>,
    pub group_removed: HashSet<(String, u64)>,
    /// Who was inside each watched trigger last frame.
    pub trigger_contacts: HashMap<u64, Vec<u64>>,
    /// Triggers scripts enabled or disabled (others as placed).
    pub trigger_enabled: HashMap<u64, bool>,
    /// Look-at triggers the player sees now.
    pub looked_at: HashSet<u64>,
    /// The character each look-at trigger rides on (`triggers::carrier_of`),
    /// found once.
    pub look_carriers: HashMap<u64, Option<(u64, [f32; 3])>>,
    /// When the world will be ready around the player after a teleport
    /// (`DominoGameReady` then).
    pub world_ready_at: Option<f64>,
    /// When the player's new pawn will be ready after a switch
    /// (`LocalPlayerRespawnedReady` then).
    pub pawn_ready_at: Option<f64>,
    /// Where the player was when triggers were last tested.
    pub last_player: Option<[f32; 3]>,
    /// Specific gameplays running (minigames, QTEs, boss fights): id ->
    /// name.
    pub gameplays: BTreeMap<u64, String>,
    /// Those of them that play a QTE move (`QTE_move_file`), whatever their
    /// name (JAS_150's escape from the ropes): the host's QTE runner ends
    /// them.
    pub qte_gameplays: std::collections::BTreeSet<u64>,
    /// RealTree growths the host is running (`SetRealTreeGrowthSpeed`): id
    /// -> what to call when the tree reaches its size.
    pub growths: BTreeMap<u64, Option<Callback>>,
    pub dialogs: crate::natives::dialog::Dialogs,
    /// Script control requests the engine answers next frame.
    pub control_answers: Vec<(u64, String)>,
    /// Characters scripts have taken from their AI or given back
    /// (`CAPI_AI.AcquireControl`); the rest as placed (`script_held`).
    pub script_control: HashMap<u64, bool>,
    /// Characters that died.
    pub dead: HashSet<u64>,
    /// The contextual actions characters are on.
    pub ca: crate::natives::ai::Actions,
    /// What each companion was last told (`CAPI_Companion`).
    pub companions: HashMap<u64, crate::Companion>,
    /// Where the player comes back after a reload (the last checkpoint's
    /// spawn point).
    pub respawn: Option<u64>,
    /// Objective markers shown (marker entity -> sub type).
    pub objective_markers: BTreeMap<u64, String>,
    /// Entities bound to others (`CAPI_Entity.Bind`): child -> parent.
    pub binds: HashMap<u64, u64>,
    /// Scripted poses for entities not currently reported by the host.
    pub poses: HashMap<u64, crate::Pose>,
    /// Runtime overrides of proximity-trigger use restrictions.
    pub use_rules: HashMap<u64, crate::triggers::UseRules>,
    pub vehicle_rules: HashMap<u64, crate::vehicles::Rules>,
    pub vehicle_departures: HashMap<u64, crate::vehicles::Occupant>,
    /// Pawns a script has made usable (`SetUsability`: an activity's keeper,
    /// a mission giver), and whether they're used whichever way the player
    /// faces.
    pub usable_pawns: HashMap<u64, bool>,
}

impl Core {
    pub fn new(data: Rc<StoryData>) -> Core {
        let safehouses_unlocked = data.entities.values().filter(|e| {
            e.component("CSafeHouseComponent").is_some_and(|c| !c.bool("bLocked").unwrap_or(true))
        }).map(|e| e.id).collect();
        let ca = crate::natives::ai::Actions::from_data(&data);
        Core {
            data,
            standing: HashMap::new(),
            view: View::default(),
            out: Vec::new(),
            actions: VecDeque::new(),
            next_id: 1,
            listeners: BTreeMap::new(),
            delays: BTreeMap::new(),
            alarms: HashMap::new(),
            ticks: BTreeMap::new(),
            missions: Missions::default(),
            layers: HashMap::new(),
            blocks: HashMap::new(),
            running: HashSet::new(),
            session: HashMap::new(),
            safehouses_unlocked,
            radio_towers_unlocked: HashSet::new(),
            unlocked_tags: HashSet::new(),
            removed: HashSet::new(),
            log: Vec::new(),
            time: 0.0,
            game_ready: false,
            play_game: false,
            media: Default::default(),
            item_ids: HashMap::new(),
            spawned: HashMap::new(),
            moves: HashMap::new(),
            markups: Vec::new(),
            action_markups: Vec::new(),
            group_states: HashMap::new(),
            battlefields: HashMap::new(),
            leaving: Vec::new(),
            step_listeners: Vec::new(),
            told_loaded: HashSet::new(),
            group_added: HashSet::new(),
            group_removed: HashSet::new(),
            trigger_contacts: HashMap::new(),
            trigger_enabled: HashMap::new(),
            looked_at: HashSet::new(),
            look_carriers: HashMap::new(),
            world_ready_at: None,
            pawn_ready_at: None,
            last_player: None,
            gameplays: BTreeMap::new(),
            qte_gameplays: Default::default(),
            growths: BTreeMap::new(),
            dialogs: Default::default(),
            control_answers: Vec::new(),
            script_control: HashMap::new(),
            dead: HashSet::new(),
            ca,
            companions: HashMap::new(),
            respawn: None,
            objective_markers: BTreeMap::new(),
            binds: HashMap::new(),
            poses: HashMap::new(),
            use_rules: HashMap::new(),
            vehicle_rules: HashMap::new(),
            vehicle_departures: HashMap::new(),
            usable_pawns: HashMap::new(),
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn listen(&mut self, on: Listen, callback: Callback) -> u64 {
        let id = self.next_id();
        self.listeners.insert(id, Listener { on, callback });
        self.step_listeners.push(id);
        id
    }

    /// A pawn plays a move (`CAPI_Pawn.PlayAnimation`): it runs `MOVE_SECS`,
    /// and the markup events scripts registered on the pawn in the step that
    /// started it (the move's own, like a bomb leaving the hand) come halfway.
    pub fn play_move(&mut self, pawn: u64, name: &str) {
        self.moves.insert(pawn, (name.to_string(), MOVE_SECS));
        let markups: Vec<String> = self
            .step_listeners
            .iter()
            .filter_map(|id| match self.listeners.get(id).map(|l| &l.on) {
                Some(Listen::Entity(p, event)) if *p == pawn && is_markup(event) => Some(event.clone()),
                _ => None,
            })
            .collect();
        for event in markups {
            self.markups.push((MOVE_SECS * 0.5, pawn, event));
        }
    }

    /// The move a pawn is playing (until it ends).
    pub fn current_move(&self, pawn: u64) -> Option<&str> {
        self.moves.get(&pawn).filter(|m| m.1 > 0.0).map(|m| m.0.as_str())
    }

    /// Moves run on; their markups come when due.
    pub fn update_moves(&mut self, dt: f64) {
        for m in self.moves.values_mut() {
            m.1 -= dt;
        }
        self.moves.retain(|_, m| m.1 > 0.0);
        let mut due = Vec::new();
        self.markups.retain_mut(|m| {
            m.0 -= dt;
            if m.0 <= 0.0 {
                due.push((m.1, m.2.clone()));
            }
            m.0 > 0.0
        });
        for (pawn, event) in due {
            self.raise(&Listen::Entity(pawn, event), vec![]);
        }
        let mut up = Vec::new();
        self.leaving.retain_mut(|l| {
            l.0 -= dt;
            if l.0 <= 0.0 {
                up.push((l.1, l.2));
            }
            l.0 > 0.0
        });
        for (action, pawn) in up {
            self.action_event(action, "CA_End", pawn);
        }
        // A contextual action's markups, while its user is still on it.
        let mut marked = Vec::new();
        self.action_markups.retain_mut(|m| {
            m.0 -= dt;
            if m.0 <= 0.0 {
                marked.push((m.1, m.2, std::mem::take(&mut m.3)));
            }
            m.0 > 0.0
        });
        for (action, pawn, event) in marked {
            if self.ca.current.get(&pawn).is_some_and(|c| c.action == action && c.started) {
                self.action_event(action, &event, pawn);
            }
        }
    }

    /// A pawn's state machine enters a state: the callbacks waiting for a
    /// state of that name (its path's last part, as `::Main Avatar/Common/Stun`
    /// ends in `Stun`).
    pub fn enter_state(&mut self, pawn: u64, leaf: &str) {
        self.raise_where(
            |l| matches!(l, Listen::State(p, s) if *p == pawn && s.rsplit('/').next().is_some_and(|last| last.eq_ignore_ascii_case(leaf))),
            vec![],
        );
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
        // (A base block's own layer comes in with any of its others.)
        if state.present() {
            let own = self.data.layers.get(&layer).and_then(|l| self.standing.get(&l.block)).cloned().unwrap_or_default();
            for l in own.into_iter().filter(|&l| l != layer) {
                if !self.layer_state(l).present() {
                    self.set_layer(l, LayerState::Active);
                }
            }
        }
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
        // Spawned by the engine at a script's word (reinforcements): there
        // until removed.
        if self.spawned.contains_key(&id) {
            return true;
        }
        if let Some(name) = self.item_ids.get(&id) {
            let p = &self.view.player_state;
            return p.weapons.iter().chain(&p.gadgets).any(|i| i.count > 0 && i.name.eq_ignore_ascii_case(name));
        }
        match self.data.layer_of(id) {
            Some(layer) => self.layer_state(layer).present(),
            None => false,
        }
    }

    /// Where an entity is now (the host's live position, else its placement;
    /// bound entities ride on what they are bound to).
    pub fn position(&self, id: u64) -> Option<[f32; 3]> {
        if id == crate::PLAYER {
            return Some(self.view.player);
        }
        if let Some(&parent) = self.binds.get(&id) {
            if parent != id {
                if let Some(p) = self.view.spawned.get(&parent).map(|l| l.pos) {
                    return Some([p[0], p[1], p[2] + 1.2]);
                }
            }
        }
        self.view.spawned.get(&id).map(|l| l.pos).or_else(|| self.poses.get(&id).map(|p| p.position)).or_else(|| self.data.position(id))
    }

    pub fn angles(&self, id: u64) -> [f32; 3] {
        if id == crate::PLAYER {
            return [0.0, 0.0, self.view.player_yaw];
        }
        self.poses.get(&id).map(|p| p.angles)
            .or_else(|| self.data.entities.get(&id).map(|e| e.angles))
            .or_else(|| self.data.spawn_points.get(&id).map(|s| s.angles))
            .unwrap_or_default()
    }

    pub fn pose(&self, id: u64) -> Option<crate::Pose> {
        Some(crate::Pose { position: self.position(id)?, angles: self.angles(id) })
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
        // Blocks share world state through referenced blocks (a mission's
        // `*_common` characters and props): active while any block using
        // them is.
        for r in b.refs.iter().copied().filter(|&r| r != block) {
            let wanted = match state {
                LayerState::Active => LayerState::Active,
                LayerState::Enabled => continue,
                _ => {
                    let still_used = self
                        .blocks
                        .iter()
                        .any(|(other, s)| *other != block && *s == LayerState::Active && self.data.blocks.get(other).is_some_and(|o| o.refs.contains(&r)));
                    if still_used {
                        continue;
                    }
                    state
                }
            };
            if self.blocks.get(&r) != Some(&wanted) {
                self.set_block(r, wanted);
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

    /// Whether a trigger is on (as placed, or as scripts set it).
    pub fn trigger_on(&self, id: u64) -> bool {
        self.trigger_enabled.get(&id).copied().unwrap_or_else(|| self.data.entities.get(&id).is_some_and(crate::triggers::starts_enabled))
    }

    /// Pawns explicitly selected by live proximity listeners.
    pub fn selected_trigger_pawns(&self, trigger: u64) -> Vec<u64> {
        self.listeners.values().filter(|l| matches!(&l.on, Listen::Entity(id, event) if *id == trigger && matches!(event.as_str(), "Enter" | "Leave" | "Use")))
            .filter_map(|l| l.callback.target.get::<crate::values::Text>("colliderFilterId").ok())
            .flat_map(|group| self.groups(&group)).collect()
    }

    /// Who is inside a trigger now: what the host reports, and the player
    /// when inside its volume.
    pub fn contacts(&self, trigger: u64) -> Vec<u64> {
        if !self.trigger_on(trigger) || !self.is_loaded(trigger) {
            return Vec::new();
        }
        let entry = self.data.entities.get(&trigger);
        // Some authored mission proximity volumes disable generic NPC
        // activation, yet their live Lua listener explicitly selects a pawn
        // (e.g. the opening rock-distraction guard). Let those named pawns
        // reach the listener's own filter, without admitting unrelated NPCs
        // or relaxing PlayerOnly mission-boundary regions.
        let selected: Vec<u64> = if entry.is_some_and(|e| e.component("CProximityTriggerComponent").is_some() && e.component("CTrigger2DComponent").is_none()) {
            self.selected_trigger_pawns(trigger)
        } else { Vec::new() };
        let allows = |id: u64| selected.contains(&id) || entry.is_none_or(|e| crate::triggers::allows_entity(e, id));
        let mut out: Vec<u64> = self.view.contacts.get(&trigger).into_iter().flatten().copied().filter(|id| allows(*id)).collect();
        let Some(shape) = entry.and_then(crate::triggers::shape_of) else { return out };
        // Bodies, not points; the player's since last frame (thin triggers
        // across a path are crossed between frames).
        use crate::triggers::{BODY_HALF_HEIGHT as H, BODY_RADIUS as R};
        let p = self.view.player;
        // (A jump of more than a few metres is a teleport: only where it lands.)
        let from = self.last_player.filter(|l| (0..3).map(|i| (l[i] - p[i]).powi(2)).sum::<f32>() < 9.0).unwrap_or(p);
        if !out.contains(&crate::PLAYER) && shape.holds_along(from, self.view.player, R, H) {
            out.push(crate::PLAYER);
        }
        // The characters the host has in the world, by where they are now
        // (their positions are their feet).
        let mut chars: Vec<u64> = self
            .view
            .spawned
            .iter()
            .filter(|(id, l)| allows(**id) && l.alive && !out.contains(id) && shape.holds([l.pos[0], l.pos[1], l.pos[2] + H], R, H))
            .map(|(id, _)| *id)
            .collect();
        chars.sort_unstable();
        out.extend(chars);
        out
    }

    /// A specific gameplay ended: its box hears `OnDestroyed` with its id.
    pub fn end_gameplay(&mut self, id: u64) {
        self.qte_gameplays.remove(&id);
        if let Some(name) = self.gameplays.remove(&id) {
            self.emit(Command::Native { name: "DestroySpecificGameplay".into(), args: vec![id.to_string(), name] });
            self.raise(&Listen::Generic("OnDestroyed".into()), vec![Arg::Str(id.to_string())]);
        }
    }

    /// The host gets a command.
    pub fn emit(&mut self, c: Command) {
        self.out.push(c);
    }
}
