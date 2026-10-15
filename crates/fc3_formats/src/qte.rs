//! Quick-time events as the game authors them: the `QTEGSF` gameplay plays
//! an exotic move (`Ex_FirstStruggleQTE_Jason`, `ex_finalescape_jason`) from
//! the named move database, and the move's own data runs it.
//!
//! A QTE move is a tree of states. Each state names the QTE results it
//! plays under (`QTEn_Result`, n the QTE's id plus one, equal to one of the
//! values it lists: 3 none, 0 in progress, 1 failed, 2 succeeded); a group
//! state's children play only under its own conditions too, and a group
//! with no child to play plays its own clip. The first state in order whose
//! conditions hold plays ([`select`]): the escape's crawl, for one, is
//! `Dead` (QTE 2 failed), `Exit` (QTE 2 succeeded), `Crawl` (QTE 2 in
//! progress: `CrawlRight` when QTE 0 succeeded, `CrawlLeft` when QTE 1 did,
//! else `Idle`), then `Intro`.
//!
//! The QTEs themselves are events in the states' clips (`QuickTimeEvent*`,
//! FCB objects among a clip's events, [`clip_events`]): a press, a hold or
//! a mash starts a QTE on an input and the player's answer decides it; a
//! "zombie" QTE is in progress until a script decides it (`QteSetResult`);
//! others end, reset or set a QTE (its progress and result), end the whole
//! sequence, or message the scripts. The converter keeps the moves and
//! their clips' events in the cache ([`QTE_REL`], [`QteTable`]).
//!
//! Each QTE also has a progress, 0 to 1 (a mash's intensity, a hold's time
//! held, or what an event sets: `QuickTimeEventSetPMSValues`), which the
//! moves read as `Interactive_Float<n>` (n the QTE's id plus one). A state
//! may play only while one is within a range (`ranges`): JAS_160's mass
//! grave climbs `Reach1` to `Reach5` as each reach's clip sets QTE 2's
//! progress (0.25, 0.35...); a push's pose follows its mash.
//!
//! In the move database (`fc3_formats::moves::MOVE_DB`), values are tagged
//! (`VALUE`, then a type); a move opens with `TSvM`, a state with `rGvM`
//! (after a tagged null string when it is its list's first, a tagged `1`
//! when it follows a sibling), a condition with `EECM` (a result) or `FIcM`
//! (a progress range: the range, then the variable's name), a clip with
//! `aPnA` (or `pMCT`: a clip posed from a time, the time first), and an
//! anchor (a state played from the scene's anchor, not from where the
//! player is) with `cnAA`. A tagged `1` then three `-2` references
//! closes a list of states; a clip after a sibling's tagged `1`, where a
//! state would be, is the group's own and closes its list too.

use std::collections::BTreeMap;

/// The QTE moves and their clips' events, in the cache.
pub const QTE_REL: &str = "story/qte_moves.txt";

/// The cached table's first line: which version wrote it.
const QTE_MAGIC: &str = "# fc3-qte-moves 2";

/// What tags a value in the move database.
const VALUE: [u8; 4] = [0xb1, 0x68, 0xde, 0x3a];

/// A QTE's result, as the moves test them and the scripts hear them.
pub const NONE: u8 = 3;
pub const IN_PROGRESS: u8 = 0;
pub const FAILED: u8 = 1;
pub const SUCCEEDED: u8 = 2;

/// A state of a QTE move.
#[derive(Clone, Debug, PartialEq)]
pub struct QteState {
    /// Nesting: 1 for the move's own states, 2 for a group's, ...
    pub depth: u8,
    pub name: String,
    /// The results it plays under: (QTE id, result). For each QTE it names,
    /// the result must be one of the values listed for it.
    pub conds: Vec<(u8, u8)>,
    /// Its clip (archive path), if it has one.
    pub clip: Option<String>,
    /// Where in the clip it plays from (a posed clip's time).
    pub start: f32,
    /// It plays from the scene's anchor (else from where the player is).
    pub anchored: bool,
    /// The progress ranges it plays within: (QTE id, low, high).
    pub ranges: Vec<(u8, f32, f32)>,
}

impl QteState {
    /// Its own conditions hold for these results.
    pub fn holds(&self, results: &[u8]) -> bool {
        let result = |id: u8| results.get(id as usize).copied().unwrap_or(NONE);
        self.conds.iter().all(|&(id, _)| self.conds.iter().any(|&(i, v)| i == id && v == result(id)))
    }

