//! What scripts ask of world entities: groups, whether an entity is
//! loaded, names and positions, distances, commands to components, spawning
//! and removing, and the triggers whose enter/leave events scripts listen
//! for.

use mlua::Value;

use super::{callback, id_list, vec3, Reg};
use crate::core::{Core, Listen};
use crate::values::{Arg, Num, Text};
use crate::{Command, PLAYER};

fn dist(a: [f32; 3], b: [f32; 3], flat: bool) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], if flat { 0.0 } else { a[2] - b[2] }];
    ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as f64).sqrt()
}

/// A `{x, y, z}` table's values.
pub(crate) fn read_vec3(v: &Value) -> [f32; 3] {
    match v {
        Value::Table(t) => [1, 2, 3].map(|i| t.get::<Num>(i).map(|n| n.or(0.0) as f32).unwrap_or(0.0)),
        _ => [0.0; 3],
    }
}

impl Core {
    /// The members of a group: a `#hash` name (its members as the editor
    /// placed them, with the changes scripts made), or a single id.
    pub fn groups(&self, group: &Text) -> Vec<u64> {
        if let Some(id) = group.id() {
            return vec![id];
        }
        let mut out: Vec<u64> = group.group().and_then(|h| self.data.groups.get(&h)).cloned().unwrap_or_default();
        out.retain(|e| !self.group_removed.contains(&(group.0.clone(), *e)));
        out.extend(self.group_added.iter().filter(|(g, _)| *g == group.0).map(|(_, e)| *e));
        out
    }

    /// Whether an id names something that exists (in the data, spawned by
    /// a script or by the host).
    pub fn is_valid(&self, id: u64) -> bool {
        id == PLAYER
            || (!self.removed.contains(&id)
                && (self.data.entities.contains_key(&id)
                    || self.data.spawn_points.contains_key(&id)
                    || self.data.markers.contains_key(&id)
                    || self.spawned.contains_key(&id)
                    || self.view.spawned.contains_key(&id)))
    }

