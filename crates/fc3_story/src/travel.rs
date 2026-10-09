//! Safehouse travel uses the marker's authored spawn point, never an outpost
//! centre or an objective marker's approximate position.

use crate::core::Listen;
use crate::missions::State;
use crate::{Pose, Story};

#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub safehouse: u64,
    pub marker: u64,
    pub name: String,
    pub position: [f32; 3],
    pub arrival: Pose,
    pub unlocked: bool,
}

impl Story {
    pub fn fast_travel_enabled(&self) -> bool {
        let c = self.core.borrow();
        c.missions.fast_travel_enabled() && !c.view.player_dead && !c.view.player_in_vehicle
    }

    pub fn travel_destinations(&self) -> Vec<Destination> {
        let c = self.core.borrow();
        let mut destinations: Vec<_> = c.data.entities.values().filter_map(|e| {
            let safehouse = e.component("CSafeHouseComponent")?;
            if !c.is_loaded(e.id) { return None; }
            let marker = c.data.markers.get(&safehouse.u64("entMarkerEntity")?)?;
            let spawn = c.data.spawn_points.get(&marker.spawn_point?)?;
            if !c.is_loaded(marker.id) || !c.is_loaded(spawn.id) { return None; }
            Some(Destination {
                safehouse: e.id, marker: marker.id,
                name: c.data.strings.get(marker.tooltip).filter(|s| !s.is_empty()).unwrap_or(&e.name).to_string(),
                position: marker.pos,
                arrival: Pose { position: spawn.pos, angles: spawn.angles },
                unlocked: c.safehouses_unlocked.contains(&e.id),
            })
        }).collect();
        destinations.sort_by_key(|d| d.marker);
        destinations
    }

    pub fn fast_travel_tutorial(&self) -> bool {
        self.core.borrow().missions.by_id.values().any(|m| m.state == State::Running && m.name.eq_ignore_ascii_case("JAS_080_B30"))
    }

    /// Compatibility for liberated host outposts missing an authored safehouse
    /// association. It cannot complete the Amanaki tutorial. Authored markers
    /// always take precedence over this approximate destination in the map.
    pub fn report_outpost_travel(&mut self, pose: Pose) -> bool {
        if !self.fast_travel_enabled() { return false; }
        let mut c = self.core.borrow_mut();
        c.view.player = pose.position;
        c.view.player_yaw = pose.angles[2];
        c.last_player = None;
        c.world_ready_at = Some(c.time + 1.0);
        true
    }

