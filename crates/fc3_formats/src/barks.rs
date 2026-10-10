//! What the enemies say, by voice and stimulus. Each voice (`dialog\<voice>\`)
//! has a stimulus resource per situation (`stimuli\reflex_threatseen`, ...)
//! naming the barks it may play, and the single-player bark manager
//! (`dialog\spbarkmanager.xml`) gives each bark's voiced variations. The cache
//! keeps the joined table ([`BARKS_REL`]): per voice and stimulus, the sound
//! ids it may play and how long each lasts.

use std::collections::HashMap;

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const BARKS_REL: &str = "dialog/barks.txt";
pub const BARK_MANAGER: &str = r"dialog\spbarkmanager.xml";
const MAGIC: &str = "fc3-barks 1";

/// One voice's barks for one stimulus.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Barks {
    pub voice: String,
    pub stimulus: String,
    /// (sound id, seconds).
    pub lines: Vec<(u32, f32)>,
}

/// Bark id -> its variations (sound id, seconds), from the bark manager.
pub fn bark_variations(xml: &str) -> HashMap<u32, Vec<(u32, f32)>> {
    let mut out: HashMap<u32, Vec<(u32, f32)>> = HashMap::new();
    let mut current: Option<u32> = None;
    let attr = |tag: &str, name: &str| -> Option<String> {
        let key = format!("{name}=\"");
        let at = tag.find(&key)? + key.len();
        Some(tag[at..].split('"').next()?.to_string())
    };
    for tag in xml.split('<').skip(1) {
        let tag = tag.split('>').next().unwrap_or_default();
        if tag.starts_with("Bark ") {
            current = attr(tag, "id").and_then(|v| v.parse().ok());
        } else if tag.starts_with("Variation ") {
            if let (Some(id), Some(sound)) = (current, attr(tag, "soundId").and_then(|v| v.parse().ok())) {
                let seconds = attr(tag, "duration").and_then(|v| v.parse().ok()).unwrap_or(1.0);
                out.entry(id).or_default().push((sound, seconds));
            }
        }
    }
    out
}

/// The bark ids a stimulus resource lists.
pub fn stimulus_lines(resource: &Object) -> Vec<u32> {
    resource
        .child("StateLines")
        .iter()
        .flat_map(|l| l.children_named("StateLine"))
        .filter_map(|l| {
            let b = l.field("id")?;
            (b.len() <= 4 && !b.is_empty()).then(|| b.iter().rev().fold(0u32, |v, &x| (v << 8) | x as u32))
        })
        .collect()
}

pub fn to_text(barks: &[Barks]) -> String {
    let mut s = format!("{MAGIC}\n");
    for b in barks {
        let lines = b.lines.iter().map(|(id, secs)| format!("{id}:{secs}")).collect::<Vec<_>>().join(",");
        s += &format!("{}\t{}\t{lines}\n", b.voice, b.stimulus);
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<Barks>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a bark table");
    }
    Ok(lines
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (voice, stimulus, list) = (f.next()?, f.next()?, f.next()?);
            let lines = list
                .split(',')
                .filter_map(|p| {
                    let (id, secs) = p.split_once(':')?;
                    Some((id.parse().ok()?, secs.parse().ok()?))
                })
                .collect();
            Some(Barks { voice: voice.into(), stimulus: stimulus.into(), lines })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_bark_manager_and_round_trips() {
        let xml = r#"<BarkManager><BarkVariations><Bark id="7" fallbackBark="-1"><Variation soundId="100" duration="1.5" debugSubtitle="x"/><Variation soundId="101" duration="2"/></Bark><Bark id="8"/></BarkVariations></BarkManager>"#;
        let v = bark_variations(xml);
        assert_eq!(v[&7], vec![(100, 1.5), (101, 2.0)]);
        assert!(!v.contains_key(&8));
        let b = vec![Barks { voice: "pirate_01".into(), stimulus: "reflex_threatseen".into(), lines: vec![(100, 1.5)] }];
        assert_eq!(from_text(&to_text(&b)).unwrap(), b);
    }
}
