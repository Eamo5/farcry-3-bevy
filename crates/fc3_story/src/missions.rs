//! The mission manager (the engine's `CMissionManager`), behind the mission
//! flow boxes.
//!
//! Missions are registered by the master graph's chapters (`Mission_v7`
//! boxes) with the path of their main graph; a mission's main graph
//! registers its blocks (checkpoints) the same way. States follow the
//! engine's: Locked -> Running -> Completed / Failed. When a mission starts
//! running, its main graph is created on an entity of its own and scanned
//! then entered; when a block starts running, its layers are activated and
//! the script of its zone entity starts. Completion and failure come back to
//! the boxes listening on the mission (`RegisterDominoEvent`).

use std::collections::HashMap;

use crate::core::{Action, Core, LayerState, Listen};
use crate::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Invalid,
    Locked,
    Enabled,
    Running,
    Completed,
    Failed,
    Hidden,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Invalid => "Invalid",
            State::Locked => "Locked",
            State::Enabled => "Enabled",
            State::Running => "Running",
            State::Completed => "Completed",
            State::Failed => "Failed",
            State::Hidden => "Hidden",
        }
    }

    pub fn parse(s: &str) -> State {
        match s {
            "Locked" => State::Locked,
            "Enabled" => State::Enabled,
            "Running" => State::Running,
            "Completed" => State::Completed,
            "Failed" => State::Failed,
            "Hidden" => State::Hidden,
            _ => State::Invalid,
        }
    }
}

/// A block registered by a mission's main graph.
#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub block: u64,
    /// The mission whose graph registered it.
    pub parent: Option<String>,
    pub spawn: Option<u64>,
    pub end_spawn: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Mission {
    /// The id scripts hold.
    pub id: String,
    pub script: String,
    /// The main graph's name (`DOC_020_MAIN`), how rewards and saves know it.
    pub name: String,
    pub giver: bool,
    pub type_name: String,
    pub objective: Option<u32>,
    pub spawn: Option<u64>,
    pub exclusive: bool,
    pub title_screen: bool,
    pub end_screen: bool,
    pub state: State,
    pub acknowledged: bool,
    /// The entity its graph runs on.
    pub owner: u64,
    pub block: Option<BlockInfo>,
    /// The mission's current objective text.
    pub current_objective: Option<u32>,
    /// Travel permission owned by this graph, not by whichever graph ran last.
    pub fast_travel: bool,
}

/// Saved mission progress, by mission name (or block id).
#[derive(Debug, Clone, Default)]
pub struct Saved {
    pub missions: HashMap<String, (State, bool)>,
}

#[derive(Default)]
pub struct Missions {
    pub by_id: HashMap<String, Mission>,
    pub by_owner: HashMap<u64, String>,
    /// Owners for main graphs (synthetic entity ids).
    next_owner: u64,
    pub chapter: i32,
    pub givers: HashMap<String, String>,
    pub saved: Saved,
    pub fast_travel_disabled: bool,
    pub abort_enabled: bool,
}

/// Synthetic entity ids for the graphs that run on no world entity (mission
/// main graphs): far from any editor id.
pub const OWNER_BASE: u64 = 0x7FFF_0000_0000_0000;

/// A main graph's name from its path: `.../JAS_060_MAIN.JAS_060_MAIN.lua`
/// -> `JAS_060_MAIN`.
pub fn script_name(path: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = file.strip_suffix(".lua").or_else(|| file.strip_suffix(".LUA")).unwrap_or(file);
    stem.rsplit('.').next().unwrap_or(stem).to_ascii_uppercase()
}

impl Missions {
    pub fn fast_travel_enabled(&self) -> bool {
        !self.fast_travel_disabled && self.by_id.values().filter(|m| m.state == State::Running).all(|m| {
            // An active block's MissionMiscInfos overrides its main graph's
            // default (JAS_080_MAIN forbids travel, B30 permits it). Other
            // concurrently running missions keep their own restrictions.
            m.fast_travel || self.by_id.values().any(|b| b.state == State::Running
                && b.block.as_ref().and_then(|b| b.parent.as_deref()) == Some(&m.id))
        })
    }

