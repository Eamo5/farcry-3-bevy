//! Scripted dialogue (`CAPI_NPC.StartDialog`) and the engine's answers to
//! script control.
//!
//! A line is a sound id; its length comes from the game's speech length
//! table, so the story times lines itself: one line at a time per speaker,
//! queued or superseding by priority as asked, with the dialog events the
//! boxes wait for (`Domino_DialogStarted`, `..Finished`, `..Queued`,
//! `..Interrupted`, `..FailedPriority`, `..Stopped`, each with the speaker
//! and the sound id as the script gave it). The host plays the voice and
//! shows the subtitle from the commands.

use std::collections::{HashMap, VecDeque};

use super::Reg;
use crate::core::{Core, Listen};
use crate::values::{Arg, Flag, Text};
use crate::Command;

/// The priority ladder, low to high.
fn rank(priority: &str) -> u8 {
    match priority {
        "Idle" => 0,
        "Interested" => 1,
        "FuzzyThreat" => 2,
        "Combat" => 3,
        "SafetyReflex" => 4,
        "ForcePlay" => 5,
        _ => 0,
    }
}

#[derive(Debug, Clone)]
pub struct Line {
    pub speaker: u64,
    /// The sound id as the script wrote it (events carry it back).
    pub sound: String,
    pub target: Option<u64>,
    pub priority: u8,
    pub remaining: f64,
}

#[derive(Default)]
pub struct Dialogs {
    /// The line each speaker is saying.
    pub speaking: HashMap<u64, Line>,
    pub queued: HashMap<u64, VecDeque<Line>>,
}

/// A line's length: the speech table's, or a reading time for lines it
/// lacks.
fn seconds(c: &Core, sound: &str) -> f64 {
    let id = Text(sound.to_string()).sound();
    match id.and_then(|i| c.data.speech.get(&i)) {
        Some(&s) if s > 0.0 => s as f64,
        _ => 2.5,
    }
}

impl Core {
    fn dialog_event(&mut self, speaker: u64, event: &str, sound: &str) {
        if std::env::var_os("FC3_STORY_DIALOG_DEBUG").is_some() {
            eprintln!("story dialog: speaker {speaker} {event} {sound}");
        }
        self.raise(&Listen::Entity(speaker, event.to_string()), vec![Arg::id(speaker), Arg::Str(sound.to_string())]);
    }

    fn start_line(&mut self, line: Line) {
        let id = Text(line.sound.clone()).sound().unwrap_or(0);
        let subtitle = self.data.strings.subtitle(id).map(str::to_string);
        self.emit(Command::Dialog { speaker: line.speaker, sound: id, target: line.target, subtitle, seconds: line.remaining as f32 });
        self.dialog_event(line.speaker, "Domino_DialogStarted", &line.sound);
        self.dialogs.speaking.insert(line.speaker, line);
    }

    /// `StartDialog(speaker, sound, target, priority, supersede, queue)`.
    pub fn start_dialog(&mut self, speaker: u64, sound: &str, target: Option<u64>, priority: &str, supersede: bool, queue: bool) {
        let line = Line { speaker, sound: sound.to_string(), target, priority: rank(priority), remaining: seconds(self, sound) };
        match self.dialogs.speaking.get(&speaker).cloned() {
            None => self.start_line(line),
            Some(current) => {
                // FC3.dll 0x114053d7..0x114054b5: without queueing only
                // strictly lower priorities fail. With queueing, defer a
                // lower priority, or an equal one when Supersede is set.
                // Equal unqueued ForcePlay lines interrupt; rejecting them
                // strands DOC_020's door-opening dialogue chain.
                if queue && (line.priority < current.priority || (line.priority == current.priority && supersede)) {
                    self.dialog_event(speaker, "Domino_DialogQueued", sound);
                    self.dialogs.queued.entry(speaker).or_default().push_back(line);
                } else if line.priority >= current.priority {
                    self.dialogs.speaking.remove(&speaker);
                    self.emit(Command::StopDialog { speaker });
                    self.dialog_event(speaker, "Domino_DialogInterrupted", &current.sound);
                    self.start_line(line);
                } else {
                    self.dialog_event(speaker, "Domino_DialogFailedPriority", sound);
                }
            }
        }
    }

