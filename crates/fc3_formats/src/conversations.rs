//! Conversations (`dialog\conversationtable.dat`): the lines pirates and
//! privateers (and others) say to each other or at an encounter's victims.
//! Each conversation has a name (`PirateCivExecute_01`; its CRC-32 is the
//! `AnimTemplate` a scene names it by), which speaker starts, and numbered
//! lines; each line has a variant (or several) per voice
//! (`DialogActors.PIRATE_01`), its sound event. The cache keeps them as
//! text ([`CONVERSATIONS_REL`]): by name, the first speaker and the lines'
//! variants.

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const CONVERSATION_TABLE: &str = r"dialog\conversationtable.dat";
pub const CONVERSATIONS_REL: &str = "dialog/conversations.txt";
const MAGIC: &str = "fc3-conversations 1";

/// The fields the table names by hash: a conversation's name, a line
/// variant's speaker.
const NAME: u32 = 0xAB29_2DF5;
const ACTOR: u32 = 0xD300_C3A0;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Conversation {
    pub name: String,
    /// Which speaker says the first line (0 or 1); they take turns.
    pub first_speaker: u8,
    /// Each line's variants: (voice, lower case as the bark voices are
    /// named, `pirate_01`; the sound event).
    pub lines: Vec<Vec<(String, u32)>>,
}

fn u32_field(o: &Object, name: &str) -> Option<u32> {
    let b = o.field(name)?;
    match b.len() {
        4 => Some(u32::from_le_bytes(b.try_into().ok()?)),
        1 => Some(b[0] as u32),
        _ => None,
    }
}

/// The table's conversations.
pub fn conversations(root: &Object) -> Vec<Conversation> {
    let mut out = Vec::new();
    root.walk(&mut |o, _| {
        if !o.is("Conversation") {
            return;
        }
        let Some(name) = o.str_h(NAME) else { return };
        let mut lines: Vec<(u32, Vec<(String, u32)>)> = Vec::new();
        for list in o.children_named("ConversationLineList") {
            for line in list.children_named("ConversationLine") {
                let number = u32_field(line, "Number").unwrap_or(lines.len() as u32);
                let mut variants = Vec::new();
                line.walk(&mut |a, _| {
                    if a.is("ActorData") {
                        let voice = a.str_h(ACTOR).and_then(|s| s.rsplit('.').next().map(str::to_ascii_lowercase));
                        if let (Some(voice), Some(sound)) = (voice, u32_field(a, "ConversationSndId").filter(|s| *s != 0 && *s != u32::MAX)) {
                            variants.push((voice, sound));
                        }
                    }
                });
                lines.push((number, variants));
            }
        }
        lines.sort_by_key(|(n, _)| *n);
        out.push(Conversation { name, first_speaker: u32_field(o, "selFirstSpeaker").unwrap_or(0).min(1) as u8, lines: lines.into_iter().map(|(_, v)| v).collect() });
    });
    out
}

pub fn to_text(list: &[Conversation]) -> String {
    let mut s = format!("{MAGIC}\n");
    for c in list {
        let lines: Vec<String> = c.lines.iter().map(|l| l.iter().map(|(v, id)| format!("{v}:{id}")).collect::<Vec<_>>().join(",")).collect();
        s += &format!("{}\t{}\t{}\n", c.name, c.first_speaker, lines.join(";"));
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<Conversation>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a conversations table");
    }
    Ok(lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let lines = f
                .get(2)?
                .split(';')
                .map(|line| line.split(',').filter_map(|v| v.split_once(':')).filter_map(|(voice, id)| Some((voice.to_string(), id.parse().ok()?))).collect())
                .collect();
            Some(Conversation { name: f.first()?.to_string(), first_speaker: f.get(1)?.parse().ok()?, lines })
        })
        .collect())
}

/// A conversation's line for a voice: one of its variants in that voice
/// (by a roll in 0..1), else any.
pub fn line_for<'a>(line: &'a [(String, u32)], voice: &str, roll: f32) -> Option<u32> {
    let own: Vec<&'a (String, u32)> = line.iter().filter(|(v, _)| v == voice).collect();
    let pool: Vec<&'a (String, u32)> = if own.is_empty() { line.iter().collect() } else { own };
    pool.get((roll * pool.len() as f32) as usize % pool.len().max(1)).map(|(_, id)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_round_trip_and_lines_go_by_voice() {
        let list = vec![Conversation {
            name: "Talk_01".into(),
            first_speaker: 1,
            lines: vec![vec![("pirate_01".into(), 10), ("pirate_02".into(), 20), ("pirate_02".into(), 21)], vec![("pirate_03".into(), 30)]],
        }];
        let back = from_text(&to_text(&list)).unwrap();
        assert_eq!(back, list);
        assert_eq!(line_for(&list[0].lines[0], "pirate_01", 0.9), Some(10));
        assert_eq!(line_for(&list[0].lines[0], "pirate_02", 0.9), Some(21));
        // A voice the line lacks: any of its variants.
        assert!(line_for(&list[0].lines[1], "pirate_01", 0.2) == Some(30));
    }
}
