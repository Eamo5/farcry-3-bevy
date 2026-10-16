//! What scripts ask about pawns, the player, inventories and vehicles. The
//! player's state comes from the host each frame ([`PlayerView`]); changes
//! scripts ask for go out as commands (see `actions`), with the queries that
//! also act registered here.

use mlua::Value;

use super::{vec3, Reg};
use crate::core::{Core, Listen};
use crate::values::{Flag, Num, Text};
use crate::{Command, Item, PLAYER};

/// Where an item is: 0 nowhere, 1 in hands, 2 in the inventory.
fn where_is(c: &Core, items: &[Item], test: impl Fn(&Item) -> bool) -> f64 {
    let p = &c.view.player_state;
    match items.iter().find(|i| test(i)) {
        None => 0.0,
        Some(i) if p.in_hands.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(&i.name)) => 1.0,
        Some(_) => 2.0,
    }
}

impl Core {
    /// A stand-in entity id for an inventory item (scripts pass weapon
    /// "entities" back to remove or drop them).
    pub fn item_id(&mut self, name: &str) -> String {
        if name.is_empty() {
            return fc3_domino::INVALID_ID.to_string();
        }
        let id = fc3_formats::hash::crc64(format!("item:{}", name.to_ascii_lowercase()).as_bytes()) | 1 << 63;
        self.item_ids.insert(id, name.to_string());
        id.to_string()
    }

    /// Vehicles that boxes watch for damage (`OnStateChange`), and whether
    /// each is wrecked now.
    pub fn watched_vehicles(&self) -> Vec<(u64, bool)> {
        let mut ids: Vec<u64> = self
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Entity(id, event) if event == "OnStateChange" => Some(*id),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().map(|id| (id, self.is_dead(id))).collect()
    }

    pub fn is_dead(&self, id: u64) -> bool {
        if id == PLAYER {
            return self.view.player_dead;
        }
        self.removed.contains(&id) || self.dead.contains(&id) || self.view.spawned.get(&id).is_some_and(|l| !l.alive)
    }