    pub fn remove_entity(&mut self, id: u64) {
        if id != PLAYER && self.removed.insert(id) {
            self.emit(Command::Remove(id));
            self.raise(&Listen::Remove(id), vec![]);
        }
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    r.lua("GetEntitiesInGroup", |lua, c, group: Text| {
        let ids: Vec<u64> = match group.id() {
            Some(id) => vec![id],
            None => c.groups(&group).into_iter().filter(|e| c.is_loaded(*e) && !c.dead.contains(e)).collect(),
        };
        id_list(lua, ids)
    })?;
    r.lua("GetEntitiesStartsWith", |lua, c, token: Text| {
        let t = token.0.to_ascii_lowercase();
        let ids: Vec<u64> = c.data.entities.values().filter(|e| e.name.to_ascii_lowercase().starts_with(&t) && c.is_loaded(e.id)).map(|e| e.id).collect();
        id_list(lua, ids)
    })?;
    r.f("CAPI_Entity.AddEntityToGroup", |c, (entity, group): (Text, Text)| {
        if let Some(e) = entity.id() {
            c.group_removed.remove(&(group.0.clone(), e));
            c.group_added.insert((group.0, e));
        }
    })?;
    r.f("CAPI_Entity.RemoveEntityFromGroup", |c, (entity, group): (Text, Text)| {
        if let Some(e) = entity.id() {
            c.group_added.remove(&(group.0.clone(), e));
            c.group_removed.insert((group.0, e));
        }
    })?;
    r.f("CAPI_Entity.IsEntityLoaded", |c, id: Text| {
        let loaded = id.id().filter(|i| c.is_loaded(*i));
        if let Some(i) = loaded {
            c.told_loaded.insert(i);
        }
        loaded.is_some()
    })?;
    r.f("IsEntityValid", |c, id: Text| id.id().is_some_and(|i| c.is_valid(i)))?;
    // (Simple animations, an outpost's flag raised, aren't played: none
    // takes any time.)
    r.f("CAPI_Entity.GetSimpleAnimDuration", |_, _id: Text| 0.0)?;
    r.f("CAPI_Entity.GetName", |c, id: Text| match id.id() {
        Some(PLAYER) => "Player".to_string(),
        Some(i) => c.data.name_of(i).map(str::to_string).or_else(|| c.spawned.get(&i).cloned()).unwrap_or_default(),
        None => String::new(),
    })?;
    r.f("CAPI_Entity.GetEntityIdType", |c, id: Text| match id.id() {
        Some(PLAYER) => 1.0,
        Some(i) if c.data.entities.get(&i).is_some_and(|e| e.class.contains("Pawn")) => 1.0,
        _ => 0.0,
    })?;
    r.lua("CAPI_Entity.GetEntityPositionVec3", |lua, c, id: Text| match id.id().and_then(|i| c.position(i)) {
        Some(p) => Ok(Value::Table(vec3(lua, p)?)),
        None => Ok(Value::Nil),
    })?;
    r.f("CAPI_Entity.GetEntityPosition", |c, (id, axis): (Text, Num)| {
        id.id().and_then(|i| c.position(i)).map(|p| p[(axis.or(0.0) as usize).min(2)] as f64).unwrap_or(0.0)
    })?;
    let angles = |c: &Core, id: Option<u64>| id.map(|i| c.angles(i)).unwrap_or_default();
    r.lua("CAPI_Entity.GetEntityAngles", move |lua, c, id: Text| vec3(lua, angles(c, id.id())))?;
    r.f("CAPI_Entity.GetEntityAngle", move |c, (id, axis): (Text, Num)| angles(c, id.id())[(axis.or(0.0) as usize).min(2)] as f64)?;
    r.lua("CAPI_Entity.GetBonePos", |lua, c, (id, bone): (Text, Text)| {
        let mut p = id.id().and_then(|i| c.position(i)).unwrap_or_default();
        // Eye height for the head and camera bones; the root otherwise.
        let b = bone.0.to_ascii_lowercase();
        if b.contains("camera") || b.contains("head") {
            p[2] += 1.65;
        }
        vec3(lua, p)
    })?;
    r.lua("CAPI_Entity.GetBoneRot", move |lua, c, (id, _bone): (Text, Text)| vec3(lua, angles(c, id.id())))?;
    r.f("GetDistance3D", |c, (a, b): (Text, Text)| match (a.id().and_then(|i| c.position(i)), b.id().and_then(|i| c.position(i))) {
        (Some(a), Some(b)) => dist(a, b, false),
        _ => f64::MAX,
    })?;
    r.f("GetDistance2D", |c, (a, b): (Text, Text)| match (a.id().and_then(|i| c.position(i)), b.id().and_then(|i| c.position(i))) {
        (Some(a), Some(b)) => dist(a, b, true),
        _ => f64::MAX,
    })?;
    r.lua("GetAnglesFromDir", |lua, _, dir: Value| {
        let d = read_vec3(&dir);
        let yaw = d[1].atan2(d[0]).to_degrees() - 90.0;
        let pitch = d[2].atan2((d[0] * d[0] + d[1] * d[1]).sqrt()).to_degrees();
        vec3(lua, [pitch, 0.0, yaw])
    })?;
    r.lua("GetShortestDiffVec3", |lua, _, (a, b): (Value, Value)| {
        let (a, b) = (read_vec3(&a), read_vec3(&b));
        let wrap = |x: f32| (x + 180.0).rem_euclid(360.0) - 180.0;
        vec3(lua, [wrap(b[0] - a[0]), wrap(b[1] - a[1]), wrap(b[2] - a[2])])
    })?;
    r.f("CAPI_Entity.GetEntityInPrefab", |c, (prefab, object): (Text, Text)| {
        // A prefab's parts carry its name as a prefix ("<prefab>/<object>").
        let found = prefab.id().and_then(|p| {
            if let Some(children) = c.data.entities.get(&p).and_then(|e| e.fields.child("Children")) {
                return children.children.iter().find(|child| child.str("Name").is_some_and(|name| name.eq_ignore_ascii_case(&object.0)))
                    .and_then(|child| child.u64("ID"));
            }
            let base = c.data.name_of(p)?.to_ascii_lowercase();
            let want = object.0.to_ascii_lowercase();
            c.data
                .entities
                .values()
                .find(|e| {
                    let n = e.name.to_ascii_lowercase();
                    n == format!("{base}/{want}") || (n.starts_with(&base) && n.ends_with(&format!("/{want}")))
                })
                .map(|e| e.id)
        });
        found.map(|i| i.to_string()).unwrap_or_else(|| fc3_domino::INVALID_ID.to_string())
    })?;
    r.lua("GetTriggerComponentContacts", |lua, c, trigger: Text| {
        let ids = trigger.id().map(|t| c.contacts(t)).unwrap_or_default();
        id_list(lua, ids)
    })?;
    r.f("GetMissionAckTriggerState", |c, trigger: Text| trigger.id().is_some_and(|t| c.contacts(t).contains(&PLAYER)))?;
    r.f("CAPI_Entity.IsEntityTagged", |c, id: Text| id.id().is_some_and(|i| c.view.tagged.contains(&i)))?;
    // An entity resized (the `ScaleModifier` box: DEN_060's vignettes of
    // Buck, Hoyt and Vaas, JAS_240's apparitions), blended a tick at a time
    // from its size as read back.
    r.f("CAPI_Entity.GetScale", |c, id: Text| id.id().and_then(|i| c.scales.get(&i).copied()).unwrap_or(1.0))?;
    r.f("CAPI_Entity.SetScale", |c, (id, scale): (Text, Num)| {
        let Some(i) = id.id() else { return };
        let scale = scale.or(1.0).max(0.0);
        c.scales.insert(i, scale);
        c.emit(Command::Native { name: "CAPI_Entity.SetScale".into(), args: vec![i.to_string(), scale.to_string()] });
    })?;
    r.f("CAPI_Entity.GetAlarmState", |c, alarm: Text| alarm.id().and_then(|a| c.alarms.get(&a)).map_or(0.0, |s| s.0 as f64))?;
    r.f("CAPI_Entity.GetAlarmUserId", |c, alarm: Text| {
        alarm.id().and_then(|a| c.alarms.get(&a)).map_or(fc3_domino::INVALID_ID.to_string(), |s| s.1.to_string())
    })?;
    // Alarms a script switches (the `AlarmModifier` box: 0 off, 1 set off,
    // 2 disabled, 3 destroyed; a reload's `RestoreAlarmState`): each box in
    // the group that changes takes the state, its groups hear of it as when
    // a guard or the player switches it, and the host sounds or silences it.
    r.f("SetAlarmState", |c, (group, state): (Text, Num)| {
        let state = state.or(0.0).clamp(0.0, 3.0) as u8;
        for id in c.groups(&group) {
            if c.alarms.get(&id).map_or(0, |s| s.0) == state {
                continue;
            }
            c.alarm_event(id, state, u64::MAX);
            c.emit(Command::Native { name: "SetAlarmState".into(), args: vec![id.to_string(), state.to_string()] });
        }
    })?;
    r.f("CAPI_Entity.GetLookAtTriggerState", |c, trigger: Text| trigger.id().is_some_and(|t| c.looked_at.contains(&t)))?;
    r.constant("CAPI_Entity.IsEntityBurnedOut", false)?;
    r.constant("CAPI_Entity.IsEntityBurning", false)?;
    r.constant("CAPI_Entity.IsEntityStoppedBurning", false)?;
    r.constant("CEntitySpawner_StaticGetSpawnedEntityId", fc3_domino::INVALID_ID)?;
    r.constant("CheckIsAnActiveArmyTarget", false)?;
    // Members in an AI state (0 relaxed, 1 investigating, 2 alert, 3 combat,
    // 4 dead), as the host reports them.
    r.f("GetGroupInStateMemberCount", |c, (group, state): (Text, Num)| {
        let state = state.or(0.0) as u8;
        c.groups(&group)
            .into_iter()
            .filter(|e| c.is_loaded(*e))
            .filter(|e| match state {
                4 => c.is_dead(*e),
                s => !c.is_dead(*e) && c.view.spawned.get(e).map(|l| l.ai_state).unwrap_or(0) == s,
            })
            .count() as f64
    })?;
    r.f("RegisterThreateningAction", |c, emitter: Text| c.emit(Command::Native { name: "RegisterThreateningAction".into(), args: vec![emitter.0] }))?;
    for name in ["CAPI_Entity.EnableZiplineGetOutPoint", "CAPI_Entity.UpdateMaterialFloat"] {
        let n = name.to_string();
        r.f(name, move |c, args: mlua::Variadic<Text>| {
            c.emit(Command::Native { name: n.clone(), args: args.into_iter().map(|t| t.0).collect() });
            true
        })?;
    }
    // A tree grows (or shrinks) to a size at a speed; the host reports when
    // it's there (`Story::growth_finished`, with the id this returns).
    r.f("CAPI_Entity.SetRealTreeGrowthSpeed", |c, (entity, growth, speed, target, method): (Text, Num, Num, Value, Option<String>)| {
        let id = c.next_id();
        c.growths.insert(id, callback(target, method));
        c.emit(Command::Native {
            name: "CAPI_Entity.SetRealTreeGrowthSpeed".into(),
            args: vec![entity.0, growth.or(1.0).to_string(), speed.or(1.0).to_string(), id.to_string()],
        });
        id.to_string()
    })?;
    // (Stopped, it never calls back.)
    r.f("CAPI_Entity.StopRealTreeGrowth", |c, (entity, id): (Text, Text)| {
        if let Ok(n) = id.0.parse::<u64>() {
            c.growths.remove(&n);
        }
        c.emit(Command::Native { name: "CAPI_Entity.StopRealTreeGrowth".into(), args: vec![entity.0, id.0] });
        true
    })?;
    // Asynchronous helpers whose completion the host doesn't report: done
    // at once.
    r.f("CRecastNavmeshDynamicManager:RegenerateShape", |c, (_region, target, method): (Text, Value, Option<String>)| {
        let id = c.next_id();
        if let Some(cb) = callback(target, method) {
            c.call(cb, vec![]);
        }
        id.to_string()
    })?;
    r.f("CreateParticle", |c, (particle, pos, rot): (Text, Value, Value)| {
        let id = c.next_id();
        let (p, a) = (read_vec3(&pos), read_vec3(&rot));
        c.emit(Command::Native {
            name: "CreateParticle".into(),
            args: vec![id.to_string(), particle.0, format!("{} {} {}", p[0], p[1], p[2]), format!("{} {} {}", a[0], a[1], a[2])],
        });
        id as f64
    })?;
    // Specific gameplays (minigames, scripted QTEs, boss fights): the host
    // runs them; they end when destroyed, when the host says, or (one the
    // host doesn't run) when a sequence sets a QTE's result. A QTE gameplay
    // (`QTEGSF`) hears the result and ends as its move does.
    r.f("CreateSpecificGameplay", |c, (name, params): (Text, Value)| {
        let id = c.next_id();
        let mut args = vec![id.to_string(), name.0.clone()];
        if let Value::Table(t) = params {
            for (k, v) in t.pairs::<String, Text>().flatten() {
                if k == "QTE_move_file" {
                    c.qte_gameplays.insert(id);
                }
                args.push(format!("{k}={}", v.0));
            }
        }
        c.gameplays.insert(id, name.0);
        c.emit(Command::Native { name: "CreateSpecificGameplay".into(), args });
        id as f64
    })?;
    r.f("DestroySpecificGameplay", |c, id: Text| {
        if let Some(i) = id.id() {
            c.end_gameplay(i);
        }
    })?;
    r.f("SendGSFGenericEventToEntity", |c, args: mlua::Variadic<Text>| {
        let args: Vec<String> = args.into_iter().map(|t| t.0).collect();
        if args.get(2).is_some_and(|p| p == "QteSetResult") {
            if let Some((&latest, _)) = c.gameplays.iter().next_back().filter(|(id, name)| *name != "QTEGSF" && !c.qte_gameplays.contains(id)) {
                c.end_gameplay(latest);
            }
        }
        c.emit(Command::Native { name: "SendGSFGenericEventToEntity".into(), args });
    })?;
    // Reinforcements: a point's mercs (by archetype) come in under ids of
    // their own, join the point's groups, and the box hears who came
    // (`OnSpawned(mercs, vehicles)`); the host puts them in the world
    // (`point, mode, battlefield, id=archetype...`).
    r.f("SpawnReinforcement", |c, args: mlua::Variadic<Value>| {
        let a: Vec<Value> = args.into_iter().collect();
        let text = |i: usize| a.get(i).and_then(|v| v.to_string().ok()).unwrap_or_default();
        let Some(point) = text(0).parse::<u64>().ok() else { return false };
        let mode = text(1);
        let battlefield = text(2).parse::<u64>().unwrap_or(u64::MAX);
        let (mut archetypes, mut groups) = (Vec::new(), Vec::new());
        // The vehicle that brings them (the first the point names), when it
        // has a way in, and the groups it joins.
        let (mut vehicle, mut vehicle_groups) = (None, Vec::new());
        if let Some(rp) = c
            .data
            .entities
            .get(&point)
            .and_then(|e| e.component("CFCXAIComponent"))
            .and_then(|ai| ai.child("AIObject"))
            .and_then(|o| o.child("CReinforcementPoint"))
        {
            let path = rp.u64("entVehiclePathId").filter(|&p| p != 0 && p != u64::MAX);
            rp.walk(&mut |o, _| {
                if let Some(ark) = o.u64("arkMerc").filter(|&k| k != 0 && k != u64::MAX) {
                    archetypes.push(ark);
                }
                if let Some(g) = o.u32("MercGroupName").filter(|&g| g != 0 && g != u32::MAX) {
                    groups.push(format!("#{g:08X}"));
                }
                if let Some(ark) = o.u64("arkVehicle").filter(|&k| k != 0 && k != u64::MAX && path.is_some()) {
                    vehicle.get_or_insert(ark);
                }
                if let Some(g) = o.u32("VehicleGroupName").filter(|&g| g != 0 && g != u32::MAX) {
                    vehicle_groups.push(format!("#{g:08X}"));
                }
            });
        }
        let ids: Vec<u64> = archetypes.iter().map(|_| c.next_id() | crate::SPAWNED_IDS).collect();
        for (id, ark) in ids.iter().zip(&archetypes) {
            c.spawned.insert(*id, format!("{ark:016x}"));
            for g in &groups {
                c.group_added.insert((g.clone(), *id));
            }
        }
        let vehicle = vehicle.map(|ark| (c.next_id() | crate::SPAWNED_IDS, ark));
        if let Some((id, ark)) = vehicle {
            c.spawned.insert(id, format!("{ark:016x}"));
            for g in &vehicle_groups {
                c.group_added.insert((g.clone(), id));
            }
        }
        let mut out = vec![point.to_string(), mode, battlefield.to_string(), vehicle.map_or(String::new(), |(i, k)| format!("{i}={k:016x}"))];
        out.extend(ids.iter().zip(&archetypes).map(|(i, k)| format!("{i}={k:016x}")));
        c.emit(Command::Native { name: "SpawnReinforcement".into(), args: out });
        let cb = a.iter().position(|v| matches!(v, Value::Table(_))).and_then(|i| {
            let method = a.get(i + 1).and_then(|m| m.as_string().map(|s| s.to_string_lossy()));
            callback(a[i].clone(), method)
        });
        if let Some(cb) = cb {
            c.actions.push_back(crate::core::Action::Call(cb, vec![Arg::Ids(ids), Arg::Ids(vehicle.map(|v| v.0).into_iter().collect())]));
        }
        true
    })?;
    r.f("SpawnEntityFromArchetype", |c, (archetype, x, y, z, ax, ay, az): (Text, Num, Num, Num, Num, Num, Num)| {
        let id = c.next_id() | crate::SPAWNED_IDS;
        c.spawned.insert(id, archetype.0.clone());
        // (Where it stands, for the scripts and for using it.)
        let v = |n: Num| n.or(0.0) as f32;
        c.poses.insert(id, crate::Pose { position: [v(x), v(y), v(z)], angles: [v(ax), v(ay), v(az)] });
        let f = |n: Num| n.or(0.0).to_string();
        c.emit(Command::Native { name: "SpawnEntityFromArchetype".into(), args: vec![id.to_string(), archetype.0, f(x), f(y), f(z), f(ax), f(ay), f(az)] });
        id.to_string()
    })?;
    r.f("CAPI_Entity.RemoveEntity", |c, id: Text| {
        if let Some(i) = id.id() {
            c.remove_entity(i)
        }
    })?;
    r.f("RemoveEntityOrGroup", |c, group: Text| {
        for e in c.groups(&group) {
            c.remove_entity(e);
        }
    })?;
    // Objective markers (shown by the host where `Story::objective_markers`
    // says) and entities riding on others.
    r.f("GlobalSetObjectiveMarker", |c, (marker, on, _text, _oasis, sub): (Text, crate::values::Flag, Text, Num, Text)| {
        let Some(m) = marker.id() else { return };
        if on.0 {
            c.objective_markers.insert(m, sub.0);
        } else {
            c.objective_markers.remove(&m);
        }
    })?;
    r.f("MissionSetObjectiveMarker", |c, (on, marker, _oasis): (crate::values::Flag, Text, Num)| {
        let Some(m) = marker.id() else { return };
        if on.0 {
            c.objective_markers.insert(m, String::new());
        } else {
            c.objective_markers.remove(&m);
        }
    })?;
    r.f("CAPI_Entity.Bind", |c, (a, b, bone, _offset): (Text, Text, Text, Value)| {
        if let (Some(a), Some(b)) = (a.id(), b.id()) {
            c.binds.insert(b, a);
            c.emit(Command::Native { name: "CAPI_Entity.Bind".into(), args: vec![a.to_string(), b.to_string(), bone.0] });
        }
    })?;
    r.f("CAPI_Entity.UnBind", |c, (a, b): (Text, Text)| {
        if let Some(b) = b.id() {
            c.binds.remove(&b);
            c.emit(Command::Native { name: "CAPI_Entity.UnBind".into(), args: vec![a.0, b.to_string()] });
        }
    })?;
    // A teleported player waits (behind a loading screen) for the world
    // round them: `DominoGameReady` comes once it has had time to stream.
    r.f("CAPI_Entity.TeleportEntity", |c, args: mlua::Variadic<Text>| {
        let (Some(entity), Some(destination)) = (args.first().and_then(Text::id), args.get(1).and_then(Text::id)) else { return };
        let Some(pose) = c.pose(destination) else { return };
        c.poses.insert(entity, pose);
        // Queries made later in this same script call see the new position.
        if let Some(live) = c.view.spawned.get_mut(&entity) {
            live.pos = pose.position;
        }
        if entity == PLAYER {
            c.view.player = pose.position;
            c.view.player_yaw = pose.angles[2];
            c.last_player = None;
            c.world_ready_at = Some(c.time + 1.0);
        }
        c.emit(Command::Teleport { entity, pose });
    })?;
    r.f("CAPI_Vehicle.LockPosition", |c, (vehicle, locked): (Text, crate::values::Flag)| {
        if let Some(v) = vehicle.id() {
            c.vehicle_rules.entry(v).or_default().position_locked = locked.0;
        }
    })?;
    r.f("SendRegisteredEventToEntity", |c, (entity, event, _params): (Text, Text, Value)| {
        let Some(e) = entity.id() else { return };
        match event.0.as_str() {
            // (Who was inside counts afresh: enabled with someone in it, it
            // reports them entering.)
            "CBaseTriggerComponent_SetEnabled" => {
                c.trigger_enabled.insert(e, true);
                c.trigger_contacts.remove(&e);
            }
            "CBaseTriggerComponent_SetDisabled" => {
                c.trigger_enabled.insert(e, false);
                c.trigger_contacts.remove(&e);
            }
            "CProximityTriggerComponent_SetAsUsable" => c.use_rules.entry(e).or_default().usable = Some(true),
            "CProximityTriggerComponent_SetAsUnusable" => c.use_rules.entry(e).or_default().usable = Some(false),
            "CProximityTriggerComponent_SetCheckLookAt" => c.use_rules.entry(e).or_default().look_at = Some(true),
            "CProximityTriggerComponent_SetNotCheckLookAt" => c.use_rules.entry(e).or_default().look_at = Some(false),
            "CProximityTriggerComponent_SetCheckAngle" => c.use_rules.entry(e).or_default().angle = Some(true),
            "CProximityTriggerComponent_SetNotCheckAngle" => c.use_rules.entry(e).or_default().angle = Some(false),
            "CVehicle_SetAsUsable" => c.vehicle_rules.entry(e).or_default().unusable = false,
            "CVehicle_SetAsUnusable" => c.vehicle_rules.entry(e).or_default().unusable = true,
            "CVehicle_SetExitDisabled" => c.vehicle_rules.entry(e).or_default().exit_disabled = true,
            "CVehicle_SetExitEnabled" => c.vehicle_rules.entry(e).or_default().exit_disabled = false,
            "CVehicle_SetAsAnimated" => c.vehicle_rules.entry(e).or_default().animated = true,
            "CVehicle_SetAsNotAnimated" => c.vehicle_rules.entry(e).or_default().animated = false,
            "CVehicle_SetAsIndestructable" => c.vehicle_rules.entry(e).or_default().indestructible = true,
            "CVehicle_SetAsDestructable" => c.vehicle_rules.entry(e).or_default().indestructible = false,
            "CVehicle_SetEngineMinorDamage" => c.vehicle_rules.entry(e).or_default().engine = 1,
            "CVehicle_SetEngineMajorDamage" => c.vehicle_rules.entry(e).or_default().engine = 2,
            "CVehicle_SetEngineBroken" | "CVehicle_SetEngineDisabled" => c.vehicle_rules.entry(e).or_default().engine = 3,
            _ => {}
        }
        c.emit(Command::EntityEvent { entity: e, event: event.0 });
    })?;
    r.f("SendCommandEventToEntity", |c, (entity, event, params): (Text, Text, Value)| {
        let mut args = vec![entity.0, event.0];
        if let Value::Table(t) = params {
            args.extend(t.sequence_values::<Text>().filter_map(Result::ok).map(|t| t.0));
        }
        // A pawn made usable (`SetUsability <on> <ignore orientation>`): the
        // player can use it (`use_target`).
        if args[1] == "SetUsability" {
            if let Some(id) = Text(args[0].clone()).id() {
                let flag = |i: usize| args.get(i).is_some_and(|a| a == "true" || a == "1");
                if flag(2) {
                    c.usable_pawns.insert(id, flag(3));
                } else {
                    c.usable_pawns.remove(&id);
                }
            }
        }
        c.emit(Command::Native { name: "SendCommandEventToEntity".into(), args });
        true
    })?;
    Ok(())
}

impl Core {
    /// Where a look-at trigger is now: one placed on a character rides with
    /// it (`triggers::carrier_of`), others stay where they were placed.
    pub fn look_at_point(&mut self, trigger: u64) -> Option<[f32; 3]> {
        let e = self.data.entities.get(&trigger)?;
        let carrier = *self.look_carriers.entry(trigger).or_insert_with(|| crate::triggers::carrier_of(e, &self.data.entities));
        let live = carrier.and_then(|(pawn, d)| self.view.spawned.get(&pawn).map(|l| [l.pos[0] + d[0], l.pos[1] + d[1], l.pos[2] + d[2]]));
        Some(live.unwrap_or(e.pos))
    }
}

/// Raises trigger enter/leave events for the triggers scripts listen on:
/// the host reports who is inside each trigger (`View::contacts`); changes
/// since the last frame become `Enter` / `Leave` events with the entity.
pub fn update_triggers(c: &mut Core) {
    let watched: Vec<u64> = c
        .listeners
        .values()
        .filter_map(|l| match &l.on {
            Listen::Entity(t, e) if e == "Enter" || e == "Leave" => Some(*t),
            _ => None,
        })
        .collect();
    // A trigger nobody listens on any more forgets who was in it; one
    // listened on again reports them entering.
    c.trigger_contacts.retain(|t, _| watched.contains(t));
    // Look-at triggers: the player's view coming onto them and leaving.
    let looks: Vec<u64> = c
        .listeners
        .values()
        .filter_map(|l| match &l.on {
            Listen::Entity(t, e) if e == "EnterFOV" || e == "LeaveFOV" => Some(*t),
            _ => None,
        })
        .collect();
    c.looked_at.retain(|t| looks.contains(t));
    let eye = [c.view.player[0], c.view.player[1], c.view.player[2] + 1.6];
    for t in looks {
        let at = c.look_at_point(t);
        let seen = c.trigger_on(t)
            && c.is_loaded(t)
            && at
                .zip(c.data.entities.get(&t))
                .is_some_and(|(at, e)| crate::triggers::looked_at(e, at, eye, c.view.look, if c.view.fov > 1.0 { c.view.fov } else { 60.0 }));
        if seen && c.looked_at.insert(t) {
            c.raise(&Listen::Entity(t, "EnterFOV".into()), vec![Arg::id(PLAYER)]);
        } else if !seen && c.looked_at.remove(&t) {
            c.raise(&Listen::Entity(t, "LeaveFOV".into()), vec![Arg::id(PLAYER)]);
        }
    }
    for t in watched {
        // (Another mission's acknowledge shape is closed while the player is
        // on one: it reports him entering once it opens.)
        let now: Vec<u64> = if c.ack_shape_closed(t) { Vec::new() } else { c.contacts(t) };
        let before = c.trigger_contacts.get(&t).cloned().unwrap_or_default();
        for e in now.iter().filter(|e| !before.contains(e)) {
            c.raise(&Listen::Entity(t, "Enter".into()), vec![Arg::id(*e)]);
        }
        for e in before.iter().filter(|e| !now.contains(e)) {
            c.raise(&Listen::Entity(t, "Leave".into()), vec![Arg::id(*e)]);
        }
        c.trigger_contacts.insert(t, now);
    }
    c.last_player = Some(c.view.player);
}
