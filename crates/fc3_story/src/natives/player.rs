//! What scripts ask about pawns, the player, inventories and vehicles. The
//! player's state comes from the host each frame ([`PlayerView`]); changes
//! scripts ask for go out as commands (see `actions`), with the queries that
//! also act registered here.

use mlua::Value;

use super::{vec3, Reg};
use crate::core::Core;
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

    pub fn is_dead(&self, id: u64) -> bool {
        if id == PLAYER {
            return self.view.player_dead;
        }
        self.removed.contains(&id) || self.dead.contains(&id) || self.view.spawned.get(&id).is_some_and(|l| !l.alive)
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
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
    r.f("CAPI_Pawn.GetWeaponClassFromName", |c, (_pawn, name): (Text, Text)| {
        c.view.player_state.weapons.iter().position(|w| w.name.eq_ignore_ascii_case(&name.0)).map(|_| 1.0).unwrap_or(0.0)
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
        let crc = item.or(-1.0) as u32;
        let p = &c.view.player_state;
        p.gadgets
            .iter()
            .chain(&p.weapons)
            .find(|i| fc3_formats::hash::crc32(i.name.to_ascii_lowercase().as_bytes()) == crc)
            .map(|i| i.count as f64)
            .unwrap_or(0.0)
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
    r.f("CAPI_Player.GetMovementSensitivity", |_, _p: Text| 1.0)?;
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
        "CAPI_Companion.CompanionSetFollow",
        "CAPI_Companion.CompanionSetLead",
        "CAPI_Companion.CompanionSetWait",
    ] {
        let n = name.to_string();
        r.f(name, move |c, args: mlua::Variadic<Text>| {
            c.emit(Command::Native { name: n.clone(), args: args.into_iter().map(|t| t.0).collect() });
            true
        })?;
    }
    r.f("CAPI_Player.StartPhoneCall", |c, (pawn, sound, caller, incoming): (Text, Text, Num, Flag)| {
        let caller_name = caller.0.and_then(|id| c.data.strings.get(id as u32)).unwrap_or_default().to_string();
        c.emit(Command::Native { name: "CAPI_Player.StartPhoneCall".into(), args: vec![pawn.0, sound.0, caller_name, incoming.0.to_string()] });
        true
    })?;

    // ---- pawn state
    r.f("CAPI_Pawn.IsPawnDead", |c, pawn: Text| pawn.id().is_some_and(|p| c.is_dead(p)))?;
    r.f("CAPI_Pawn.GetPawnVehicle", |c, pawn: Text| match pawn.id() {
        Some(PLAYER) => c.view.player_state.vehicle.map(|v| v.to_string()).unwrap_or_else(|| fc3_domino::INVALID_ID.to_string()),
        _ => fc3_domino::INVALID_ID.to_string(),
    })?;
    r.f("CAPI_Pawn.GetPawnVehicleSeat", |c, _p: Text| c.view.player_state.seat as f64)?;
    r.f("CAPI_Pawn.GetPawnVehicleType", |c, _a: mlua::Variadic<Value>| c.view.player_state.vehicle_type as f64)?;
    r.f("CAPI_Pawn.GetPawnStance", |c, _p: Text| c.view.player_state.stance as f64)?;
    r.f("CAPI_Pawn.IsPawnOnZipline", |c, _a: mlua::Variadic<Value>| c.view.player_state.on_zipline)?;
    r.constant("CAPI_Pawn.GetCurrentMoveName", "")?;
    r.constant("CAPI_Pawn.IsAnimationCutable", true)?;
    r.constant("CAPI_Pawn.BelongToGOSMGroup", false)?;
    r.f("GetPlayerInSlot", |_, slot: Num| if slot.or(0.0) as i64 == 0 { PLAYER.to_string() } else { fc3_domino::INVALID_ID.to_string() })?;
    r.constant("GetNumberOfPlayers", 1.0)?;
    r.f("GetDifficulty", |c, ()| c.view.difficulty as f64)?;
    r.constant("PlayCoopBark", false)?;

    // ---- vehicles
    r.constant("CAPI_Vehicle.GetVehicleEngineState", 0.0)?;
    r.f("CAPI_Vehicle.IsVehicleDisabled", |c, v: Text| v.id().is_some_and(|i| c.is_dead(i)))?;
    r.constant("CAPI_Vehicle.IsVehicleFlipped", false)?;
    r.constant("CAPI_Vehicle.IsVehicleUnderwater", false)?;
    r.f("CAPI_Vehicle.IsVehicleUsable", |c, v: Text| v.id().is_some_and(|i| !c.is_dead(i)))?;
    Ok(())
}