    /// Commit a validated map selection. The host must place the player at the
    /// returned pose before resuming simulation. Rejected requests have no
    /// callbacks, respawn changes, or other side effects.
    pub fn fast_travel(&mut self, marker: u64) -> Option<Pose> {
        if !self.fast_travel_enabled() { return None; }
        let destination = self.travel_destinations().into_iter().find(|d| d.marker == marker && d.unlocked)?;
        let tutorial = self.fast_travel_tutorial() && self.data().markers.get(&marker)
            .is_some_and(|m| m.name.eq_ignore_ascii_case("amanakitown_safehousemarker_01"));
        {
            let mut c = self.core.borrow_mut();
            c.view.player = destination.arrival.position;
            c.view.player_yaw = destination.arrival.angles[2];
            c.last_player = None;
            c.world_ready_at = Some(c.time + 1.0);
            c.respawn = c.data.markers.get(&marker).and_then(|m| m.spawn_point);
            // FC3.dll's map handler (0x1106e81e, then 0x1106e82d) starts
            // tutorial travel before broadcasting CRC32 0x860fdf42. This is
            // an engine/UI event, not a substitute for selecting a destination.
            if tutorial {
                c.raise(&Listen::Message("jas_080_fast_travel".into()), vec![]);
            }
        }
        self.pump();
        Some(destination.arrival)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use fc3_formats::fcb::Object;
    use fc3_formats::hash::name_hash;
    use fc3_formats::scripts::ScriptPack;
    use crate::data::{Entry, Marker, Origin, SpawnPoint, StoryData};
    use crate::{Pose, Story};

    fn fixture() -> Story {
        let mut data = StoryData::default();
        let safehouse = Object { name_hash: name_hash("CSafeHouseComponent"), fields: vec![
            (name_hash("bLocked"), vec![0]), (name_hash("entMarkerEntity"), 20_u64.to_le_bytes().to_vec()),
        ], ..Default::default() };
        let components = Object { children: vec![Arc::new(safehouse)], ..Default::default() };
        data.entities.insert(10, Entry { id: 10, name: "Safehouse".into(), class: String::new(), pos: [0.0; 3], angles: [0.0; 3],
            layer: fc3_formats::world::DEFAULT_MISSION_LAYER, origin: Origin::Global, components: components.into(), fields: Arc::new(Object::default()) });
        data.markers.insert(20, Marker { id: 20, name: "amanakitown_safehousemarker_01".into(), layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            pos: [1.0; 3], kind: 0, sub_kind: 0, enabled: true, tooltip: 0, spawn_point: Some(30), area: None });
        data.spawn_points.insert(30, SpawnPoint { id: 30, name: "Arrival".into(), layer: fc3_formats::world::DEFAULT_MISSION_LAYER,
            pos: [2.0, 3.0, 4.0], angles: [0.0, 0.0, 54.0] });
        Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), data).unwrap()
    }

    #[test]
    fn travel_uses_spawn_pose_and_rejects_locked_unknown_and_disabled_requests() {
        let mut story = fixture();
        assert_eq!(story.travel_destinations().len(), 1);
        assert!(story.travel_destinations()[0].unlocked);
        assert!(story.fast_travel(999).is_none());
        story.domino().lua().load("LockSafeHouse('10')").exec().unwrap();
        assert!(story.fast_travel(20).is_none());
        assert!(story.respawn_point().is_none());
        story.domino().lua().load("UnlockSafeHouse('10', true); SetFastTravelEnabled(false)").exec().unwrap();
        assert!(story.fast_travel(20).is_none());
        story.domino().lua().load("SetFastTravelEnabled(true)").exec().unwrap();
        assert_eq!(story.fast_travel(20), Some(Pose { position: [2.0, 3.0, 4.0], angles: [0.0, 0.0, 54.0] }));
        assert_eq!(story.respawn_point(), Some(30));
    }

    #[test]
    fn mission_travel_locks_are_owned_and_only_running_graphs_block_travel() {
        let story = fixture();
        let (one, two) = {
            let mut c = story.core.borrow_mut();
            let one = c.register_mission("one.lua", false, None, None, false, false, false, "");
            let two = c.register_mission("two.lua", false, None, None, false, false, false, "");
            for id in [&one, &two] { c.missions.by_id.get_mut(id).unwrap().state = crate::missions::State::Running; }
            (c.missions.by_id[&one].owner, c.missions.by_id[&two].owner)
        };
        story.domino().lua().load(format!("CMissionManager.GetInstance():EnableFastTravel('{one}', false); CMissionManager.GetInstance():EnableFastTravel('{two}', true)")).exec().unwrap();
        assert!(!story.fast_travel_enabled());
        story.core.borrow_mut().missions.by_id.values_mut().find(|m| m.owner == one).unwrap().state = crate::missions::State::Completed;
        assert!(story.fast_travel_enabled());
        story.domino().lua().load("SetFastTravelEnabled(false)").exec().unwrap();
        assert!(!story.fast_travel_enabled());
    }

    #[test]
    fn tutorial_callback_requires_a_successful_amanaki_selection() {
        let mut story = fixture();
        {
            let mut c = story.core.borrow_mut();
            let id = c.register_mission("JAS_080_B30.lua", false, None, None, false, false, false, "");
            c.missions.by_id.get_mut(&id).unwrap().state = crate::missions::State::Running;
        }
        story.domino().lua().load(r#"
            travel = { count = 0 }
            travel._type = { Got = function(self) self.count = self.count + 1 end }
            CScriptCallbackSystem_GetInstance():RegisterMessageListener("jas_080_fast_travel", travel, "Got")
        "#).exec().unwrap();
        let counter = || story.domino().lua().globals().get::<mlua::Table>("travel").unwrap().get::<u32>("count").unwrap();
        assert_eq!(counter(), 0);
        assert!(story.report_outpost_travel(Pose { position: [10.0; 3], angles: [0.0; 3] }));
        assert_eq!(story.domino().lua().globals().get::<mlua::Table>("travel").unwrap().get::<u32>("count").unwrap(), 0);
        assert!(story.fast_travel(999).is_none());
        story.domino().lua().load("SetFastTravelEnabled(false)").exec().unwrap();
        assert!(story.fast_travel(20).is_none());
        assert_eq!(story.domino().lua().globals().get::<mlua::Table>("travel").unwrap().get::<u32>("count").unwrap(), 0);
        story.domino().lua().load("SetFastTravelEnabled(true)").exec().unwrap();
        assert!(story.fast_travel(20).is_some());
        assert_eq!(story.domino().lua().globals().get::<mlua::Table>("travel").unwrap().get::<u32>("count").unwrap(), 1);
    }

    #[test]
    fn active_block_permission_overrides_its_main_graph_default() {
        let story = fixture();
        let mut c = story.core.borrow_mut();
        let main = c.register_mission("main.lua", false, None, None, false, false, false, "");
        let owner = c.missions.by_id[&main].owner;
        let block = c.register_block(owner, 100, "block.lua", None, None);
        c.missions.by_id.get_mut(&main).unwrap().state = crate::missions::State::Running;
        c.missions.by_id.get_mut(&main).unwrap().fast_travel = false;
        assert!(!c.missions.fast_travel_enabled());
        c.missions.by_id.get_mut(&block).unwrap().state = crate::missions::State::Running;
        assert!(c.missions.fast_travel_enabled());
        c.missions.by_id.get_mut(&block).unwrap().fast_travel = false;
        assert!(!c.missions.fast_travel_enabled());
        c.missions.by_id.get_mut(&main).unwrap().fast_travel = true;
        assert!(!c.missions.fast_travel_enabled());
        c.missions.by_id.get_mut(&block).unwrap().state = crate::missions::State::Completed;
        assert!(c.missions.fast_travel_enabled());
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_amanaki_map_selection_completes_the_original_b30_graph() {
        let cache = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let mut story = Story::open(&cache).unwrap();
        let data = story.data();
        assert_eq!(data.menu_unlocks, Some(("jas_080_main".into(), "jas_080_main".into())));
        let block = data.block_named("JAS_080_B30").unwrap();
        let script = "Domino/User/FC3/FC3_MainMissions/JAS_080/JAS_080_B30/JAS_080_B30.JAS_080_B30.lua";
        {
            let mut c = story.core.borrow_mut();
            let main = c.register_mission("test-main.lua", false, None, None, false, false, false, "");
            let owner = c.missions.by_id[&main].owner;
            let id = c.register_block(owner, block.id, script, None, None);
            c.set_mission_state(&id, crate::missions::State::Running);
        }
        let amanaki = story.travel_destinations().into_iter().find(|d| d.safehouse == 0x1ccb5e07b7802dc4).unwrap();
        assert_eq!(amanaki.marker, 0x1ccb5e25f660443c);
        assert_eq!(amanaki.arrival.position, [-857.93, 2299.19, 119.478]);
        assert!((amanaki.arrival.angles[2] - 54.0).abs() < 0.0001);
        let mut view = crate::View { player: [-465.0, 2883.0, 101.0], ..Default::default() };
        story.update(0.0, view.clone());
        assert!(story.fast_travel_tutorial());
        assert!(story.waiting_on().iter().any(|w| w.contains("jas_080_fast_travel")));
        assert!(story.fast_travel_enabled());
        assert!(story.fast_travel(0).is_none());
        assert!(story.fast_travel_tutorial());
        let pose = story.fast_travel(amanaki.marker).unwrap();
        view.player = pose.position;
        let commands = story.update(0.0, view);
        assert!(commands.iter().any(|c| matches!(c, crate::Command::MissionState { name, state, .. }
            if name == "JAS_080_B30" && state == "Completed")), "{commands:?}");
        assert!(!story.fast_travel_tutorial());
        assert_eq!(story.errors(), 0);
    }
}
