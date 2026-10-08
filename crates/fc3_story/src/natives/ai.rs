//! Contextual actions ("smart terrain points") scripts give characters
//! through AI facts: `DominoCAOverride` (do this action), `DominoCAInterrupted`
//! and `DominoCAStopped` (stop it). The story keeps which action each
//! character is on and raises the action's events (`CA_Select`, `CA_Start`,
//! `CA_End`, `CA_Interrupt`, on the action entity with the character as its
//! user) as the engine does: interrupts and selections at once, starts and
//! ends when the host reports the character got there or finished.

use std::collections::HashMap;

use super::Reg;
use crate::core::{Core, Listen};
use crate::values::{Arg, Text};
use crate::Command;

/// A character's current contextual action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub action: u64,
    /// The character reached it and began.
    pub started: bool,
}

#[derive(Default)]
pub struct Actions {
    pub current: HashMap<u64, Current>,
}

impl Core {
    /// Raises a contextual action's event for the scripts listening on it.
    pub fn action_event(&mut self, action: u64, event: &str, user: u64) {
        let text = action.to_string();
        self.raise(&Listen::Group(text, event.to_string()), vec![Arg::Str(event.to_string()), Arg::id(user)]);
    }

    fn give_action(&mut self, pawn: u64, action: u64) {
        if let Some(cur) = self.ca.current.get(&pawn).copied() {
            if cur.started {
                self.action_event(cur.action, "CA_Interrupt", pawn);
            }
        }
        self.ca.current.insert(pawn, Current { action, started: false });
        self.emit(Command::Action { pawn, action: Some(action) });
        self.action_event(action, "CA_Select", pawn);
    }

    fn stop_action(&mut self, pawn: u64, action: u64, interrupted: bool) {
        if self.ca.current.get(&pawn).is_some_and(|c| c.action == action) {
            self.ca.current.remove(&pawn);
            self.emit(Command::Action { pawn, action: None });
            self.action_event(action, if interrupted { "CA_Interrupt" } else { "CA_End" }, pawn);
        }
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    r.f("SetAIFactEntityId", |c, (group, fact, value): (Text, Text, Text)| {
        let pawns = c.groups(&group);
        match (fact.0.as_str(), value.id()) {
            ("DominoCAOverride", Some(action)) => {
                for p in pawns {
                    c.give_action(p, action);
                }
            }
            ("DominoCAInterrupted", Some(action)) => {
                for p in pawns {
                    c.stop_action(p, action, true);
                }
            }
            ("DominoCAStopped", Some(action)) => {
                for p in pawns {
                    c.stop_action(p, action, false);
                }
            }
            _ => c.emit(Command::Native { name: "SetAIFactEntityId".into(), args: vec![group.0, fact.0, value.0] }),
        }
    })?;
    Ok(())
}
