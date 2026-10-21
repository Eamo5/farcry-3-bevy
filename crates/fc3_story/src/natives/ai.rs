//! Contextual actions ("smart terrain points") scripts give characters
//! through AI facts: `DominoCAOverride` (do this action), `DominoCAInterrupted`
//! and `DominoCAStopped` (stop it). The story keeps which action each
//! character is on and raises the action's events (`CA_Select`, `CA_Start`,
//! `CA_End`, `CA_Interrupt`, on the action entity with the character as its
//! user) as the engine does: interrupts and selections at once, starts and
//! ends when the host reports the character got there or finished. An action
//! switched off (`ToggleContextualActionState`) sends whoever is on it away;
//! its end comes once they're up, [`LEAVE_SECS`] on (scripts turn to listen
//! for it just after switching it off). One switched on is taken up, as the
//! characters' own AI takes up the actions around them, by a free member of
//! the group it is for (`GroupFilter`) within its idle radius; one whose
//! action ends takes up a switched-on one near it the same way.

use std::collections::HashMap;

use super::Reg;
use crate::core::{Core, Listen};
use crate::values::{Arg, Num, Text};
use crate::Command;

/// How long a character takes to get up from an action switched off.
pub const LEAVE_SECS: f64 = 1.0;

/// When an action's markups come after it starts (s), for actions that
/// don't stage a sequence (whose event clips say: `StoryData::action_markups`):
/// their moves' own markup times aren't read, so the first comes a little in
/// and the second after it.
pub const MARKUP_SECS: [(f64, &str); 2] = [(2.0, "CA_Markup1"), (4.0, "CA_Markup2")];

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
    /// Points scripts switched on (`ToggleContextualActionState`), for the
    /// characters free to take them up.
    pub enabled: std::collections::BTreeSet<u64>,
    /// When the switched-on points no one is on are next offered round.
    pub next_offer: f64,
    /// Points that found no one, told once (for working out a stall).
    pub unclaimed: std::collections::HashSet<u64>,
    /// Points ended or broken off, when each is free again (its idle
    /// `CoolDownTime` on) and who was on it: `CA_Available` then.
    pub cooling: Vec<(f64, u64, u64)>,
}

impl Actions {
    /// The points on from the start (`Enable` set where placed) that a group
    /// takes up by itself.
    pub fn from_data(data: &crate::data::StoryData) -> Self {
        let enabled = data
            .entities
            .values()
            .filter(|e| e.component("CAIContextualActionComponent").and_then(|c| c.bool("Enable")).unwrap_or(false) && offer_of(e).is_some())
            .map(|e| e.id)
            .collect();
        Actions { enabled, ..Default::default() }
    }
}

/// How often (s) a switched-on point no one is on is offered again to its
/// group (as the characters' idle AI keeps looking for one: a guard who
/// arrives after it was switched on takes it up).
const OFFER_EVERY: f64 = 1.0;

/// The states a contextual action point is for (`Idle`'s relax, cautious
/// and combat flags; none set: any): whether a character relaxed (AI state
/// 0), on its guard (1, 2) or fighting (3, or sent to a battlefield) takes it.
fn fits_state(e: &crate::data::Entry, state: u8, fighting: bool) -> bool {
    let Some(idle) = e.component("CAIContextualActionComponent").and_then(|c| c.child("Idle")) else { return true };
    let flag = |n: &str| idle.bool(n).unwrap_or(false);
    let (relax, cautious, combat) = (flag("bIsRelaxAction"), flag("bIsCautiousAction"), flag("bIsCombatAction"));
    if !(relax || cautious || combat) {
        return true;
    }
    (relax && state == 0 && !fighting) || (cautious && matches!(state, 1 | 2)) || (combat && (state == 3 || fighting))
}

/// Who may take up a contextual action point by themselves, and from how
/// far: its group filter and its idle radius.
fn offer_of(e: &crate::data::Entry) -> Option<(u32, f32)> {
    let ca = e.component("CAIContextualActionComponent")?;
    let group = ca.u32("GroupFilter").filter(|&g| g != 0 && g != u32::MAX)?;
    let radius = ca.child("Idle").and_then(|i| i.f32("Radius")).unwrap_or(5.0).max(1.0);
    Some((group, radius))
}

