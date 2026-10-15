//! Authored vehicle seats and script-owned interaction restrictions. Physical
//! boarding remains a host transaction; queries and events follow its snapshot.

use std::collections::{HashMap, HashSet};
use crate::{core::Listen, data::Entry, Arg, Story, View, PLAYER};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occupant {
    pub vehicle: u64,
    /// Original seat type: 1 driver, 2 gunner, 3 passenger.
    pub kind: i32,
    /// Index within this type, not within the combined seat array.
    pub index: i32,
    pub vehicle_type: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fc3_formats::scripts::ScriptPack;

    fn empty() -> Story {
        Story::new(ScriptPack::from_bytes(&fc3_formats::scripts::pack(&[])).unwrap(), crate::data::StoryData::default()).unwrap()
    }

    #[test]
    fn physical_occupancy_generates_enter_change_and_filtered_leave_once() {
        let mut story = empty();
        story.domino().lua().load(r#"
            vehicle = { enter = 0, leave = 0, change = 0 }
            vehicle._type = {
                Enter = function(self)
                    assert(CAPI_Pawn.GetPawnVehicle("1") == "100")
                    assert(CAPI_Pawn.GetPawnVehicleSeat("1") == 3)
                    self.enter = self.enter + 1
                end,
                Change = function(self)
                    assert(CAPI_Pawn.GetPawnVehicle("1") == "100")
                    self.change = self.change + 1
                end,
                Leave = function(self)
                    assert(CAPI_Pawn.GetPawnVehicle("1") == "100")
                    self.leave = self.leave + 1
                end
            }
            local callbacks = CScriptCallbackSystem_GetInstance()
            callbacks:RegisterEventCallback("1", vehicle, "Enter", "IsInVehicle")
            callbacks:RegisterEventCallback("1", vehicle, "Leave", "IsOutVehicle")
            callbacks:RegisterEventCallback("1", vehicle, "Change", "ChangeSeat")
        "#).exec().unwrap();
        let mut view = View::default();
        view.player_state.vehicle = Some(100);
        view.player_state.seat = 3;
        story.update(0.0, view.clone());
        for _ in 0..5 { story.update(0.0, view.clone()); }
        view.player_state.seat_index = 1;
        story.update(0.0, view.clone());
        story.update(0.0, View::default());
        let lua = story.domino().lua();
        let counts: (u32, u32, u32) = lua.load("return vehicle.enter, vehicle.change, vehicle.leave").eval().unwrap();
        assert_eq!(counts, (1, 1, 1));
        assert_eq!(lua.load(r#"return CAPI_Pawn.GetPawnVehicle("1")"#).eval::<String>().unwrap(), fc3_domino::INVALID_ID);
        assert_eq!(story.errors(), 0);
    }

    #[test]
    fn seat_requests_do_not_fabricate_occupancy_and_npc_queries_use_their_own_seat() {
        let mut story = empty();
        story.domino().lua().load(r#"CAPI_Vehicle.VehicleUseTakeSeat("100", "200", 0, true, false, false, false, false)"#).exec().unwrap();
        assert!(matches!(story.take_commands().as_slice(), [crate::Command::Native { name, .. }] if name == "CAPI_Vehicle.VehicleUseTakeSeat"));
        assert_eq!(story.domino().lua().load(r#"return CAPI_Pawn.GetPawnVehicle("200")"#).eval::<String>().unwrap(), fc3_domino::INVALID_ID);
        let mut view = View::default();
        view.player_state.vehicle = Some(100);
        view.player_state.seat = 3;
        view.pawn_vehicles.insert(200, Occupant { vehicle: 100, kind: 1, index: 0, vehicle_type: 0 });
        story.update(0.0, view);
        assert_eq!(story.domino().lua().load(r#"return CAPI_Pawn.GetPawnVehicle("200"), CAPI_Pawn.GetPawnVehicleSeat("200"), CAPI_Pawn.GetPawnVehicleType("200")"#)
            .eval::<(String, i32, i32)>().unwrap(), ("100".into(), 1, 0));
    }

    #[test]
    fn vehicle_controls_keep_locks_by_type_and_index_and_can_be_cleared() {
        let story = empty();
        story.domino().lua().load(r#"
            CAPI_Vehicle.SetIsSeatLocked("100", 1, 0, true)
            CAPI_Vehicle.SetIsSeatLocked("100", 3, 2, true)
            SendRegisteredEventToEntity("100", "CVehicle_SetExitDisabled", {})
            SendRegisteredEventToEntity("100", "CVehicle_SetAsAnimated", {})
            SendRegisteredEventToEntity("100", "CVehicle_SetAsUnusable", {})
            SendRegisteredEventToEntity("100", "CVehicle_SetAsIndestructable", {})
        "#).exec().unwrap();
        let rules = story.vehicle_rules(100);
        assert!(rules.exit_disabled && rules.animated && rules.unusable && rules.indestructible);
        assert_eq!(rules.locked, HashSet::from([(1, 0), (3, 2)]));
        story.domino().lua().load(r#"
            CAPI_Vehicle.SetIsSeatLocked("100", 1, 0, false)
            SendRegisteredEventToEntity("100", "CVehicle_SetExitEnabled", {})
            SendRegisteredEventToEntity("100", "CVehicle_SetAsNotAnimated", {})
            SendRegisteredEventToEntity("100", "CVehicle_SetAsUsable", {})
            SendRegisteredEventToEntity("100", "CVehicle_SetAsDestructable", {})
        "#).exec().unwrap();
        let rules = story.vehicle_rules(100);
        assert!(!rules.exit_disabled && !rules.animated && !rules.unusable && !rules.indestructible);
        assert_eq!(rules.locked, HashSet::from([(3, 2)]));
    }

    #[test]
    #[ignore = "requires FC3_TEST_CACHE pointing at converted installed-game assets"]
    fn installed_vehicle_listener_filters_physical_player_and_npc_occupants() {
        let root = std::path::PathBuf::from(std::env::var("FC3_TEST_CACHE").expect("set FC3_TEST_CACHE"));
        let pack = ScriptPack::from_bytes(&std::fs::read(root.join(fc3_formats::scripts::DOMINO_PACK_REL)).unwrap()).unwrap();
        let mut story = Story::new(pack, crate::data::StoryData::default()).unwrap();
        let listener = story.domino().create("Domino/System/VehicleListener_v2.lua").unwrap();
        let lua = story.domino().lua();
        for name in ["Enabled", "Disabled", "OnSit", "OnStandUp", "OnChangeSeat"] {
            listener.set(name, lua.globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        }
        listener.set("vehicle", "100").unwrap();
        listener.set("pawn", "200").unwrap();
        listener.set("autoDisable", false).unwrap();
        for (output, field) in [("OnSit", "sit"), ("OnStandUp", "stand"), ("OnChangeSeat", "change")] {
            listener.set(output, lua.load(format!("return function(self) self.{field} = (self.{field} or 0) + 1 end")).eval::<mlua::Function>().unwrap()).unwrap();
        }
        story.domino().call(&listener, "Enable").unwrap();
        assert!(story.awaits_vehicle_entry(200));
        assert!(story.awaits_vehicle_exit(200));
        assert!(!story.awaits_vehicle_exit(PLAYER));
        let mut view = View::default();
        view.player_state.vehicle = Some(100); // wrong pawn must not fire
        story.update(0.0, view.clone());
        assert_eq!(listener.get::<Option<u32>>("sit").unwrap(), None);
        view.pawn_vehicles.insert(200, Occupant { vehicle: 999, kind: 1, index: 0, vehicle_type: 0 });
        story.update(0.0, view.clone()); // wrong vehicle must not fire
        assert_eq!(listener.get::<Option<u32>>("sit").unwrap(), None);
        view.pawn_vehicles.get_mut(&200).unwrap().vehicle = 100;
        story.update(0.0, view.clone());
        story.update(0.0, view.clone());
        assert_eq!(listener.get::<u32>("sit").unwrap(), 1);
        let info = story.domino().create("Domino/System/GetPawnVehicleInfo.lua").unwrap();
        info.set("pawn", "200").unwrap();
        info.set("Out", story.domino().lua().globals().get::<mlua::Function>("DummyFunction").unwrap()).unwrap();
        story.domino().call(&info, "In").unwrap();
        assert_eq!(info.get::<String>("seatType").unwrap(), "Driver");
        assert_eq!(info.get::<String>("vehicleType").unwrap(), "Wheeled");
        view.pawn_vehicles.get_mut(&200).unwrap().kind = 3;
        story.update(0.0, view.clone());
        assert_eq!(listener.get::<u32>("change").unwrap(), 1);
        view.pawn_vehicles.clear();
        story.update(0.0, view.clone());
        story.update(0.0, view);
        assert_eq!(listener.get::<u32>("stand").unwrap(), 1);
        story.domino().call(&info, "In").unwrap();
        assert_eq!(info.get::<String>("seatType").unwrap(), "NoSeat");
        assert_eq!(info.get::<String>("vehicleType").unwrap(), "NoVehicle");
        story.domino().call(&listener, "Disable").unwrap();
        assert!(!story.awaits_vehicle_entry(200));
        assert!(!story.awaits_vehicle_exit(200));
        assert_eq!(story.errors(), 0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub bone: u32,
    pub kind: i32,
    /// Among the seats of its kind.
    pub index: i32,
    /// Among all the vehicle's seats, as scripts' `SeatIndex` counts them
    /// (the Zodiac's gun, after the wheel and four benches: 5).
    pub slot: i32,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub animation_type: i32,
    pub vehicle_type: i32,
    pub seats: Vec<Seat>,
}

impl Layout {
    pub fn from_entry(entry: &Entry) -> Option<Self> {
        let vehicle = entry.component("CFCXVehicle")?;
        let mut layout = Self { animation_type: vehicle.u32("iAnimVehicleType").unwrap_or(0) as i32,
            vehicle_type: vehicle.u32("selVehicleType").unwrap_or(0) as i32, ..Self::default() };
        let mut indices = HashMap::<i32, i32>::new();
        for (slot, seat) in vehicle.child("SeatsInfo").into_iter().flat_map(|c| &c.children).filter(|c| c.is("SeatInfo")).enumerate() {
            let (Some(bone), Some(kind)) = (seat.u32("disSeatBoneInfo"), seat.u32("disSeatTypeInfo")) else { continue };
            if !(1..=3).contains(&kind) { continue; }
            let index = indices.entry(kind as i32).or_default();
            layout.seats.push(Seat { bone, kind: kind as i32, index: *index, slot: slot as i32 });
            *index += 1;
        }
        Some(layout)
    }
}

/// The health a vehicle's own data gives it (its physics' part's:
/// HUR_040's barge), where it has one.
pub fn max_health(entry: &Entry) -> Option<f32> {
    ["CVehicleFloatingPhysComponent", "CVehicleWheeledPhysComponent"]
        .iter()
        .find_map(|c| entry.component(c))
        .and_then(|p| p.child("Parts")?.children.first()?.f32("fMaxHealth"))
        .filter(|h| *h > 0.0)
}

#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub unusable: bool,
    pub exit_disabled: bool,
    pub animated: bool,
    /// Nothing destroys it (`CVehicle_SetAsIndestructable`: HUR_040's barge
    /// until its defence begins).
    pub indestructible: bool,
    pub locked: HashSet<(i32, i32)>,
}

pub(crate) fn occupants(view: &View) -> HashMap<u64, Occupant> {
    let mut occupants = view.pawn_vehicles.clone();
    if let Some(vehicle) = view.player_state.vehicle {
        occupants.insert(PLAYER, Occupant { vehicle, kind: view.player_state.seat, index: view.player_state.seat_index,
            vehicle_type: view.player_state.vehicle_type });
    }
    occupants
}

impl Story {
    pub fn awaits_vehicle_entry(&self, pawn: u64) -> bool {
        self.core.borrow().listeners.values().any(|l| matches!(&l.on, Listen::Entity(p, event) if *p == pawn && event == "IsInVehicle"))
    }

    /// The vehicles scripts wait for this pawn to get into, named by their
    /// listeners (a `VehicleListener`'s `vehicle`: JAS_170's pirate truck,
    /// nothing else of which a script listens to).
    pub fn awaited_vehicles(&self, pawn: u64) -> Vec<u64> {
        self.core
            .borrow()
            .listeners
            .values()
            .filter(|l| matches!(&l.on, Listen::Entity(p, event) if *p == pawn && event == "IsInVehicle"))
            .filter_map(|l| l.callback.target.get::<crate::values::Text>("vehicle").ok()?.id())
            .collect()
    }

    /// A script listens for this event of this entity's (the player's
    /// landing, `DominoEvent_PlayerLanded`).
    pub fn awaits_event(&self, id: u64, event: &str) -> bool {
        self.core.borrow().listeners.values().any(|l| matches!(&l.on, Listen::Entity(p, e) if *p == id && e == event))
    }

    /// A script listens for something of this entity's (a vehicle's
    /// health, its arrival).
    pub fn watches_entity(&self, id: u64) -> bool {
        self.core.borrow().listeners.values().any(|l| matches!(&l.on, Listen::Entity(p, _) if *p == id))
    }

    pub fn awaits_vehicle_exit(&self, pawn: u64) -> bool {
        self.core.borrow().listeners.values().any(|l| matches!(&l.on, Listen::Entity(p, event) if *p == pawn && event == "IsOutVehicle"))
    }

    pub fn vehicle_rules(&self, id: u64) -> Rules {
        self.core.borrow().vehicle_rules.get(&id).cloned().unwrap_or_default()
    }

    // Leave listeners in the installed game filter GetPawnVehicle against the
    // vehicle being left. Keep only this transient query context during their
    // callbacks, then publish the new occupancy before enter/change callbacks.
    pub(crate) fn vehicle_changes(&mut self, before: HashMap<u64, Occupant>) {
        let after = occupants(&self.core.borrow().view);
        let mut pawns: Vec<u64> = before.keys().chain(after.keys()).copied().collect();
        pawns.sort_unstable();
        pawns.dedup();
        for pawn in pawns {
            let (old, new) = (before.get(&pawn).copied(), after.get(&pawn).copied());
            if old == new { continue; }
            if let Some(old) = old.filter(|o| new.is_none_or(|n| o.vehicle != n.vehicle)) {
                self.core.borrow_mut().vehicle_departures.insert(pawn, old);
                self.entity_event(pawn, "IsOutVehicle", vec![]);
                self.core.borrow_mut().vehicle_departures.remove(&pawn);
            }
            if let Some(new) = new {
                let event = if old.is_some_and(|o| o.vehicle == new.vehicle) { "ChangeSeat" } else { "IsInVehicle" };
                self.core.borrow_mut().raise(&Listen::Entity(pawn, event.into()), Vec::<Arg>::new());
                self.pump();
            }
        }
    }
}