    /// Advances the lines being said; a finished one lets the next queued
    /// line start.
    pub fn update_dialogs(&mut self, dt: f64) {
        let mut done = Vec::new();
        for (speaker, line) in self.dialogs.speaking.iter_mut() {
            line.remaining -= dt;
            if line.remaining <= 0.0 {
                done.push(*speaker);
            }
        }
        for speaker in done {
            if let Some(line) = self.dialogs.speaking.remove(&speaker) {
                self.dialog_event(speaker, "Domino_DialogFinished", &line.sound);
            }
            if let Some(next) = self.dialogs.queued.get_mut(&speaker).and_then(VecDeque::pop_front) {
                self.start_line(next);
            }
        }
        // Script control answers, a frame after they were asked.
        for (pawn, event) in std::mem::take(&mut self.control_answers) {
            self.raise(&Listen::Entity(pawn, "Domino_ScriptControl".into()), vec![Arg::id(pawn), Arg::Str(event)]);
        }
    }

    pub fn stop_dialogs(&mut self, speaker: u64) {
        self.dialogs.queued.remove(&speaker);
        if let Some(line) = self.dialogs.speaking.remove(&speaker) {
            self.emit(Command::StopDialog { speaker });
            self.dialog_event(speaker, "Domino_DialogStopped", &line.sound);
        }
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    r.f("CAPI_NPC.StartDialog", |c, (speaker, sound, target, priority, supersede, queue): (Text, Text, Text, Text, Flag, Flag)| {
        let Some(s) = speaker.id() else { return };
        c.start_dialog(s, &sound.0, target.id(), &priority.0, supersede.0, queue.0);
    })?;
    r.f("StopAllDialog", |c, group: Text| {
        for e in c.groups(&group) {
            c.stop_dialogs(e);
        }
    })?;
    r.f("CancelQueuedDialog", |c, (group, sound): (Text, Text)| {
        for e in c.groups(&group) {
            if let Some(q) = c.dialogs.queued.get_mut(&e) {
                q.retain(|l| !sound.0.is_empty() && l.sound != sound.0);
            }
        }
    })?;
    // The engine hands a pawn to the script (or back to its AI) and says so.
    r.f("CAPI_AI.AcquireControl", |c, (pawn, acquire): (Text, Flag)| {
        let Some(p) = pawn.id() else { return false };
        c.emit(Command::Native { name: "CAPI_AI.AcquireControl".into(), args: vec![p.to_string(), acquire.0.to_string()] });
        c.control_answers.push((p, if acquire.0 { "Controlled".into() } else { "Released".into() }));
        true
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use crate::data::StoryData;

    #[test]
    fn installed_priority_branch_matrix() {
        for (priority, relation) in [("Idle", -1), ("Combat", 0), ("ForcePlay", 1)] {
            for supersede in [false, true] {
                for queue in [false, true] {
                    let mut c = Core::new(Rc::new(StoryData::default()));
                    c.start_dialog(1, "0x00154be3", None, "Combat", false, false);
                    c.out.clear();
                    c.start_dialog(1, "0x00164caf", None, priority, supersede, queue);
                    let deferred = queue && (relation < 0 || (relation == 0 && supersede));
                    let replaced = !deferred && relation >= 0;
                    assert_eq!(c.dialogs.speaking[&1].sound, if replaced { "0x00164caf" } else { "0x00154be3" });
                    assert_eq!(c.dialogs.queued.get(&1).map_or(0, VecDeque::len), usize::from(deferred));
                    assert_eq!(c.out.iter().filter(|cmd| matches!(cmd, Command::StopDialog { speaker: 1 })).count(), usize::from(replaced));
                    c.update_dialogs(3.0);
                    if deferred {
                        assert_eq!(c.dialogs.speaking[&1].sound, "0x00164caf");
                    } else {
                        assert!(!c.dialogs.speaking.contains_key(&1));
                    }
                }
            }
        }
    }

    #[test]
    fn greenhouse_forceplay_replaces_equal_priority_line() {
        let mut c = Core::new(Rc::new(StoryData::default()));
        c.start_dialog(1, "0x00154be3", None, "ForcePlay", false, false);
        c.out.clear();
        c.start_dialog(1, "0x00164caf", None, "ForcePlay", false, false);
        assert_eq!(c.dialogs.speaking[&1].sound, "0x00164caf");
        assert!(matches!(c.out.as_slice(), [Command::StopDialog { speaker: 1 }, Command::Dialog { sound: 0x00164caf, .. }]));
    }
}