/// How near (m, across the ground) a point must be, both to where a
/// character its scripts hold stands and to where it is placed, for it to
/// take the point up by itself: its own post (JAS_020's rock-throw guard,
/// placed asleep at his), not one across the map (Grant's first stop, 50 m
/// from the window the prologue's lure waits for him at), nor one a scene
/// left it by (Grant's spot at that window: taken up again, his next move
/// broke it off once more, and the window's scene, started again, failed
/// the mission).
const HELD_REACH: f32 = 2.5;

/// Whether a character is placed under its scripts' control
/// (`CFCXAIComponent`'s agent's `bStartOnScriptControl`: a mission's own,
/// JAS_020's Grant), its AI going nowhere by itself until let go.
pub fn starts_held(e: &crate::data::Entry) -> bool {
    e.component("CFCXAIComponent")
        .and_then(|c| c.child("AIObject"))
        .and_then(|o| o.child("CPawnAgent"))
        .and_then(|a| a.bool("bStartOnScriptControl"))
        .unwrap_or(false)
}

impl Core {
    /// Whether scripts hold a character (taken with `CAPI_AI.AcquireControl`,
    /// or placed so and not let go): its AI goes nowhere by itself.
    pub fn script_held(&self, pawn: u64) -> bool {
        self.script_control.get(&pawn).copied().unwrap_or_else(|| self.data.entities.get(&pawn).is_some_and(starts_held))
    }

    /// Whether a character scripts hold takes up by itself a point `d` from
    /// it (`HELD_REACH`).
    fn held_takes(&self, pawn: u64, d: f32, point: [f32; 3]) -> bool {
        d <= HELD_REACH && self.data.entities.get(&pawn).is_some_and(|e| (e.pos[0] - point[0]).hypot(e.pos[1] - point[1]) <= HELD_REACH)
    }

