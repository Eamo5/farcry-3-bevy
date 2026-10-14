//! Script plumbing: identity and logging, callback registration (entity,
//! generic, spawn, message, state, console, group and input events),
//! timers and per-frame ticks.

use mlua::Value;

use super::{callback, Reg};
use crate::core::{Delay, Listen};
use crate::values::{Flag, Num, Text};
use crate::{Command, PLAYER};

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    // ---- identity, game mode, logging
    r.f("GetLocalPlayerId", |_, ()| PLAYER.to_string())?;
    r.constant("GetInvalidEntityId", fc3_domino::INVALID_ID)?;
    // Names compared by id: CRC-32 of the text (or of it in lower case).
    r.f("GetStringID", |_, t: Text| fc3_formats::hash::crc32(t.0.as_bytes()))?;
    r.f("GetNoCaseStringID", |_, t: Text| fc3_formats::hash::crc32(t.0.to_ascii_lowercase().as_bytes()))?;
    r.f("GetStringIDLogStr", |_, t: Text| t.0)?;
    r.constant("GetCurrentGameModeName", "FCXSingle")?;
    r.constant("IsScriptAutorunEnabled", true)?;
    r.constant("IsQuickLaunching", false)?;
    r.constant("IsPlayerSetupOnQuicklaunchDisabled", true)?;
    r.constant("IsNudityAllowed", true)?;
    r.constant("IsSavingGame", false)?;
    r.constant("GetDemoType", "None")?;
    // The add-on content is part of the installed game's data.
    r.constant("IsLockedContentUnlocked", true)?;
    r.f("LogConsole", |c, t: Text| c.warn(format!("console: {}", t.0)))?;
    r.f("DrawTextToScreen", |c, (t, _secs): (Text, Num)| c.emit(Command::Print(t.0)))?;
    r.f("DoConsoleCommand", |c, t: Text| c.warn(format!("console command: {}", t.0)))?;

    // ---- callbacks (CScriptCallbackSystem)
    r.f("CScriptCallbackSystem:RegisterEventCallback", |c, (entity, target, method, event): (Text, Value, Option<String>, Text)| {
        let cb = callback(target, method)?;
        let entity = entity.id().unwrap_or(u64::MAX);
        Some(c.listen(Listen::Entity(entity, event.0), cb).to_string())
    })?;
    r.f("CScriptCallbackSystem:RemoveCallback", |c, (_entity, id): (Text, Text)| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;
    r.f("CScriptCallbackSystem:RegisterGenericCallback", |c, (target, method, event): (Value, Option<String>, Text)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Generic(event.0), cb).to_string())
    })?;
    r.f("CScriptCallbackSystem:RemoveGenericCallback", |c, (_event, id): (Text, Text)| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;
    r.f("CScriptCallbackSystem:RegisterOnSpawnCallback", |c, (entity, target, method): (Text, Value, Option<String>)| {
        let cb = callback(target, method)?;
        let entity = entity.id().unwrap_or(u64::MAX);
        let id = c.listen(Listen::Spawn(entity), cb.clone());
        // Subscriptions often come after the layer was made present (where
        // the engine would spawn the entity a moment later). Queue the
        // callback so the script can finish storing its subscription id;
        // not when the script has just been told it's there (it deals with
        // that itself, as the entity status listener does).
        if c.is_loaded(entity) && !c.told_loaded.contains(&entity) {
            c.call(cb, vec![]);
        }
        Some(id.to_string())
    })?;
    r.f("CScriptCallbackSystem:RegisterOnRemoveCallback", |c, (entity, target, method): (Text, Value, Option<String>)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Remove(entity.id().unwrap_or(u64::MAX)), cb).to_string())
    })?;
    r.f("CScriptCallbackSystem:RegisterMessageListener", |c, (message, target, method): (Text, Value, Option<String>)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Message(message.0), cb).to_string())
    })?;
    r.f("CScriptCallbackSystem:UnregisterMessageListener", |c, id: Text| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;
    r.f("CScriptCallbackSystem:BroadcastMessage", |c, message: Text| {
        c.emit(Command::Message(message.0.clone()));
        c.raise(&Listen::Message(message.0), vec![]);
    })?;
    // The message the engine sends for exclusive missions (also bound as a
    // plain global).
    r.f("BroadcastDominoMessage", |c, message: Text| {
        c.emit(Command::Message(message.0.clone()));
        c.raise(&Listen::Message(message.0), vec![]);
    })?;

    // ---- pawn state-machine states
    r.f("CGOStateMachineScriptCallbackSystem:RegisterStateCallback", |c, (entity, target, method, state): (Text, Value, Option<String>, Text)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::State(entity.id().unwrap_or(PLAYER), state.0), cb).to_string())
    })?;
    r.f("CGOStateMachineScriptCallbackSystem:UnregisterStateCallback", |c, (_entity, id): (Text, Text)| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;

    // ---- debug console commands
    r.f("CDominoConsoleCommandManager:RegisterConsoleCommand", |c, (name, target, method): (Text, Value, Option<String>)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Console(name.0.to_ascii_lowercase()), cb).to_string())
    })?;
    r.f("CDominoConsoleCommandManager:UnregisterConsoleCommand", |c, id: Text| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;

    // ---- group events (numbers for ids)
    r.f("CEntityGroupManager:RegisterScriptEvent", |c, (group, event, target, method): (Text, Text, Value, Option<String>)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Group(group.0, event.0), cb) as f64)
    })?;
    r.f("CEntityGroupManager:UnregisterScriptEvent", |c, (_group, id): (Text, Num)| {
        if let Some(id) = id.0 {
            c.unlisten(id as u64)
        }
    })?;

    // ---- input signals on the player
    r.f("RegisterDominoInputListenerSignal", |c, (_entity, target, method, signal): (Text, Value, Option<String>, Text)| {
        let cb = callback(target, method)?;
        Some(c.listen(Listen::Input(signal.0), cb).to_string())
    })?;
    r.f("UnregisterDominoInputListenerSignal", |c, (_entity, id): (Text, Text)| {
        if let Some(id) = id.id() {
            c.unlisten(id)
        }
    })?;

    // ---- timers (numbers for ids)
    r.f("CDominoDelayManager:CreateDelay", |c, (seconds, target, method): (Num, Value, Option<String>)| {
        let cb = callback(target, method)?;
        let id = c.next_id();
        c.delays.insert(id, Delay { seconds: seconds.or(1.0), elapsed: 0.0, running: false, callback: cb });
        Some(id as f64)
    })?;
    r.f("CDominoDelayManager:SetDelay", |c, (id, seconds): (Num, Num)| {
        if let Some(d) = id.0.and_then(|i| c.delays.get_mut(&(i as u64))) {
            d.seconds = seconds.or(d.seconds);
        }
    })?;
    r.f("CDominoDelayManager:SendCommand", |c, (id, command): (Num, Text)| {
        let Some(d) = id.0.and_then(|i| c.delays.get_mut(&(i as u64))) else { return };
        match command.0.as_str() {
            "start" | "restart" => {
                d.elapsed = 0.0;
                d.running = true;
            }
            "pause" => d.running = false,
            "resume" => d.running = true,
            "stop" => {
                d.running = false;
                d.elapsed = 0.0;
            }
            _ => {}
        }
    })?;
    r.f("CDominoDelayManager:RemoveDelay", |c, id: Num| {
        if let Some(i) = id.0 {
            c.delays.remove(&(i as u64));
        }
    })?;
    r.f("CDominoTickManager:RegisterTickFunction", |c, (target, method): (Value, Option<String>)| {
        let cb = callback(target, method)?;
        let id = c.next_id();
        c.ticks.insert(id, cb);
        Some(id as f64)
    })?;
    r.f("CDominoTickManager:UnregisterTickFunction", |c, id: Num| {
        if let Some(i) = id.0 {
            c.ticks.remove(&(i as u64));
        }
    })?;

    // ---- the loading screen hand-shake: the engine waits for a script to
    // say the game may start (after a load, a reload or a teleport).
    r.f("SendPlayGame", |c, ()| {
        c.play_game = true;
        c.emit(Command::PlayGame);
        c.raise(&Listen::Generic("PlayGame".into()), vec![]);
    })?;
    r.f("IsPlayGameReceived", |c, ()| c.play_game)?;
    r.f("IsWaitingPlayGame", |_, _will_send: Flag| true)?;
    Ok(())
}
