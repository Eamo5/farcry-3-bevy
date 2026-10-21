//! Presentation natives: HUD texts, message and tutorial boxes, Flash
//! movies, timers on screen, sounds, cinematic sequences, music and the
//! environment (time of day, sun, water levels). The host shows and plays
//! them from the commands; what it reports back (a sequence finished, a box
//! answered) comes in through `Story`'s event functions.

use std::collections::{BTreeMap, HashMap};

use mlua::Value;

use super::{callback, vec3, Reg};
use crate::core::{Callback, Core, Listen};
use crate::values::{oasis_id, Arg, Flag, Num, Text};
use crate::Command;

/// A HUD text (`CFCXHudDisplayHelper`): one shows per slot at a time; a
/// later one interrupts it (when it lets itself be), waits for it (when it
/// may queue) or is turned away.
pub struct HudText {
    /// How long it shows (negative: until removed).
    pub seconds: f64,
    pub shown: bool,
    /// Waiting for its slot.
    pub queued: bool,
    /// Counting down while shown.
    pub remaining: Option<f64>,
    pub slot: u32,
    pub text: Option<u32>,
    pub title: Option<u32>,
    pub interruptible: bool,
    pub queueable: bool,
}

/// An on-screen timer (`RegisterNamedTimerLocalized`).
pub struct NamedTimer {
    pub value: f64,
    pub increasing: bool,
    pub shown: bool,
    pub label: String,
    pub append_time: bool,
    /// (callback id, time, fired)
    pub marks: Vec<(u64, f64, bool)>,
}

#[derive(Default)]
pub struct Media {
    pub hud: BTreeMap<u64, HudText>,
    /// Message / tutorial boxes waiting for an answer.
    pub boxes: HashMap<u64, Callback>,
    /// Sounds playing for scripts, with their end callbacks.
    pub sounds: HashMap<u64, Callback>,
    pub timers: BTreeMap<u64, NamedTimer>,
    /// The last sync point each sequence reached (scene, file).
    pub sync_points: HashMap<(String, String), String>,
    /// Water levels scripts have set or moved, by sector id (whatever
    /// world a box names: single-player has the one).
    pub water: HashMap<u32, f64>,
    /// Water levels being filled or discharged, by the id their scripts
    /// were given.
    pub fills: BTreeMap<u64, Fill>,
    pub sun_yaw: f64,
}

/// A sector's water rising or falling to a level (`FillWater`,
/// `DischargeWater`): fills of the same sector add their speeds, as the
/// engine's do.
pub struct Fill {
    pub sector: u32,
    pub to: f64,
    /// Metres a second.
    pub speed: f64,
    pub callback: Option<Callback>,
}

impl Core {
    /// A sector's water level: as a script last set or moved it, else as
    /// the world has it.
    pub fn water_level(&self, sector: u32) -> f64 {
        self.media.water.get(&sector).copied().or_else(|| self.data.water.get(&sector).copied()).unwrap_or(0.0)
    }

    /// Moves the water being filled or discharged, and calls back each fill
    /// that has reached its level.
    fn update_water(&mut self, dt: f64) {
        let mut done = Vec::new();
        let ids: Vec<u64> = self.media.fills.keys().copied().collect();
        for id in ids {
            let Some(f) = self.media.fills.get(&id) else { continue };
            let (sector, to, step) = (f.sector, f.to, f.speed.abs() * dt);
            let now = self.water_level(sector);
            let next = if now < to { (now + step).min(to) } else { (now - step).max(to) };
            self.media.water.insert(sector, next);
            if (next - to).abs() < 1e-6 {
                done.push(id);
            }
        }
        for id in done {
            if let Some(cb) = self.media.fills.remove(&id).and_then(|f| f.callback) {
                self.call(cb, vec![]);
            }
        }
    }
}

