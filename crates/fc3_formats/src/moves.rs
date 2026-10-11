//! Moves: what the engine's animation system plays by name.
//!
//! The engine's named move database (`graphics\move\movemgrnamed.bin`)
//! holds every move by name (`STP_RelaxGuard_Pirate_v04_Wait`,
//! `Ex_FirstStruggleQTE_Jason`) with its states (a QTE move's `Intro`,
//! `Struggle`, `Exit`, `Fail`) and the clip each plays; [`parse_move_db`]
//! reads it and the converter keeps it in the cache ([`MOVES_REL`]).
//!
//! A contextual action (a "smart terrain point") names its moves in its
//! parameters, e.g. `stp_relaxguard_pirate_v04_start` and `..._wait`. Their
//! clips come from the move database; the few moves it lacks are matched by
//! name among the game's animations: the clip whose name (past its
//! `3rdge_fulbst_` part) is the move's, with the phase written the clips'
//! way (`wait` is a `loop`). The converter keeps that table too
//! ([`CA_MOVES_REL`]): one line per move, its clips' archive paths after it
//! (a build per weapon held).

use std::collections::BTreeMap;

/// Moves and their clips, in the cache: `move<TAB>clip|clip|...`.
pub const CA_MOVES_REL: &str = "story/ca_moves.txt";

/// The engine's named move database, and its moves in the cache:
/// `move<TAB>state=clip|state=clip...` (lower-case move names, clips'
/// archive paths).
pub const MOVE_DB: &str = r"graphics\move\movemgrnamed.bin";
pub const MOVES_REL: &str = "story/moves.txt";

/// A move's states and the clip each plays: (state, clip archive path).
pub type MoveStates = Vec<(String, String)>;

/// What tags a value in the move database.
const VALUE: [u8; 4] = [0xb1, 0x68, 0xde, 0x3a];

/// The named move database's moves, each (by lower-case name) with its
/// states and the clips they play. Values in the file are tagged (`VALUE`,
/// then a type); type 3 is a string after its length. Chunks open with a
/// four-letter tag before a value: `TSvM` a move (named by the next string),
/// `rGvM` one of its states (likewise), `aPnA` an animation (its clip the
/// next string that is a path).
pub fn parse_move_db(data: &[u8]) -> BTreeMap<String, MoveStates> {
    #[derive(PartialEq)]
    enum Want {
        Nothing,
        Move,
        State,
        Clip,
    }
    let u32_at = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
    let mut out: BTreeMap<String, MoveStates> = BTreeMap::new();
    let (mut want, mut current, mut state) = (Want::Nothing, None::<String>, String::new());
    let mut i = 0;
    while i + 12 <= data.len() {
        if data[i + 4..i + 8] == VALUE && data[i..i + 4].iter().all(|b| b.is_ascii_alphabetic()) {
            want = match &data[i..i + 4] {
                b"TSvM" => Want::Move,
                b"rGvM" => Want::State,
                b"aPnA" => Want::Clip,
                _ => Want::Nothing,
            };
            i += 4;
            continue;
        }
        if data[i..i + 4] == VALUE && u32_at(i + 4) == 3 {
            let n = u32_at(i + 8);
            if n > 0 && n < 1024 && i + 12 + n <= data.len() && data[i + 12..i + 12 + n].iter().all(|b| (0x20..0x7f).contains(b)) {
                let text = String::from_utf8_lossy(&data[i + 12..i + 12 + n]).into_owned();
                match want {
                    Want::Move => {
                        let name = text.to_ascii_lowercase();
                        out.entry(name.clone()).or_default();
                        current = Some(name);
                        state.clear();
                        want = Want::Nothing;
                    }
                    Want::State => {
                        state = text;
                        want = Want::Nothing;
                    }
                    Want::Clip if text.to_ascii_lowercase().starts_with("graphics\\") => {
                        if let Some(m) = current.as_ref().and_then(|m| out.get_mut(m)) {
                            m.push((state.clone(), format!("{}.mab", text.to_ascii_lowercase())));
                        }
                        want = Want::Nothing;
                    }
                    _ => {}
                }
                i += 12 + n;
                continue;
            }
        }
        i += 1;
    }
    out.retain(|_, v| !v.is_empty());
    out
}