    /// The QTEs' progress is within its ranges.
    pub fn in_range(&self, progress: &[f32]) -> bool {
        self.ranges.iter().all(|&(id, lo, hi)| (lo..=hi).contains(&progress.get(id as usize).copied().unwrap_or(0.0)))
    }
}

/// The state that plays for these results and progress: the first, in
/// order, whose conditions hold (a group's under its own; a group with no
/// child to play plays its own clip, if it has one).
pub fn select(states: &[QteState], results: &[u8], progress: &[f32]) -> Option<usize> {
    fn pick(states: &[QteState], results: &[u8], progress: &[f32], start: usize, depth: u8) -> Option<usize> {
        let mut i = start;
        while i < states.len() && states[i].depth >= depth {
            let s = &states[i];
            if s.depth == depth && s.holds(results) && s.in_range(progress) {
                let group = states.get(i + 1).is_some_and(|c| c.depth > depth);
                if let Some(found) = group.then(|| pick(states, results, progress, i + 1, depth + 1)).flatten() {
                    return Some(found);
                }
                if s.clip.is_some() {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }
    pick(states, results, progress, 0, states.first()?.depth)
}

/// Whether a state's result conditions, and every group's it is in, still
/// hold (its progress ranges are for choosing it: a reach's clip moves the
/// climb's progress on as it plays).
pub fn holds_within(states: &[QteState], index: usize, results: &[u8]) -> bool {
    let Some(s) = states.get(index) else { return false };
    let mut depth = s.depth;
    if !s.holds(results) {
        return false;
    }
    for parent in states[..index].iter().rev() {
        if parent.depth < depth {
            if !parent.holds(results) {
                return false;
            }
            depth = parent.depth;
        }
    }
    true
}

/// Every QTE move in the move database (a move with a state that tests a
/// QTE's result), with its states in order.
pub fn parse_moves(db: &[u8]) -> BTreeMap<String, Vec<QteState>> {
    let mut starts: Vec<usize> = Vec::new();
    let mut i = 0;
    while let Some(at) = find(db, i, b"TSvM").filter(|&at| db.get(at + 4..at + 8) == Some(&VALUE[..])) {
        starts.push(at);
        i = at + 4;
    }
    starts.push(db.len());
    let mut out = BTreeMap::new();
    for w in starts.windows(2) {
        let seg = &db[w[0]..w[1]];
        let Some(name) = string_at(seg, 28) else { continue };
        let states = parse_states(seg);
        if states.iter().any(|s| !s.conds.is_empty()) {
            out.insert(name.to_ascii_lowercase(), states);
        }
    }
    out
}

fn find(d: &[u8], from: usize, what: &[u8]) -> Option<usize> {
    d.get(from..)?.windows(what.len()).position(|w| w == what).map(|p| from + p)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

/// A length-prefixed string at `at` (its length, then its bytes).
fn string_at(d: &[u8], at: usize) -> Option<String> {
    let n = u32_at(d, at)? as usize;
    let b = d.get(at + 4..at + 4 + n).filter(|b| n > 0 && n < 1024 && b.iter().all(|c| (0x20..0x7f).contains(c)))?;
    Some(String::from_utf8_lossy(b).into_owned())
}

/// A move's states (see the module's notes on the layout).
fn parse_states(seg: &[u8]) -> Vec<QteState> {
    let tagged = |at: usize, ty: u32| seg.get(at..at + 4) == Some(&VALUE[..]) && u32_at(seg, at + 4) == Some(ty);
    let nulls = |at: usize| (0..3).all(|k| seg.get(at + 4 * k..at + 4 * k + 4) == Some(&[0xfe, 0xff, 0xff, 0xff][..]));
    let mut states: Vec<QteState> = Vec::new();
    let (mut depth, mut owner, mut in_transition) = (0u8, None::<usize>, false);
    // Transitions name the state they go into and its clip
    // (`move/Crawl/Idle/<clip>`): (state, clip name) while one is read, and
    // the clips found for each state.
    let mut transition: Option<(String, String)> = None;
    let mut into: BTreeMap<String, String> = BTreeMap::new();
    let mut i = 0;
    while i + 16 <= seg.len() {
        // A state: the first of a list, or one after a sibling.
        let first = tagged(i, 3) && seg[i + 8..i + 12] == [0xff; 4] && &seg[i + 12..i + 16] == b"rGvM";
        let after = tagged(i, 1) && seg.get(i + 8) == Some(&1) && seg.get(i + 9..i + 13) == Some(&[0xff; 4][..]);
        let next_state = after && seg.get(i + 13..i + 17) == Some(&b"rGvM"[..]);
        let group_clip = after && seg.get(i + 13..i + 17) == Some(&b"aPnA"[..]);
        let end = tagged(i, 1) && seg.get(i + 8) == Some(&1) && (nulls(i + 9) || nulls(i + 13));
        if first || next_state {
            depth += first as u8;
            let tag = if first { i + 12 } else { i + 13 };
            let name = if tagged(tag + 4, 3) { string_at(seg, tag + 12).unwrap_or_default() } else { String::new() };
            states.push(QteState { depth, name, conds: Vec::new(), clip: None, start: 0.0, anchored: false, ranges: Vec::new() });
            owner = Some(states.len() - 1);
            in_transition = false;
            i = tag + 4;
            continue;
        }
        if group_clip || end {
            depth = depth.saturating_sub(1);
            owner = if group_clip { states.iter().rposition(|s| s.depth == depth) } else { None };
            in_transition = false;
            i += 9;
            continue;
        }
        if &seg[i..i + 4] == b"EECM" && tagged(i + 4, 1) {
            let value = seg[i + 12];
            let text = &seg[i + 16..(i + 80).min(seg.len())];
            let id = text.windows(11).find(|w| w.starts_with(b"QTE") && w[3].is_ascii_digit() && &w[4..] == b"_Result").map(|w| w[3] - b'0');
            if let (Some(o), Some(id)) = (owner, id.filter(|&n| n > 0)) {
                states[o].conds.push((id - 1, value));
            }
            i += 16;
            continue;
        }
        // A progress range: its kind (2), low and high, then the variable
        // (`Interactive_Float<n>`, QTE n - 1's).
        if &seg[i..i + 4] == b"FIcM" && seg.get(i + 4..i + 8) == Some(&VALUE[..]) && u32_at(seg, i + 8) == Some(2) && !in_transition {
            let f = |at: usize| u32_at(seg, at).map(f32::from_bits).unwrap_or(0.0);
            let var = (i + 20..(i + 96).min(seg.len().saturating_sub(4))).find_map(|k| string_at(seg, k).filter(|s| s.starts_with("Interactive_Float")));
            let id = var.and_then(|v| v["Interactive_Float".len()..].parse::<u8>().ok()).filter(|&n| n > 0);
            if let (Some(o), Some(n)) = (owner, id) {
                states[o].ranges.push((n - 1, f(i + 12), f(i + 16)));
            }
            i += 20;
            continue;
        }
        // A posed clip: its time, then its path.
        if &seg[i..i + 4] == b"pMCT" && tagged(i + 4, 3) && !in_transition {
            let start = u32_at(seg, i + 20).map(f32::from_bits).filter(|t| t.is_finite() && *t >= 0.0).unwrap_or(0.0);
            let path = (i + 32..(i + 160).min(seg.len().saturating_sub(4)))
                .find_map(|k| string_at(seg, k).filter(|s| s.to_ascii_lowercase().starts_with("graphics\\")));
            if let (Some(o), Some(p)) = (owner, path) {
                if states[o].clip.is_none() {
                    states[o].clip = Some(format!("{}.mab", p.to_ascii_lowercase()));
                    states[o].start = start;
                }
            }
            i += 20;
            continue;
        }
        if &seg[i..i + 4] == b"sALT" && tagged(i + 4, 4) {
            in_transition = true;
            let label = if tagged(i + 12, 3) { string_at(seg, i + 20).unwrap_or_default() } else { String::new() };
            let parts: Vec<&str> = label.split('/').collect();
            transition = (parts.len() >= 3).then(|| (parts[parts.len() - 2].to_string(), parts[parts.len() - 1].to_ascii_lowercase()));
        }
        if &seg[i..i + 4] == b"cnAA" && tagged(i + 4, 10) && !in_transition {
            if let Some(o) = owner {
                states[o].anchored = true;
            }
        }
        if &seg[i..i + 4] == b"aPnA" && tagged(i + 4, 30) {
            // Its clip: the next string that is a path.
            let mut k = i + 8;
            while k + 12 <= seg.len() && k < i + 512 {
                if tagged(k, 3) {
                    if let Some(s) = string_at(seg, k + 8).filter(|s| s.to_ascii_lowercase().starts_with("graphics\\")) {
                        let path = format!("{}.mab", s.to_ascii_lowercase());
                        if !in_transition {
                            if let Some(o) = owner.filter(|&o| states[o].clip.is_none()) {
                                states[o].clip = Some(path);
                            }
                        } else if let Some((state, _)) = transition.take_if(|(_, c)| path.ends_with(&format!("\\{c}.mab"))) {
                            into.entry(state).or_insert(path);
                        }
                        break;
                    }
                }
                k += 1;
            }
        }
        i += 1;
    }
    // A state with no clip or children of its own plays the clip the
    // transitions into it name.
    for k in 0..states.len() {
        let parent = states.get(k + 1).is_some_and(|c| c.depth > states[k].depth);
        if states[k].clip.is_none() && !parent {
            states[k].clip = into.get(&states[k].name).cloned();
        }
    }
    states
}

/// What a QTE event does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A QTE starts: the input pressed once decides it.
    Press,
    /// A QTE starts: the input held long enough decides it.
    Hold,
    /// A QTE starts: the input mashed until the intensity is reached.
    Mash,
    /// A QTE starts that the scripts decide.
    Zombie,
    /// A QTE ends (its result stays).
    End,
    /// A QTE's progress and result are set (`values`: progress, result).
    Reset,
    /// A QTE's progress (set, or added to) and result are set (`values`:
    /// progress, result, 1 to add).
    Set,
    /// The whole sequence ends: the gameplay is over.
    EndSequence,
    /// The player dies.
    PlayerDeath,
    /// A message to the scripts (`text`).
    Message,
}

impl Kind {
    const NAMES: [(&'static str, Kind); 10] = [
        ("QuickTimeEventPress", Kind::Press),
        ("QuickTimeEventHold", Kind::Hold),
        ("QuickTimeEventMash", Kind::Mash),
        ("QuickTimeEventZombie", Kind::Zombie),
        ("QuickTimeEventEndQTE", Kind::End),
        ("QuickTimeEventResetPMS", Kind::Reset),
        ("QuickTimeEventSetPMSValues", Kind::Set),
        ("QuickTimeEventEndSequence", Kind::EndSequence),
        ("QuickTimeEventPlayerDeath", Kind::PlayerDeath),
        ("QuickTimeEventMessageDomino", Kind::Message),
    ];

    fn from_name(name: &str) -> Option<Kind> {
        Self::NAMES.iter().find(|(n, _)| *n == name).map(|&(_, k)| k)
    }

    fn name(self) -> &'static str {
        Self::NAMES.iter().find(|(_, k)| *k == self).map_or("", |(n, _)| n.trim_start_matches("QuickTimeEvent"))
    }

    fn from_short(name: &str) -> Option<Kind> {
        Self::NAMES.iter().find(|(n, _)| n.trim_start_matches("QuickTimeEvent") == name).map(|&(_, k)| k)
    }
}

/// A QTE event in a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Seconds into the clip.
    pub time: f32,
    pub kind: Kind,
    /// The QTE it is about (`selQTE_Id`).
    pub id: u8,
    /// The input asked for (`selInputs`, [`input_name`]).
    pub input: u8,
    /// The prompt (a string id: the word for a press, a hold or a mash), if
    /// any.
    pub prompt: Option<u32>,
    /// A mash: the intensity required, how fast it drops, the interval
    /// between counted presses, the time allowed. A hold: how long to hold,
    /// the time to react (negative: no limit). A reset: progress, result.
    pub values: [f32; 4],
    /// A message's text.
    pub text: String,
}

/// The inputs a QTE asks for, by `selInputs` (the engine's own names).
pub const INPUTS: [&str; 25] = [
    "None",
    "Button_1",
    "Button_2",
    "Button_3",
    "Button_4",
    "RT",
    "LT",
    "LB",
    "RB",
    "Up",
    "Down",
    "Left",
    "Right",
    "LeftStickLeft",
    "RightStickLeft",
    "LeftStickRight",
    "RightStickRight",
    "LeftStickUp",
    "RightStickUp",
    "LeftStickDown",
    "RightStickDown",
    "Undefined",
    "RightStickButton",
    "LeftStickButton",
    "RandomButton",
];

pub fn input_name(input: u8) -> &'static str {
    INPUTS.get(input as usize).copied().unwrap_or("Undefined")
}

/// A clip's QTE events (FCB objects among its events, each after its time
/// and size; some have no size and read on), in time order.
pub fn clip_events(d: &[u8]) -> Vec<Event> {
    let mut out = Vec::new();
    let mut q = 0;
    while let Some(o) = find(d, q, b"nbCF") {
        q = o + 4;
        if o < 12 {
            continue;
        }
        let time = f32::from_le_bytes(d[o - 12..o - 8].try_into().unwrap());
        let size = u16::from_le_bytes([d[o - 2], d[o - 1]]) as usize;
        let end = if size < 16 { d.len() } else { (o + size).min(d.len()) };
        let Ok(obj) = crate::fcb::parse(&d[o..end]) else { continue };
        if size >= 16 {
            q = o + size;
        }
        let Some(kind) = obj.str("hidEventName").and_then(|n| Kind::from_name(&n)) else { continue };
        if !time.is_finite() {
            continue;
        }
        let f = |name: &str| obj.f32(name).unwrap_or(0.0);
        let values = match kind {
            Kind::Mash => [f("fIntensityRequired"), f("fDecreaseSpeed"), f("fMashInterval"), f("fDuration")],
            Kind::Hold => [f("fDuration"), obj.f32("fReactionTime").unwrap_or(-1.0), f("fdecreaseSpeed"), obj.u32("selHoldType").unwrap_or(0) as f32],
            Kind::Press => [f("fDuration"), obj.f32("fReactionTime").unwrap_or(-1.0), 0.0, 0.0],
            Kind::Reset => [f("fQTE_ProgressValue"), obj.u32("selQTE_ResultValue").unwrap_or(NONE as u32) as f32, 0.0, 0.0],
            Kind::Set => [
                f("fQTE_ProgressValue"),
                obj.u32("selQTE_ResultValue").unwrap_or(NONE as u32) as f32,
                obj.bool("bAddProgress").unwrap_or(false) as u8 as f32,
                0.0,
            ],
            _ => [0.0; 4],
        };
        out.push(Event {
            time: time.max(0.0),
            kind,
            id: obj.u32("selQTE_Id").unwrap_or(0).min(15) as u8,
            input: obj.u32("selInputs").unwrap_or(0).min(255) as u8,
            prompt: obj.u32("iOasisPromptId").filter(|&p| p != u32::MAX && p != 0),
            values,
            text: obj.str("sParam").unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| a.time.total_cmp(&b.time));
    out
}

/// The QTE moves and their clips' events, as the cache keeps them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QteTable {
    pub moves: BTreeMap<String, Vec<QteState>>,
    /// By clip (archive path).
    pub clips: BTreeMap<String, Vec<Event>>,
}

impl QteTable {
    /// The QTE moves in the move database, and the events of the clips
    /// their states play (`read`: a clip's bytes by archive path).
    pub fn build(db: &[u8], mut read: impl FnMut(&str) -> Option<Vec<u8>>) -> Self {
        let moves = parse_moves(db);
        let mut clips = BTreeMap::new();
        for s in moves.values().flatten() {
            let Some(clip) = &s.clip else { continue };
            if !clips.contains_key(clip) {
                let events = read(clip).map(|b| clip_events(&b)).unwrap_or_default();
                clips.insert(clip.clone(), events);
            }
        }
        QteTable { moves, clips }
    }

    pub fn is_current(text: &str) -> bool {
        text.lines().next().map(str::trim) == Some(QTE_MAGIC)
    }

    /// `M move`, its states (`S depth name anchored conds clip ranges
    /// start`, ranges as `id:low:high` joined by commas), then
    /// `C clip` and its events (`E time kind id input prompt values text`),
    /// tab-separated.
    pub fn to_text(&self) -> String {
        let mut s = format!("{QTE_MAGIC}\n");
        for (name, states) in &self.moves {
            s += &format!("M\t{name}\n");
            for st in states {
                let conds: Vec<String> = st.conds.iter().map(|(i, v)| format!("{i}={v}")).collect();
                let ranges: Vec<String> = st.ranges.iter().map(|(i, lo, hi)| format!("{i}:{lo}:{hi}")).collect();
                s += &format!(
                    "S\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    st.depth,
                    st.name,
                    st.anchored as u8,
                    conds.join(","),
                    st.clip.as_deref().unwrap_or(""),
                    ranges.join(","),
                    st.start
                );
            }
        }
        for (clip, events) in &self.clips {
            s += &format!("C\t{clip}\n");
            for e in events {
                let v = e.values.map(|x| x.to_string()).join("\t");
                s += &format!("E\t{}\t{}\t{}\t{}\t{}\t{v}\t{}\n", e.time, e.kind.name(), e.id, e.input, e.prompt.unwrap_or(0), e.text);
            }
        }
        s
    }

    pub fn from_text(text: &str) -> Self {
        let mut t = QteTable::default();
        let (mut mv, mut clip) = (None::<String>, None::<String>);
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["M", name] => mv = Some(name.to_string()),
                ["S", depth, name, anchored, conds, path, ranges, start] => {
                    let Some(m) = &mv else { continue };
                    let conds = conds.split(',').filter_map(|c| c.split_once('=')).filter_map(|(i, v)| Some((i.parse().ok()?, v.parse().ok()?))).collect();
                    let ranges = ranges
                        .split(',')
                        .filter_map(|r| {
                            let mut p = r.split(':');
                            Some((p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?))
                        })
                        .collect();
                    t.moves.entry(m.clone()).or_default().push(QteState {
                        depth: depth.parse().unwrap_or(1),
                        name: name.to_string(),
                        conds,
                        clip: (!path.is_empty()).then(|| path.to_string()),
                        start: start.parse().unwrap_or(0.0),
                        anchored: *anchored == "1",
                        ranges,
                    });
                }
                ["C", path] => {
                    clip = Some(path.to_string());
                    t.clips.entry(path.to_string()).or_default();
                }
                ["E", time, kind, id, input, prompt, v0, v1, v2, v3, text] => {
                    let (Some(c), Some(kind)) = (&clip, Kind::from_short(kind)) else { continue };
                    let n = |s: &str| s.parse::<f32>().unwrap_or(0.0);
                    t.clips.entry(c.clone()).or_default().push(Event {
                        time: n(time),
                        kind,
                        id: id.parse().unwrap_or(0),
                        input: input.parse().unwrap_or(0),
                        prompt: prompt.parse().ok().filter(|&p| p != 0),
                        values: [n(v0), n(v1), n(v2), n(v3)],
                        text: text.to_string(),
                    });
                }
                _ => {}
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(depth: u8, name: &str, conds: &[(u8, u8)], clip: bool) -> QteState {
        QteState { depth, name: name.into(), conds: conds.to_vec(), clip: clip.then(|| format!("{name}.mab")), start: 0.0, anchored: false, ranges: Vec::new() }
    }

    /// The escape's crawl, as the move database has it.
    fn crawl() -> Vec<QteState> {
        vec![
            state(1, "Dead", &[(2, FAILED)], true),
            state(1, "Exit", &[(2, SUCCEEDED)], true),
            state(1, "Crawl", &[(2, IN_PROGRESS)], false),
            state(2, "CrawlRight", &[(0, SUCCEEDED)], true),
            state(2, "CrawlLeft", &[(1, SUCCEEDED)], true),
            state(2, "Idle", &[(0, NONE), (0, FAILED), (0, IN_PROGRESS), (1, NONE), (1, IN_PROGRESS), (1, FAILED)], true),
            state(1, "Intro", &[(2, NONE)], true),
        ]
    }

    #[test]
    fn a_crawl_plays_its_intro_then_crawls_on_each_press_until_the_script_decides() {
        let s = crawl();
        let name = |r: [u8; 3]| select(&s, &r, &[]).map(|i| s[i].name.as_str());
        assert_eq!(name([NONE, NONE, NONE]), Some("Intro"));
        assert_eq!(name([IN_PROGRESS, NONE, IN_PROGRESS]), Some("Idle"));
        assert_eq!(name([SUCCEEDED, NONE, IN_PROGRESS]), Some("CrawlRight"));
        assert_eq!(name([NONE, SUCCEEDED, IN_PROGRESS]), Some("CrawlLeft"));
        assert_eq!(name([SUCCEEDED, IN_PROGRESS, SUCCEEDED]), Some("Exit"));
        assert_eq!(name([IN_PROGRESS, NONE, FAILED]), Some("Dead"));
        // A state under a group holds only while the group's does.
        assert!(holds_within(&s, 3, &[SUCCEEDED, NONE, IN_PROGRESS]));
        assert!(!holds_within(&s, 3, &[SUCCEEDED, NONE, SUCCEEDED]));
    }

    #[test]
    fn a_group_with_no_child_to_play_plays_its_own_clip() {
        let s = vec![
            state(1, "Struggle", &[(2, SUCCEEDED)], true),
            state(2, "Progress", &[(3, IN_PROGRESS)], true),
            state(2, "Fail", &[(3, FAILED)], true),
            state(1, "Fight", &[], false),
            state(2, "SecA", &[(0, NONE), (0, IN_PROGRESS)], true),
        ];
        assert_eq!(select(&s, &[NONE, NONE, SUCCEEDED, NONE], &[]), Some(0));
        assert_eq!(select(&s, &[NONE, NONE, SUCCEEDED, IN_PROGRESS], &[]), Some(1));
        assert_eq!(select(&s, &[NONE; 4], &[]), Some(4));
        assert_eq!(select(&s, &[SUCCEEDED, NONE, NONE, NONE], &[]), None);
    }

    #[test]
    fn a_climb_reaches_on_as_its_progress_moves() {
        // JAS_160's mass grave: the reaches by QTE 2's progress (each
        // reach's clip sets it on), each played once QTE 0's hold succeeds;
        // the way out by a mash once the progress is near its end.
        let banded = |depth, name, conds: &[(u8, u8)], clip, lo, hi| QteState { ranges: vec![(2, lo, hi)], ..state(depth, name, conds, clip) };
        let s = vec![
            banded(1, "MashExit", &[], false, 0.9, 1.0),
            state(2, "SuccessExit", &[(1, SUCCEEDED)], true),
            state(2, "Mashing", &[], true),
            state(1, "Climbing", &[(2, IN_PROGRESS)], false),
            banded(2, "Reach2", &[], false, 0.2, 0.3),
            state(3, "Reach2Success", &[(0, SUCCEEDED)], true),
            state(3, "Reach2Pose", &[], true),
            banded(2, "Reach1", &[], false, 0.0, 0.2),
            state(3, "Reach1Success", &[(0, SUCCEEDED)], true),
            state(3, "Reach1Pose", &[], true),
            banded(1, "Intro", &[(0, NONE)], true, 0.0, 0.01),
        ];
        let name = |r: [u8; 3], p: [f32; 3]| select(&s, &r, &p).map(|i| s[i].name.as_str());
        assert_eq!(name([NONE, NONE, NONE], [0.0; 3]), Some("Intro"));
        assert_eq!(name([SUCCEEDED, NONE, IN_PROGRESS], [1.0, 0.0, 0.0]), Some("Reach1Success"));
        assert_eq!(name([IN_PROGRESS, NONE, IN_PROGRESS], [0.0, 0.0, 0.25]), Some("Reach2Pose"));
        assert_eq!(name([SUCCEEDED, NONE, IN_PROGRESS], [1.0, 0.0, 0.25]), Some("Reach2Success"));
        assert_eq!(name([SUCCEEDED, IN_PROGRESS, IN_PROGRESS], [1.0, 0.4, 0.95]), Some("Mashing"));
        assert_eq!(name([SUCCEEDED, SUCCEEDED, IN_PROGRESS], [1.0, 1.0, 0.95]), Some("SuccessExit"));
    }

    /// A move's bytes as the database lays them out (enough of it).
    fn move_bytes(name: &str, states: &[(&str, u8, &[(u8, u32)], Option<&str>)]) -> Vec<u8> {
        let v = |ty: u32| [&VALUE[..], &ty.to_le_bytes()].concat();
        let s = |text: &str| [&v(3)[..], &(text.len() as u32).to_le_bytes(), text.as_bytes()].concat();
        let mut b = [b"TSvM".to_vec(), v(2), v(7), s(name), vec![0; 8]].concat();
        let mut depth = 0u8;
        for (i, (state, d, conds, clip)) in states.iter().enumerate() {
            while depth > *d {
                b.extend([&v(1)[..], &[1], &[0xfe, 0xff, 0xff, 0xff].repeat(3), &[0; 8]].concat());
                depth -= 1;
            }
            if *d > depth || i == 0 {
                b.extend([&v(3)[..], &[0xff; 4], b"rGvM"].concat());
                depth = *d;
            } else {
                b.extend([&v(1)[..], &[1], &[0xff; 4], b"rGvM"].concat());
            }
            b.extend([s(state), vec![0; 16], vec![0xff; 4]].concat());
            for (id, value) in conds.iter() {
                let var = format!("QTE{}_Result", id + 1);
                b.extend([&b"EECM"[..], &v(1), &value.to_le_bytes(), &v(4), &[0x80 | (id + 1)], &s(&var), &[0; 8]].concat());
            }
            if let Some(clip) = clip {
                b.extend([&v(3)[..], &[0xff; 4], b"aPnA", &v(30), &[0; 32], &s(clip), &[0; 8], b"cnAA", &v(10), &[0; 8]].concat());
            }
        }
        while depth > 0 {
            b.extend([&v(1)[..], &[1], &[0xfe, 0xff, 0xff, 0xff].repeat(3), &[0; 8]].concat());
            depth -= 1;
        }
        b
    }

    #[test]
    fn move_trees_come_from_the_database_layout() {
        let db = [
            move_bytes(
                "Ex_Test_Jason",
                &[
                    ("Exit", 1, &[(2, 2)], Some(r"graphics\a\exit")),
                    ("Crawl", 1, &[(2, 0)], None),
                    ("CrawlRight", 2, &[(0, 2)], Some(r"graphics\a\right")),
                    ("Idle", 2, &[(0, 3), (0, 0)], Some(r"graphics\a\idle")),
                    ("Intro", 1, &[(2, 3)], Some(r"graphics\a\intro")),
                ],
            ),
            move_bytes("Plain_Move", &[("Loop", 1, &[], Some(r"graphics\a\loop"))]),
        ]
        .concat();
        let moves = parse_moves(&db);
        assert_eq!(moves.keys().collect::<Vec<_>>(), ["ex_test_jason"], "a move without QTE conditions is not a QTE move");
        let s = &moves["ex_test_jason"];
        let tree: Vec<(u8, &str, &[(u8, u8)])> = s.iter().map(|s| (s.depth, s.name.as_str(), s.conds.as_slice())).collect();
        assert_eq!(
            tree,
            [
                (1, "Exit", &[(2, 2)][..]),
                (1, "Crawl", &[(2, 0)][..]),
                (2, "CrawlRight", &[(0, 2)][..]),
                (2, "Idle", &[(0, 3), (0, 0)][..]),
                (1, "Intro", &[(2, 3)][..]),
            ]
        );
        assert_eq!(s[0].clip.as_deref(), Some(r"graphics\a\exit.mab"));
        assert_eq!(s[1].clip, None);
        assert!(s[0].anchored);
    }

    #[test]
    fn the_table_survives_the_cache() {
        let mut t = QteTable::default();
        t.moves.insert("ex_test".into(), crawl());
        t.moves.get_mut("ex_test").unwrap()[0].anchored = true;
        t.moves.get_mut("ex_test").unwrap()[2].ranges = vec![(2, 0.2, 0.3), (1, 0.0, 0.5)];
        t.moves.get_mut("ex_test").unwrap()[3].start = 17.828;
        t.clips.insert(
            "Intro.mab".into(),
            vec![
                Event { time: 0.546, kind: Kind::Zombie, id: 2, input: 0, prompt: None, values: [0.0; 4], text: String::new() },
                Event { time: 0.6, kind: Kind::Press, id: 0, input: 5, prompt: Some(143240), values: [0.0, -1.0, 0.0, 0.0], text: String::new() },
                Event { time: 6.8, kind: Kind::Message, id: 0, input: 0, prompt: None, values: [0.0; 4], text: "PlayerDie".into() },
            ],
        );
        let text = t.to_text();
        assert!(QteTable::is_current(&text));
        assert_eq!(QteTable::from_text(&text), t);
    }

    #[test]
    #[ignore = "requires locally installed Far Cry 3 archives"]
    fn installed_qte_moves_have_their_trees_and_events() {
        let vfs = crate::Vfs::open_install(&crate::find_install().expect("installed Far Cry 3"), None).unwrap();
        let t = QteTable::build(&vfs.read(crate::moves::MOVE_DB).unwrap(), |c| vfs.read(c).ok());
        let escape = &t.moves["ex_finalescape_jason"];
        let names: Vec<(u8, &str)> = escape.iter().map(|s| (s.depth, s.name.as_str())).collect();
        assert_eq!(names, [(1, "Dead"), (1, "Exit"), (1, "Crawl"), (2, "CrawlRight"), (2, "CrawlLeft"), (2, "Idle"), (1, "Intro")]);
        let intro = &t.clips[escape[6].clip.as_ref().unwrap()];
        assert!(intro.iter().any(|e| e.kind == Kind::Zombie && e.id == 2));
        assert!(intro.iter().any(|e| e.kind == Kind::Press && e.id == 0 && input_name(e.input) == "RT"));
        let fight = &t.moves["ex_bambifightqte_jason"];
        let top: Vec<&str> = fight.iter().filter(|s| s.depth == 1).map(|s| s.name.as_str()).collect();
        assert_eq!(top, ["Struggle", "Insert", "KnifeFight"]);
        // The mass grave's reaches, by QTE 2's progress; their poses are
        // times in one long clip; a reach's clip sets the progress on.
        let grave = &t.moves["ex_massgraveqte_jason"];
        let band = |name: &str| grave.iter().find(|s| s.name == name).map(|s| s.ranges.clone()).unwrap();
        assert_eq!(band("Reach2"), [(2, 0.2, 0.3)]);
        assert_eq!(band("Intro"), [(2, 0.0, 0.01)]);
        let reach1 = grave.iter().position(|s| s.name == "Reach1").unwrap();
        let pose = &grave[reach1 + 2];
        assert_eq!(pose.name, "Reach");
        assert!(pose.clip.as_deref().is_some_and(|c| c.ends_with("ex_jas_170_massgrave_jason.mab")) && (pose.start - 12.5).abs() < 0.01);
        let success = &t.clips[grave[reach1 + 1].clip.as_ref().unwrap()];
        assert!(success.iter().any(|e| e.kind == Kind::Set && e.id == 2 && (e.values[0] - 0.25).abs() < 1e-6));
    }
}