/// A string's first printf placeholder (`%ls`, `%s`, `%d`) filled in.
fn fill(format: &str, value: &str) -> String {
    for p in ["%ls", "%s", "%d", "%i"] {
        if let Some(at) = format.find(p) {
            return format!("{}{value}{}", &format[..at], &format[at + p.len()..]);
        }
    }
    format.to_string()
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn checklist_values_fill_the_localized_counter_not_its_goal() {
        assert_eq!(fill("Green leaves: %d/2", "1"), "Green leaves: 1/2");
        assert_eq!(fill("Boar hides: %i/2", "2"), "Boar hides: 2/2");
        assert_eq!(fill("Collected %ls", "3"), "Collected 3");
        assert_eq!(fill("Fixed objective", "4"), "Fixed objective");
    }
}

/// The name a sequence's events carry (`<name>Started`, ...).
pub fn sequence_name(scene: &str, file: &str) -> String {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let base = base.split('.').next().unwrap_or(base);
    format!("{scene}_{base}_")
}

impl Core {
    /// Puts a HUD text on screen.
    fn show_hud(&mut self, id: u64) {
        let Some(t) = self.media.hud.get_mut(&id) else { return };
        t.shown = true;
        t.queued = false;
        t.remaining = (t.seconds > 0.0).then_some(t.seconds);
        let (slot, text, title) = (t.slot, t.text, t.title);
        let text = text.and_then(|t| self.data.strings.get(t)).map(str::to_string).unwrap_or_default();
        let title = title.and_then(|t| self.data.strings.get(t)).map(str::to_string).unwrap_or_default();
        self.emit(Command::Native { name: "HudText".into(), args: vec![id.to_string(), slot.to_string(), title, text] });
    }

    /// Takes a HUD text away (it ended, was hidden or deleted), raising
    /// `event` for it, and shows the next one waiting for its slot.
    fn hud_gone(&mut self, id: u64, event: Option<&str>) {
        let Some(t) = self.media.hud.remove(&id) else { return };
        if t.shown {
            self.emit(Command::Native { name: "HudTextHidden".into(), args: vec![id.to_string()] });
        }
        if let Some(e) = event {
            self.raise(&Listen::HudText(id, e.into()), vec![]);
        }
        if t.shown && !self.media.hud.values().any(|o| o.shown && o.slot == t.slot) {
            if let Some(next) = self.media.hud.iter().find(|(_, o)| o.queued && o.slot == t.slot).map(|(i, _)| *i) {
                self.show_hud(next);
                self.raise(&Listen::HudText(next, "Event_Displayed".into()), vec![]);
            }
        }
    }

    /// Counts down HUD texts and on-screen timers.
    pub fn update_media(&mut self, dt: f64) {
        let mut done = Vec::new();
        for (id, t) in self.media.hud.iter_mut() {
            if let Some(r) = &mut t.remaining {
                *r -= dt;
                if *r <= 0.0 {
                    done.push(*id);
                }
            }
        }
        for id in done {
            self.hud_gone(id, Some("Event_Completed"));
        }
        let mut fire = Vec::new();
        for t in self.media.timers.values_mut() {
            t.value += if t.increasing { dt } else { -dt };
            if !t.increasing && t.value < 0.0 {
                t.value = 0.0;
            }
            for (cb, at, fired) in &mut t.marks {
                let reached = if t.increasing { t.value >= *at } else { t.value <= *at };
                if reached && !*fired {
                    *fired = true;
                    fire.push((*cb, t.value));
                }
            }
        }
        for (cb, value) in fire {
            self.raise(&Listen::Generic(format!("NamedTimerCallback_{cb}")), vec![Arg::Num(value)]);
        }
        self.update_water(dt);
    }
}