    pub(crate) fn vehicle_occupant(&self, pawn: u64) -> Option<crate::vehicles::Occupant> {
        self.vehicle_departures.get(&pawn).copied().or_else(|| {
            if pawn == PLAYER {
                self.view.player_state.vehicle.map(|vehicle| crate::vehicles::Occupant { vehicle, kind: self.view.player_state.seat,
                    index: self.view.player_state.seat_index, vehicle_type: self.view.player_state.vehicle_type })
            } else { self.view.pawn_vehicles.get(&pawn).copied() }
        })
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    // Original Lua registration: wheeled=0; seat types are 1/2/3. Vehicle
    // animation kits (Technical=10) are a different enum, not query types.
    for (name, value) in [("VEHICLE_TYPE_WHEELED", 0), ("VEHICLE_TYPE_FLOATING", 1), ("VEHICLE_TYPE_PARAGLIDER", 2),
        ("VEHICLE_TYPE_MOTORCYCLE", 3), ("VEHICLE_TYPE_HELICOPTER", 4), ("VEHICLE_TYPE_PLANE", 5), ("VEHICLE_TYPE_WINGSUIT", 6),
        ("VEHICLE_TYPE_NONE", 7), ("VEHICLE_SEAT_NO_SEAT", 0), ("VEHICLE_SEAT_DRIVER", 1), ("VEHICLE_SEAT_GUNNER", 2), ("VEHICLE_SEAT_PASSENGER", 3)] {
        r.d.lua().globals().set(name, value)?;
    }
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

    // ---- inventory
    r.f("CAPI_Pawn.CheckPawnGadgetInventoryByCategory", move |c, (_pawn, cat): (Text, Text)| {
        let items = c.view.player_state.gadgets.clone();
        where_is(c, &items, |i| eq(&i.category, &cat.0) || eq(&i.class, &cat.0))
    })?;
    r.f("CAPI_Pawn.CheckPawnWeaponInventoryByCategory", move |c, (_pawn, cat): (Text, Text)| {
        let items = c.view.player_state.weapons.clone();
        where_is(c, &items, |i| eq(&i.category, &cat.0))
    })?;
    r.f("CAPI_Pawn.CheckPawnWeaponInventoryByClass", move |c, (_pawn, class): (Text, Text)| {
        let items = c.view.player_state.weapons.clone();
        where_is(c, &items, |i| eq(&i.class, &class.0))
    })?;
    r.f("CAPI_Pawn.CheckPawnWeaponInventoryByName", move |c, (_pawn, name): (Text, Text)| {
        let items = c.view.player_state.weapons.clone();
        where_is(c, &items, |i| eq(&i.name, &name.0) || i.name.to_ascii_lowercase().ends_with(&format!("/{}", name.0.to_ascii_lowercase())))
    })?;
    // The engine's weapon classes, as the hunts' weapon checks number them
    // (`Lib_SideQuest.AnimalSQWeaponsClassCheck`), of a weapon the player has.
    r.f("CAPI_Pawn.GetWeaponClassFromName", |c, (_pawn, name): (Text, Text)| {
        let class = c.view.player_state.weapons.iter().find(|w| w.name.eq_ignore_ascii_case(&name.0)).map(|w| w.class.as_str());
        weapon_class_number(class.unwrap_or_default())
    })?;
    r.f("CAPI_Pawn.GetWeaponIdInSlot", |c, (_pawn, slot): (Text, Num)| {
        let name = c.view.player_state.weapons.get(slot.or(0.0) as usize).map(|w| w.name.clone()).unwrap_or_default();
        c.item_id(&name)
    })?;
    r.f("CAPI_Pawn.GetEquippedWeaponEntityID", |c, _pawn: Text| {
        let name = c.view.player_state.in_hands.clone().unwrap_or_default();
        c.item_id(&name)
    })?;
    r.f("CAPI_Pawn.GetBestDroppableWeaponEntityID", |c, _pawn: Text| {
        let name = c.view.player_state.weapons.last().map(|w| w.name.clone()).unwrap_or_default();
        c.item_id(&name)
    })?;
    r.f("CAPI_Pawn.GetWeaponEntityID", |c, (_pawn, name): (Text, Text)| {
        let has = c.view.player_state.weapons.iter().any(|w| w.name.eq_ignore_ascii_case(&name.0));
        if has {
            c.item_id(&name.0)
        } else {
            fc3_domino::INVALID_ID.to_string()
        }
    })?;
    r.f("CAPI_Pawn.SpawnWeapon", |c, (pawn, name): (Text, Text)| {
        c.emit(Command::Native { name: "CAPI_Pawn.SpawnWeapon".into(), args: vec![pawn.0, name.0.clone()] });
        c.item_id(&name.0)
    })?;
    r.f("CAPI_Pawn.GetLootQuantity", |c, (_pawn, item): (Text, Num)| {
        c.view.player_state.loot.get(&(item.or(-1.0) as u32)).copied().unwrap_or(0) as f64
    })?;
    r.f("CAPI_Player.GetPlayerMoney", |c, _p: Text| c.view.player_state.money as f64)?;
    r.f("CAPI_Player.GetPlayerMedPackCount", |c, _p: Text| c.view.player_state.medpacks as f64)?;
    r.f("CAPI_Player.GetNumWeaponSlotsUnlocked", |c, _p: Text| c.view.player_state.weapon_slots.max(1) as f64)?;
    r.f("CAPI_Player.HasBonus", |c, (_p, bonus): (Text, Text)| {
        // A skill by name or by its case-folded name id.
        let wanted = bonus.0.to_ascii_lowercase();
        let id: Option<u32> = bonus.0.parse().ok();
        c.view.player_state.skills.iter().any(|s| *s == wanted || id == Some(fc3_formats::hash::crc32(s.as_bytes())))
    })?;
    // A bonus plan lent or taken away: `HasBonus` sees it at once
    // (`BonusPlanModifier` asks again right after adding one).
    for (name, add) in [("CAPI_Player.AddBonusPlan", true), ("CAPI_Player.RemoveBonusPlan", false)] {
        r.f(name, move |c, (pawn, plan): (Text, Text)| {
            let wanted = plan.0.to_ascii_lowercase();
            if add {
                c.view.player_state.skills.insert(wanted);
            } else {
                c.view.player_state.skills.remove(&wanted);
            }
            c.emit(Command::Native { name: name.into(), args: vec![pawn.0, plan.0] });
        })?;
    }
    r.f("CAPI_Player.GetMovementSensitivity", |_, _p: Text| 1.0)?;
    r.f("CAPI_Player.IsInQTE", |c, _p: Text| c.view.player_state.in_qte)?;
    r.lua("CAPI_Player.GetPlayerLookAngles", |lua, c, _p: Text| vec3(lua, c.view.player_state.look_angles))?;

    // Actions with a success result: asked of the host, reported done.
    for name in [
        "CAPI_Pawn.DrawWeapon",
        "CAPI_Pawn.Draw",
        "CAPI_Pawn.Holster",
        "CAPI_Pawn.ForceHolster",
        "CAPI_Pawn.SetAutoDraw",
        "CAPI_Pawn.SetConditionalZoneEventsEnabled",
        "CAPI_Pawn.SetKickOnZipline",
        "CAPI_Pawn.PermitPawnToDetachFromZipline",
        "CAPI_Pawn.PreventPawnFromDetachingFromZipline",
        "CAPI_Pawn.RemovePawnFromZipline",
        "CAPI_Player.EnableAnimalLooting",
        "CAPI_Player.EnablePlantLooting",
        "CAPI_Player.EquipCostume",
        "CAPI_Player.DrawGadget",
        "CAPI_AI.StartPlayAnim",
        "CAPI_AI.StopPlayAnim",
    ] {
        let n = name.to_string();
        r.f(name, move |c, args: mlua::Variadic<Text>| {
            c.emit(Command::Native { name: n.clone(), args: args.into_iter().map(|t| t.0).collect() });
            true
        })?;
    }
    // A companion told to follow the player, lead along a path (waiting
    // while the player is further off than the distance given) or wait.
    r.f("CAPI_Companion.CompanionSetFollow", |c, companion: Text| {
        if let Some(id) = companion.id() {
            c.companions.insert(id, crate::Companion::Follow);
        }
        c.emit(Command::Native { name: "CAPI_Companion.CompanionSetFollow".into(), args: vec![companion.0] });
        true
    })?;
    r.f("CAPI_Companion.CompanionSetLead", |c, (companion, path, distance): (Text, Text, Num)| {
        if let (Some(id), Some(path)) = (companion.id(), path.id()) {
            c.companions.insert(id, crate::Companion::Lead { path, distance: distance.0.unwrap_or(15.0) as f32 });
        }
        c.emit(Command::Native { name: "CAPI_Companion.CompanionSetLead".into(), args: vec![companion.0, path.0] });
        true
    })?;
    r.f("CAPI_Companion.CompanionSetWait", |c, companion: Text| {
        if let Some(id) = companion.id() {
            c.companions.insert(id, crate::Companion::Wait);
        }
        c.emit(Command::Native { name: "CAPI_Companion.CompanionSetWait".into(), args: vec![companion.0] });
        true
    })?;
    // The player's pawn swapped for another archetype (a later model of
    // him): `LocalPlayerRespawnedReady` once it is in, as the box that asked
    // turns to listen for it just after.
    r.f("CAPI_Player.SwitchPlayerPawn", |c, args: mlua::Variadic<Text>| {
        c.pawn_ready_at = Some(c.time + 0.1);
        c.emit(Command::Native { name: "CAPI_Player.SwitchPlayerPawn".into(), args: args.into_iter().map(|t| t.0).collect() });
    })?;
    r.f("CAPI_Player.StartPhoneCall", |c, (pawn, sound, caller, incoming): (Text, Text, Num, Flag)| {
        let caller_name = caller.0.and_then(|id| c.data.strings.get(id as u32)).unwrap_or_default().to_string();
        c.emit(Command::Native { name: "CAPI_Player.StartPhoneCall".into(), args: vec![pawn.0, sound.0, caller_name, incoming.0.to_string()] });
        true
    })?;

    // ---- pawn state
    r.f("CAPI_Pawn.IsPawnDead", |c, pawn: Text| pawn.id().is_some_and(|p| c.is_dead(p)))?;
    r.f("CAPI_Pawn.GetPawnVehicle", |c, pawn: Text| pawn.id().and_then(|id| c.vehicle_occupant(id))
        .map(|o| o.vehicle.to_string()).unwrap_or_else(|| fc3_domino::INVALID_ID.to_string()))?;
    r.f("CAPI_Pawn.GetPawnVehicleSeat", |c, pawn: Text| pawn.id().and_then(|id| c.vehicle_occupant(id)).map_or(0.0, |o| o.kind as f64))?;
    r.f("CAPI_Pawn.GetPawnVehicleType", |c, pawn: Text| pawn.id().and_then(|id| c.vehicle_occupant(id)).map_or(7.0, |o| o.vehicle_type as f64))?;
    r.f("CAPI_Pawn.GetPawnStance", |c, _p: Text| c.view.player_state.stance as f64)?;
    r.f("CAPI_Pawn.IsPawnOnZipline", |c, _a: mlua::Variadic<Value>| c.view.player_state.on_zipline)?;
    // Moves a script plays on a pawn: the host plays them; the story times
    // them and their markups.
    r.f("CAPI_Pawn.PlayAnimation", |c, args: mlua::Variadic<Text>| {
        let args: Vec<String> = args.into_iter().map(|t| t.0).collect();
        if let (Some(pawn), Some(name)) = (Text(args.first().cloned().unwrap_or_default()).id(), args.get(1)) {
            c.play_move(pawn, name);
        }
        c.emit(Command::Native { name: "CAPI_Pawn.PlayAnimation".into(), args });
    })?;
    r.f("CAPI_Pawn.GetCurrentMoveName", |c, (pawn, _layer): (Text, Num)| pawn.id().and_then(|p| c.current_move(p)).unwrap_or_default().to_string())?;
    r.f("CAPI_Pawn.IsAnimationCutable", |c, (pawn, name, _layer): (Text, Text, Num)| pawn.id().and_then(|p| c.current_move(p)) != Some(name.0.as_str()))?;
    // A pawn's state machine shut down (it stands stunned) and restored
    // (back to idle): the callbacks waiting for those states.
    r.f("CAPI_Pawn.ShutdownGOSM", |c, pawn: Text| {
        c.emit(Command::Native { name: "CAPI_Pawn.ShutdownGOSM".into(), args: vec![pawn.0.clone()] });
        c.enter_state(pawn.id().unwrap_or(PLAYER), "Stun");
    })?;
    r.f("CAPI_Pawn.RestoreGOSM", |c, (pawn, how): (Text, Text)| {
        c.emit(Command::Native { name: "CAPI_Pawn.RestoreGOSM".into(), args: vec![pawn.0.clone(), how.0] });
        c.enter_state(pawn.id().unwrap_or(PLAYER), "Idle");
    })?;
    r.constant("CAPI_Pawn.BelongToGOSMGroup", false)?;
    r.f("GetPlayerInSlot", |_, slot: Num| if slot.or(0.0) as i64 == 0 { PLAYER.to_string() } else { fc3_domino::INVALID_ID.to_string() })?;
    r.constant("GetNumberOfPlayers", 1.0)?;
    r.f("GetDifficulty", |c, ()| c.view.difficulty as f64)?;
    r.constant("PlayCoopBark", false)?;

    // ---- vehicles
    // A vehicle's damage state as `VehicleDamageListener` reads it (0
    // destroyed ... 5 mint): destroyed once the host has it wrecked, mint
    // until then (a listener reads it the moment it is enabled).
    r.f("CAPI_Vehicle.GetVehicleEngineState", |c, v: Text| match v.id() {
        Some(i) if c.is_dead(i) => crate::VEHICLE_DESTROYED,
        _ => crate::VEHICLE_MINT,
    })?;
    r.f("CAPI_Vehicle.IsVehicleDisabled", |c, v: Text| v.id().is_some_and(|i| c.is_dead(i)))?;
    r.constant("CAPI_Vehicle.IsVehicleFlipped", false)?;
    r.constant("CAPI_Vehicle.IsVehicleUnderwater", false)?;
    r.f("CAPI_Vehicle.IsVehicleUsable", |c, v: Text| v.id().is_some_and(|i| c.is_loaded(i) && !c.is_dead(i)
        && !c.vehicle_rules.get(&i).is_some_and(|r| r.unusable)))?;
    r.f("CAPI_Vehicle.SetIsSeatLocked", |c, (vehicle, kind, index, locked): (Text, Num, Num, Flag)| {
        let Some(id) = vehicle.id() else { return };
        let key = (kind.or(1.0) as i32, index.or(0.0) as i32);
        let rules = c.vehicle_rules.entry(id).or_default();
        if locked.0 { rules.locked.insert(key); } else { rules.locked.remove(&key); }
    })?;
    Ok(())
}

/// The engine's number for a weapon class (`CAPI_Pawn.GetWeaponClassFromName`),
/// -1 for one it doesn't number.
fn weapon_class_number(class: &str) -> f64 {
    match class {
        "flamethrower" => 0.0,
        "handgun" | "pistol" => 1.0,
        "melee" => 2.0,
        "lmg" => 3.0,
        "rpg" => 4.0,
        "sniper" => 5.0,
        "shotgun" => 6.0,
        "assault" => 9.0,
        "smg" => 15.0,
        _ => -1.0,
    }
}