    /// Raises a contextual action's event for the scripts listening on it.
    pub fn action_event(&mut self, action: u64, event: &str, user: u64) {
        if event == "CA_Interrupt" {
            self.warn(format!("contextual action {action}: {user} interrupted"));
        }
        if matches!(event, "CA_End" | "CA_Interrupt") {
            let rest = self.data.entities.get(&action).and_then(|e| e.component("CAIContextualActionComponent")?.child("Idle")?.f32("CoolDownTime"));
            self.ca.cooling.push((self.time + rest.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(0.0) as f64, action, user));
        }
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

    /// A switched-on point taken up by the nearest free member of its group
    /// within its radius.
    fn offer_action(&mut self, action: u64) {
        let Some((group, radius, at)) = self.data.entities.get(&action).and_then(|e| Some((offer_of(e)?, e.pos))).map(|((g, r), at)| (g, r, at)) else {
            return;
        };
        let entry = self.data.entities.get(&action).cloned();
        let fits = |c: &Core, p: u64| {
            let state = c.view.spawned.get(&p).map(|l| l.ai_state).unwrap_or(0);
            entry.as_ref().is_none_or(|e| fits_state(e, state, c.battlefields.contains_key(&p)))
        };
        let near = |p: [f32; 3]| (p[0] - at[0]).hypot(p[1] - at[1]);
        // (Only one in the world: a placed character whose layer is off
        // isn't there to take it; nor a companion leading or following the
        // player: HOY_010's Sam, waiting on his way for the player, was taken
        // off it by the next block's spot and never led on.)
        let busy = |p: &u64| matches!(self.companions.get(p), Some(crate::Companion::Lead { .. } | crate::Companion::Follow));
        let taker = self
            .groups(&Text(format!("#{group:08X}")))
            .into_iter()
            .filter(|p| !self.ca.current.contains_key(p) && !busy(p) && self.is_loaded(*p) && fits(self, *p))
            .filter_map(|p| Some((p, near(self.position(p)?))))
            // (One its scripts hold takes up only its own post.)
            .filter(|&(p, d)| if self.script_held(p) { self.held_takes(p, d, at) } else { d <= radius })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((pawn, _)) = taker {
            self.give_action(pawn, action);
        }
    }

    /// The groups scripts watch for combat events hear their members' AI
    /// states change, as the engine tells them (`CGroupCombatEvent`, then
    /// `<State>Start` when the first member enters a state, `<State>MemberIn`
    /// as each does, `<State>Stop` when the last leaves it, with how many are
    /// in it; deaths are told as they happen, `Story::killed`).
    pub fn group_state_events(&mut self) {
        const STATES: [&str; 4] = ["Idle", "Investigate", "Alert", "Combat"];
        let groups: std::collections::BTreeSet<String> = self
            .listeners
            .values()
            .filter_map(|l| match &l.on {
                Listen::Group(g, e) if e == "CGroupCombatEvent" => Some(g.clone()),
                _ => None,
            })
            .collect();
        self.group_states.retain(|g, _| groups.contains(g));
        for group in groups {
            let mut now = [0usize; 4];
            for e in self.groups(&Text(group.clone())) {
                if !self.is_loaded(e) || self.is_dead(e) {
                    continue;
                }
                let state = self.view.spawned.get(&e).map(|l| l.ai_state).unwrap_or(0) as usize;
                if let Some(n) = now.get_mut(state) {
                    *n += 1;
                }
            }
            // (A group first seen sets its counts: nothing changed yet.)
            let Some(before) = self.group_states.insert(group.clone(), now) else { continue };
            // (Logged: who is out of idle, as the counts change.)
            if before != now {
                let members: Vec<(u64, u8)> = self
                    .groups(&Text(group.clone()))
                    .into_iter()
                    .filter(|&e| self.is_loaded(e) && !self.is_dead(e))
                    .filter_map(|e| self.view.spawned.get(&e).map(|l| (e, l.ai_state)).filter(|m| m.1 > 0))
                    .collect();
                self.warn(format!("group {group}: states {before:?} -> {now:?}, out of idle {members:?}"));
            }
            for (i, name) in STATES.iter().enumerate() {
                let (was, is) = (before[i], now[i]);
                let mut tell = |event: String, count: usize| {
                    self.raise(
                        &Listen::Group(group.clone(), "CGroupCombatEvent".into()),
                        vec![Arg::Str("CGroupCombatEvent".into()), Arg::Str(event), Arg::Num(count as f64)],
                    );
                };
                if is > was {
                    if was == 0 {
                        tell(format!("{name}Start"), is);
                    }
                    tell(format!("{name}MemberIn"), is);
                } else if is == 0 && was > 0 {
                    tell(format!("{name}Stop"), 0);
                }
            }
        }
    }

    /// Points whose cooldown has run out since they were ended or broken
    /// off: the scripts listening hear them available again (`CA_Available`:
    /// JAS_020 B10's skinner sent back to his animal each time).
    pub fn cooled_actions(&mut self) {
        let now = self.time;
        let (due, cooling): (Vec<_>, Vec<_>) = std::mem::take(&mut self.ca.cooling).into_iter().partition(|c| c.0 <= now);
        self.ca.cooling = cooling;
        for (_, action, user) in due {
            self.raise(&Listen::Group(action.to_string(), "CA_Available".into()), vec![Arg::Str("CA_Available".into()), Arg::id(user)]);
        }
    }

    /// Offers the switched-on points no one is on (or leaving) round again,
    /// now and then.
    pub fn offer_enabled_actions(&mut self) {
        if self.time < self.ca.next_offer {
            return;
        }
        self.ca.next_offer = self.time + OFFER_EVERY;
        let free: Vec<u64> = self
            .ca
            .enabled
            .iter()
            .copied()
            .filter(|a| !self.ca.current.values().any(|c| c.action == *a) && !self.leaving.iter().any(|l| l.1 == *a) && self.is_loaded(*a))
            .collect();
        for action in free {
            self.offer_action(action);
            if self.ca.current.values().any(|c| c.action == action) || !self.ca.unclaimed.insert(action) {
                continue;
            }
            let Some(((group, radius), at)) = self.data.entities.get(&action).and_then(|e| Some((offer_of(e)?, e.pos))) else { continue };
            let members: Vec<(u64, Option<f32>)> =
                self.groups(&Text(format!("#{group:08X}"))).into_iter().map(|p| (p, self.position(p).map(|q| (q[0] - at[0]).hypot(q[1] - at[1])))).collect();
            self.warn(format!("contextual action {action}: no one of #{group:08X} within {radius} m yet (members and distances {members:?})"));
        }
    }

    /// A character free again takes up the nearest switched-on point within
    /// reach whose group it is in (not the one it has just left).
    pub fn take_up_action(&mut self, pawn: u64, left: u64) {
        let Some(here) = self.position(pawn) else { return };
        let held = self.script_held(pawn);
        let mut best: Option<(u64, f32)> = None;
        for &action in self.ca.enabled.iter().filter(|&&a| a != left) {
            let Some(e) = self.data.entities.get(&action) else { continue };
            let Some((group, radius)) = offer_of(e) else { continue };
            let d = (here[0] - e.pos[0]).hypot(here[1] - e.pos[1]);
            let reaches = if held { self.held_takes(pawn, d, e.pos) } else { d <= radius };
            if reaches && best.is_none_or(|b| d < b.1) && self.groups(&Text(format!("#{group:08X}"))).contains(&pawn) {
                best = Some((action, d));
            }
        }
        if let Some((action, _)) = best {
            self.give_action(pawn, action);
        }
    }

    /// A synchronised scene's lead (`SyncAnimLeadActor_<scene>`) off its
    /// action: whoever is on the scene's other part (`SyncAnimExtraActor_<scene>`,
    /// a captive being beaten) comes off it too, ended or broken off as the
    /// lead was, as the engine's synchronised moves stop for both. The scene
    /// is over: its other part is switched off (until a script switches it on
    /// again), not offered straight back.
    pub fn release_sync_partners(&mut self, lead: u64, interrupted: bool) {
        let Some(name) = self.data.entities.get(&lead).map(|e| e.name.clone()).filter(|n| n.contains("SyncAnimLeadActor")) else { return };
        let other = name.replace("SyncAnimLeadActor", "SyncAnimExtraActor");
        let extras: Vec<u64> = self.data.entities.values().filter(|e| e.name == other).map(|e| e.id).collect();
        for extra in &extras {
            self.ca.enabled.remove(extra);
        }
        let users: Vec<(u64, u64)> = self.ca.current.iter().filter(|(_, c)| extras.contains(&c.action)).map(|(p, c)| (*p, c.action)).collect();
        for (pawn, action) in users {
            self.stop_action(pawn, action, interrupted);
        }
    }

    fn stop_action(&mut self, pawn: u64, action: u64, interrupted: bool) {
        if self.ca.current.get(&pawn).is_some_and(|c| c.action == action) {
            self.ca.current.remove(&pawn);
            self.emit(Command::Action { pawn, action: None });
            if interrupted {
                self.action_event(action, "CA_Interrupt", pawn);
            } else {
                // (Its end comes once the character is up, as the AI ends
                // it: scripts turn to listen right after asking, as JAS_200's
                // visitor's arrival does.)
                self.leaving.push((LEAVE_SECS, action, pawn));
            }
        }
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    // Battlefields (the host moves the group there): kinds 0 (where to
    // fight) and 3 (the combat one) put the group's characters in a fight.
    r.f("SetBattlefields", |c, (group, battlefield, kind): (Text, Text, Num)| {
        let pawns = c.groups(&group);
        if matches!(kind.or(0.0) as u32, 0 | 3) {
            for p in pawns {
                match battlefield.id() {
                    Some(b) => c.battlefields.insert(p, b),
                    None => c.battlefields.remove(&p),
                };
            }
        }
        let kind = kind.or(0.0).to_string();
        c.emit(Command::Native { name: "SetBattlefields".into(), args: vec![group.0, battlefield.0, kind] });
    })?;
    r.f("ClearBattlefields", |c, group: Text| {
        for p in c.groups(&group) {
            c.battlefields.remove(&p);
        }
        c.emit(Command::Native { name: "ClearBattlefields".into(), args: vec![group.0] });
    })?;
    r.f("SetAIFactEntityId", |c, (group, fact, value): (Text, Text, Text)| {
        let pawns = c.groups(&group);
        match (fact.0.as_str(), value.id()) {
            // (Told to one who isn't in the world, nothing happens: the
            // action is left for others.)
            ("DominoCAOverride", Some(action)) => {
                let present: Vec<u64> = pawns.into_iter().filter(|&p| c.is_loaded(p)).collect();
                for p in present {
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
    r.f("ToggleContextualActionState", |c, (action, on): (Text, mlua::Value)| {
        let on = !matches!(on, mlua::Value::Nil | mlua::Value::Boolean(false));
        c.emit(Command::Native { name: "ToggleContextualActionState".into(), args: vec![action.0.clone(), on.to_string()] });
        let Some(action) = action.id() else { return };
        c.warn(format!("contextual action {action} switched {}", if on { "on" } else { "off" }));
        if on {
            c.ca.enabled.insert(action);
            c.offer_action(action);
            return;
        }
        c.ca.enabled.remove(&action);
        let on_it: Vec<u64> = c.ca.current.iter().filter(|(_, cur)| cur.action == action).map(|(&pawn, _)| pawn).collect();
        for pawn in on_it {
            c.ca.current.remove(&pawn);
            c.emit(Command::Action { pawn, action: None });
            c.leaving.push((LEAVE_SECS, action, pawn));
        }
    })?;
    Ok(())
}