pub(super) fn register(r: &Reg) -> mlua::Result<()> {
    r.f("SetDynamicCheckListObjective", |c, (shown, id, value): (Flag, Num, Num)| {
        let id = id.or(0.0) as u32;
        let format = c.data.strings.get(id).unwrap_or_default();
        let text = fill(format, &(value.or(0.0) as i32).to_string());
        c.emit(Command::Native {
            name: "ChecklistObjective".into(),
            args: vec![shown.0.to_string(), id.to_string(), text],
        });
    })?;
    // ---- HUD texts (`DisplayText` answers 0 shown, 1 turned away, 2
    // waiting for the slot)
    r.lua(
        "CFCXHudDisplayHelper:CreateText",
        |_, c, (seconds, _priority, slot, text, title, interruptible, queueable): (Num, Num, Num, Value, Value, Flag, Flag)| {
            let id = c.next_id();
            let text = oasis_id(&text);
            let title = oasis_id(&title).filter(|&t| t != u32::MAX);
            c.media.hud.insert(
                id,
                HudText {
                    seconds: seconds.or(12.0),
                    shown: false,
                    queued: false,
                    remaining: None,
                    slot: slot.or(0.0) as u32,
                    text,
                    title,
                    interruptible: interruptible.0,
                    queueable: queueable.0,
                },
            );
            Ok(id.to_string())
        },
    )?;
    r.f("CFCXHudDisplayHelper:DisplayText", |c, id: Text| {
        let Some(i) = id.id() else { return 1.0 };
        let Some(t) = c.media.hud.get(&i) else { return 1.0 };
        if t.shown {
            return 0.0;
        }
        let (slot, queueable) = (t.slot, t.queueable);
        if let Some((&other, o)) = c.media.hud.iter().find(|(k, o)| **k != i && o.shown && o.slot == slot) {
            if o.interruptible {
                c.hud_gone(other, Some("Event_Interrupted"));
            } else if queueable {
                if let Some(t) = c.media.hud.get_mut(&i) {
                    t.queued = true;
                }
                return 2.0;
            } else {
                return 1.0;
            }
        }
        c.show_hud(i);
        0.0
    })?;
    r.f("CFCXHudDisplayHelper:RegisterDominoEvent", |c, (id, target, method, event): (Text, Value, Option<String>, Text)| {
        let cb = callback(target, method)?;
        let hud = id.id().unwrap_or(0);
        Some(c.listen(Listen::HudText(hud, event.0), cb).to_string())
    })?;
    r.f("CFCXHudDisplayHelper:RemoveDominoEvent", |c, (_id, cb, _event): (Text, Text, Text)| {
        if let Some(cb) = cb.id() {
            c.unlisten(cb)
        }
    })?;
    r.f("CFCXHudDisplayHelper:DeleteText", |c, id: Text| {
        if let Some(i) = id.id() {
            c.hud_gone(i, None);
        }
    })?;
    r.f("CFCXHudDisplayHelper:HideDisplayedText", |c, slot: Num| {
        let slot = slot.or(0.0) as u32;
        let shown: Vec<u64> = c.media.hud.iter().filter(|(_, t)| t.slot == slot && t.shown).map(|(i, _)| *i).collect();
        for i in shown {
            c.hud_gone(i, Some("Event_Interrupted"));
        }
    })?;
    r.f("CFCXHudDisplayHelper:ClearSlotQueue", |c, slot: Num| {
        let slot = slot.or(0.0) as u32;
        c.media.hud.retain(|_, t| t.slot != slot || !t.queued);
    })?;
    r.f("CFCXHudDisplayHelper:ClearAll", |c, ()| {
        let all: Vec<(u64, bool)> = c.media.hud.iter().map(|(i, t)| (*i, t.shown)).collect();
        c.media.hud.clear();
        for (i, shown) in all {
            if shown {
                c.emit(Command::Native { name: "HudTextHidden".into(), args: vec![i.to_string()] });
            }
        }
    })?;

    // ---- message and tutorial boxes (answered by the host)
    // (package, title, message, message params 1-3, footer, param 1-2,
    // target, method, param 1-2 formats, accept/cancel): the footer is
    // followed by each param in its format ("$ %ls").
    r.f("ShowDialogBox", |c, args: mlua::Variadic<Value>| {
        let a: Vec<Value> = args.into_iter().collect();
        let text = |i: usize| a.get(i).and_then(oasis_id).filter(|&id| id != u32::MAX).and_then(|id| c.data.strings.get(id)).map(str::to_string);
        let plain = |i: usize| match a.get(i) {
            Some(Value::Integer(n)) => n.to_string(),
            Some(Value::Number(n)) if n.fract() == 0.0 => format!("{n:.0}"),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) => s.to_string_lossy(),
            _ => String::new(),
        };
        let title = text(1).unwrap_or_default();
        let mut message = text(2).unwrap_or_default();
        for i in 3..=5 {
            if let Some(param) = text(i) {
                message = fill(&message, &param);
            }
        }
        let mut footer = text(6).unwrap_or_default();
        for (value, format) in [(plain(7), 11), (plain(8), 12)] {
            if value.is_empty() {
                continue;
            }
            let shown = text(format).map(|f| fill(&f, &value)).unwrap_or(value);
            footer = if footer.is_empty() { shown } else { format!("{footer}  {shown}") };
        }
        let body = if footer.is_empty() { message } else { format!("{message}\n\n{footer}") };
        let choice = !matches!(a.get(13), Some(Value::Boolean(false)));
        let cb = a.iter().position(|v| matches!(v, Value::Table(_))).and_then(|i| {
            let method = a.get(i + 1).and_then(|m| m.as_string().map(|s| s.to_string_lossy()));
            callback(a[i].clone(), method)
        });
        let id = c.next_id();
        if let Some(cb) = cb {
            c.media.boxes.insert(id, cb);
        }
        c.emit(Command::Native { name: "ShowDialogBox".into(), args: vec![id.to_string(), title, body, choice.to_string()] });
        id.to_string()
    })?;
    r.f("ShowTutorialBox", |c, (_package, title, message, target, method): (Text, Value, Value, Value, Option<String>)| {
        let id = c.next_id();
        if let Some(cb) = callback(target, method) {
            c.media.boxes.insert(id, cb);
        }
        let t = oasis_id(&title).and_then(|i| c.data.strings.get(i)).unwrap_or_default().to_string();
        let m = oasis_id(&message).and_then(|i| c.data.strings.get(i)).unwrap_or_default().to_string();
        c.emit(Command::Native { name: "ShowTutorialBox".into(), args: vec![id.to_string(), t, m] });
        id.to_string()
    })?;
    // The newer tutorial box queues localized text without an answer callback.
    r.f("PushTutorialBox", |c, (_package, title, message, prompt, priority): (Text, Value, Value, Value, Flag)| {
        let text = |value: &Value| oasis_id(value).and_then(|i| c.data.strings.get(i)).unwrap_or_default().to_string();
        let (title, body, prompt) = (text(&title), text(&message), text(&prompt));
        let id = c.next_id();
        c.emit(Command::Native {
            name: "ShowTutorialBox".into(),
            args: vec![id.to_string(), title, body, "false".into(), prompt, priority.0.to_string()],
        });
    })?;
    for name in ["UnregisterDialogMsgBoxCb", "UnregisterTutorialCb"] {
        r.f(name, |c, id: Text| {
            if let Some(i) = id.id() {
                c.media.boxes.remove(&i);
            }
        })?;
    }
    r.f("UICreatePlayer", |c, (name, _player): (Text, Text)| {
        let id = c.next_id();
        c.emit(Command::Native { name: "UICreatePlayer".into(), args: vec![id.to_string(), name.0.clone()] });
        // The movie is ready at once.
        c.raise(&Listen::Entity(crate::PLAYER, format!("{}_Loaded", name.0)), vec![Arg::id(crate::PLAYER)]);
        id as f64
    })?;
    r.constant("GetFlashOverrideLoadingScreen", "")?;

    // ---- on-screen timers
    r.f("RegisterNamedTimerLocalized", |c, (name, initial, increasing, show, section, item, append): (Text, Num, Flag, Flag, Text, Text, Flag)| {
        let id = c.next_id();
        let label = c.data.strings.named(&section.0, &item.0).unwrap_or(&name.0).to_string();
        c.media.timers.insert(id, NamedTimer {
            value: initial.or(0.0), increasing: increasing.0, shown: show.0,
            label, append_time: append.0, marks: Vec::new(),
        });
        c.emit(Command::Native { name: "ShowTimer".into(), args: vec![id.to_string(), name.0, show.0.to_string(), item.0] });
        id as f64
    })?;
    r.f("RegisterNamedTimerCallback", |c, (timer, at, _mode): (Num, Num, Num)| {
        let cb = c.next_id();
        if let Some(t) = timer.0.and_then(|t| c.media.timers.get_mut(&(t as u64))) {
            t.marks.push((cb, at.or(0.0), false));
        }
        cb as f64
    })?;
    r.f("UnregisterNamedTimer", |c, timer: Num| {
        if let Some(t) = timer.0 {
            c.media.timers.remove(&(t as u64));
            c.emit(Command::Native { name: "HideTimer".into(), args: vec![(t as u64).to_string()] });
        }
    })?;

    // ---- sounds
    r.f("CDominoSoundManager:PlaySound", |c, (entity, sound, kind, target, method): (Text, Text, Num, Value, Option<String>)| {
        let id = c.next_id();
        // (The box hears "started" now, and "finished" when the host says.)
        if let Some(cb) = callback(target, method) {
            c.call(cb.clone(), vec![Arg::Str("started".into())]);
            c.media.sounds.insert(id, cb);
        }
        c.emit(Command::Native { name: "PlaySound".into(), args: vec![id.to_string(), entity.0, sound.0, kind.or(0.0).to_string()] });
        id.to_string()
    })?;
    r.f("CDominoSoundManager:StopSound", |c, (id, fade): (Text, Num)| {
        if let Some(i) = id.id() {
            c.emit(Command::Native { name: "StopSound".into(), args: vec![i.to_string(), fade.or(0.5).to_string()] });
        }
    })?;
    r.f("CFCXMusicService:PlayMusicStateExternal", |c, (state, _a, _b): (Text, Num, Num)| {
        c.emit(Command::Native { name: "MusicState".into(), args: vec![state.0, "play".into()] });
    })?;
    r.f("CFCXMusicService:StopMusicStateExternal", |c, (state, _a): (Text, Num)| {
        c.emit(Command::Native { name: "MusicState".into(), args: vec![state.0, "stop".into()] });
    })?;
    r.lua("GetBestTalkerInGroup", |_, c, group: Text| {
        let id = match group.id() {
            Some(i) => Some(i),
            None => c.groups(&group).into_iter().find(|e| c.is_loaded(*e) && !c.is_dead(*e)),
        };
        Ok(id.map(|i| i.to_string()).unwrap_or_else(|| fc3_domino::INVALID_ID.to_string()))
    })?;

    // ---- cinematic sequences (played by the host)
    r.f("CSequenceManager:StartCinematicMetaSequence", |c, ()| c.next_id() as f64)?;
    r.f("CSequenceManager:GetCinematicSequenceDominoName", |_, (scene, file): (Text, Text)| sequence_name(&scene.0, &file.0))?;
    r.f("CSequenceManager:GetCinematicSequenceSyncPointName", |c, (scene, file): (Text, Text)| {
        c.media.sync_points.get(&(scene.0, file.0.to_ascii_lowercase())).cloned().unwrap_or_default()
    })?;
    for (name, mode) in [
        ("StartCinematicSequenceWithMetaId", "play"),
        ("StartCinematicSequenceFromBeginningWithMetaId", "play_from_start"),
        ("StartCinematicSequenceAtSyncPointWithMetaId", "play_at_sync_point"),
    ] {
        // The sync point by index or by name.
        r.f(&format!("CSequenceManager:{name}"), move |c, (meta, scene, file, sync): (Num, Text, Text, Text)| {
            c.emit(Command::Native { name: "Sequence".into(), args: vec![mode.into(), scene.0, file.0, meta.or(0.0).to_string(), sync.0] });
        })?;
    }

    // ---- videos (Bink): the host plays them and reports their start and
    // end (`Story::video_event`).
    r.f("PlayBinkVideo", |c, (path, screen, material, volume, looped, _music): (Text, Text, Text, Num, Flag, Flag)| {
        c.emit(Command::Native { name: "PlayBinkVideo".into(), args: vec![path.0, screen.0, material.0, volume.or(1.0).to_string(), looped.0.to_string()] });
    })?;
    r.f("StopBinkVideo", |c, screen: Text| c.emit(Command::Native { name: "StopBinkVideo".into(), args: vec![screen.0] }))?;
    r.f("PreloadBinkVideo", |_, _path: Text| ())?;
    r.f("PlayFullscreenVideo", |c, path: Text| c.emit(Command::Native { name: "PlayFullscreenVideo".into(), args: vec![path.0] }))?;
    r.f("StopFullscreenVideo", |c, ()| {
        c.emit(Command::Native { name: "StopFullscreenVideo".into(), args: vec![] });
        c.raise(&Listen::Generic("StopVideo".into()), vec![]);
    })?;
    for (name, mode) in [("StopCinematicSequence", "stop"), ("SkipCinematicSequence", "skip"), ("PauseCinematicSequence", "pause")] {
        r.f(&format!("CSequenceManager:{name}"), move |c, (scene, file): (Text, Text)| {
            c.emit(Command::Native { name: "Sequence".into(), args: vec![mode.into(), scene.0, file.0] });
        })?;
    }

    // ---- environment
    r.lua("GetTimeOfDay", |lua, c, ()| {
        let h = c.view.time_of_day.rem_euclid(24.0);
        let (hh, mm) = (h.floor(), ((h - h.floor()) * 60.0).floor());
        let ss = (((h - hh) * 60.0 - mm) * 60.0).floor();
        vec3(lua, [hh, mm, ss])
    })?;
    r.f("GetSunYaw", |c, ()| c.media.sun_yaw)?;
    r.f("SetSunYaw", |c, yaw: Num| {
        c.media.sun_yaw = yaw.or(0.0);
        c.emit(Command::Native { name: "SetSunYaw".into(), args: vec![yaw.or(0.0).to_string()] });
    })?;
    // Water levels: the story moves them (the host shows what
    // `Story::water_levels` gives).
    r.f("CDominoWaterLevelManager:GetWaterLevel", |c, (_world, sector): (Text, Num)| c.water_level(sector.or(0.0) as u32))?;
    for name in ["FillWater", "DischargeWater"] {
        r.f(
            &format!("CDominoWaterLevelManager:{name}"),
            move |c, (_world, sector, speed, level, target, method): (Text, Num, Num, Num, Value, Option<String>)| {
                let id = c.next_id();
                let fill = Fill { sector: sector.or(0.0) as u32, to: level.or(0.0), speed: speed.or(1.0), callback: callback(target, method) };
                c.media.fills.insert(id, fill);
                id.to_string()
            },
        )?;
    }
    r.f("CDominoWaterLevelManager:StopChangingWaterLevel", |c, id: Text| {
        if let Ok(id) = id.0.parse::<u64>() {
            c.media.fills.remove(&id);
        }
    })?;
    r.f("CDominoWaterLevelManager:SetWaterLevel", |c, (_world, sector, level): (Text, Num, Num)| {
        c.media.water.insert(sector.or(0.0) as u32, level.or(0.0));
    })?;
    Ok(())
}
