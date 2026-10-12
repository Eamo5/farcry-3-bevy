//! The engine's input action maps (`actionmaps\inputactionmap*.xml`): named
//! sets of the signals the player's controls can send. A map imports others
//! and adds its own actions, then masks some out; scripts push and pop maps
//! (`CAPI_Player.PushPlayerActionMap`) to narrow what the player can do (a
//! narrative scene takes away moving and shooting). The converter keeps, for
//! each map, the signals it lets through ([`ACTION_MAPS_REL`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::xml::Element;

/// The action map files, the shared one first.
pub const SOURCES: [&str; 2] = [r"actionmaps\inputactionmapcommon.xml", r"actionmaps\inputactionmapsingle.xml"];

/// Maps and their signals, in the cache: `map<TAB>signal signal ...`.
pub const ACTION_MAPS_REL: &str = "story/actionmaps.txt";

/// Each map's signals (by name), from the files' text, imports followed and
/// masks applied. A later file's map replaces an earlier one's of the name.
pub fn resolve(texts: &[&str]) -> BTreeMap<String, BTreeSet<String>> {
    let mut maps: BTreeMap<String, Element> = BTreeMap::new();
    for text in texts {
        let Some(root) = crate::xml::parse(text) else { continue };
        root.walk(&mut |e| {
            if e.name == "ActionMap" {
                if let Some(name) = e.attr("name") {
                    maps.insert(name.to_string(), e.clone());
                }
            }
        });
    }
    maps.keys().map(|name| (name.clone(), signals(&maps, name, &mut Vec::new()))).collect()
}

fn signals(maps: &BTreeMap<String, Element>, name: &str, seen: &mut Vec<String>) -> BTreeSet<String> {
    let Some(map) = maps.get(name).filter(|_| !seen.iter().any(|s| s == name)) else { return BTreeSet::new() };
    seen.push(name.to_string());
    let mut out = BTreeSet::new();
    for c in map.children.iter().filter(|c| c.name == "Import") {
        if let Some(other) = c.attr("actionmap") {
            out.extend(signals(maps, other, seen));
        }
    }
    map.walk(&mut |e| {
        if e.name != "MaskOut" {
            if let Some(s) = e.attr("signal") {
                out.insert(s.to_string());
            }
        }
    });
    for c in map.children.iter().filter(|c| c.name == "MaskOut") {
        if let Some(s) = c.attr("signal") {
            out.remove(s);
        }
    }
    seen.pop();
    out
}

pub fn to_text(maps: &BTreeMap<String, BTreeSet<String>>) -> String {
    maps.iter().map(|(m, s)| format!("{m}\t{}\n", s.iter().cloned().collect::<Vec<_>>().join(" "))).collect()
}

pub fn from_text(text: &str) -> BTreeMap<String, BTreeSet<String>> {
    text.lines().filter_map(|l| l.split_once('\t')).map(|(m, s)| (m.to_string(), s.split_whitespace().map(str::to_string).collect())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_import_add_and_mask_signals() {
        let common = r#"<ActionMaps><ActionMap name="walking"><Action><Key signal="stroll"/></Action></ActionMap></ActionMaps>"#;
        let single = r#"<ActionMaps>
            <ActionMap name="all"><Import actionmap="walking"/><Import actionmap="missing" optional=""/>
                <Action><Key signal="wave"/><Key signal="nod"/></Action></ActionMap>
            <ActionMap name="quiet"><Import actionmap="all"/><MaskOut signal="wave"/></ActionMap>
            <ActionMap name="loop"><Import actionmap="loop"/><Action><Key signal="spin"/></Action></ActionMap>
            </ActionMaps>"#;
        let maps = resolve(&[common, single]);
        let set = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(maps["all"], set(&["nod", "stroll", "wave"]));
        assert_eq!(maps["quiet"], set(&["nod", "stroll"]));
        assert_eq!(maps["loop"], set(&["spin"]));
        assert_eq!(from_text(&to_text(&maps)), maps);
    }
}
