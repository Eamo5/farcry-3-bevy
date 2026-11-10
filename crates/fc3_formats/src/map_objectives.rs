//! The kinds of map markers (`generated\databases\generic\mapobjectivetype.lib`):
//! for each, by the key a marker names it with (`mapobjtypeMapObjectiveType`),
//! where it shows (the HUD in the world, the minimap, the map), how near it
//! must be to show in the world or on the minimap, its order among others,
//! and the clip the world's indicator plays for it (`Marker3DUILabel`, a
//! frame label of `ObjectiveIndicator.feu`'s indicators); its sub-kinds
//! (`MapObjectiveSubType`) may play their own. The cache keeps them as text
//! ([`MAP_OBJECTIVES_REL`]).

use anyhow::{bail, Result};

use crate::fcb::Object;

pub const MAP_OBJECTIVES_REL: &str = "ui/map_objective_types.txt";
pub const SOURCE: &str = r"generated\databases\generic\mapobjectivetype.lib";
const MAGIC: &str = "fc3-map-objectives 1";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapObjectiveType {
    /// The key markers name it by (a hash of its name).
    pub key: u64,
    pub name: String,
    pub on_hud: bool,
    pub on_minimap: bool,
    pub on_map: bool,
    /// How near (m) it shows in the world, on the minimap (0: however far).
    pub hud_within: f32,
    pub minimap_within: f32,
    /// Its order among others (lower first).
    pub priority: u32,
    /// The world indicator's clip (empty: none of its own).
    pub label: String,
    pub subtypes: Vec<SubType>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubType {
    pub id: u64,
    pub name: String,
    pub label: String,
    pub priority: u32,
}

impl MapObjectiveType {
    /// The world indicator's clip for a marker of this kind and sub-kind (by
    /// id, or a script's name for it).
    pub fn label_for(&self, sub_id: u64, sub_name: &str) -> &str {
        self.subtypes
            .iter()
            .find(|s| (!sub_name.is_empty() && s.name.eq_ignore_ascii_case(sub_name)) || (sub_id != 0 && s.id == sub_id))
            .map(|s| s.label.as_str())
            .filter(|l| !l.is_empty())
            .unwrap_or(&self.label)
    }

    /// Its order for a sub-kind (the sub-kind's own where it has one).
    pub fn priority_for(&self, sub_id: u64) -> u32 {
        self.subtypes.iter().find(|s| sub_id != 0 && s.id == sub_id).map(|s| s.priority).filter(|p| *p != 0).unwrap_or(self.priority)
    }
}

/// A text field of the library, empty when it holds a bare zero.
fn text(o: &Object, name: &str) -> String {
    o.field(name).filter(|b| b.len() > 1).map(crate::fcb::bytes_to_string).unwrap_or_default()
}

fn number(o: &Object, name: &str) -> f32 {
    o.f32(name).unwrap_or(0.0)
}

/// The kinds from the library's root object.
pub fn from_library(root: &Object) -> Vec<MapObjectiveType> {
    let mut out = Vec::new();
    for t in &root.children {
        let Some(key) = t.u64("hidKey") else { continue };
        let subtypes = t
            .child("MapObjectiveSubTypesList")
            .map(|l| {
                l.children
                    .iter()
                    .filter_map(|s| {
                        Some(SubType { id: s.u64("disSubTypeId")?, name: text(s, "Name"), label: text(s, "Marker3DUILabel"), priority: s.u32("DisplayPriority").unwrap_or(0) })
                    })
                    .filter(|s| !s.name.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        out.push(MapObjectiveType {
            key,
            name: text(t, "hidName"),
            on_hud: t.bool("DisplayOnHUD").unwrap_or(false),
            on_minimap: t.bool("DisplayOnMinimap").unwrap_or(false),
            on_map: t.bool("DisplayOnMap").unwrap_or(false),
            hud_within: number(t, "DisplayDistanceOn3DWorld"),
            minimap_within: number(t, "DisplayDistanceOnMinimap"),
            priority: t.u32("DisplayPriority").unwrap_or(0),
            label: text(t, "Marker3DUILabel"),
            subtypes,
        });
    }
    out.sort_by_key(|t| t.key);
    out
}

/// One line a kind (`T`, tab-separated), then one a sub-kind (`S`).
pub fn to_text(types: &[MapObjectiveType]) -> String {
    let mut s = format!("{MAGIC}\n");
    for t in types {
        s += &format!(
            "T\t{:#x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            t.key,
            t.name,
            u8::from(t.on_hud),
            u8::from(t.on_minimap),
            u8::from(t.on_map),
            t.hud_within,
            t.minimap_within,
            t.priority,
            t.label
        );
        for st in &t.subtypes {
            s += &format!("S\t{:#x}\t{}\t{}\t{}\n", st.id, st.name, st.label, st.priority);
        }
    }
    s
}

pub fn from_text(text: &str) -> Result<Vec<MapObjectiveType>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(MAGIC) {
        bail!("not a map objectives table");
    }
    let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0);
    let mut out: Vec<MapObjectiveType> = Vec::new();
    for line in lines {
        let f: Vec<&str> = line.split('\t').collect();
        match f.first() {
            Some(&"T") if f.len() >= 10 => out.push(MapObjectiveType {
                key: hex(f[1]),
                name: f[2].to_string(),
                on_hud: f[3] == "1",
                on_minimap: f[4] == "1",
                on_map: f[5] == "1",
                hud_within: f[6].parse().unwrap_or(0.0),
                minimap_within: f[7].parse().unwrap_or(0.0),
                priority: f[8].parse().unwrap_or(0),
                label: f[9].to_string(),
                subtypes: Vec::new(),
            }),
            Some(&"S") if f.len() >= 5 => {
                if let Some(t) = out.last_mut() {
                    t.subtypes.push(SubType { id: hex(f[1]), name: f[2].to_string(), label: f[3].to_string(), priority: f[4].parse().unwrap_or(0) });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kind and its sub-kinds come back from text as they went, and a
    /// sub-kind's own clip, by id or by a script's name for it, wins over
    /// the kind's.
    #[test]
    fn kinds_round_trip_and_sub_kinds_pick_their_clip() {
        let giver = MapObjectiveType {
            key: 0x7d8e_4092,
            name: "FC3/QuestGiver".into(),
            on_hud: true,
            on_minimap: true,
            on_map: true,
            hud_within: 50.0,
            minimap_within: 150.0,
            priority: 30,
            label: "SidequestGiverIndicator".into(),
            subtypes: vec![
                SubType { id: 0x703_82fb_b3bf, name: "HuntingQuest".into(), label: "HuntingIndicator".into(), priority: 33 },
                SubType { id: 0x6f6_b3f3_2506, name: "Quest".into(), label: String::new(), priority: 31 },
            ],
        };
        let back = from_text(&to_text(std::slice::from_ref(&giver))).unwrap();
        assert_eq!(back, vec![giver.clone()]);
        assert_eq!(giver.label_for(0x703_82fb_b3bf, ""), "HuntingIndicator");
        assert_eq!(giver.label_for(0, "huntingquest"), "HuntingIndicator");
        assert_eq!(giver.label_for(0x6f6_b3f3_2506, ""), "SidequestGiverIndicator");
        assert_eq!(giver.priority_for(0x703_82fb_b3bf), 33);
        assert_eq!(giver.priority_for(0), 30);
    }
}
