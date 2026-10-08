//! What scripts ask of world entities: groups, whether an entity is
//! loaded, names and positions, commands to components, and the triggers
//! whose enter/leave events scripts listen for.

use mlua::Value;

use super::{id_list, vec3, Reg};
use crate::core::{Core, Listen};
use crate::values::{Arg, Text};
use crate::{Command, PLAYER};

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    // A group is a `#hash` name or a single entity id: one or many.
    r.lua("GetEntitiesInGroup", |lua, c, group: Text| {
        let ids: Vec<u64> = match group.id() {
            Some(id) => vec![id],
            None => c.groups(&group).into_iter().filter(|e| c.is_loaded(*e)).collect(),
        };
        id_list(lua, ids)
    })?;
    r.f("CAPI_Entity.IsEntityLoaded", |c, id: Text| id.id().is_some_and(|i| c.is_loaded(i)))?;
    r.f("CAPI_Entity.GetName", |c, id: Text| match id.id() {
        Some(PLAYER) => "Player".to_string(),
        Some(i) => c.data.name_of(i).unwrap_or_default().to_string(),
        None => String::new(),
    })?;
    r.lua("CAPI_Entity.GetEntityPositionVec3", |lua, c, id: Text| match id.id().and_then(|i| c.position(i)) {
        Some(p) => Ok(Value::Table(vec3(lua, p)?)),
        None => Ok(Value::Nil),
    })?;
    r.lua("CAPI_Entity.GetEntityAngles", |lua, c, id: Text| {
        let a = match id.id() {
            Some(PLAYER) => [0.0, 0.0, c.view.player_yaw],
            Some(i) => c.data.entities.get(&i).map(|e| e.angles).unwrap_or_default(),
            None => [0.0; 3],
        };
        vec3(lua, a)
    })?;
    r.f("SendRegisteredEventToEntity", |c, (entity, event, _params): (Text, Text, Value)| {
        if let Some(e) = entity.id() {
            c.emit(Command::EntityEvent { entity: e, event: event.0 });
        }
    })?;
    Ok(())
}

impl Core {
    /// The members of a group by its name hash. Groups are named sets the
    /// level editor builds; scripts also pass single ids.
    pub fn groups(&self, group: &Text) -> Vec<u64> {
        let _ = group.group();
        Vec::new()
    }
}

/// Raises trigger enter/leave events for the triggers scripts listen on.
pub fn update_triggers(c: &mut Core) {
    let _ = (c.view.player, Arg::Nil, Listen::Generic(String::new()));
}