/// The move table as the cache keeps it.
pub fn moves_to_text(moves: &BTreeMap<String, MoveStates>) -> String {
    moves.iter().map(|(m, v)| format!("{m}\t{}\n", v.iter().map(|(s, c)| format!("{s}={c}")).collect::<Vec<_>>().join("|"))).collect()
}

pub fn moves_from_text(text: &str) -> BTreeMap<String, MoveStates> {
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(m, v)| (m.to_string(), v.split('|').filter_map(|p| p.split_once('=')).map(|(s, c)| (s.to_string(), c.to_string())).collect()))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    Start,
    Loop,
    End,
}

const STARTS: [&str; 5] = ["start", "in", "enter", "intro", "begin"];
const LOOPS: [&str; 4] = ["loop", "wait", "idle", "lp"];
const ENDS: [&str; 5] = ["end", "out", "exit", "outro", "stop"];

fn phase_word(w: &str) -> Option<Phase> {
    if STARTS.contains(&w) {
        Some(Phase::Start)
    } else if LOOPS.contains(&w) {
        Some(Phase::Loop)
    } else if ENDS.contains(&w) {
        Some(Phase::End)
    } else {
        None
    }
}

/// A move's base name and phase (`stp_smoke_start` -> `smoke`, start); a
/// move without a phase word plays as a loop.
pub fn split_move(mv: &str) -> (String, Phase) {
    let m = mv.to_ascii_lowercase();
    let m = m.strip_prefix("stp_").unwrap_or(&m);
    match m.rsplit_once('_') {
        Some((base, last)) => match phase_word(last) {
            Some(p) => (base.to_string(), p),
            None => (m.to_string(), Phase::Loop),
        },
        None => (m.to_string(), Phase::Loop),
    }
}

/// Whether a parameter value looks like a move name.
pub fn is_move_name(s: &str) -> bool {
    s.len() > 4 && s.starts_with("stp_") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The moves a contextual action's parameters name.
pub fn move_names(params: &crate::fcb::Object) -> Vec<String> {
    params
        .fields
        .iter()
        .filter(|(_, v)| v.last() == Some(&0) && v.len() > 5)
        .map(|(_, v)| crate::fcb::bytes_to_string(v).to_ascii_lowercase())
        .filter(|s| is_move_name(s))
        .collect()
}

/// A clip's words past its view and body part (`3rdge_fulbst_`), without
/// its angle and weapon words (`+000fw`, `ak74`): lower case, no
/// extension.
fn clip_words(path: &str) -> Option<Vec<String>> {
    let stem = path.rsplit(['\\', '/']).next()?.strip_suffix(".mab")?.to_ascii_lowercase();
    let mut words: Vec<&str> = stem.split('_').collect();
    if words.first().is_some_and(|w| w.starts_with("3rd") || w.starts_with("1st")) {
        words.remove(0);
    }
    if words.first().is_some_and(|w| ["ful", "upp", "low"].iter().any(|p| w.starts_with(p))) {
        words.remove(0);
    }
    let noise = |w: &str| {
        w.starts_with('+') || w.starts_with('-') || ["ak74", "nowep", "noweap", "noweapon", "pistol", "shotgun", "sniper", "rpg", "machete"].contains(&w)
    };
    Some(words.into_iter().filter(|w| !noise(w)).map(str::to_string).collect())
}

/// Words a move may name that its clips leave out (who does it).
const WHO: [&str; 9] = ["pirate", "pirates", "priv", "privateer", "privateers", "rebel", "rebels", "civ", "merc"];

/// Whether a clip word says a phase (also inside a longer word: `begloop`,
/// `loopclose`).
fn says_phase(w: &str, phase: Phase) -> bool {
    match phase {
        Phase::Loop => w.contains("loop") || w.contains("wait") || w == "idle" || w == "lp",
        Phase::Start => w.contains("start") || ["in", "enter", "intro", "begin"].contains(&w),
        Phase::End => (w.ends_with("end") && !w.ends_with("tend")) || w.contains("exit") || ["out", "outro", "stop"].contains(&w),
    }
}

/// A version word (`v00`).
fn is_version(w: &str) -> bool {
    w.len() == 3 && w.starts_with('v') && w[1..].bytes().all(|b| b.is_ascii_digit())
}

/// How a clip's words follow a move's base: the base words in order from
/// the first, then the words left over. Either side may leave out a version
/// word, and the move's words naming who does it may be missing.
fn after_base<'w>(clip: &'w [String], base: &[&str], strict: bool) -> Option<&'w [String]> {
    if clip.first().map(String::as_str) != base.first().copied() {
        return None;
    }
    let (mut i, mut j) = (0, 0);
    while j < base.len() {
        let c = clip.get(i).map(String::as_str);
        if c == Some(base[j]) {
            i += 1;
            j += 1;
        } else if strict && is_version(base[j]) && c.is_some_and(is_version) {
            return None;
        } else if WHO.contains(&base[j]) || is_version(base[j]) {
            j += 1;
        } else if c.is_some_and(is_version) {
            i += 1;
        } else {
            return None;
        }
    }
    Some(&clip[i..])
}