    pub fn get(&self, id: &str) -> Option<&Mission> {
        self.by_id.get(id)
    }

    pub fn state(&self, id: &str) -> State {
        self.by_id.get(id).map(|m| m.state).unwrap_or(State::Invalid)
    }

    /// The mission (or block) whose graph runs on an entity.
    pub fn by_owner(&self, owner: u64) -> Option<&Mission> {
        self.by_owner.get(&owner).and_then(|id| self.by_id.get(id))
    }

    /// The main mission an owner's graph belongs to (a block's parent).
    pub fn main_of(&self, owner: u64) -> Option<&Mission> {
        let m = self.by_owner(owner)?;
        match m.block.as_ref().and_then(|b| b.parent.as_ref()) {
            Some(parent) => self.by_id.get(parent),
            None => Some(m),
        }
    }

    pub fn by_name(&self, name: &str) -> Option<&Mission> {
        let n = name.to_ascii_uppercase();
        self.by_id.values().find(|m| m.block.is_none() && m.name == n)
    }

    /// The running main missions.
    pub fn running(&self) -> impl Iterator<Item = &Mission> {
        self.by_id.values().filter(|m| m.block.is_none() && m.state == State::Running)
    }

    fn new_owner(&mut self) -> u64 {
        self.next_owner += 1;
        OWNER_BASE + self.next_owner
    }
}

impl Core {
    /// `RegisterMission` / `RegisterMissionGiver`: a mission by its main
    /// graph (once; registering again returns the same id).
    #[allow(clippy::too_many_arguments)]
    pub fn register_mission(
        &mut self,
        script: &str,
        giver: bool,
        objective: Option<u32>,
        spawn: Option<u64>,
        exclusive: bool,
        title_screen: bool,
        end_screen: bool,
        type_name: &str,
    ) -> String {
        let id = fc3_domino::path_id(script);
        if self.missions.by_id.contains_key(&id) {
            return id;
        }
        let name = script_name(script);
        let (state, acknowledged) = self.missions.saved.missions.get(&name).copied().unwrap_or((State::Locked, false));
        let owner = self.missions.new_owner();
        self.missions.by_owner.insert(owner, id.clone());
        self.missions.by_id.insert(
            id.clone(),
            Mission {
                id: id.clone(),
                script: script.to_string(),
                name,
                giver,
                type_name: type_name.to_string(),
                objective,
                spawn,
                exclusive,
                title_screen,
                end_screen,
                state,
                acknowledged,
                owner,
                block: None,
                current_objective: None,
                fast_travel: true,
            },
        );
        if state == State::Running {
            self.start_mission_graph(&id);
        }
        id
    }

    /// `RegisterMissionBlock(owner, blockId, script, spawn, endSpawn)`: a
    /// block of the mission whose graph runs on `owner`.
    pub fn register_block(&mut self, owner: u64, block: u64, script: &str, spawn: Option<u64>, end_spawn: Option<u64>) -> String {
        let id = block.to_string();
        if self.missions.by_id.contains_key(&id) {
            return id;
        }
        let parent = self.missions.by_owner.get(&owner).cloned();
        let zone = self.data.blocks.get(&block).and_then(|b| b.zone);
        let block_owner = match zone {
            Some(z) => z,
            None => self.missions.new_owner(),
        };
        let (state, acknowledged) = self.missions.saved.missions.get(&id).copied().unwrap_or((State::Locked, false));
        self.missions.by_owner.insert(block_owner, id.clone());
        self.missions.by_id.insert(
            id.clone(),
            Mission {
                id: id.clone(),
                script: script.to_string(),
                name: self.data.blocks.get(&block).map(|b| b.name.to_ascii_uppercase()).unwrap_or_else(|| script_name(script)),
                giver: false,
                type_name: String::new(),
                objective: None,
                spawn: None,
                exclusive: false,
                title_screen: false,
                end_screen: false,
                state,
                acknowledged,
                owner: block_owner,
                block: Some(BlockInfo { block, parent, spawn, end_spawn }),
                current_objective: None,
                fast_travel: true,
            },
        );
        // A block a save left running plays again (the mission's graph may
        // only start blocks as the one before completes).
        if state == State::Running {
            self.activate_block_mission(block, block_owner);
        }
        id
    }

