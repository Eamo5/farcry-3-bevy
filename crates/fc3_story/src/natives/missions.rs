//! The mission manager's natives (`CMissionManager`, `CMissionBlockManager`)
//! and the mission-flow globals: safehouses, radio towers, session flags.

use mlua::Value;

use super::{callback, Reg};
use crate::core::{LayerState, Listen};
use crate::missions::State;
use crate::values::{oasis_id, Flag, Num, Text};
use crate::Command;

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    const M: &str = "CMissionManager:";
    let m = |name: &str| format!("{M}{name}");

    r.f(&m("RegisterMission"), |c, args: (Text, Text, Num, Text, Flag, Option<bool>, Option<bool>, Option<bool>, Text)| {
        let (script, _giver_name, objective, spawn, exclusive, _disable_enc, title, end, type_name) = args;
        let objective = objective.0.filter(|&o| o >= 0.0).map(|o| o as u32);
        c.register_mission(&script.0, false, objective, spawn.id(), exclusive.0, title.unwrap_or(true), end.unwrap_or(true), &type_name.0)
    })?;
    r.f(&m("RegisterMissionGiver"), |c, (script, _giver_name, objective, spawn, type_name): (Text, Text, Num, Text, Text)| {
        let objective = objective.0.filter(|&o| o >= 0.0).map(|o| o as u32);
        c.register_mission(&script.0, true, objective, spawn.id(), false, false, false, &type_name.0)
    })?;
    r.f(&m("RegisterMissionBlock"), |c, (owner, block, script, spawn, end): (Text, Text, Text, Text, Text)| {
        let Some(block) = block.id() else {
            c.warn(format!("RegisterMissionBlock without a block ({})", block.0));
            return String::new();
        };
        c.register_block(owner.id().unwrap_or(0), block, &script.0, spawn.id(), end.id())
    })?;
    r.f(&m("ScanComplete"), |_, _owner: Text| ())?;
    r.f(&m("GetMissionState"), |c, id: Text| c.missions.state(&id.0).name())?;
    r.f(&m("SetMissionState"), |c, (id, state): (Text, Text)| c.set_mission_state(&id.0, State::parse(&state.0)))?;
    r.f(&m("GetWayPointMissionId"), |c, block: Text| if c.missions.by_id.contains_key(&block.0) { block.0 } else { String::new() })?;
    r.f(&m("GetMissionIdFromScriptName"), |c, name: Text| c.missions.by_name(&name.0).map(|m| m.id.clone()).unwrap_or_default())?;
    r.f(&m("RegisterDominoEvent"), |c, (mission, target, method, event): (Text, Value, Option<String>, Text)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Mission(mission.0, event.0), cb).to_string())
    })?;
    r.f(&m("RemoveDominoEvent"), |c, (_mission, id, _event): (Text, Text, Text)| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;
    r.f(&m("MissionBlockComplete"), |c, (owner, save): (Text, Option<bool>)| c.block_complete(owner.id().unwrap_or(0), save.unwrap_or(true)))?;
    r.lua(&m("MissionBlockFail"), |lua, c, (owner, reload, reason): (Text, Option<bool>, Num)| {
        if std::env::var("FC3_STORY_WAIT_DEBUG").is_ok() {
            eprintln!("story block failure: owner {} player {:?} {}", owner.0, c.view.player,
                lua.load("return debug.traceback()").eval::<String>().unwrap_or_default());
        }
        c.block_fail(owner.id().unwrap_or(0), reload.unwrap_or(true), reason.0.filter(|&r| r >= 0.0).map(|r| r as u32));
        Ok(())
    })?;
    r.f(&m("MissionComplete"), |c, (owner, text): (Text, Num)| c.mission_complete(owner.id().unwrap_or(0), text.0.filter(|&t| t >= 0.0).map(|t| t as u32)))?;
    r.f(&m("MissionCompleteNotify"), |c, (owner, text): (Text, Num)| {
        let name = c.missions.main_of(owner.id().unwrap_or(0)).map(|m| m.name.clone()).unwrap_or_default();
        let text = text.0.and_then(|t| c.data.strings.get(t as u32)).map(str::to_string);
        c.emit(Command::Native { name: "MissionCompleteNotify".into(), args: vec![name, text.unwrap_or_default()] });
    })?;
    r.f(&m("MissionFail"), |c, (owner, text): (Text, Num)| c.mission_fail(owner.id().unwrap_or(0), text.0.filter(|&t| t >= 0.0).map(|t| t as u32)))?;
    r.f(&m("MissionAcknowledged"), |c, (owner, save): (Text, Flag)| c.acknowledge(owner.id().unwrap_or(0), save.0))?;
    r.f(&m("IsMissionAcknowledged"), |c, owner: Text| c.is_acknowledged(owner.id().unwrap_or(0)))?;
    r.f(&m("IsMissionAcknowledged_FromMissionId"), |c, id: Text| c.missions.get(&id.0).is_some_and(|m| m.acknowledged))?;
    r.lua(&m("SetMissionObjective"), |_, c, (owner, objective, play_complete): (Text, Value, Flag)| {
        c.set_objective(owner.id().unwrap_or(0), oasis_id(&objective), play_complete.0);
        Ok(())
    })?;
    r.f(&m("CompleteCurrentObjective"), |c, owner: Text| {
        let _ = owner;
        c.emit(Command::Objective { text: None, id: None, play_complete: true });
    })?;
    r.f(&m("DisplayMissionTitle"), |c, owner: Text| {
        let Some(m) = c.missions.main_of(owner.id().unwrap_or(0)).cloned() else { return };
        let title = m.objective.and_then(|o| c.data.strings.get(o)).map(str::to_string);
        let description = c.data.strings.named("Description", &format!("{}_DESC", m.name)).map(str::to_string);
        c.emit(Command::MissionTitle { title, description });
    })?;
    r.f(&m("EnableFastTravel"), |c, (owner, on): (Text, Flag)| {
        if let Some(id) = owner.id().and_then(|o| c.missions.by_owner.get(&o)).cloned() {
            if let Some(mission) = c.missions.by_id.get_mut(&id) {
                mission.fast_travel = on.0;
            }
        }
    })?;
    r.f(&m("EnableMissionAbort"), |c, (_owner, on): (Text, Flag)| c.missions.abort_enabled = on.0)?;
    r.f(&m("LockMission"), |c, script: Text| {
        let id = fc3_domino::path_id(&script.0);
        if c.missions.by_id.contains_key(&id) {
            c.set_mission_state(&id, State::Locked);
        }
    })?;
    r.f(&m("MissionGiverAssoc"), |c, (giver, mission): (Text, Text)| {
        c.missions.givers.insert(fc3_domino::path_id(&giver.0), fc3_domino::path_id(&mission.0));
    })?;
    r.f(&m("DisableMissionBlock"), |c, id: Text| {
        if let Some(b) = c.missions.get(&id.0).and_then(|m| m.block.as_ref()).map(|b| b.block) {
            c.set_block(b, LayerState::Disabled);
        }
    })?;
    r.f(&m("SetMBlockLayerStateById"), |c, (_block, layer, state): (Text, Text, Num)| match (layer.id(), LayerState::from_code(state.or(-1.0) as i64)) {
        (Some(l), Some(s)) => c.set_layer(l, s),
        _ => c.warn(format!("SetMBlockLayerStateById({}, {:?})", layer.0, state.0)),
    })?;
    r.f(&m("GetMBlockLayerStateByName"), |c, (block, layer): (Text, Text)| {
        let b = c.data.block_named(&block.0).map(|b| b.id);
        c.data.layer_named(b, &layer.0).map(|l| c.layer_state(l.id) as i32 as f64).unwrap_or(0.0)
    })?;
    r.f(&m("SetAcknowledgeShape"), |_, _: (Text, Text)| ())?;
    r.f(&m("SetCurrentChapter"), |c, n: Num| c.missions.chapter = n.or(0.0) as i32)?;
    r.f(&m("UnlockItemsFromScript"), |c, group: Text| {
        c.unlocked_tags.insert(group.0.clone());
        c.emit(Command::Native { name: "UnlockItemsFromScript".into(), args: vec![group.0] });
    })?;
    r.f(&m("CampaignPrimarySoftSave"), |c, ()| c.emit(Command::Checkpoint))?;
    r.f("CMissionBlockManager:SetMBlockStateById", |c, (block, state): (Text, Num)| match (block.id(), LayerState::from_code(state.or(-1.0) as i64)) {
        (Some(b), Some(s)) => c.set_block(b, s),
        _ => c.warn(format!("SetMBlockStateById({}, {:?})", block.0, state.0)),
    })?;

    // ---- globals around missions
    r.f("MissionGetSessionGlobal", |c, name: Text| c.session.get(&name.0).copied().unwrap_or(false))?;
    r.f("MissionSetSessionGlobal", |c, (name, value): (Text, Flag)| {
        c.session.insert(name.0, value.0);
    })?;
    r.f("HasExclusiveMissionRunning", |c, ()| c.missions.running().any(|m| m.exclusive))?;
    r.f("GetMissionXpReward", |c, name: Text| c.data.rewards.get(&name.0.to_ascii_lowercase()).map(|r| r.xp).unwrap_or(0))?;
    r.f("GetMissionMoneyReward", |c, name: Text| c.data.rewards.get(&name.0.to_ascii_lowercase()).map(|r| r.money).unwrap_or(0))?;
    r.f("AbandonMission", |c, ()| c.emit(Command::Native { name: "AbandonMission".into(), args: vec![] }))?;

    // ---- safehouses and radio towers (unlocked state kept here; the host
    // shows it and sends the unlock events)
    r.f("IsSafeHouseUnlocked", |c, id: Text| id.id().is_some_and(|i| c.safehouses_unlocked.contains(&i)))?;
    r.f("UnlockSafeHouse", |c, (id, silent): (Text, Flag)| {
        if let Some(i) = id.id() {
            if c.safehouses_unlocked.insert(i) {
                c.emit(Command::SafeHouse { id: i, unlocked: true, silent: silent.0 });
                c.raise(&Listen::Entity(i, "SafeHouseUnlockedEvent".into()), vec![]);
            }
        }
    })?;
    r.f("LockSafeHouse", |c, id: Text| {
        if let Some(i) = id.id() {
            if c.safehouses_unlocked.remove(&i) {
                c.emit(Command::SafeHouse { id: i, unlocked: false, silent: true });
            }
        }
    })?;
    r.f("IsRadioTowerUnlocked", |c, id: Text| id.id().is_some_and(|i| c.radio_towers_unlocked.contains(&i)))?;
    r.constant("IsSafeHouseAIInCombat", false)?;
    r.constant("IsSafeHouseAlarmActivated", false)?;
    r.command("SafeHouseDeactivateMissionBlock")?;
    r.command("SafeHouseDeactivateMissionBlockOnUnlock")?;
    r.f("SetFastTravelEnabled", |c, on: Flag| c.missions.fast_travel_disabled = !on.0)?;
    r.command("SetEnableSaveGame")?;
    Ok(())
}