/// The clips (archive paths) for each move: the clips whose words begin
/// with the move's base and go on with a word for its phase.
pub fn match_moves<'a>(moves: impl IntoIterator<Item = &'a str>, clips: &[&'a str]) -> BTreeMap<String, Vec<String>> {
    // Clips by their first word, to keep the search short.
    let mut by_first: BTreeMap<String, Vec<(Vec<String>, &str)>> = BTreeMap::new();
    for c in clips {
        if let Some(w) = clip_words(c) {
            if let Some(f) = w.first().cloned() {
                by_first.entry(f).or_default().push((w, c));
            }
        }
    }
    // And by their folders (`guard_camps\pirate_comms`: `piratecomms`, and
    // `guardcampspiratecomms`), for moves named after them.
    let mut by_folder: BTreeMap<String, Vec<(Vec<String>, &str)>> = BTreeMap::new();
    for c in clips {
        let Some(w) = clip_words(c) else { continue };
        let folders: Vec<String> = c.rsplit(['\\', '/']).skip(1).take(2).map(|f| f.to_ascii_lowercase().replace('_', "")).collect();
        if let Some(last) = folders.first() {
            by_folder.entry(last.clone()).or_default().push((w.clone(), c));
            if let Some(up) = folders.get(1) {
                by_folder.entry(format!("{up}{last}")).or_default().push((w, c));
            }
        }
    }
    let mut out = BTreeMap::new();
    for mv in moves {
        let (base, phase) = split_move(mv);
        let base_words: Vec<&str> = base.split('_').collect();
        // The move's own build first, then another version of it.
        let mut found = Vec::new();
        if let Some(cands) = by_first.get(base_words[0]) {
            for strict in [true, false] {
                found = clips_for(cands, &base_words, phase, strict);
                if !found.is_empty() {
                    break;
                }
            }
        }
        // Else the clips in the folder it is named after, for its phase.
        if found.is_empty() {
            if let Some(cands) = by_folder.get(&base.replace('_', "")) {
                found = cands.iter().filter(|(w, _)| w.iter().any(|x| says_phase(x, phase))).map(|(_, c)| c.to_string()).collect();
            }
        }
        if !found.is_empty() {
            found.sort();
            out.insert(mv.to_ascii_lowercase(), found);
        }
    }
    out
}

/// A move's clips among those sharing its first word: the clips going on
/// past its base with a word for its phase, else (one clip for the action)
/// with none for any.
fn clips_for(cands: &[(Vec<String>, &str)], base_words: &[&str], phase: Phase, strict: bool) -> Vec<String> {
    let pick = |keep: &dyn Fn(&[String]) -> bool| -> Vec<String> {
        cands.iter().filter(|(w, _)| after_base(w, base_words, strict).is_some_and(keep)).map(|(_, c)| c.to_string()).collect()
    };
    let found = pick(&|rest| rest.iter().any(|x| says_phase(x, phase)));
    if !found.is_empty() {
        return found;
    }
    pick(&|rest| ![Phase::Start, Phase::Loop, Phase::End].iter().any(|p| rest.iter().any(|x| says_phase(x, *p))))
}