    /// `SetMissionState`: the state machine's transitions and what they set
    /// off.
    pub fn set_mission_state(&mut self, id: &str, state: State) {
        let Some(m) = self.missions.by_id.get_mut(id) else {
            self.warn(format!("SetMissionState on unregistered mission {id}"));
            return;
        };
        let before = m.state;
        m.state = state;
        let block = m.block.clone();
        let owner = m.owner;
        let name = m.name.clone();
        if before == state {
            // Running again (a reload's flow): make sure it plays.
            if state == State::Running {
                match &block {
                    Some(b) => self.activate_block_mission(b.block, owner),
                    None => self.start_mission_graph(id),
                }
            }
            return;
        }
        self.emit(Command::MissionState { name: name.clone(), state: state.name().to_string(), block: block.as_ref().map(|b| b.block) });
        match state {
            State::Running => match &block {
                Some(b) => self.activate_block_mission(b.block, owner),
                None => {
                    self.start_mission_graph(id);
                    if self.missions.by_id.get(id).is_some_and(|m| m.exclusive) {
                        self.raise(&Listen::Message("OnExclusiveMissionStart".into()), vec![]);
                    }
                }
            },
            State::Enabled => {
                if let Some(b) = &block {
                    self.set_block(b.block, LayerState::Enabled);
                }
            }
            State::Locked | State::Completed | State::Failed | State::Hidden | State::Invalid => {
                if before == State::Running {
                    if let Some(b) = &block {
                        self.set_block(b.block, LayerState::Deactivated);
                    } else {
                        // The main graph stays: its outputs still wire the
                        // chapter's flow (it is shut down with the save).
                        if self.missions.by_id.get(id).is_some_and(|m| m.exclusive) {
                            self.raise(&Listen::Message("OnExclusiveMissionStop".into()), vec![]);
                        }
                    }
                    if self.running.contains(&owner) && block.is_some() {
                        self.actions.push_back(Action::Stop(owner));
                    }
                }
            }
        }
    }

    fn activate_block_mission(&mut self, block: u64, owner: u64) {
        self.set_block(block, LayerState::Active);
        // A block without a zone entity: its script runs on its own owner.
        if self.data.blocks.get(&block).is_none_or(|b| b.zone.is_none()) && !self.running.contains(&owner) {
            if let Some(m) = self.missions.by_owner(owner).cloned() {
                self.running.insert(owner);
                self.actions.push_back(Action::Start { script: m.script, owner, prefab: None, inputs: vec![], entries: vec!["In"] });
            }
        }
    }

    /// Creates a main mission's graph on its owner: scanned (registering its
    /// blocks), then entered.
    pub fn start_mission_graph(&mut self, id: &str) {
        let Some(m) = self.missions.by_id.get(id) else { return };
        if m.block.is_some() || self.running.contains(&m.owner) {
            return;
        }
        let (script, owner) = (m.script.clone(), m.owner);
        self.running.insert(owner);
        self.actions.push_back(Action::Start { script, owner, prefab: None, inputs: vec![], entries: vec!["Scan", "In"] });
    }

    /// The end of a mission or block, with its listeners told.
    pub fn finish_mission(&mut self, id: &str, state: State) {
        self.set_mission_state(id, state);
        let event = if state == State::Completed { "Completed" } else { "Failed" };
        self.raise(&Listen::Mission(id.to_string(), event.to_string()), vec![]);
    }