/// The clips for contextual actions' moves: the move database's (exact),
/// else those found by name.
pub fn ca_clips<'a>(moves: impl IntoIterator<Item = &'a str>, db: &BTreeMap<String, MoveStates>, clips: &[&'a str]) -> BTreeMap<String, Vec<String>> {
    let names: Vec<&str> = moves.into_iter().collect();
    let mut out = match_moves(names.iter().copied().filter(|m| !db.contains_key(&m.to_ascii_lowercase())), clips);
    for m in names {
        if let Some(states) = db.get(&m.to_ascii_lowercase()) {
            let mut c: Vec<String> = states.iter().map(|(_, c)| c.clone()).collect();
            c.sort();
            c.dedup();
            out.insert(m.to_ascii_lowercase(), c);
        }
    }
    out
}

/// The cached table's first line: which version wrote it (the story's
/// contextual actions' moves, and the encounters' spots').
const CA_MOVES_MAGIC: &str = "# fc3-ca-moves 4";

/// Whether a cached table is this version's.
pub fn ca_moves_current(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(CA_MOVES_MAGIC)
}

/// The table as the cache keeps it.
pub fn to_text(table: &BTreeMap<String, Vec<String>>) -> String {
    let mut s = format!("{CA_MOVES_MAGIC}\n");
    s.extend(table.iter().map(|(m, c)| format!("{m}\t{}\n", c.join("|"))));
    s
}

pub fn from_text(text: &str) -> BTreeMap<String, Vec<String>> {
    text.lines().filter_map(|l| l.split_once('\t')).map(|(m, c)| (m.to_string(), c.split('|').map(str::to_string).collect())).collect()
}

/// The build of a move's clips for a character holding a weapon or not.
pub fn pick_clip(clips: &[String], armed: bool) -> Option<&String> {
    let unarmed = |c: &&String| {
        let c = c.to_ascii_lowercase();
        c.contains("nowep") || c.contains("noweap") || c.contains("noweapon") || c.contains("_nw")
    };
    clips.iter().find(|c| unarmed(c) != armed).or_else(|| clips.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_find_their_clips() {
        let clips = [
            r"a\b\3rdge_fulbst_relaxguard_pirate_v04_loop_ak74.mab",
            r"a\b\3rdge_fulbst_relaxguard_pirate_v04_start_ak74.mab",
            r"a\b\3rdge_fulbst_relaxguard_pirate_v01_loop_nowep.mab",
            r"a\b\3rdge_fulbst_smoke_loop_nowep.mab",
        ];
        let t = match_moves(["stp_relaxguard_pirate_v04_wait", "stp_relaxguard_pirate_v04_start", "stp_smoke_loop", "stp_nothing_wait"], &clips);
        assert_eq!(t["stp_relaxguard_pirate_v04_wait"], vec![clips[0].to_string()]);
        assert_eq!(t["stp_relaxguard_pirate_v04_start"], vec![clips[1].to_string()]);
        assert_eq!(t["stp_smoke_loop"], vec![clips[3].to_string()]);
        assert!(!t.contains_key("stp_nothing_wait"));
        // A move named after its clips' folders.
        let folder = [
            r"a\guard_camps\pirate_comms\3rdge_fulbst_piratecomms_pirate_loop_ak74.mab",
            r"a\guard_camps\pirate_comms\3rdge_fulbst_piratecomms_pirate_end_ak74.mab",
        ];
        let f = match_moves(["stp_guardcampspiratecomms_loop"], &folder);
        assert_eq!(f["stp_guardcampspiratecomms_loop"], vec![folder[0].to_string()]);
        assert_eq!(split_move("stp_smoke_end"), ("smoke".into(), Phase::End));
        assert_eq!(from_text(&to_text(&t)), t);
    }
}