    /// `MissionBlockComplete(owner, save)`.
    pub fn block_complete(&mut self, owner: u64, save: bool) {
        let Some(m) = self.missions.by_owner(owner).cloned() else {
            self.warn(format!("MissionBlockComplete from {owner}, which runs no block"));
            return;
        };
        self.finish_mission(&m.id, State::Completed);
        if let Some(end) = m.block.as_ref().and_then(|b| b.end_spawn) {
            self.respawn = Some(end);
            self.emit(Command::RespawnPoint(end));
        }
        if save {
            self.emit(Command::Checkpoint);
        }
    }

    /// `MissionBlockFail(owner, reload, reason)`.
    pub fn block_fail(&mut self, owner: u64, reload: bool, reason: Option<u32>) {
        let text = reason.and_then(|r| self.data.strings.get(r)).map(str::to_string);
        self.emit(Command::MissionFailed { text, reload });
        if let Some(m) = self.missions.by_owner(owner).cloned() {
            if !reload {
                self.finish_mission(&m.id, State::Failed);
            }
        }
    }

    /// `MissionComplete(owner, text)`: the end of a main mission.
    pub fn mission_complete(&mut self, owner: u64, text: Option<u32>) {
        let Some(m) = self.missions.main_of(owner).cloned() else {
            self.warn(format!("MissionComplete from {owner}, which runs no mission"));
            return;
        };
        let reward = self.data.rewards.get(&m.name.to_ascii_lowercase()).copied().unwrap_or_default();
        let title = m.objective.and_then(|o| self.data.strings.get(o)).map(str::to_string);
        self.emit(Command::MissionComplete {
            name: m.name.clone(),
            title,
            text: text.and_then(|t| self.data.strings.get(t)).map(str::to_string),
            xp: reward.xp,
            money: reward.money,
            screen: m.end_screen,
        });
        self.finish_mission(&m.id, State::Completed);
        self.emit(Command::Checkpoint);
    }

    /// `MissionFail(owner, text)`.
    pub fn mission_fail(&mut self, owner: u64, text: Option<u32>) {
        let Some(m) = self.missions.main_of(owner).cloned() else { return };
        let text = text.and_then(|t| self.data.strings.get(t)).map(str::to_string);
        self.emit(Command::MissionFailed { text, reload: false });
        self.finish_mission(&m.id, State::Failed);
    }

    /// `MissionAcknowledged(owner, save)`: the player has taken the mission
    /// on (fail conditions and zone warnings apply from now).
    pub fn acknowledge(&mut self, owner: u64, save: bool) {
        let Some(m) = self.missions.by_owner(owner).cloned() else { return };
        let main = m.block.as_ref().and_then(|b| b.parent.clone()).unwrap_or(m.id.clone());
        for id in [m.id.clone(), main] {
            if let Some(x) = self.missions.by_id.get_mut(&id) {
                x.acknowledged = true;
            }
        }
        if save {
            self.emit(Command::Checkpoint);
        }
    }

    pub fn is_acknowledged(&self, owner: u64) -> bool {
        self.missions.main_of(owner).is_some_and(|m| m.acknowledged) || self.missions.by_owner(owner).is_some_and(|m| m.acknowledged)
    }

    /// `SetMissionObjective(owner, text, ...)`.
    pub fn set_objective(&mut self, owner: u64, text: Option<u32>, play_complete: bool) {
        let main = self.missions.main_of(owner).map(|m| m.id.clone());
        if let Some(id) = &main {
            if let Some(m) = self.missions.by_id.get_mut(id) {
                m.current_objective = text;
            }
        }
        let s = text.and_then(|t| self.data.strings.get(t)).map(str::to_string);
        self.emit(Command::Objective { text: s, id: text, play_complete });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_paths() {
        assert_eq!(script_name("Domino/user/fc3/FC3_MainMissions/JAS_060/JAS_060_MAIN.JAS_060_MAIN.lua"), "JAS_060_MAIN");
        assert_eq!(script_name(r"domino\user\fc3\x\doc_020_brief.doc_020_brief.lua"), "DOC_020_BRIEF");
    }
}
